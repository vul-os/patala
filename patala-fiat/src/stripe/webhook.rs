//! Verify and parse a Stripe webhook — ported from cackle's
//! `internal/payments/stripe.go`'s `Webhook` method
//! (<https://docs.stripe.com/webhooks/signatures>).
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
//! Verification, exactly as cackle's `Webhook` and Stripe's own docs
//! describe:
//! 1. Read the RAW body (before any JSON decode).
//! 2. Parse the `Stripe-Signature` header: comma-separated `key=value`
//!    pairs, e.g. `t=1614556800,v0=...,v1=...`. Only `v1` is ever trusted —
//!    `v0` is a legacy/test-mode scheme Stripe's own docs say to ignore in
//!    production.
//! 3. Recompute `HMAC-SHA256("{t}.{raw_body}", webhook_secret)` and compare
//!    against `v1`, constant-time.
//! 4. Reject if the timestamp is more than [`SIGNATURE_TOLERANCE_SECS`] (5
//!    minutes, Stripe's documented default) away from `now` — the
//!    replay-window check Stripe explicitly warns against disabling.

use serde::Deserialize;

use crate::stripe::models::{self, StripeSessionPayload};

/// Mirrors cackle's `stripeSignatureTolerance` (5 minutes).
pub const SIGNATURE_TOLERANCE_SECS: u64 = 5 * 60;

/// Sentinel errors specific to Stripe webhook handling — mirrors cackle's
/// `ErrStripeMissingSignature` / `ErrStripeInvalidSignature` /
/// `ErrStripeStaleSignature` / `ErrStripeMalformedResponse` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StripeWebhookError {
    #[error("payments: stripe: empty request body")]
    EmptyBody,
    #[error("payments: stripe: missing Stripe-Signature header")]
    MissingSignature,
    #[error("payments: stripe: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: stripe: webhook timestamp outside replay tolerance")]
    StaleSignature,
    #[error("payments: stripe: malformed API response: {0}")]
    MalformedResponse(String),
    /// Mirrors cackle's `ErrUnhandledEvent`: a validly-signed webhook for an
    /// event type this build does not treat as a settlement. Callers
    /// wiring an HTTP route should treat this as "ack with 200, do
    /// nothing" — not a hard failure — so Stripe doesn't retry forever.
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery — mirrors the subset of
/// cackle's `Result` a webhook produces (`Provider`/`Raw` are the caller's
/// concern, not this parser's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeWebhookEvent {
    /// Stripe's own top-level event id (`evt_...`), preferred over the
    /// session id for replay dedup — mirrors cackle's comment: *"two
    /// DIFFERENT events ... must never collapse onto the same dedup key."*
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's own two-step decode: the outer envelope first (`data`
/// kept as a raw, not-yet-typed value, exactly like cackle's
/// `json.RawMessage`), so an event type this build doesn't handle can be
/// rejected via [`StripeWebhookError::UnhandledEvent`] BEFORE attempting to
/// decode a `data.object` shape that may not even be a session at all (a
/// non-checkout event's `data.object` need not have an `id`/
/// `payment_status` — see this module's own
/// `unhandled_event_type` test, which deliberately sends
/// `data:{object:{}}}`).
#[derive(Deserialize)]
struct Envelope {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    data: EnvelopeData,
}

#[derive(Deserialize)]
struct EnvelopeData {
    object: serde_json::Value,
}

/// Mirrors cackle's `parseStripeSignatureHeader`: only `t` (timestamp) and
/// `v1` (the current HMAC-SHA256 scheme) are extracted; `v0` (legacy) is
/// never even looked at.
fn parse_signature_header(header: &str) -> Result<(String, String), StripeWebhookError> {
    let mut timestamp = None;
    let mut v1 = None;
    for part in header.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            match k {
                "t" => timestamp = Some(v.to_string()),
                "v1" => v1 = Some(v.to_string()),
                _ => {}
            }
        }
    }
    match (timestamp, v1) {
        (Some(t), Some(v)) => Ok((t, v)),
        _ => Err(StripeWebhookError::InvalidSignature),
    }
}

/// Verify `signature_header` against `raw_body` under `webhook_secret`,
/// then parse the event, failing closed at every step -- mirrors cackle's
/// `StripeProvider.Webhook`.
pub fn verify_and_parse(
    webhook_secret: &str,
    raw_body: &[u8],
    signature_header: &str,
    now_unix: u64,
) -> Result<StripeWebhookEvent, StripeWebhookError> {
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(StripeWebhookError::MissingSignature);
    }
    if raw_body.is_empty() {
        return Err(StripeWebhookError::EmptyBody);
    }

    let (ts, v1) = parse_signature_header(signature_header)?;

    let mut signed_payload = Vec::with_capacity(ts.len() + 1 + raw_body.len());
    signed_payload.extend_from_slice(ts.as_bytes());
    signed_payload.push(b'.');
    signed_payload.extend_from_slice(raw_body);

    if !crate::httpshared::verify_hmac_sha256_hex(webhook_secret.as_bytes(), &signed_payload, &v1) {
        return Err(StripeWebhookError::InvalidSignature);
    }

    let ts_unix: i64 = ts
        .parse()
        .map_err(|_| StripeWebhookError::InvalidSignature)?;
    let age = (now_unix as i64 - ts_unix).unsigned_abs();
    if age > SIGNATURE_TOLERANCE_SECS {
        return Err(StripeWebhookError::StaleSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| StripeWebhookError::MalformedResponse(e.to_string()))?;

    if envelope.event_type != "checkout.session.completed"
        && envelope.event_type != "checkout.session.async_payment_succeeded"
    {
        return Err(StripeWebhookError::UnhandledEvent(envelope.event_type));
    }

    let session: StripeSessionPayload = serde_json::from_value(envelope.data.object)
        .map_err(|e| StripeWebhookError::MalformedResponse(format!("event data.object: {e}")))?;
    let outcome = models::evaluate_session(&session)
        .map_err(|e| StripeWebhookError::MalformedResponse(e.to_string()))?;

    Ok(StripeWebhookEvent {
        event_id: if envelope.id.is_empty() {
            session.id.clone()
        } else {
            envelope.id
        },
        reference: outcome.reference,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_fake_secret_for_unit_tests";

    fn checkout_completed_event(
        id: &str,
        session_id: &str,
        ref_: &str,
        currency: &str,
        amount_total: u64,
        payment_status: &str,
    ) -> Vec<u8> {
        format!(
            r#"{{"id":"{id}","type":"checkout.session.completed","data":{{"object":{{"id":"{session_id}","payment_status":"{payment_status}","amount_total":{amount_total},"currency":"{currency}","client_reference_id":"{ref_}"}}}}}}"#
        )
        .into_bytes()
    }

    fn sig_header(secret: &str, ts: i64, body: &[u8]) -> String {
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        use hmac::Mac;
        let mut payload = format!("{ts}.").into_bytes();
        payload.extend_from_slice(body);
        mac.update(&payload);
        format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    // Ported from cackle's internal/payments/stripe_test.go (webhook section).

    #[test]
    fn success() {
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);

        let event = verify_and_parse(SECRET, &body, &sig, now).unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "USD");
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.event_id, "evt_1");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        assert_eq!(
            verify_and_parse(SECRET, &body, "", 1_700_000_000),
            Err(StripeWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);
        let tampered = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 1, "paid");
        assert_eq!(
            verify_and_parse(SECRET, &tampered, &sig, now),
            Err(StripeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        let now = 1_700_000_000u64;
        let sig = sig_header("whsec_wrong_secret", now as i64, &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig, now),
            Err(StripeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn stale_timestamp_fails_closed() {
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        let now = 1_700_000_000u64;
        let stale_ts = now as i64 - 3600;
        let sig = sig_header(SECRET, stale_ts, &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig, now),
            Err(StripeWebhookError::StaleSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json at all".to_vec();
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);
        assert!(matches!(
            verify_and_parse(SECRET, &body, &sig, now),
            Err(StripeWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"id":"evt_1","type":"charge.refunded","data":{"object":{}}}"#.to_vec();
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig, now),
            Err(StripeWebhookError::UnhandledEvent(
                "charge.refunded".to_string()
            ))
        );
    }

    #[test]
    fn completed_but_unpaid_is_not_settled() {
        // checkout.session.completed can still be payment_status=unpaid for
        // async payment methods -- must never be treated as settled.
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "unpaid");
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);
        let event = verify_and_parse(SECRET, &body, &sig, now).unwrap();
        assert!(!event.settled);
    }

    #[test]
    fn replayed_event_produces_stable_event_id() {
        // Webhook handling itself does not dedupe (that is the caller's
        // job, keyed by (rail_id, event_id)) -- but replay protection is
        // only possible if repeated delivery of the exact same event
        // produces the exact same, non-empty event_id every time.
        let body = checkout_completed_event("evt_1", "cs_test_1", "ord_1", "usd", 5000, "paid");
        let now = 1_700_000_000u64;
        let sig = sig_header(SECRET, now as i64, &body);

        let first = verify_and_parse(SECRET, &body, &sig, now).unwrap();
        let second = verify_and_parse(SECRET, &body, &sig, now).unwrap();
        assert_eq!(first.event_id, second.event_id);
        assert!(!first.event_id.is_empty());
    }
}
