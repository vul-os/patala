//! Verify and parse a Paystack webhook — ported from cackle's
//! `internal/payments/paystack.go`'s `Webhook` method
//! (<https://paystack.com/docs/payments/webhooks/>).
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
//! Verification, exactly as cackle's `Webhook`: HMAC-SHA512, hex-encoded,
//! computed over the exact RAW request body (unlike Stripe, there is no
//! `"{timestamp}."` prefix — Paystack signs the body directly), header
//! `X-Paystack-Signature`.

use serde::Deserialize;

/// Sentinel errors specific to Paystack webhook handling — mirrors cackle's
/// `ErrMissingSignature` / `ErrInvalidSignature` / `ErrMalformedResponse` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaystackWebhookError {
    #[error("payments: paystack: missing webhook signature")]
    MissingSignature,
    #[error("payments: paystack: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: paystack: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaystackWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub amount_minor: u64,
    pub currency: String,
}

#[derive(Deserialize)]
struct Envelope {
    event: String,
    data: EventData,
}

#[derive(Deserialize, Default)]
struct EventData {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    reference: String,
    #[serde(default)]
    amount: u64,
    #[serde(default)]
    currency: String,
}

/// Verify `signature_header` against `raw_body` under `secret`, then parse
/// the event, failing closed at every step — mirrors cackle's
/// `PaystackProvider.Webhook`.
pub fn verify_and_parse(
    secret: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<PaystackWebhookEvent, PaystackWebhookError> {
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(PaystackWebhookError::MissingSignature);
    }
    if !crate::httpshared::verify_hmac_sha512_hex(secret.as_bytes(), raw_body, signature_header) {
        return Err(PaystackWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| PaystackWebhookError::MalformedResponse(e.to_string()))?;

    if envelope.event != "charge.success" {
        return Err(PaystackWebhookError::UnhandledEvent(envelope.event));
    }
    if envelope.data.status != "success" {
        // A charge.success EVENT whose nested data disagrees is
        // inconsistent -- refuse rather than guess which field to trust.
        return Err(PaystackWebhookError::MalformedResponse(format!(
            "charge.success event carried data.status={:?}",
            envelope.data.status
        )));
    }
    if envelope.data.reference.is_empty() || envelope.data.amount == 0 {
        return Err(PaystackWebhookError::MalformedResponse(
            "missing reference or non-positive amount".to_string(),
        ));
    }
    Ok(PaystackWebhookEvent {
        event_id: envelope.data.id.to_string(),
        reference: envelope.data.reference,
        amount_minor: envelope.data.amount,
        currency: envelope.data.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk_test_fake_secret_for_unit_tests";

    fn sign(body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/paystack_test.go (webhook section).

    #[test]
    fn valid_signature_succeeds() {
        let body = br#"{"event":"charge.success","data":{"id":555,"status":"success","reference":"ord_1","amount":5000,"currency":"ZAR","paid_at":"2026-07-20T10:00:00.000Z"}}"#;
        let sig = sign(body);
        let event = verify_and_parse(SECRET, body, &sig).unwrap();
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "ZAR");
        assert_eq!(event.event_id, "555");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"ord_1","amount":5000,"currency":"ZAR"}}"#;
        assert_eq!(
            verify_and_parse(SECRET, body, ""),
            Err(PaystackWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"ord_1","amount":5000,"currency":"ZAR"}}"#;
        let sig = sign(body);
        let tampered = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"ord_1","amount":999999999,"currency":"ZAR"}}"#;
        assert_eq!(
            verify_and_parse(SECRET, tampered, &sig),
            Err(PaystackWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"ord_1","amount":5000,"currency":"ZAR"}}"#;
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(b"some-other-secret").unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        assert_eq!(
            verify_and_parse(SECRET, body, &sig),
            Err(PaystackWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn invalid_hex_signature_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"ord_1","amount":5000,"currency":"ZAR"}}"#;
        assert_eq!(
            verify_and_parse(SECRET, body, "not-hex-zzz"),
            Err(PaystackWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"event":"transfer.success","data":{"id":1,"status":"success","reference":"trf_1","amount":5000,"currency":"ZAR"}}"#;
        let sig = sign(body);
        assert_eq!(
            verify_and_parse(SECRET, body, &sig),
            Err(PaystackWebhookError::UnhandledEvent(
                "transfer.success".to_string()
            ))
        );
    }

    #[test]
    fn malformed_json_with_valid_signature_fails_closed() {
        let body = b"{not valid json at all";
        let sig = sign(body);
        assert!(matches!(
            verify_and_parse(SECRET, body, &sig),
            Err(PaystackWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn inconsistent_status_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"failed","reference":"ord_1","amount":5000,"currency":"ZAR"}}"#;
        let sig = sign(body);
        assert!(matches!(
            verify_and_parse(SECRET, body, &sig),
            Err(PaystackWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn missing_reference_or_amount_fails_closed() {
        let body = br#"{"event":"charge.success","data":{"id":1,"status":"success","reference":"","amount":5000,"currency":"ZAR"}}"#;
        let sig = sign(body);
        assert!(matches!(
            verify_and_parse(SECRET, body, &sig),
            Err(PaystackWebhookError::MalformedResponse(_))
        ));
    }
}
