//! Verify a Coinbase Commerce webhook signature and extract which charge
//! it's about — ported from cackle's
//! `internal/payments/coinbasecommerce.go`'s `Webhook` method
//! (<https://docs.cloud.coinbase.com/commerce/docs/webhooks-security>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: same reasoning as
//! `stripe/webhook.rs`/`paystack/webhook.rs` — the trait has no webhook
//! method at all.
//!
//! **Same deliberate narrowing as `btcpay`/`lnbits`/`opennode`'s webhook
//! modules, preserving cackle's own refetch-required security property**:
//! cackle's `coinbasecommerce.go` `Webhook` verifies `X-CC-Webhook-Signature`
//! (hex HMAC-SHA256 over the raw body), then ALWAYS refetches the charge
//! from Coinbase Commerce's authenticated API rather than trust the (also
//! present) embedded charge/timeline data in the webhook payload — its own
//! doc comment: *"the same defense-in-depth pattern used by every adapter
//! in this file group."* This function preserves exactly that: it verifies
//! the signature and extracts ONLY the charge id; it makes NO settlement
//! claim and performs NO network call itself. The caller MUST take the
//! returned `charge_id`, find the [`patala_core::Receipt`] whose
//! [`super::proof::ChargeProof::charge_id`] matches it, and call
//! [`patala_core::PaymentRail::verify`] on that receipt (which DOES
//! refetch) to get the authoritative answer.

use serde::Deserialize;

/// Sentinel errors specific to Coinbase Commerce webhook handling — mirrors
/// cackle's `ErrCoinbaseCommerceMissingSignature` /
/// `ErrCoinbaseCommerceInvalidSignature` /
/// `ErrCoinbaseCommerceMalformedResponse` / `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoinbaseCommerceWebhookError {
    #[error("payments: coinbasecommerce: missing X-CC-Webhook-Signature header")]
    MissingSignature,
    #[error("payments: coinbasecommerce: invalid X-CC-Webhook-Signature")]
    InvalidSignature,
    #[error("payments: coinbasecommerce: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// A signature-verified Coinbase Commerce webhook, naming which charge to
/// re-verify. Carries NO settlement claim — see module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinbaseCommerceWebhookEvent {
    /// Mirrors cackle's choice of dedupe key: a charge settles at most
    /// once, so its id is stable.
    pub event_id: String,
    /// The Coinbase Commerce charge id this webhook is about.
    pub charge_id: String,
}

#[derive(Deserialize, Default)]
struct EventData {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize, Default)]
struct Event {
    #[serde(rename = "type", default)]
    event_type: String,
    #[serde(default)]
    data: EventData,
}

#[derive(Deserialize, Default)]
struct Envelope {
    #[serde(default)]
    event: Event,
}

/// Verify `signature_header` (the raw `X-CC-Webhook-Signature` header
/// value, hex) against `raw_body` under `secret`, then extract the charge
/// id — mirrors cackle's `CoinbaseCommerceProvider.Webhook` up to (but not
/// including) its refetch, which this port defers to `verify()` — see
/// module docs.
pub fn verify_and_extract(
    secret: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<CoinbaseCommerceWebhookEvent, CoinbaseCommerceWebhookError> {
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(CoinbaseCommerceWebhookError::MissingSignature);
    }
    if !crate::httpshared::verify_hmac_sha256_hex(secret.as_bytes(), raw_body, signature_header) {
        return Err(CoinbaseCommerceWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| CoinbaseCommerceWebhookError::MalformedResponse(e.to_string()))?;
    if !envelope.event.event_type.starts_with("charge:") {
        return Err(CoinbaseCommerceWebhookError::UnhandledEvent(
            envelope.event.event_type,
        ));
    }
    if envelope.event.data.id.is_empty() {
        return Err(CoinbaseCommerceWebhookError::MalformedResponse(
            "missing event.data.id".to_string(),
        ));
    }
    Ok(CoinbaseCommerceWebhookEvent {
        event_id: envelope.event.data.id.clone(),
        charge_id: envelope.event.data.id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-webhook-secret";

    fn sign(body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/coinbasecommerce_test.go
    // (webhook section).

    #[test]
    fn valid_signature_extracts_charge_id() {
        let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_1"}}}"#;
        let sig = sign(body);
        let event = verify_and_extract(SECRET, body, &sig).unwrap();
        assert_eq!(event.charge_id, "charge_1");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_1"}}}"#;
        assert_eq!(
            verify_and_extract(SECRET, body, ""),
            Err(CoinbaseCommerceWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_1"}}}"#;
        let tampered = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_evil"}}}"#;
        let sig = sign(body);
        assert_eq!(
            verify_and_extract(SECRET, tampered, &sig),
            Err(CoinbaseCommerceWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_1"}}}"#;
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"wrong-secret").unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        assert_eq!(
            verify_and_extract(SECRET, body, &sig),
            Err(CoinbaseCommerceWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json at all";
        let sig = sign(body);
        assert!(matches!(
            verify_and_extract(SECRET, body, &sig),
            Err(CoinbaseCommerceWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"event":{"type":"some:other:event","data":{"id":"charge_1"}}}"#;
        let sig = sign(body);
        assert_eq!(
            verify_and_extract(SECRET, body, &sig),
            Err(CoinbaseCommerceWebhookError::UnhandledEvent(
                "some:other:event".to_string()
            ))
        );
    }
}
