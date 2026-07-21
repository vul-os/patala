//! Wire shapes and the three-decimal-currency refusal for the Square
//! adapter — ported from cackle's `internal/payments/square.go`.
//!
//! Doc sources (as cackle's own file header states, "verified live against
//! developer.squareup.com" by cackle's author; NOT re-verified from this
//! environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note):
//! - Payment Links (Checkout API): <https://developer.squareup.com/reference/square/checkout-api/create-payment-link>
//! - Money object: <https://developer.squareup.com/reference/square/objects/Money>
//! - Payment object / statuses: <https://developer.squareup.com/reference/square/objects/Payment>
//!
//! **HONESTY note (mirrors cackle's file-header HONESTY note 2 verbatim):**
//! cackle's author could not confirm Square's exact minor-unit handling for
//! ISO-4217 three-decimal currencies (KWD etc) or any Square-specific
//! zero-decimal exception list beyond the single confirmed JPY example.
//! Three-decimal currencies are refused rather than guessed at, matching
//! this crate's pattern elsewhere (see `stripe::models`'s own three-decimal
//! refusal).
#![allow(dead_code)]

use patala_core::Error;

/// Mirrors cackle's `squareThreeDecimalCurrencies` verbatim.
const SQUARE_THREE_DECIMAL: &[&str] = &["KWD", "BHD", "JOD", "OMR", "TND"];

fn contains(list: &[&str], code: &str) -> bool {
    list.iter().any(|c| c.eq_ignore_ascii_case(code))
}

/// Mirrors cackle's `squareAmount`: validates and passes through
/// `amount_minor` for Square's `Money.amount` field. Square's Money object
/// documents `amount` as "the smallest denomination of the currency"
/// (matching ISO-4217 minor units) with JPY confirmed as a zero-decimal
/// example -- the same convention this crate's own `PayRequest::amount_minor`
/// already uses, so ordinary AND zero-decimal currencies are a direct
/// passthrough, unlike Stripe's ISK/UGX forced-two-decimal quirk (which does
/// not apply here). Three-decimal currencies are refused (see module docs).
pub fn square_amount(amount_minor: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(SQUARE_THREE_DECIMAL, &cur) {
        return Err(unsupported_currency(&cur));
    }
    Ok(amount_minor)
}

/// Mirrors cackle's `ErrSquareUnsupportedCurrency`.
pub fn unsupported_currency(cur: &str) -> Error {
    Error::InvalidRequest(format!(
        "square: {cur} is a three-decimal ISO-4217 currency, not verified against Square's documented Money semantics; refusing rather than guessing"
    ))
}

/// Mirrors cackle's `ErrSquareMalformedResponse`.
pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("square: malformed API response: {detail}"))
}

/// Square's documented error response shape:
/// `{"errors":[{"category","code","detail"}]}`. Mirrors cackle's
/// `squareErrorEnvelope`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SquareErrorEnvelope {
    #[serde(default)]
    pub errors: Vec<SquareErrorDetail>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SquareErrorDetail {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub detail: String,
}

/// Mirrors cackle's `classifySquareError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: SquareErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = match env.errors.first() {
        Some(e) => format!("{}: {}", e.code, e.detail),
        None => "no message".to_string(),
    };
    Error::Rail(format!(
        "square: unexpected API response status: http {status}: {msg}"
    ))
}

/// The subset of a Square Payment object
/// (<https://developer.squareup.com/reference/square/objects/Payment>) this
/// adapter reads -- mirrors cackle's `squarePaymentPayload`. Used both for
/// the Verify-path API response and the webhook-path event's nested
/// `data.object.payment`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SquarePaymentPayload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reference_id: String,
    #[serde(default)]
    pub order_id: String,
    #[serde(default)]
    pub amount_money: SquareAmountMoney,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SquareAmountMoney {
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub currency: String,
}

/// The result of evaluating a [`SquarePaymentPayload`] -- mirrors cackle's
/// `parseSquarePayment`.
pub struct SquareSettlementOutcome {
    pub reference: String,
    pub event_id: String,
    /// `true` only for Square's `status == "COMPLETED"` -- every other
    /// value (APPROVED, PENDING, CANCELED, FAILED, or anything
    /// unrecognised) is `false`, mirroring cackle's fail-closed default.
    pub settled: bool,
    /// `0` unless `settled` -- mirrors this crate's honest "`amount_minor`
    /// reports what ACTUALLY moved" convention.
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `parseSquarePayment`: turns a Payment payload into a
/// settlement outcome, failing closed on anything malformed or ambiguous.
pub fn parse_square_payment(p: &SquarePaymentPayload) -> Result<SquareSettlementOutcome, Error> {
    if p.id.is_empty() {
        return Err(malformed("missing payment id"));
    }
    if p.reference_id.is_empty() {
        return Err(malformed(
            "payment has no reference_id to reconcile against",
        ));
    }
    let currency = p.amount_money.currency.trim().to_ascii_uppercase();

    if p.status == "COMPLETED" {
        if p.amount_money.amount == 0 {
            return Err(malformed("status=COMPLETED with non-positive amount"));
        }
        Ok(SquareSettlementOutcome {
            reference: p.reference_id.clone(),
            event_id: p.id.clone(),
            settled: true,
            amount_minor: p.amount_money.amount,
            currency,
        })
    } else {
        // APPROVED | PENDING | CANCELED | FAILED | anything else: fail
        // closed -- never treated as paid.
        Ok(SquareSettlementOutcome {
            reference: p.reference_id.clone(),
            event_id: p.id.clone(),
            settled: false,
            amount_minor: 0,
            currency,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/square_test.go.

    #[test]
    fn ordinary_and_zero_decimal_pass_through() {
        assert_eq!(square_amount(5000, "USD").unwrap(), 5000);
        assert_eq!(square_amount(1000, "JPY").unwrap(), 1000);
    }

    #[test]
    fn three_decimal_currency_refused() {
        assert!(square_amount(1000, "KWD").is_err());
    }

    #[test]
    fn parse_square_payment_completed_settles() {
        let payload = SquarePaymentPayload {
            id: "pay_1".into(),
            status: "COMPLETED".into(),
            reference_id: "ord_1".into(),
            order_id: "ORDER1".into(),
            amount_money: SquareAmountMoney {
                amount: 5000,
                currency: "USD".into(),
            },
        };
        let outcome = parse_square_payment(&payload).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 5000);
        assert_eq!(outcome.currency, "USD");
        assert_eq!(outcome.reference, "ord_1");
    }

    #[test]
    fn parse_square_payment_pending_does_not_settle() {
        let payload = SquarePaymentPayload {
            id: "pay_1".into(),
            status: "PENDING".into(),
            reference_id: "ord_1".into(),
            order_id: "ORDER1".into(),
            amount_money: SquareAmountMoney {
                amount: 5000,
                currency: "USD".into(),
            },
        };
        let outcome = parse_square_payment(&payload).unwrap();
        assert!(!outcome.settled);
        assert_eq!(outcome.amount_minor, 0);
    }

    #[test]
    fn parse_square_payment_missing_reference_fails_closed() {
        let payload = SquarePaymentPayload {
            id: "pay_1".into(),
            status: "COMPLETED".into(),
            reference_id: String::new(),
            order_id: "ORDER1".into(),
            amount_money: SquareAmountMoney {
                amount: 5000,
                currency: "USD".into(),
            },
        };
        assert!(parse_square_payment(&payload).is_err());
    }
}
