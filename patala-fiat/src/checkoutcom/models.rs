//! Wire shapes and the currency-bucket amount handling for the Checkout.com
//! adapter — ported from cackle's `internal/payments/checkoutcom.go`.
//!
//! Doc sources (cited verbatim from checkoutcom.go, verified live against
//! checkout.com/docs by cackle's own author; not re-verified from this
//! environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note):
//! - Hosted Payments Page: <https://checkout.com/docs/payments/accept-payments/accept-a-payment-on-a-hosted-page/manage-your-hosted-payments-page>
//! - Get payment details: <https://checkout.com/docs/payments/manage-payments/get-payment-details>
//! - Webhooks setup: <https://checkout.com/docs/developer-resources/webhooks/manage-webhooks/set-up-your-webhook-receiver>
//! - Currency minor units: <https://checkout.com/docs/developer-resources/testing/codes/calculating-the-amount>
#![allow(dead_code)]

use patala_core::Error;

/// Mirrors cackle's `checkoutComZeroDecimalCurrencies` — informational only
/// (see [`checkout_com_amount`]'s doc comment: every bucket except
/// [`CHECKOUTCOM_FORCED_TWO_DECIMAL`] is a direct passthrough). **Includes
/// ISK**, which Stripe (`stripe::models::STRIPE_FORCED_TWO_DECIMAL`) treats
/// DIFFERENTLY (forced two-decimal, ×100) — providers genuinely disagree on
/// this currency, exactly as cackle's own file doc comment notes, which is
/// why this port keeps its own independent constant rather than sharing one
/// across adapters.
const CHECKOUTCOM_ZERO_DECIMAL: &[&str] = &[
    "BIF", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VUV", "VND", "XAF",
    "XOF", "XPF",
];

/// Mirrors cackle's `checkoutComThreeDecimalCurrencies` — informational
/// only, same reasoning as [`CHECKOUTCOM_ZERO_DECIMAL`]: these also pass
/// straight through (Checkout.com's `amount` field is already
/// minor-unit/1000ths for these, matching the plain ISO-4217 exponent).
const CHECKOUTCOM_THREE_DECIMAL: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

/// Mirrors cackle's `checkoutComForcedTwoDecimalCurrencies`: Checkout.com's
/// OWN documented special case for CLP — ISO-4217 gives CLP a zero exponent,
/// but Checkout.com's docs specifically note "the last two digits must be
/// 00" for CLP, i.e. it is sent as an ordinary ×100 amount restricted to
/// whole-peso values.
const CHECKOUTCOM_FORCED_TWO_DECIMAL: &[&str] = &["CLP"];

fn contains(list: &[&str], code: &str) -> bool {
    list.iter().any(|c| c.eq_ignore_ascii_case(code))
}

/// Mirrors cackle's `checkoutComAmount`: converts `amount_minor` into
/// Checkout.com's `amount` field. Zero-decimal, three-decimal, and the
/// ordinary two-decimal default are all a direct passthrough; only CLP is
/// multiplied by 100 (checked, overflow refused).
pub fn checkout_com_amount(amount_minor: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(CHECKOUTCOM_FORCED_TWO_DECIMAL, &cur) {
        return amount_minor.checked_mul(100).ok_or_else(|| {
            Error::InvalidRequest(format!(
                "checkoutcom: amount {amount_minor} overflows for {cur}"
            ))
        });
    }
    Ok(amount_minor)
}

/// Mirrors cackle's `checkoutComAmountToMinor`: the inverse of
/// [`checkout_com_amount`], used to convert a SETTLED amount Checkout.com
/// reports back into `amount_minor`.
pub fn checkout_com_amount_to_minor(amt: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(CHECKOUTCOM_FORCED_TWO_DECIMAL, &cur) {
        if !amt.is_multiple_of(100) {
            return Err(malformed(&format!(
                "{cur} amount {amt} is not a whole multiple of 100 as Checkout.com documents"
            )));
        }
        return Ok(amt / 100);
    }
    Ok(amt)
}

/// Mirrors cackle's `checkoutComErrorEnvelope`:
/// `{"request_id":"...","error_type":"...","error_codes":[...]}`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default, rename = "error_type")]
    pub error_type: String,
    #[serde(default, rename = "error_codes")]
    pub error_codes: Vec<String>,
}

/// Mirrors cackle's `classifyCheckoutComError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    Error::Rail(format!(
        "checkoutcom: unexpected API response status: http {status}: {} {:?}",
        env.error_type, env.error_codes
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("checkoutcom: malformed API response: {detail}"))
}

/// The subset of a Checkout.com Payment object (from `GET /payments/{id}` or
/// an event's `data`) this adapter reads — mirrors cackle's
/// `checkoutComPaymentPayload`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PaymentPayload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

/// The result of evaluating a [`PaymentPayload`] — mirrors the shape
/// cackle's `parseCheckoutComPayment` builds into a `Result`.
pub struct PaymentOutcome {
    pub reference: String,
    /// `true` only for Checkout.com's `status == "Captured"` — every other
    /// value (`"Authorized"`, `"Pending"`, `"Declined"`, or anything
    /// unrecognised) is `false`, mirroring cackle's fail-closed default.
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `parseCheckoutComPayment`: turns a Payment payload into
/// a settlement outcome, failing closed on anything malformed or ambiguous.
/// Only `"Captured"` is ever treated as settled — the confirmed status
/// values from Checkout.com's docs are Captured/Authorized/Pending/Declined
/// (not exhaustively confirmed, see module docs); anything else, known or
/// not, is not-settled rather than guessed as paid.
///
/// **Reconciliation key**: unlike cackle (whose own metadata key is
/// `cackle_reference`), this port follows this crate's own convention (see
/// `PORTING.md` §3): the metadata key is `patala_reference`, the same key
/// `stripe`/`paystack` set, so `charge()` must set
/// `metadata["patala_reference"] = req.reference` for this fallback to
/// work.
pub fn evaluate_payment(p: &PaymentPayload) -> Result<PaymentOutcome, Error> {
    if p.id.is_empty() {
        return Err(malformed("missing payment id"));
    }
    let reference = if !p.reference.is_empty() {
        p.reference.clone()
    } else if let Some(r) = p.metadata.get("patala_reference") {
        r.clone()
    } else {
        return Err(malformed(
            "payment has no reference or patala_reference metadata to reconcile against",
        ));
    };
    let currency = p.currency.trim().to_ascii_uppercase();

    if p.status == "Captured" {
        if p.amount == 0 {
            return Err(malformed("status=Captured with non-positive amount"));
        }
        let amount_minor = checkout_com_amount_to_minor(p.amount, &currency)?;
        Ok(PaymentOutcome {
            reference,
            settled: true,
            amount_minor,
            currency,
        })
    } else {
        Ok(PaymentOutcome {
            reference,
            settled: false,
            amount_minor: 0,
            currency,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/checkoutcom_test.go.

    #[test]
    fn ordinary_and_zero_decimal_pass_through() {
        assert_eq!(checkout_com_amount(5000, "USD").unwrap(), 5000);
        assert_eq!(checkout_com_amount(1000, "JPY").unwrap(), 1000);
        assert_eq!(
            checkout_com_amount(1000, "ISK").unwrap(),
            1000,
            "Checkout.com treats ISK as zero-decimal, unlike Stripe"
        );
    }

    #[test]
    fn clp_forced_two_decimal() {
        assert_eq!(checkout_com_amount(500, "CLP").unwrap(), 50000);
    }

    #[test]
    fn amount_to_minor_round_trips() {
        assert_eq!(checkout_com_amount_to_minor(50000, "CLP").unwrap(), 500);
        assert!(checkout_com_amount_to_minor(555, "CLP").is_err());
    }
}
