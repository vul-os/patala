//! Verify and parse a Midtrans webhook notification — ported from cackle's
//! `internal/payments/midtrans.go`'s `Webhook` method
//! (<https://docs.midtrans.com/docs/https-notification-webhooks>).
//!
//! **Reached through the trait** by
//! [`patala_core::PaymentRail::verify_webhook`] on this adapter's rail,
//! which is a thin wrapper over the function below. That wrapper is what
//! makes this verification usable from the UniFFI binding and the sidecar
//! and not only from Rust — a free function alone is invisible to every
//! consumer that dispatches through `dyn PaymentRail`. The function itself
//! stays public and pure: it takes exactly what the scheme signs and no
//! `&self`, which is what keeps it directly testable.
//!
//! Unlike Stripe (HMAC-SHA256 over a header) or Paystack (HMAC-SHA512 over
//! the raw body), Midtrans's `signature_key` is a PLAIN, UNKEYED SHA512
//! digest of `order_id + status_code + gross_amount + ServerKey`, carried
//! INSIDE the JSON notification body itself (not an HTTP header) — mirrors
//! cackle's `Webhook` exactly. Because everything this check needs (the
//! server key, the notification body) is available with no network access,
//! this webhook module — unlike `iyzico`'s and `payfast`'s — CAN be a pure
//! free function, same shape as `stripe::webhook`/`paystack::webhook`.

use crate::midtrans::models::{self, MidtransTransactionStatus};

/// Sentinel errors specific to Midtrans webhook handling — mirrors cackle's
/// `ErrMidtransMissingSignature` / `ErrMidtransInvalidSignature` /
/// `ErrMidtransMalformedResponse`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MidtransWebhookError {
    #[error("payments: midtrans: webhook body has no signature_key")]
    MissingSignature,
    #[error("payments: midtrans: invalid signature_key")]
    InvalidSignature,
    #[error("payments: midtrans: malformed API response: {0}")]
    MalformedResponse(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidtransWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `hmac.Equal([]byte(expected), []byte(strings.ToLower(...)))`:
/// case-insensitive, constant-time hex comparison.
fn constant_time_eq_hex_ci(expected_lower_hex: &str, given: &str) -> bool {
    let given_lower = given.to_ascii_lowercase();
    let a = expected_lower_hex.as_bytes();
    let b = given_lower.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify the `signature_key` embedded in `raw_body` (SHA512 of
/// `order_id + status_code + gross_amount + server_key`), then parse the
/// settlement outcome, failing closed at every step — mirrors cackle's
/// `MidtransProvider.Webhook`.
pub fn verify_and_parse(
    server_key: &str,
    raw_body: &[u8],
) -> Result<MidtransWebhookEvent, MidtransWebhookError> {
    let parsed: MidtransTransactionStatus = serde_json::from_slice(raw_body)
        .map_err(|e| MidtransWebhookError::MalformedResponse(e.to_string()))?;
    if parsed.signature_key.is_empty() {
        return Err(MidtransWebhookError::MissingSignature);
    }
    if parsed.order_id.is_empty() || parsed.status_code.is_empty() || parsed.gross_amount.is_empty()
    {
        return Err(MidtransWebhookError::MalformedResponse(
            "missing order_id/status_code/gross_amount".to_string(),
        ));
    }

    let mut hasher = sha2::Sha512::new();
    use sha2::Digest;
    hasher.update(parsed.order_id.as_bytes());
    hasher.update(parsed.status_code.as_bytes());
    hasher.update(parsed.gross_amount.as_bytes());
    hasher.update(server_key.as_bytes());
    let expected = hex::encode(hasher.finalize());

    if !constant_time_eq_hex_ci(&expected, &parsed.signature_key) {
        return Err(MidtransWebhookError::InvalidSignature);
    }

    let outcome = models::evaluate_status(&parsed)
        .map_err(|e| MidtransWebhookError::MalformedResponse(e.to_string()))?;

    Ok(MidtransWebhookEvent {
        event_id: outcome.event_id,
        reference: parsed.order_id,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    const SERVER_KEY: &str = "SB-Mid-server-fake-key";

    fn sign(order_id: &str, status_code: &str, gross_amount: &str) -> String {
        let mut hasher = sha2::Sha512::new();
        hasher.update(order_id.as_bytes());
        hasher.update(status_code.as_bytes());
        hasher.update(gross_amount.as_bytes());
        hasher.update(SERVER_KEY.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn notification(order_id: &str, status: &str, gross_amount: &str, sig: &str) -> Vec<u8> {
        serde_json::json!({
            "order_id": order_id,
            "transaction_id": "txn_1",
            "transaction_status": status,
            "gross_amount": gross_amount,
            "currency": "IDR",
            "status_code": "200",
            "signature_key": sig,
            "settlement_time": "2026-07-20 10:00:00",
        })
        .to_string()
        .into_bytes()
    }

    // Ported from cackle's internal/payments/midtrans_test.go (webhook section).

    #[test]
    fn valid_signature_succeeds() {
        let sig = sign("ord_1", "200", "10000.00");
        let body = notification("ord_1", "settlement", "10000.00", &sig);
        let event = verify_and_parse(SERVER_KEY, &body).unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 1_000_000);
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = notification("ord_1", "settlement", "10000.00", "");
        assert_eq!(
            verify_and_parse(SERVER_KEY, &body),
            Err(MidtransWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_amount_fails_closed() {
        let sig = sign("ord_1", "200", "10000.00");
        // Attacker changes gross_amount after the signature was computed.
        let body = notification("ord_1", "settlement", "1.00", &sig);
        assert_eq!(
            verify_and_parse(SERVER_KEY, &body),
            Err(MidtransWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_server_key_fails_closed() {
        let mut hasher = sha2::Sha512::new();
        hasher.update(b"ord_1");
        hasher.update(b"200");
        hasher.update(b"10000.00");
        hasher.update(b"some-other-key");
        let sig = hex::encode(hasher.finalize());
        let body = notification("ord_1", "settlement", "10000.00", &sig);
        assert_eq!(
            verify_and_parse(SERVER_KEY, &body),
            Err(MidtransWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid".to_vec();
        assert!(matches!(
            verify_and_parse(SERVER_KEY, &body),
            Err(MidtransWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn deny_status_is_not_paid() {
        let sig = sign("ord_1", "200", "10000.00");
        let body = notification("ord_1", "deny", "10000.00", &sig);
        let event = verify_and_parse(SERVER_KEY, &body).unwrap();
        assert!(!event.settled);
    }
}
