//! Verify and parse a Square webhook — ported from cackle's
//! `internal/payments/square.go`'s `Webhook` method
//! (<https://developer.squareup.com/docs/webhooks/step3validate>,
//! <https://developer.squareup.com/docs/webhooks/v2webhook-events-tech-ref>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: the trait has no webhook
//! method at all (see `stripe::webhook`/`paystack::webhook`'s identical
//! point).
//!
//! **HONESTY note (mirrors cackle's file-header HONESTY note 1 verbatim):**
//! Square's webhook signature IS confirmed to be an HMAC-SHA256 over "the
//! signature key, the notification URL, and the raw body" per Square's own
//! prose docs -- but the EXACT concatenation order/encoding
//! (notification_url + raw_body, base64-encoded HMAC output) was only
//! corroborated via widely-known SDK behaviour, not quoted verbatim from
//! Square's prose. This module implements that widely-known construction;
//! if wrong, every signature check fails closed (genuine webhooks get
//! rejected) rather than accepting a forged one.
//!
//! Verification, exactly as cackle's `Webhook`:
//! 1. Bound the RAW body's length before any JSON decode.
//! 2. Concatenate `notification_url.as_bytes() ++ raw_body` into one buffer
//!    -- cackle's own `Webhook` calls `mac.Write` twice in sequence
//!    (`mac.Write([]byte(p.notificationURL))` then `mac.Write(body)`); HMAC
//!    is a stateful/sequential construction, so concatenating first and
//!    calling [`crate::httpshared::verify_hmac_sha256_base64`] once over the
//!    combined buffer is byte-for-byte identical to those two sequential
//!    writes.
//! 3. Recompute `base64(HMAC-SHA256(webhook_signature_key, that buffer))`
//!    and compare against the `x-square-hmacsha256-signature` header,
//!    constant-time.
//!
//! **Relocated tests, not dropped:** cackle's `TestSquareWebhook_*AmountMismatch/CurrencyMismatchFailsClosedViaReconcile`
//! exercise cackle's registry-layer `Reconcile` against a caller-supplied
//! `OrderRef` -- this crate's seam has no equivalent "stored order"
//! concept at the webhook-parsing layer (see `manual.rs`'s module docs on
//! the same point), so those same amount/currency anti-fraud assertions
//! are ported instead onto [`crate::square::SquareRail::verify`]'s own
//! tests in `rail.rs`, which is where this crate's equivalent fail-closed
//! check actually lives.

use serde::Deserialize;

use crate::square::models::{self, SquarePaymentPayload};

/// Sentinel errors specific to Square webhook handling — mirrors cackle's
/// `ErrSquareMissingSignature` / `ErrSquareInvalidSignature` /
/// `ErrSquareMalformedResponse` / `ErrSquareResponseTooLarge` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SquareWebhookError {
    #[error("payments: square: missing x-square-hmacsha256-signature header")]
    MissingSignature,
    #[error("payments: square: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: square: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: square: response body exceeds size limit")]
    ResponseTooLarge,
    /// Mirrors cackle's `ErrUnhandledEvent` -- covers BOTH a non-
    /// `payment.updated` event type AND a `payment.updated` event whose
    /// nested payment isn't (yet) `COMPLETED` (cackle treats both the same
    /// way: not a settlement yet, will arrive again if/when it happens).
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquareWebhookEvent {
    /// Square's own top-level `event_id`, preferred over the payment id for
    /// replay dedup -- mirrors cackle's `if envelope.EventID != "" { ... }`.
    pub event_id: String,
    pub reference: String,
    /// The real Square Payment id -- feed this into
    /// [`crate::square::proof::ChargeProof::with_resolved_payment_id`] to
    /// let [`crate::square::SquareRail::verify`] confirm this event
    /// directly against Square. See `proof.rs`'s module docs.
    pub payment_id: String,
    pub amount_minor: u64,
    pub currency: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    event_id: String,
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: EnvelopeData,
}

#[derive(Deserialize, Default)]
struct EnvelopeData {
    #[serde(default)]
    object: EnvelopeObject,
}

#[derive(Deserialize, Default)]
struct EnvelopeObject {
    #[serde(default)]
    payment: SquarePaymentPayload,
}

/// Verify `signature_header` against `notification_url` + `raw_body` under
/// `webhook_signature_key`, then parse the event, failing closed at every
/// step -- mirrors cackle's `SquareProvider.Webhook`.
pub fn verify_and_parse(
    webhook_signature_key: &str,
    notification_url: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<SquareWebhookEvent, SquareWebhookError> {
    crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
        .map_err(|_| SquareWebhookError::ResponseTooLarge)?;

    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(SquareWebhookError::MissingSignature);
    }

    let mut signed_payload = Vec::with_capacity(notification_url.len() + raw_body.len());
    signed_payload.extend_from_slice(notification_url.as_bytes());
    signed_payload.extend_from_slice(raw_body);

    if !crate::httpshared::verify_hmac_sha256_base64(
        webhook_signature_key.as_bytes(),
        &signed_payload,
        signature_header,
    ) {
        return Err(SquareWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| SquareWebhookError::MalformedResponse(e.to_string()))?;

    if envelope.event_type != "payment.updated" {
        return Err(SquareWebhookError::UnhandledEvent(envelope.event_type));
    }

    let payment_id = envelope.data.object.payment.id.clone();
    let outcome = models::parse_square_payment(&envelope.data.object.payment)
        .map_err(|e| SquareWebhookError::MalformedResponse(e.to_string()))?;

    if !outcome.settled {
        return Err(SquareWebhookError::UnhandledEvent(
            "payment.updated with status!=COMPLETED".to_string(),
        ));
    }

    Ok(SquareWebhookEvent {
        event_id: if envelope.event_id.is_empty() {
            outcome.event_id
        } else {
            envelope.event_id
        },
        reference: outcome.reference,
        payment_id,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use hmac::Mac;

    const KEY: &str = "square-test-webhook-signature-key";
    const NOTIFICATION_URL: &str = "https://example.com/webhooks/square";

    fn event_body(
        event_id: &str,
        payment_id: &str,
        reference: &str,
        currency: &str,
        amount: u64,
        status: &str,
    ) -> Vec<u8> {
        format!(
            r#"{{"event_id":"{event_id}","type":"payment.updated","data":{{"object":{{"payment":{{"id":"{payment_id}","status":"{status}","reference_id":"{reference}","amount_money":{{"amount":{amount},"currency":"{currency}"}}}}}}}}}}"#
        )
        .into_bytes()
    }

    fn sign(key: &str, notification_url: &str, body: &[u8]) -> String {
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(notification_url.as_bytes());
        mac.update(body);
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/square_test.go (webhook section).

    #[test]
    fn success() {
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "COMPLETED");
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        let event = verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig).unwrap();
        assert_eq!(event.event_id, "evt_1");
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.payment_id, "pay_1");
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "USD");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "COMPLETED");
        assert_eq!(
            verify_and_parse(KEY, NOTIFICATION_URL, &body, ""),
            Err(SquareWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_signature_fails_closed() {
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "COMPLETED");
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        let tampered = String::from_utf8(body)
            .unwrap()
            .replace("\"amount\":5000", "\"amount\":1");
        assert_eq!(
            verify_and_parse(KEY, NOTIFICATION_URL, tampered.as_bytes(), &sig),
            Err(SquareWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_notification_url_fails_closed() {
        // The signature depends on the configured notification URL
        // matching exactly what Square signed against -- a mismatch here
        // must fail closed just like a wrong key would.
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "COMPLETED");
        let sig = sign(KEY, "https://wrong.example.com/hook", &body);
        assert_eq!(
            verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig),
            Err(SquareWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json".to_vec();
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        assert!(matches!(
            verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig),
            Err(SquareWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"event_id":"evt_1","type":"refund.updated","data":{}}"#.to_vec();
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        assert_eq!(
            verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig),
            Err(SquareWebhookError::UnhandledEvent("refund.updated".into()))
        );
    }

    #[test]
    fn non_completed_payment_is_unhandled() {
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "PENDING");
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        assert!(matches!(
            verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig),
            Err(SquareWebhookError::UnhandledEvent(_))
        ));
    }

    #[test]
    fn replayed_event_produces_stable_event_id() {
        let body = event_body("evt_1", "pay_1", "ord_1", "USD", 5000, "COMPLETED");
        let sig = sign(KEY, NOTIFICATION_URL, &body);
        let first = verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig).unwrap();
        let second = verify_and_parse(KEY, NOTIFICATION_URL, &body, &sig).unwrap();
        assert_eq!(first.event_id, second.event_id);
        assert!(!first.event_id.is_empty());
    }
}
