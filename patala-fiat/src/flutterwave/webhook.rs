//! Verify and parse a Flutterwave webhook — ported from cackle's
//! `internal/payments/flutterwave.go`'s `Webhook` method
//! (<https://developer.flutterwave.com/docs/integration-guides/webhooks>).
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
//! Unlike most adapters here, Flutterwave's webhook
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
    // `data.id` is `i64` with `#[serde(default)]`, so a body without one
    // produced `event_id: "0"` — not empty, and therefore invisible to
    // `WebhookEvent::event_id`'s "never empty" contract, but WORSE than empty:
    // every such delivery gets the SAME id, so a consumer deduplicating on it
    // discards the second and subsequent distinct events as replays of the
    // first. Checked here, at the webhook boundary, and not in
    // `models::evaluate_transaction`, which `verify()` also calls and which
    // answers a different question (`verify` reconciles on `tx_ref`, and its
    // contract is `Ok(false)` on doubt rather than a dedup key).
    if envelope.data.id == 0 {
        return Err(FlutterwaveWebhookError::MalformedResponse(
            "no data.id: this delivery carries no id to deduplicate on".to_string(),
        ));
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

    /// `data.id` is `i64` with `#[serde(default)]`, so a delivery without one
    /// produced `event_id: "0"` — never empty, so invisible to the "never
    /// empty" contract, and worse than empty: EVERY such delivery gets the
    /// same id, so a consumer deduplicating on it discards distinct events as
    /// replays of the first. Delete the `data.id == 0` guard and this reports:
    /// `two distinct deliveries both got event_id "0" -- deduplicating on that
    /// discards the second`.
    #[test]
    fn a_delivery_with_no_data_id_is_refused_rather_than_named_zero() {
        for status in ["successful", "failed"] {
            let body = format!(
                r#"{{"event":"charge.completed","data":{{"tx_ref":"ord_1","amount":100,"currency":"NGN","status":"{status}"}}}}"#
            )
            .into_bytes();
            match verify_and_parse(HASH, &body, HASH) {
                Err(FlutterwaveWebhookError::MalformedResponse(m)) => assert!(
                    m.contains("data.id"),
                    "{status}: refused, but not for the missing id: {m}"
                ),
                Err(e) => panic!("{status}: refused, but not as malformed: {e}"),
                Ok(ev) => panic!(
                    "{status}: two distinct deliveries both got event_id {:?} -- \
                     deduplicating on that discards the second",
                    ev.event_id
                ),
            }
        }
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
