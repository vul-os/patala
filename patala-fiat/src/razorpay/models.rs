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
/// a settlement outcome, failing closed on anything malformed or ambiguous.
///
/// **An absent `status` is not `"captured"`.** This used to open with cackle's
/// webhook-path tolerance — `if pay.Status == "" { pay.Status = "captured" }`,
/// on the reasoning that the `payment.captured` event name already asserts it
/// — which made "the field Razorpay uses to say whether money moved is
/// missing" read as "money moved". That is the one default a payments library
/// cannot have: a processor payload change, or an entity shape this adapter
/// has not seen, becomes a settlement nobody reported. `stripe::models`'
/// `evaluate_session` matches `"paid"` positively with no default arm at all,
/// and this now does the same. The event name still gates which deliveries
/// reach here (`webhook.rs` rejects anything but `payment.captured`); it no
/// longer supplies the settlement claim as well.
pub fn evaluate_payment(pay: &RazorpayPayment) -> Result<PaymentOutcome, Error> {
    if pay.order_id.is_empty() {
        return Err(malformed("missing order_id"));
    }
    // `id` becomes `WebhookEvent::event_id`, which is documented "Never empty:
    // a caller cannot suppress a duplicate it cannot name." It was checked only
    // inside the `"captured"` arm, so a signed `authorized`/`failed` entity
    // arrived with no dedup key. Required before the status is looked at.
    if pay.id.is_empty() {
        return Err(malformed(
            "no payment id: this entity carries no id to deduplicate on",
        ));
    }
    let currency = pay.currency.trim().to_ascii_uppercase();

    match pay.status.as_str() {
        "captured" => {
            if pay.amount == 0 {
                return Err(malformed("captured with non-positive amount"));
            }
            Ok(PaymentOutcome {
                order_id: pay.order_id.clone(),
                event_id: pay.id.clone(),
                settled: true,
                amount_minor: pay.amount,
                currency,
            })
        }
        // "" (absent) | "created" | "authorized" (not yet captured) |
        // "failed" | anything else: fail closed -- never treated as captured.
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

    /// This test asserted the OPPOSITE — `empty_status_defaults_to_captured`
    /// — and passing was the defect. An absent `status` is the field that
    /// reports whether money moved being missing; reading it as `"captured"`
    /// turns a payload shape this adapter has not seen into a settlement
    /// nobody reported. Restore the `if pay.status.is_empty() { "captured" }`
    /// rewrite and this reports: `an absent status was read as captured --
    /// settlement must be positively reported, never defaulted`.
    #[test]
    fn empty_status_is_not_captured() {
        let pay = RazorpayPayment {
            id: "pay_1".into(),
            order_id: "order_1".into(),
            amount: 5000,
            currency: "INR".into(),
            status: "".into(),
            created_at: 0,
        };
        let outcome = evaluate_payment(&pay).unwrap();
        assert!(
            !outcome.settled,
            "an absent status was read as captured -- settlement must be \
             positively reported, never defaulted"
        );
        assert_eq!(outcome.amount_minor, 0);
        assert_eq!(
            outcome.event_id, "pay_1",
            "and it is still a nameable event"
        );
    }

    /// `WebhookEvent::event_id` is documented "Never empty: a caller cannot
    /// suppress a duplicate it cannot name", and the payment `id` is where
    /// this rail's comes from. The check used to live inside the `"captured"`
    /// arm only. Delete the guard at the top of `evaluate_payment` and this
    /// reports: `authorized: reached Ok with event_id "" -- an entity with no
    /// payment id has no dedup key`.
    #[test]
    fn a_payment_entity_with_no_id_is_refused_whatever_its_status() {
        for status in ["captured", "authorized", "created", "failed", ""] {
            let pay = RazorpayPayment {
                id: String::new(),
                order_id: "order_1".into(),
                amount: 5000,
                currency: "INR".into(),
                status: status.into(),
                created_at: 0,
            };
            match evaluate_payment(&pay) {
                Err(e) => assert!(
                    e.to_string().contains("payment id"),
                    "{status:?}: refused, but not for the missing id: {e}"
                ),
                Ok(o) => panic!(
                    "{status:?}: reached Ok with event_id {:?} -- an entity with no \
                     payment id has no dedup key",
                    o.event_id
                ),
            }
        }
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
