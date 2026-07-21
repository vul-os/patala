//! Wire shapes for the Razorpay adapter — ported from cackle's
//! `internal/payments/razorpay.go`.
//!
//! Reference: <https://razorpay.com/docs/api/orders/> (Create an Order),
//! <https://razorpay.com/docs/api/payments/fetch-payments-for-order> (list
//! payments for an order — used by `verify()`). Not re-verified live from
//! this environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST
//! LIVE" note.
//!
//! Razorpay amounts are plain INTEGER MINOR UNITS on the wire (paise for
//! INR), matching `patala_core::PayRequest::amount_minor` directly — no
//! decimal-string conversion needed, same convention as Stripe/Paystack
//! (unlike PayU/Xendit, which this crate's `PORTING.md` §8 explicitly names
//! as decimal-major-unit-string providers).
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// The subset of a Razorpay Payment entity this adapter reads — mirrors
/// cackle's `razorpayPayment`, used both for the `verify()`-path API
/// response (`GET /orders/{id}/payments`'s `items[]`) and the
/// webhook-path event's nested `payload.payment.entity`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RazorpayPayment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub order_id: String,
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: i64,
}

/// The result of evaluating a [`RazorpayPayment`] — mirrors the shape
/// cackle's `razorpayPaymentToResult` builds into a `Result`, minus the
/// fields (`Provider`, `PaidAt`, `Raw`) this crate's callers attach
/// themselves.
pub struct PaymentOutcome {
    /// The Razorpay order id (`order_id`) — this rail's lookup key, NOT
    /// necessarily `patala_core::Receipt::reference` (see `rail.rs`'s
    /// module docs: `Receipt::reference` always echoes the caller's own
    /// `PayRequest::reference`; the Razorpay order id lives in `proof`
    /// instead).
    pub order_id: String,
    /// The Razorpay payment id -- preferred for webhook replay-dedup.
    pub event_id: String,
    /// `true` only for Razorpay's `status == "captured"` -- every other
    /// value (`"created"`, `"authorized"`, `"failed"`, or anything
    /// unrecognised) is `false`, mirroring cackle's fail-closed `default`
    /// case in `razorpayPaymentToResult`'s `switch`.
    pub settled: bool,
    /// `0` unless `settled` -- mirrors this crate's honest "`amount_minor`
    /// reports what ACTUALLY moved" convention.
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `razorpayPaymentToResult`: turns a payment entity into
/// a settlement outcome, failing closed on anything malformed or
/// ambiguous. If `status` is empty, it is treated as `"captured"` first
/// (mirrors cackle's webhook-path tolerance: `if pay.Status == "" { pay.Status
/// = "captured" }`, since the `payment.captured` event name itself already
/// asserts this — see `webhook.rs`, the only caller that can hit an empty
/// status; `verify()`'s own API response always has a real status).
pub fn evaluate_payment(pay: &RazorpayPayment) -> Result<PaymentOutcome, Error> {
    if pay.order_id.is_empty() {
        return Err(malformed("missing order_id"));
    }
    let currency = pay.currency.trim().to_ascii_uppercase();
    let status: &str = if pay.status.is_empty() {
        "captured"
    } else {
        pay.status.as_str()
    };

    match status {
        "captured" => {
            if pay.amount == 0 || pay.id.is_empty() {
                return Err(malformed(
                    "captured with non-positive amount or no payment id",
                ));
            }
            Ok(PaymentOutcome {
                order_id: pay.order_id.clone(),
                event_id: pay.id.clone(),
                settled: true,
                amount_minor: pay.amount,
                currency,
            })
        }
        // "created" | "authorized" (not yet captured) | "failed" | anything
        // else: fail closed -- never treated as captured.
        _ => Ok(PaymentOutcome {
            order_id: pay.order_id.clone(),
            event_id: pay.id.clone(),
            settled: false,
            amount_minor: 0,
            currency,
        }),
    }
}

/// Razorpay's documented error response shape:
/// `{"error":{"description":"..."}}`. Mirrors cackle's inline anonymous
/// struct in `classifyRazorpayError`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorBody {
    #[serde(default)]
    pub description: String,
}

/// Mirrors cackle's `classifyRazorpayError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.error.description.is_empty() {
        "no message".to_string()
    } else {
        env.error.description
    };
    Error::Rail(format!(
        "razorpay: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("razorpay: malformed API response: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_status_settles() {
        let pay = RazorpayPayment {
            id: "pay_1".into(),
            order_id: "order_1".into(),
            amount: 5000,
            currency: "inr".into(),
            status: "captured".into(),
            created_at: 0,
        };
        let outcome = evaluate_payment(&pay).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 5000);
        assert_eq!(outcome.currency, "INR");
    }

    #[test]
    fn non_captured_statuses_fail_closed() {
        for status in ["created", "authorized", "failed", "some-new-status"] {
            let pay = RazorpayPayment {
                id: "pay_1".into(),
                order_id: "order_1".into(),
                amount: 5000,
                currency: "INR".into(),
                status: status.into(),
                created_at: 0,
            };
            let outcome = evaluate_payment(&pay).unwrap();
            assert!(!outcome.settled, "{status}");
            assert_eq!(outcome.amount_minor, 0, "{status}");
        }
    }

    #[test]
    fn empty_status_defaults_to_captured() {
        let pay = RazorpayPayment {
            id: "pay_1".into(),
            order_id: "order_1".into(),
            amount: 5000,
            currency: "INR".into(),
            status: "".into(),
            created_at: 0,
        };
        let outcome = evaluate_payment(&pay).unwrap();
        assert!(outcome.settled);
    }

    #[test]
    fn missing_order_id_is_malformed() {
        let pay = RazorpayPayment {
            id: "pay_1".into(),
            order_id: "".into(),
            amount: 5000,
            currency: "INR".into(),
            status: "captured".into(),
            created_at: 0,
        };
        assert!(evaluate_payment(&pay).is_err());
    }
}
