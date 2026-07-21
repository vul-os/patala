//! Wire shapes for the LNbits adapter — ported from cackle's
//! `internal/payments/lnbits.go`.
//!
//! Reference: LNbits' core Payments API
//! (<https://legend.lnbits.com/guide/api.html>,
//! <https://github.com/lnbits/lnbits>). Not re-verified live from this
//! environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note. Cackle's own file doc comment rates confidence HIGH for
//! create-invoice/poll-status, MODERATE for the exact fiat-denominated
//! request field names across LNbits versions.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// `POST /api/v1/payments` response. Mirrors cackle's anonymous struct in
/// `Begin`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CreatePaymentResponse {
    #[serde(default)]
    pub payment_hash: String,
    #[serde(default)]
    pub payment_request: String,
}

/// `GET /api/v1/payments/{hash}` response. Mirrors cackle's anonymous struct
/// in `verifyAgainstRecord`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PaymentStatus {
    #[serde(default)]
    pub paid: bool,
}

/// Mirrors cackle's `classifyLNbitsError` (LNbits' documented error shape
/// uses a `detail` field, unlike Paystack/BTCPay's `message`).
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        detail: String,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.detail.is_empty() {
        "no message".to_string()
    } else {
        env.detail
    };
    Error::Rail(format!(
        "lnbits: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("lnbits: malformed API response: {detail}"))
}
