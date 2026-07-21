//! Wire shapes for the iyzico adapter — ported from cackle's
//! `internal/payments/iyzico.go`.
//!
//! Reference: <https://docs.iyzico.com/en/checkout-form> (Checkout Form
//! initialize + retrieve). Not re-verified live from this environment —
//! see `mod.rs`'s "UNVERIFIED AGAINST LIVE" / SPLIT-confidence disclosure.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `iyzicoCheckoutFormResult` — the shape returned by
/// `retrieveCheckoutForm` (used both by the poll path and, since iyzico's
/// callback carries no verifiable data of its own, the webhook path too —
/// see `webhook.rs`'s module docs).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct IyzicoCheckoutFormResult {
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "paymentStatus")]
    pub payment_status: String,
    #[serde(default)]
    pub token: String,
    #[serde(default, rename = "paymentId")]
    pub payment_id: String,
    #[serde(default, rename = "paidPrice")]
    pub paid_price: String,
    #[serde(default)]
    pub currency: String,
    #[serde(default, rename = "basketId")]
    pub basket_id: String,
    #[serde(default, rename = "errorMessage")]
    pub error_message: String,
}

/// The settlement outcome of evaluating an [`IyzicoCheckoutFormResult`] —
/// mirrors the fields cackle's `toResult` populates on `Result`.
pub struct CheckoutOutcome {
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `iyzicoCheckoutFormResult.toResult`: a top-level
/// `status != "success"` (a transport-level API failure, e.g. bad request
/// or invalid token — distinct from a legitimate `paymentStatus=FAILURE`
/// business outcome) is an `Err`; the caller (`rail.rs`) squashes this into
/// `Ok(false)` at the trait boundary, exactly like every other adapter's
/// model-evaluation function in this crate (see `stripe::models::evaluate_session`).
pub fn evaluate_checkout_form(res: &IyzicoCheckoutFormResult) -> Result<CheckoutOutcome, Error> {
    if res.status != "success" {
        let msg = if res.error_message.is_empty() {
            format!("status={:?}", res.status)
        } else {
            res.error_message.clone()
        };
        return Err(unexpected_status(&msg));
    }
    let currency = res.currency.trim().to_ascii_uppercase();
    let amount_minor = if res.paid_price.is_empty() {
        0
    } else {
        crate::currency::major_string_to_minor(&res.paid_price, &currency)
            .map_err(|e| malformed(&format!("paidPrice {:?}: {e}", res.paid_price)))?
    };

    match res.payment_status.as_str() {
        "SUCCESS" => {
            if amount_minor == 0 || res.payment_id.is_empty() {
                return Err(malformed(
                    "SUCCESS status with non-positive amount or no paymentId",
                ));
            }
            Ok(CheckoutOutcome {
                event_id: res.payment_id.clone(),
                settled: true,
                amount_minor,
                currency,
            })
        }
        // "FAILURE" | anything else: fail closed.
        _ => Ok(CheckoutOutcome {
            event_id: res.payment_id.clone(),
            settled: false,
            amount_minor: 0,
            currency,
        }),
    }
}

/// Mirrors cackle's `classifyIyzicoError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct Env {
        #[serde(default, rename = "errorMessage")]
        error_message: String,
    }
    let env: Env = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.error_message.is_empty() {
        "no message".to_string()
    } else {
        env.error_message
    };
    Error::Rail(format!(
        "iyzico: unexpected API response status: http {status}: {msg}"
    ))
}

/// Content-level "iyzico's own API says this request failed" — mirrors
/// cackle's `ErrIyzicoUnexpectedStatus`.
pub fn unexpected_status(msg: &str) -> Error {
    Error::Rail(format!("iyzico: unexpected API response status: {msg}"))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("iyzico: malformed API response: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_status_and_payment_status_settles() {
        let res = IyzicoCheckoutFormResult {
            status: "success".into(),
            payment_status: "SUCCESS".into(),
            token: "tok_abc".into(),
            payment_id: "pay_1".into(),
            paid_price: "100.00".into(),
            currency: "TRY".into(),
            basket_id: "ord_1".into(),
            error_message: String::new(),
        };
        let outcome = evaluate_checkout_form(&res).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 10000);
        assert_eq!(outcome.currency, "TRY");
        assert_eq!(outcome.event_id, "pay_1");
    }

    #[test]
    fn failure_payment_status_is_not_settled() {
        let res = IyzicoCheckoutFormResult {
            status: "success".into(),
            payment_status: "FAILURE".into(),
            token: "tok_abc".into(),
            currency: "TRY".into(),
            ..Default::default()
        };
        let outcome = evaluate_checkout_form(&res).unwrap();
        assert!(!outcome.settled);
        assert_eq!(outcome.amount_minor, 0);
    }

    #[test]
    fn api_level_failure_status_is_an_error() {
        let res = IyzicoCheckoutFormResult {
            status: "failure".into(),
            error_message: "token not found".into(),
            ..Default::default()
        };
        assert!(evaluate_checkout_form(&res).is_err());
    }
}
