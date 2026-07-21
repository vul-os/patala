//! Wire shapes for the Paystack adapter — ported from cackle's
//! `internal/payments/paystack.go`.
//!
//! Reference: <https://paystack.com/docs/api/transaction/> (Initialize,
//! Verify) and <https://paystack.com/docs/payments/webhooks/> (webhook
//! signature). Not re-verified live from this environment — see this
//! crate's `PORTING.md` "UNVERIFIED AGAINST LIVE" note.
//!
//! Some fields below (`message`, `VerifyData::id`/`paid_at`) are part of
//! Paystack's documented response shape and modelled here so the wire type
//! is honest/complete, even though this crate's own logic does not
//! currently read them (`id` in particular mirrors cackle's own
//! `parsed.Data.ID`, used there for `Result.EventID` — a field
//! `patala_core::PaymentRail::verify`'s bare `Result<bool>` return has no
//! place to carry; see `PORTING.md`'s gap list). `#![allow(dead_code)]`
//! says so explicitly rather than silently dropping fields Paystack
//! actually returns.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// `POST /transaction/initialize` response's `data` object — mirrors
/// cackle's inline anonymous struct in `Begin`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InitializeData {
    #[serde(default)]
    pub authorization_url: String,
    #[serde(default)]
    pub access_code: String,
    #[serde(default)]
    pub reference: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InitializeResponse {
    pub status: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: InitializeData,
}

/// `GET /transaction/verify/{reference}` response's `data` object — mirrors
/// cackle's inline anonymous struct in `Verify`.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct VerifyData {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub paid_at: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct VerifyResponse {
    #[serde(default)]
    pub status: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: VerifyData,
}

/// Paystack's error response shape: `{"status":false,"message":"..."}`.
/// Mirrors cackle's `paystackErrorEnvelope`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub message: String,
}

/// Mirrors cackle's `classifyPaystackError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "paystack: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("paystack: malformed API response: {detail}"))
}

/// Settlement outcome of a Verify/Webhook `data` payload — mirrors the
/// status-mapping `switch` in cackle's `Verify` and `Webhook`: only
/// `"success"` is ever settled; `"abandoned"`/`"failed"`/`"reversed"`/
/// anything unrecognised all fail closed to not-settled.
pub fn is_settled_success(status: &str) -> bool {
    status == "success"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_status_mapping() {
        assert!(is_settled_success("success"));
        for s in ["abandoned", "failed", "reversed", "some-new-status"] {
            assert!(!is_settled_success(s), "{s}");
        }
    }
}
