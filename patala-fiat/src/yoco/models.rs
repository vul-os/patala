//! Wire shapes for the Yoco adapter — ported from cackle's
//! `internal/payments/yoco.go`.
//!
//! Reference: <https://developer.yoco.com/online/resources/checkouts>
//! (Checkouts API). Not re-verified live — see `mod.rs`'s "UNVERIFIED
//! AGAINST LIVE" note.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `yocoCheckout`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct YocoCheckout {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub metadata: YocoMetadata,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct YocoMetadata {
    #[serde(default)]
    pub reference: String,
}

/// The settlement outcome of evaluating a [`YocoCheckout`].
pub struct CheckoutOutcome {
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `yocoCheckout.toResult`: only `"completed"`/
/// `"succeeded"` (case-insensitive) ever settle; `"cancelled"`/`"failed"`/
/// `"expired"`/anything unrecognised fail closed.
pub fn evaluate_checkout(c: &YocoCheckout) -> Result<CheckoutOutcome, Error> {
    if c.id.is_empty() {
        return Err(malformed("missing id"));
    }
    match c.status.to_ascii_lowercase().as_str() {
        "completed" | "succeeded" => {
            if c.amount == 0 {
                return Err(malformed("completed status with non-positive amount"));
            }
            Ok(CheckoutOutcome {
                event_id: c.id.clone(),
                settled: true,
                amount_minor: c.amount,
                currency: c.currency.clone(),
            })
        }
        // "cancelled" | "failed" | "expired" | anything else: fail closed.
        _ => Ok(CheckoutOutcome {
            event_id: c.id.clone(),
            settled: false,
            amount_minor: 0,
            currency: c.currency.clone(),
        }),
    }
}

/// Mirrors cackle's `classifyYocoError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct Env {
        #[serde(default)]
        message: String,
    }
    let env: Env = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "yoco: unexpected API response status: http {status}: {msg}"
    ))
}

/// Mirrors cackle's `ErrYocoUnsupportedCurrency`.
pub fn unsupported_currency(got: &str) -> Error {
    Error::InvalidRequest(format!("yoco: only ZAR is supported, got {got:?}"))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("yoco: malformed API response: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_and_succeeded_settle() {
        for status in ["completed", "succeeded", "COMPLETED"] {
            let c = YocoCheckout {
                id: "chk_abc".into(),
                status: status.into(),
                amount: 1000,
                currency: "ZAR".into(),
                metadata: YocoMetadata::default(),
            };
            let outcome = evaluate_checkout(&c).unwrap();
            assert!(outcome.settled, "{status}");
            assert_eq!(outcome.amount_minor, 1000);
        }
    }

    #[test]
    fn cancelled_failed_expired_never_settle() {
        for status in ["cancelled", "failed", "expired", "some-new-status"] {
            let c = YocoCheckout {
                id: "chk_abc".into(),
                status: status.into(),
                amount: 1000,
                currency: "ZAR".into(),
                metadata: YocoMetadata::default(),
            };
            let outcome = evaluate_checkout(&c).unwrap();
            assert!(!outcome.settled, "{status}");
        }
    }

    #[test]
    fn missing_id_is_malformed() {
        let c = YocoCheckout {
            status: "completed".into(),
            amount: 1000,
            ..Default::default()
        };
        assert!(evaluate_checkout(&c).is_err());
    }
}
