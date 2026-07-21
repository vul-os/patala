//! Verify and parse a Razorpay webhook — ported from cackle's
//! `internal/payments/razorpay.go`'s `Webhook` method
//! (<https://razorpay.com/docs/webhooks/validate-test/>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: the trait has no webhook
//! method at all -- same reasoning as every other adapter in this crate.
//!
//! Verification, exactly as cackle's `Webhook`: HMAC-SHA256, hex-encoded,
//! computed over the exact RAW request body (no timestamp prefix, no
//! notification-url prefix -- Razorpay signs the body directly), header
//! `X-Razorpay-Signature`. Reuses [`crate::httpshared::verify_hmac_sha256_hex`]
//! directly rather than re-implementing HMAC verification.

use serde::Deserialize;

use crate::razorpay::models::{self, RazorpayPayment};

/// Sentinel errors specific to Razorpay webhook handling — mirrors
/// cackle's `ErrRazorpayMissingSignature` / `ErrRazorpayInvalidSignature` /
/// `ErrRazorpayMalformedResponse` / `ErrRazorpayResponseTooLarge` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RazorpayWebhookError {
    #[error("payments: razorpay: missing X-Razorpay-Signature header")]
    MissingSignature,
    #[error("payments: razorpay: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: razorpay: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: razorpay: response body exceeds size limit")]
    ResponseTooLarge,
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RazorpayWebhookEvent {
    /// The Razorpay payment id, preferred for replay dedup.
    pub event_id: String,
    /// The Razorpay ORDER id (`payload.payment.entity.order_id`) -- **not**
    /// the original caller's `PayRequest::reference`: the webhook payload
    /// only ever carries Razorpay's own order id, so this is a genuine,
    /// cackle-inherited limitation (cackle's own webhook `Result.Reference`
    /// is `pay.OrderID` too, so this is faithful, not a regression) --
    /// unlike `charge()`'s `Receipt::reference`, which does echo the
    /// caller's own reference (see `proof.rs`'s module docs).
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

#[derive(Deserialize)]
struct Envelope {
    event: String,
    payload: EnvelopePayload,
}

#[derive(Deserialize, Default)]
struct EnvelopePayload {
    #[serde(default)]
    payment: EnvelopePayment,
}

#[derive(Deserialize, Default)]
struct EnvelopePayment {
    #[serde(default)]
    entity: RazorpayPayment,
}

/// Verify `signature_header` against `raw_body` under `webhook_secret`,
/// then parse the event, failing closed at every step -- mirrors cackle's
/// `RazorpayProvider.Webhook`.
pub fn verify_and_parse(
    webhook_secret: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<RazorpayWebhookEvent, RazorpayWebhookError> {
    // Mirrors cackle's own check order exactly: signature-header presence
    // first, THEN the body-size bound, THEN the HMAC itself.
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(RazorpayWebhookError::MissingSignature);
    }
    crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
        .map_err(|_| RazorpayWebhookError::ResponseTooLarge)?;

    if !crate::httpshared::verify_hmac_sha256_hex(
        webhook_secret.as_bytes(),
        raw_body,
        signature_header,
    ) {
        return Err(RazorpayWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| RazorpayWebhookError::MalformedResponse(e.to_string()))?;

    if envelope.event != "payment.captured" {
        return Err(RazorpayWebhookError::UnhandledEvent(envelope.event));
    }

    let outcome = models::evaluate_payment(&envelope.payload.payment.entity)
        .map_err(|e| RazorpayWebhookError::MalformedResponse(e.to_string()))?;

    Ok(RazorpayWebhookEvent {
        event_id: outcome.event_id,
        reference: outcome.order_id,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-razorpay-webhook-secret";

    fn sign(body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn captured_event(id: &str, order_id: &str, amount: u64, currency: &str) -> Vec<u8> {
        format!(
            r#"{{"event":"payment.captured","payload":{{"payment":{{"entity":{{"id":"{id}","order_id":"{order_id}","amount":{amount},"currency":"{currency}","status":"captured","created_at":1753000000}}}}}}}}"#
        )
        .into_bytes()
    }

    // Ported from cackle's internal/payments/razorpay_test.go (webhook section).

    #[test]
    fn valid_signature_succeeds() {
        let body = captured_event("pay_9", "order_abc123", 10000, "INR");
        let sig = sign(&body);
        let event = verify_and_parse(SECRET, &body, &sig).unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "order_abc123");
        assert_eq!(event.amount_minor, 10000);
        assert_eq!(event.event_id, "pay_9");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = captured_event("pay_9", "order_abc123", 10000, "INR");
        assert_eq!(
            verify_and_parse(SECRET, &body, ""),
            Err(RazorpayWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_signature_fails_closed() {
        let body = captured_event("pay_9", "order_abc123", 10000, "INR");
        let sig = sign(&body);
        let tampered = captured_event("pay_9", "order_abc123", 999_999_999, "INR");
        assert_eq!(
            verify_and_parse(SECRET, &tampered, &sig),
            Err(RazorpayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = captured_event("pay_9", "order_abc123", 10000, "INR");
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"wrong-secret").unwrap();
        mac.update(&body);
        let sig = hex::encode(mac.finalize().into_bytes());
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig),
            Err(RazorpayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn invalid_hex_signature_fails_closed() {
        let body = br#"{"event":"payment.captured","payload":{}}"#.to_vec();
        assert_eq!(
            verify_and_parse(SECRET, &body, "not-hex-zzz"),
            Err(RazorpayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn unhandled_event() {
        let body = br#"{"event":"payment.failed","payload":{"payment":{"entity":{"id":"pay_9","order_id":"order_abc123","amount":10000,"currency":"INR","status":"failed"}}}}"#.to_vec();
        let sig = sign(&body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig),
            Err(RazorpayWebhookError::UnhandledEvent(
                "payment.failed".to_string()
            ))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid json".to_vec();
        let sig = sign(&body);
        assert!(matches!(
            verify_and_parse(SECRET, &body, &sig),
            Err(RazorpayWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn oversized_body_rejected() {
        let junk = "a".repeat(crate::httpshared::DEFAULT_MAX_BODY_BYTES + 1024);
        let body = format!(
            r#"{{"event":"payment.captured","payload":{{"payment":{{"entity":{{"id":"pay_9","order_id":"order_abc123","amount":10000,"currency":"INR","status":"captured","note":"{junk}"}}}}}}}}"#
        )
        .into_bytes();
        assert_eq!(
            verify_and_parse(SECRET, &body, "irrelevant"),
            Err(RazorpayWebhookError::ResponseTooLarge)
        );
    }
}
