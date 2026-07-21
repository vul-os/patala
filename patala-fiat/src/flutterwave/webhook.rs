//! Verify and parse a Flutterwave webhook — ported from cackle's
//! `internal/payments/flutterwave.go`'s `Webhook` method
//! (<https://developer.flutterwave.com/docs/integration-guides/webhooks>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: same reasoning as
//! `stripe/webhook.rs`/`paystack/webhook.rs` — the trait has no webhook
//! method at all. Unlike those two, however, Flutterwave's webhook
//! signature is NOT a keyed MAC: it is a STATIC shared secret (the `hash`
//! configured in the Flutterwave dashboard) echoed back verbatim in the
//! `verif-hash` header on every delivery. Cackle's own file header
//! comments on this explicitly: this module still compares it in constant
//! time (mirroring cackle's `hmac.Equal`) to avoid a timing side-channel,
//! even though it isn't a MAC — this is why `flutterwave` needs no crypto
//! crate at all (see `Cargo.toml`'s feature comment).

use crate::flutterwave::models::{self, FlutterwaveTransactionPayload};
use serde::Deserialize;

/// Sentinel errors specific to Flutterwave webhook handling — mirrors
/// cackle's `ErrFlutterwaveMissingSignature` / `ErrFlutterwaveInvalidSignature`
/// / `ErrFlutterwaveMalformedResponse` / `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlutterwaveWebhookError {
    #[error("payments: flutterwave: missing verif-hash header")]
    MissingSignature,
    #[error("payments: flutterwave: invalid verif-hash")]
    InvalidSignature,
    #[error("payments: flutterwave: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterwaveWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

#[derive(Deserialize)]
struct Envelope {
    event: String,
    data: FlutterwaveTransactionPayload,
}

/// Mirrors cackle's `hmac.Equal([]byte(given), []byte(p.webhookHash))`: a
/// constant-time byte comparison of two plain strings. Not a MAC (there is
/// no digest to compute) — just avoiding a timing side-channel on the
/// comparison itself, exactly as cackle's own comment explains. Mirrors
/// `payfast::webhook`'s identical `hmacEqualHex`-style helper (same
/// rationale, different callers).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify `given_hash` against `configured_hash`, then parse the event,
/// failing closed at every step — mirrors cackle's
/// `FlutterwaveProvider.Webhook`.
pub fn verify_and_parse(
    configured_hash: &str,
    raw_body: &[u8],
    given_hash: &str,
) -> Result<FlutterwaveWebhookEvent, FlutterwaveWebhookError> {
    let given_hash = given_hash.trim();
    if given_hash.is_empty() {
        return Err(FlutterwaveWebhookError::MissingSignature);
    }
    if !constant_time_eq(given_hash.as_bytes(), configured_hash.trim().as_bytes()) {
        return Err(FlutterwaveWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| FlutterwaveWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.event != "charge.completed" {
        return Err(FlutterwaveWebhookError::UnhandledEvent(envelope.event));
    }
    let outcome = models::evaluate_transaction(&envelope.data)
        .map_err(|e| FlutterwaveWebhookError::MalformedResponse(e.to_string()))?;

    Ok(FlutterwaveWebhookEvent {
        event_id: outcome.event_id,
        reference: outcome.reference,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "test-webhook-hash";

    fn charge_completed_event(
        id: i64,
        tx_ref: &str,
        amount: &str,
        currency: &str,
        status: &str,
    ) -> Vec<u8> {
        format!(
            r#"{{"event":"charge.completed","data":{{"id":{id},"tx_ref":"{tx_ref}","amount":{amount},"currency":"{currency}","status":"{status}"}}}}"#
        )
        .into_bytes()
    }

    // Ported from cackle's internal/payments/flutterwave_test.go (webhook section).

    #[test]
    fn valid_hash_succeeds() {
        let body = charge_completed_event(9, "ord_1", "100", "NGN", "successful");
        let event = verify_and_parse(HASH, &body, HASH).unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 10000);
        assert_eq!(event.reference, "ord_1");
    }

    #[test]
    fn missing_hash_fails_closed() {
        let body = charge_completed_event(9, "ord_1", "100", "NGN", "successful");
        assert_eq!(
            verify_and_parse(HASH, &body, ""),
            Err(FlutterwaveWebhookError::MissingSignature)
        );
    }

    #[test]
    fn wrong_hash_fails_closed() {
        let body = charge_completed_event(9, "ord_1", "100", "NGN", "successful");
        assert_eq!(
            verify_and_parse(HASH, &body, "wrong-hash"),
            Err(FlutterwaveWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"event":"transfer.completed","data":{"id":9,"tx_ref":"trf_1","amount":100,"currency":"NGN","status":"successful"}}"#;
        assert_eq!(
            verify_and_parse(HASH, body, HASH),
            Err(FlutterwaveWebhookError::UnhandledEvent(
                "transfer.completed".to_string()
            ))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid json";
        assert!(matches!(
            verify_and_parse(HASH, body, HASH),
            Err(FlutterwaveWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn failed_status_is_not_settled() {
        let body = charge_completed_event(9, "ord_1", "100", "NGN", "failed");
        let event = verify_and_parse(HASH, &body, HASH).unwrap();
        assert!(!event.settled);
    }
}
