//! Wire shapes for the Midtrans adapter — ported from cackle's
//! `internal/payments/midtrans.go`.
//!
//! Reference: <https://docs.midtrans.com/reference/charge-transactions-1>
//! (Snap API), <https://docs.midtrans.com/reference/get-transaction-status>
//! (status check), <https://docs.midtrans.com/docs/https-notification-webhooks>
//! (webhook signature). Not re-verified live — see `mod.rs`'s "UNVERIFIED
//! AGAINST LIVE" note.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `midtransTransactionStatus` — used both by the
/// server-to-server status-check response (`Verify`) AND the webhook
/// notification body (they're the same shape, per cackle's own comment).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MidtransTransactionStatus {
    #[serde(default)]
    pub order_id: String,
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default)]
    pub transaction_status: String,
    #[serde(default)]
    pub fraud_status: String,
    #[serde(default)]
    pub gross_amount: String,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub status_code: String,
    #[serde(default)]
    pub signature_key: String,
    #[serde(default)]
    pub settlement_time: String,
    #[serde(default)]
    pub transaction_time: String,
}

impl MidtransTransactionStatus {
    /// Mirrors cackle's `midtransTransactionStatus.currency` method:
    /// Midtrans's status responses don't always echo a `currency` field
    /// back (it's IDR-only anyway) -- default to `"IDR"`.
    pub fn currency_or_idr(&self) -> String {
        if self.currency.is_empty() {
            "IDR".to_string()
        } else {
            self.currency.clone()
        }
    }
}

/// The settlement outcome of evaluating a [`MidtransTransactionStatus`].
pub struct StatusOutcome {
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `midtransTransactionStatus.toResult`: `"capture"` is
/// only a genuine settlement when `fraud_status == "accept"`;
/// `"settlement"` always settles; `"deny"`/`"cancel"`/`"expire"`/
/// `"failure"`/anything unrecognised (including `"pending"`) fail closed.
pub fn evaluate_status(s: &MidtransTransactionStatus) -> Result<StatusOutcome, Error> {
    if s.order_id.is_empty() {
        return Err(malformed("missing order_id"));
    }
    // `transaction_id` becomes `WebhookEvent::event_id`, which is documented
    // "Never empty: a caller cannot suppress a duplicate it cannot name." It
    // was checked only inside the two settling arms, so a signed
    // `transaction_status=deny` / `expire` notification — the ones Midtrans
    // actually redelivers — arrived with no dedup key. Required before the
    // status is looked at: the status is not what makes an event nameable.
    if s.transaction_id.is_empty() {
        return Err(malformed(
            "no transaction_id: this notification carries no id to deduplicate on",
        ));
    }
    let currency = s.currency_or_idr();
    let amount_minor = crate::currency::major_string_to_minor(&s.gross_amount, &currency)
        .map_err(|e| malformed(&format!("gross_amount {:?}: {e}", s.gross_amount)))?;

    match s.transaction_status.as_str() {
        "capture" => {
            if s.fraud_status != "accept" {
                return Ok(StatusOutcome {
                    event_id: s.transaction_id.clone(),
                    settled: false,
                    amount_minor: 0,
                    currency,
                });
            }
            if amount_minor == 0 {
                return Err(malformed("capture/accept with non-positive amount"));
            }
            Ok(StatusOutcome {
                event_id: s.transaction_id.clone(),
                settled: true,
                amount_minor,
                currency,
            })
        }
        "settlement" => {
            if amount_minor == 0 {
                return Err(malformed("settlement with non-positive amount"));
            }
            Ok(StatusOutcome {
                event_id: s.transaction_id.clone(),
                settled: true,
                amount_minor,
                currency,
            })
        }
        // "deny" | "cancel" | "expire" | "failure" | "pending" | anything
        // else: fail closed.
        _ => Ok(StatusOutcome {
            event_id: s.transaction_id.clone(),
            settled: false,
            amount_minor: 0,
            currency,
        }),
    }
}

/// Mirrors cackle's `classifyMidtransError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct Env {
        #[serde(default)]
        status_message: String,
    }
    let env: Env = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.status_message.is_empty() {
        "no message".to_string()
    } else {
        env.status_message
    };
    Error::Rail(format!(
        "midtrans: unexpected API response status: http {status}: {msg}"
    ))
}

/// Mirrors cackle's `ErrMidtransUnsupportedCurrency`.
pub fn unsupported_currency(got: &str) -> Error {
    Error::InvalidRequest(format!("midtrans: only IDR is supported, got {got:?}"))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("midtrans: malformed API response: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_settles() {
        let s = MidtransTransactionStatus {
            order_id: "ord_1".into(),
            transaction_id: "txn_1".into(),
            transaction_status: "settlement".into(),
            gross_amount: "10000.00".into(),
            currency: "IDR".into(),
            ..Default::default()
        };
        let outcome = evaluate_status(&s).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 1_000_000);
    }

    #[test]
    fn capture_without_fraud_accept_is_not_settled() {
        let s = MidtransTransactionStatus {
            order_id: "ord_1".into(),
            transaction_id: "txn_1".into(),
            transaction_status: "capture".into(),
            fraud_status: "challenge".into(),
            gross_amount: "10000.00".into(),
            currency: "IDR".into(),
            ..Default::default()
        };
        let outcome = evaluate_status(&s).unwrap();
        assert!(!outcome.settled);
    }

    #[test]
    fn capture_with_fraud_accept_settles() {
        let s = MidtransTransactionStatus {
            order_id: "ord_1".into(),
            transaction_id: "txn_1".into(),
            transaction_status: "capture".into(),
            fraud_status: "accept".into(),
            gross_amount: "10000.00".into(),
            currency: "IDR".into(),
            ..Default::default()
        };
        let outcome = evaluate_status(&s).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 1_000_000);
    }

    #[test]
    fn deny_and_friends_are_not_settled() {
        for status in ["deny", "cancel", "expire", "failure", "pending"] {
            let s = MidtransTransactionStatus {
                order_id: "ord_1".into(),
                transaction_id: "txn_1".into(),
                transaction_status: status.into(),
                gross_amount: "10000.00".into(),
                currency: "IDR".into(),
                ..Default::default()
            };
            let outcome = evaluate_status(&s).unwrap();
            assert!(!outcome.settled, "{status}");
            assert!(
                !outcome.event_id.is_empty(),
                "{status}: a non-settling notification still has to be nameable"
            );
        }
    }

    /// `WebhookEvent::event_id` is documented "Never empty: a caller cannot
    /// suppress a duplicate it cannot name", and `transaction_id` is where
    /// this rail's comes from. The check used to live inside the two SETTLING
    /// arms only — which is the half of the traffic that never needs
    /// deduplicating, since a `deny`/`expire` is what Midtrans redelivers.
    /// Delete the guard at the top of `evaluate_status` and this test reports:
    /// `deny: reached Ok with event_id "" -- a signed notification with no
    /// transaction_id has no dedup key`.
    #[test]
    fn a_notification_with_no_transaction_id_is_refused_whatever_its_status() {
        for status in [
            "settlement",
            "capture",
            "deny",
            "cancel",
            "expire",
            "pending",
        ] {
            let s = MidtransTransactionStatus {
                order_id: "ord_1".into(),
                transaction_id: String::new(),
                transaction_status: status.into(),
                fraud_status: "accept".into(),
                gross_amount: "10000.00".into(),
                currency: "IDR".into(),
                ..Default::default()
            };
            match evaluate_status(&s) {
                Err(e) => assert!(
                    e.to_string().contains("transaction_id"),
                    "{status}: refused, but not for the missing id: {e}"
                ),
                Ok(o) => panic!(
                    "{status}: reached Ok with event_id {:?} -- a signed notification \
                     with no transaction_id has no dedup key",
                    o.event_id
                ),
            }
        }
    }

    #[test]
    fn missing_order_id_is_malformed() {
        let s = MidtransTransactionStatus {
            transaction_status: "settlement".into(),
            gross_amount: "10000.00".into(),
            ..Default::default()
        };
        assert!(evaluate_status(&s).is_err());
    }

    #[test]
    fn currency_defaults_to_idr() {
        let s = MidtransTransactionStatus::default();
        assert_eq!(s.currency_or_idr(), "IDR");
    }
}
