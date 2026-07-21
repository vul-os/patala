//! Wire shapes and MAJOR-unit amount handling for the Mercado Pago adapter
//! -- ported from cackle's `internal/payments/mercadopago.go`.
//!
//! Doc sources (cited verbatim from mercadopago.go; confidence rated by
//! cackle's own author as "MEDIUM-HIGH on the webhook signature manifest
//! template... MEDIUM on the Preferences API request/response shape", not
//! re-verified from this environment -- see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note):
//! - Preferences (Checkout Pro): <https://www.mercadopago.com/developers/en/reference/preferences/_checkout_preferences/post>
//! - Webhook signature: <https://www.mercadopago.com/developers/en/docs/checkout-api/additional-content/security/signature>
//!
//! ## HONESTY note: Mercado Pago is a MAJOR-unit, JSON-NUMBER provider
//!
//! Unlike Stripe/Paystack/Adyen/Checkout.com (all integer minor units on
//! the wire), Mercado Pago's `unit_price` (Preferences) and
//! `transaction_amount` (Payments) fields are **JSON numbers in MAJOR
//! units** (e.g. `100.50` meaning $100.50) -- exactly the provider
//! `PORTING.md` §8 names by name as diverging from the Stripe/Paystack
//! convention. Per that binding rule, every conversion here routes through
//! `crate::currency::minor_to_major_string`/`major_string_to_minor` -- never
//! a hardcoded `/100`/`*100` -- exactly as cackle's own `mercadopago.go`
//! routes through `internal/payments/currency.go`'s equivalent helpers.
//!
//! **A second, more specific honesty note, since the wire format here is a
//! JSON NUMBER, not a string**: `PORTING.md` §8's "never a float, anywhere"
//! rule is about never doing money ARITHMETIC in floating point (a
//! `*100`/`/100` in `f64`). Mercado Pago's own actual documented API
//! contract requires a bare JSON number for `unit_price`, not a quoted
//! string -- there is no way to avoid a JSON number appearing on the wire
//! for this specific provider, and cackle's own adapter has the identical
//! constraint (`strconv.ParseFloat` into a `float64` for the outbound
//! request, `strconv.FormatFloat` back out for the inbound response). This
//! port narrows the float exposure to the absolute minimum: money is
//! converted to/from its exact DECIMAL STRING via `crate::currency` (no
//! arithmetic in float), and only `serde_json::Number::from_str`/
//! `.to_string()` -- a single parse/format round-trip through the
//! underlying JSON number representation, never a multiply or divide -- is
//! used to cross the wire boundary itself. This is the same unavoidable
//! compromise cackle's own adapter makes for this specific provider, not a
//! departure from it.
#![allow(dead_code)]

use std::str::FromStr;

use patala_core::Error;

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("mercadopago: malformed API response: {detail}"))
}

/// Mirrors cackle's `Begin`'s conversion of `o.AmountMinor` into a JSON
/// number for `unit_price`: `minorToMajorString` then `strconv.ParseFloat`.
/// See module doc comment's HONESTY note on why the float round-trip here
/// is unavoidable at Mercado Pago's own wire contract, not a departure from
/// cackle's identical constraint.
pub fn amount_minor_to_json_number(
    amount_minor: u64,
    currency: &str,
) -> Result<serde_json::Number, Error> {
    let major = crate::currency::minor_to_major_string(amount_minor, currency)
        .map_err(|e| Error::InvalidRequest(format!("mercadopago: {e}")))?;
    serde_json::Number::from_str(&major).map_err(|e| {
        Error::InvalidRequest(format!("mercadopago: unparseable amount {major:?}: {e}"))
    })
}

/// Mirrors cackle's `toResult`'s conversion of `pay.TransactionAmount`
/// (a `float64`) back into `amount_minor`: `strconv.FormatFloat(...,'f',-1,64)`
/// then `majorStringToMinor`. Takes a [`serde_json::Number`] (as
/// deserialized directly from the wire) rather than an `f64` argument, so no
/// additional float arithmetic happens in this port beyond the JSON
/// decoder's own unavoidable parse.
pub fn json_number_to_amount_minor(n: &serde_json::Number, currency: &str) -> Result<u64, Error> {
    crate::currency::major_string_to_minor(&n.to_string(), currency)
        .map_err(|e| malformed(&format!("transaction_amount: {e}")))
}

/// `POST /checkout/preferences` response -- mirrors cackle's anonymous
/// `Begin` response struct.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PreferenceResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "init_point")]
    pub init_point: String,
}

/// The subset of a Mercado Pago Payment object this adapter reads -- mirrors
/// cackle's `mercadoPagoPayment`.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Payment {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub status: String,
    // NOTE: no #[serde(default)] here -- serde_json::Number has no Default
    // impl, and a payment genuinely missing transaction_amount is itself
    // malformed input this port should fail closed on (a missing-field
    // deserialize error), not silently treat as zero.
    #[serde(rename = "transaction_amount")]
    pub transaction_amount: serde_json::Number,
    #[serde(default, rename = "currency_id")]
    pub currency_id: String,
    #[serde(default, rename = "external_reference")]
    pub external_reference: String,
    #[serde(default, rename = "date_approved")]
    pub date_approved: String,
}

/// The result of evaluating a [`Payment`] -- mirrors the shape cackle's
/// `toResult` builds into a `Result`.
pub struct PaymentOutcome {
    pub reference: String,
    /// `true` only for Mercado Pago's `status == "approved"` --
    /// `"rejected"`/`"cancelled"`/`"refunded"`/`"charged_back"`/
    /// `"pending"`/`"in_process"`/`"in_mediation"`/anything unrecognised is
    /// `false`, mirroring cackle's fail-closed default (cackle's own
    /// `switch` treats every one of those non-approved cases identically as
    /// `StatusFailed` -- there is no separate "still pending" signal this
    /// port could distinguish that cackle's own `Result` doesn't already
    /// collapse).
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
    pub event_id: String,
}

/// Mirrors cackle's `mercadoPagoPayment.toResult`: turns a Payment object
/// into a settlement outcome, failing closed on anything malformed or
/// ambiguous.
pub fn evaluate_payment(p: &Payment) -> Result<PaymentOutcome, Error> {
    if p.external_reference.is_empty() {
        return Err(malformed("missing external_reference"));
    }
    let currency = p.currency_id.trim().to_ascii_uppercase();
    let amount_minor = json_number_to_amount_minor(&p.transaction_amount, &currency)?;

    match p.status.as_str() {
        "approved" => {
            if amount_minor == 0 || p.id == 0 {
                return Err(malformed("approved with non-positive amount or no id"));
            }
            Ok(PaymentOutcome {
                reference: p.external_reference.clone(),
                settled: true,
                amount_minor,
                currency,
                event_id: p.id.to_string(),
            })
        }
        _ => Ok(PaymentOutcome {
            reference: p.external_reference.clone(),
            settled: false,
            amount_minor: 0,
            currency,
            event_id: p.id.to_string(),
        }),
    }
}

/// Mirrors cackle's inline `classifyMercadoPagoError` envelope:
/// `{"message":"..."}`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub message: String,
}

/// Mirrors cackle's `classifyMercadoPagoError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "mercadopago: unexpected API response status: http {status}: {msg}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/mercadopago_test.go.

    #[test]
    fn amount_minor_to_json_number_formats_major_units() {
        let n = amount_minor_to_json_number(10050, "ARS").unwrap();
        assert_eq!(n.to_string(), "100.5");
    }

    #[test]
    fn json_number_to_amount_minor_round_trips() {
        let n = serde_json::Number::from_str("100.50").unwrap();
        assert_eq!(json_number_to_amount_minor(&n, "ARS").unwrap(), 10050);
    }

    #[test]
    fn evaluate_payment_approved_is_settled() {
        let p = Payment {
            id: 123,
            status: "approved".to_string(),
            transaction_amount: serde_json::Number::from_str("100.50").unwrap(),
            currency_id: "ARS".to_string(),
            external_reference: "ord_1".to_string(),
            date_approved: String::new(),
        };
        let outcome = evaluate_payment(&p).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 10050);
        assert_eq!(outcome.currency, "ARS");
    }

    #[test]
    fn evaluate_payment_rejected_is_not_settled() {
        let p = Payment {
            id: 1,
            status: "rejected".to_string(),
            transaction_amount: serde_json::Number::from_str("100.50").unwrap(),
            currency_id: "ARS".to_string(),
            external_reference: "ord_1".to_string(),
            date_approved: String::new(),
        };
        let outcome = evaluate_payment(&p).unwrap();
        assert!(!outcome.settled);
    }

    #[test]
    fn evaluate_payment_unknown_status_fails_closed() {
        let p = Payment {
            id: 1,
            status: "some-new-status".to_string(),
            transaction_amount: serde_json::Number::from_str("100.50").unwrap(),
            currency_id: "ARS".to_string(),
            external_reference: "ord_1".to_string(),
            date_approved: String::new(),
        };
        let outcome = evaluate_payment(&p).unwrap();
        assert!(!outcome.settled);
    }
}
