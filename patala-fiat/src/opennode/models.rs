//! Wire shapes for the OpenNode adapter — ported from cackle's
//! `internal/payments/opennode.go`.
//!
//! Reference: <https://developers.opennode.com/reference> (create/fetch a
//! charge), <https://developers.opennode.com/docs/webhooks>. Not
//! re-verified live from this environment — see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note. Cackle's own file doc comment rates
//! confidence HIGH for the charge shape/status enum and MODERATE for the
//! exact webhook signing construction.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;
use serde_json::Value;

/// Mirrors cackle's `opennodeCharge` — shared by both create and fetch
/// responses, nested under a top-level `"data"` key.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct OpenNodeCharge {
    #[serde(default)]
    pub id: String,
    /// unpaid | processing | paid | underpaid | expired | refunded
    #[serde(default)]
    pub status: String,
    /// May be a JSON number OR a JSON string depending on OpenNode API
    /// version — see [`flexible_json_amount_to_string`].
    #[serde(default)]
    pub amount: Value,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub hosted_checkout_url: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub data: OpenNodeCharge,
}

/// Mirrors cackle's `classifyOpenNodeError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        message: String,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "opennode: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("opennode: malformed API response: {detail}"))
}

/// Mirrors cackle's `flexibleJSONAmountToString`: extracts a decimal amount
/// string from a JSON value that might be a bare number OR a quoted string,
/// without ever round-tripping through `f64` (cackle's own doc comment:
/// this author is not fully certain whether OpenNode's API returns
/// `"amount"` as a JSON number or a JSON string in every version, so both
/// are handled explicitly rather than assumed).
pub fn flexible_json_amount_to_string(raw: &Value) -> Result<String, Error> {
    match raw {
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(s.clone()),
        Value::Null => Err(malformed("amount is null")),
        other => Err(malformed(&format!(
            "amount is neither a JSON number nor a JSON string: {other}"
        ))),
    }
}

/// The settlement state an OpenNode charge's documented `status` enum maps
/// to. Mirrors cackle's `opennodeResultFromCharge` switch exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeState {
    /// `paid` — the only status ever reported as paid.
    Paid,
    /// `unpaid`/`processing` — not yet paid, may still complete.
    Pending,
    /// `underpaid` (never settles, by contract requirement)/`expired` (quote
    /// window closed unpaid)/`refunded` (money is no longer with the
    /// organiser), or any unrecognised status — fails closed.
    Failed,
}

/// Mirrors cackle's `opennodeResultFromCharge`'s status-mapping `switch`.
pub fn classify_charge_state(status: &str) -> ChargeState {
    match status {
        "paid" => ChargeState::Paid,
        "unpaid" | "processing" => ChargeState::Pending,
        "underpaid" | "expired" | "refunded" => ChargeState::Failed,
        // Fail closed: an unrecognised status is never treated as paid.
        _ => ChargeState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flexible_amount_handles_number_and_string() {
        assert_eq!(
            flexible_json_amount_to_string(&Value::from(12.34)).unwrap(),
            "12.34"
        );
        assert_eq!(
            flexible_json_amount_to_string(&Value::String("12.34".into())).unwrap(),
            "12.34"
        );
        assert!(flexible_json_amount_to_string(&Value::Null).is_err());
    }

    #[test]
    fn status_mapping() {
        assert_eq!(classify_charge_state("paid"), ChargeState::Paid);
        assert_eq!(classify_charge_state("unpaid"), ChargeState::Pending);
        assert_eq!(classify_charge_state("processing"), ChargeState::Pending);
        assert_eq!(classify_charge_state("underpaid"), ChargeState::Failed);
        assert_eq!(classify_charge_state("expired"), ChargeState::Failed);
        assert_eq!(classify_charge_state("refunded"), ChargeState::Failed);
        assert_eq!(
            classify_charge_state("some-future-status"),
            ChargeState::Failed
        );
    }
}
