//! Verify and parse a Yoco webhook — ported from cackle's
//! `internal/payments/yoco.go`'s `Webhook` method
//! (<https://developer.yoco.com/online/resources/webhooks>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: same reasoning as every
//! other adapter's `webhook.rs` in this crate.
//!
//! Yoco uses the Svix webhook standard: headers `webhook-id`,
//! `webhook-timestamp`, `webhook-signature`; the signed content is
//! `"{id}.{timestamp}.{body}"` joined by periods, HMAC-SHA256'd with the
//! base64-DECODED secret (`whsec_<base64>`), base64-ENCODED, compared
//! against any `"v1,<base64-sig>"` entry in the space-separated
//! `webhook-signature` header — mirrors cackle's `Webhook` exactly,
//! including the ±5-minute timestamp-tolerance replay guard Svix
//! recommends.

use serde::Deserialize;

use crate::yoco::models::{self, YocoCheckout};

/// Mirrors cackle's `yocoWebhookTolerance` (5 minutes).
pub const SIGNATURE_TOLERANCE_SECS: u64 = 5 * 60;

/// Sentinel errors specific to Yoco webhook handling — mirrors cackle's
/// `ErrYocoMissingSignature` / `ErrYocoInvalidSignature` /
/// `ErrYocoStaleTimestamp` / `ErrYocoMalformedResponse` / `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum YocoWebhookError {
    #[error("payments: yoco: missing webhook-id/webhook-timestamp/webhook-signature headers")]
    MissingSignature,
    #[error("payments: yoco: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: yoco: webhook timestamp outside tolerance window")]
    StaleTimestamp,
    #[error("payments: yoco: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YocoWebhookEvent {
    pub event_id: String,
    /// Yoco's own checkout id this event is about -- the caller correlates
    /// this against whichever stored `Receipt`'s `proof.checkout_id`
    /// matches (`PORTING.md` §6: this is the caller's job, same division of
    /// labour as every other adapter's webhook module).
    pub checkout_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

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

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    event_type: String,
    payload: YocoCheckout,
}

/// Verify `id_header`/`timestamp_header`/`signature_header` against
/// `raw_body` under `webhook_secret` (already base64-decoded — see
/// `YocoRail::new`), then parse the event, failing closed at every step —
/// mirrors cackle's `YocoProvider.Webhook`.
pub fn verify_and_parse(
    webhook_secret: &[u8],
    raw_body: &[u8],
    id_header: &str,
    timestamp_header: &str,
    signature_header: &str,
    now_unix: u64,
) -> Result<YocoWebhookEvent, YocoWebhookError> {
    let id = id_header.trim();
    let timestamp = timestamp_header.trim();
    let sig_header = signature_header.trim();
    if id.is_empty() || timestamp.is_empty() || sig_header.is_empty() {
        return Err(YocoWebhookError::MissingSignature);
    }

    let ts: i64 = timestamp
        .parse()
        .map_err(|_| YocoWebhookError::InvalidSignature)?;
    let age = (now_unix as i64 - ts).unsigned_abs();
    if age > SIGNATURE_TOLERANCE_SECS {
        return Err(YocoWebhookError::StaleTimestamp);
    }

    let mut signed = Vec::with_capacity(id.len() + timestamp.len() + raw_body.len() + 2);
    signed.extend_from_slice(id.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(timestamp.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(raw_body);

    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(webhook_secret)
        .map_err(|_| YocoWebhookError::InvalidSignature)?;
    use hmac::Mac;
    mac.update(&signed);
    let expected = mac.finalize().into_bytes();

    let mut valid = false;
    for entry in sig_header.split_whitespace() {
        if let Some((version, sig_b64)) = entry.split_once(',') {
            if version != "v1" {
                continue;
            }
            use base64::Engine as _;
            if let Ok(given) = base64::engine::general_purpose::STANDARD.decode(sig_b64) {
                if constant_time_eq(&expected, &given) {
                    valid = true;
                    break;
                }
            }
        }
    }
    if !valid {
        return Err(YocoWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| YocoWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.event_type != "payment.succeeded" {
        return Err(YocoWebhookError::UnhandledEvent(envelope.event_type));
    }
    let mut payload = envelope.payload;
    if payload.status.is_empty() {
        payload.status = "completed".to_string();
    }
    let outcome = models::evaluate_checkout(&payload)
        .map_err(|e| YocoWebhookError::MalformedResponse(e.to_string()))?;

    Ok(YocoWebhookEvent {
        event_id: outcome.event_id,
        checkout_id: payload.id,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn sign(id: &str, timestamp: &str, body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(format!("{id}.{timestamp}.").as_bytes());
        mac.update(body);
        use base64::Engine as _;
        format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        )
    }

    fn payment_succeeded(id: &str, amount: u64) -> Vec<u8> {
        format!(
            r#"{{"type":"payment.succeeded","payload":{{"id":"{id}","status":"completed","amount":{amount},"currency":"ZAR"}}}}"#
        )
        .into_bytes()
    }

    // Ported from cackle's internal/payments/yoco_test.go (webhook section).

    #[test]
    fn valid_signature_succeeds() {
        let body = payment_succeeded("chk_abc", 1000);
        let ts = "1700000000";
        let sig = sign("msg_1", ts, &body);
        let event = verify_and_parse(SECRET, &body, "msg_1", ts, &sig, 1_700_000_000).unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 1000);
        assert_eq!(event.checkout_id, "chk_abc");
    }

    #[test]
    fn missing_headers_fails_closed() {
        let body = payment_succeeded("chk_abc", 1000);
        assert_eq!(
            verify_and_parse(SECRET, &body, "", "", "", 1_700_000_000),
            Err(YocoWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = payment_succeeded("chk_abc", 1000);
        let ts = "1700000000";
        let sig = sign("msg_1", ts, &body);
        let tampered = payment_succeeded("chk_abc", 999_999_999);
        assert_eq!(
            verify_and_parse(SECRET, &tampered, "msg_1", ts, &sig, 1_700_000_000),
            Err(YocoWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = payment_succeeded("chk_abc", 1000);
        let ts = "1700000000";
        use hmac::Mac;
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(b"some-other-secret-32-bytes-long!")
                .unwrap();
        mac.update(format!("msg_1.{ts}.").as_bytes());
        mac.update(&body);
        use base64::Engine as _;
        let sig = format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        );
        assert_eq!(
            verify_and_parse(SECRET, &body, "msg_1", ts, &sig, 1_700_000_000),
            Err(YocoWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn stale_timestamp_fails_closed() {
        let body = payment_succeeded("chk_abc", 1000);
        let stale_ts = "1699996000"; // > 5 min before now
        let sig = sign("msg_1", stale_ts, &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, "msg_1", stale_ts, &sig, 1_700_000_000),
            Err(YocoWebhookError::StaleTimestamp)
        );
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"type":"payment.failed","payload":{"id":"chk_abc","status":"failed","amount":1000,"currency":"ZAR"}}"#;
        let ts = "1700000000";
        let sig = sign("msg_1", ts, body);
        assert_eq!(
            verify_and_parse(SECRET, body, "msg_1", ts, &sig, 1_700_000_000),
            Err(YocoWebhookError::UnhandledEvent(
                "payment.failed".to_string()
            ))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid json";
        let ts = "1700000000";
        let sig = sign("msg_1", ts, body);
        assert!(matches!(
            verify_and_parse(SECRET, body, "msg_1", ts, &sig, 1_700_000_000),
            Err(YocoWebhookError::MalformedResponse(_))
        ));
    }
}
