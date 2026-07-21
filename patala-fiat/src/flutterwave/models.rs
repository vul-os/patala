//! Wire shapes for the Flutterwave adapter — ported from cackle's
//! `internal/payments/flutterwave.go`.
//!
//! Reference: <https://developer.flutterwave.com/docs/collecting-payments/standard>
//! (Standard payment initialize + verify). Not re-verified live from this
//! environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note and `mod.rs`'s "confidence: MEDIUM" disclosure (carried from
//! cackle's own file header verbatim).
//!
//! **Money quirk, ported exactly** (cackle's file header, and
//! `PORTING.md` §8): unlike Paystack/Stripe, Flutterwave's `amount` field on
//! the wire is a decimal string in MAJOR units (e.g. `"100.50"` meaning
//! ₦100.50, not 10050 kobo) — this module routes every conversion through
//! [`crate::currency::minor_to_major_string`]/[`crate::currency::major_string_to_minor`]
//! rather than assuming Paystack-style integer minor units.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// The common shape of a Flutterwave transaction object, returned both by
/// `verify_by_reference` and inside a `charge.completed` webhook's `data`.
/// Mirrors cackle's `flutterwaveTransactionPayload`. `amount` is decoded as
/// a string (Flutterwave's API represents it as a bare JSON number that may
/// have a decimal point, e.g. `100.50` — cackle's own `json.Number` handles
/// this by keeping the original digit string; `serde_json`'s
/// `#[serde(with = "...")]`-free plain `String` field with
/// `deserialize_any`-style flexibility is emulated here by accepting either
/// a JSON string or number via [`amount_as_string`]).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FlutterwaveTransactionPayload {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub tx_ref: String,
    #[serde(default)]
    pub flw_ref: String,
    #[serde(default, deserialize_with = "amount_as_string")]
    pub amount: String,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub status: String,
}

/// Accepts a JSON number OR string for `amount`, always yielding its exact
/// decimal-string form — mirrors cackle's `json.Number` (which preserves
/// the original digit sequence verbatim, never round-tripping through a
/// float) so a fractional major-unit value like `100.50` is never lossily
/// reparsed.
fn amount_as_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(serde_json::Number),
    }
    Ok(match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s,
        StringOrNumber::Number(n) => n.to_string(),
    })
}

/// The settlement outcome of evaluating a [`FlutterwaveTransactionPayload`]
/// — mirrors cackle's `flutterwaveTransactionPayload.toResult`.
pub struct TransactionOutcome {
    pub reference: String,
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `flutterwaveTransactionPayload.toResult`: only
/// `status == "successful"` is ever settled; `"failed"`/`"cancelled"`/
/// anything unrecognised fail closed to not-settled.
pub fn evaluate_transaction(
    t: &FlutterwaveTransactionPayload,
) -> Result<TransactionOutcome, Error> {
    if t.tx_ref.is_empty() {
        return Err(malformed("missing tx_ref"));
    }
    let currency = t.currency.trim().to_ascii_uppercase();
    let amount_minor = if t.amount.is_empty() {
        0
    } else {
        crate::currency::major_string_to_minor(&t.amount, &currency)
            .map_err(|e| malformed(&format!("amount {:?}: {e}", t.amount)))?
    };

    match t.status.as_str() {
        "successful" => {
            if amount_minor == 0 {
                return Err(malformed("successful status with non-positive amount"));
            }
            Ok(TransactionOutcome {
                reference: t.tx_ref.clone(),
                event_id: t.id.to_string(),
                settled: true,
                amount_minor,
                currency,
            })
        }
        // "failed" | "cancelled" | anything else: fail closed.
        _ => Ok(TransactionOutcome {
            reference: t.tx_ref.clone(),
            event_id: t.id.to_string(),
            settled: false,
            amount_minor: 0,
            currency,
        }),
    }
}

/// Flutterwave's error response shape: `{"status":"error","message":"..."}`.
/// Mirrors cackle's inline anonymous struct in `classifyFlutterwaveError`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub message: String,
}

/// Mirrors cackle's `classifyFlutterwaveError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "flutterwave: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("flutterwave: malformed API response: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_status_settles() {
        let t = FlutterwaveTransactionPayload {
            id: 123,
            tx_ref: "ord_1".into(),
            flw_ref: "FLW-REF".into(),
            amount: "100.50".into(),
            currency: "NGN".into(),
            status: "successful".into(),
        };
        let outcome = evaluate_transaction(&t).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 10050);
        assert_eq!(outcome.currency, "NGN");
        assert_eq!(outcome.event_id, "123");
    }

    #[test]
    fn failed_and_cancelled_and_unknown_never_settle() {
        for status in ["failed", "cancelled", "some-new-status"] {
            let t = FlutterwaveTransactionPayload {
                id: 1,
                tx_ref: "ord_1".into(),
                flw_ref: String::new(),
                amount: "100".into(),
                currency: "NGN".into(),
                status: status.into(),
            };
            let outcome = evaluate_transaction(&t).unwrap();
            assert!(!outcome.settled, "{status}");
            assert_eq!(outcome.amount_minor, 0, "{status}");
        }
    }

    #[test]
    fn missing_tx_ref_is_malformed() {
        let t = FlutterwaveTransactionPayload {
            id: 1,
            tx_ref: String::new(),
            flw_ref: String::new(),
            amount: "100".into(),
            currency: "NGN".into(),
            status: "successful".into(),
        };
        assert!(evaluate_transaction(&t).is_err());
    }
}
