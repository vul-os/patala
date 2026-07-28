//! Verify and parse a Checkout.com webhook — ported from cackle's
//! `internal/payments/checkoutcom.go`'s `Webhook` method
//! (<https://checkout.com/docs/developer-resources/webhooks/manage-webhooks/set-up-your-webhook-receiver>,
//! <https://checkout.com/docs/developer-resources/webhooks/webhook-event-types/payment_captured>).
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
//! Verification, exactly as cackle's `Webhook`: HMAC-SHA256, hex-encoded,
//! computed over the exact RAW request body (no timestamp prefix, unlike
//! Stripe — Checkout.com signs the body directly, same shape as Paystack's
//! scheme but SHA-256 not SHA-512), header `Cko-Signature`.

use serde::Deserialize;

use crate::checkoutcom::models::{self, PaymentPayload};

/// Sentinel errors specific to Checkout.com webhook handling — mirrors
/// cackle's `ErrCheckoutComMissingSignature` /
/// `ErrCheckoutComInvalidSignature` / `ErrCheckoutComMalformedResponse` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckoutComWebhookError {
    #[error("payments: checkoutcom: missing Cko-Signature header")]
    MissingSignature,
    #[error("payments: checkoutcom: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: checkoutcom: malformed API response: {0}")]
    MalformedResponse(String),
    /// Mirrors cackle's `ErrUnhandledEvent`: `payment_approved` (pre-capture)
    /// and everything else besides `payment_captured` is not treated as a
    /// final settlement by this build.
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutComWebhookEvent {
    /// The webhook envelope's own `id`, preferred over the payment id for
    /// replay dedup — mirrors cackle's comment on preferring the event id.
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `checkoutComWebhookEnvelope`.
#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    data: PaymentPayload,
}

/// Verify `signature_header` (hex-encoded HMAC-SHA256 over `raw_body`) under
/// `webhook_secret`, then parse the event, failing closed at every step --
/// mirrors cackle's `CheckoutComProvider.Webhook`.
pub fn verify_and_parse(
    webhook_secret: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<CheckoutComWebhookEvent, CheckoutComWebhookError> {
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(CheckoutComWebhookError::MissingSignature);
    }
    if !crate::httpshared::verify_hmac_sha256_hex(
        webhook_secret.as_bytes(),
        raw_body,
        signature_header,
    ) {
        return Err(CheckoutComWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| CheckoutComWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.event_type != "payment_captured" {
        return Err(CheckoutComWebhookError::UnhandledEvent(envelope.event_type));
    }

    let outcome = models::evaluate_payment(&envelope.data)
        .map_err(|e| CheckoutComWebhookError::MalformedResponse(e.to_string()))?;
    if !outcome.settled {
        return Err(CheckoutComWebhookError::MalformedResponse(
            "payment_captured event carried a non-Captured status".to_string(),
        ));
    }

    Ok(CheckoutComWebhookEvent {
        event_id: if envelope.id.is_empty() {
            envelope.data.id.clone()
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

    const SECRET: &str = "cko_test_webhook_secret";

    fn captured_event(
        id: &str,
        payment_id: &str,
        ref_: &str,
        currency: &str,
        amount: u64,
    ) -> Vec<u8> {
        format!(
            r#"{{"id":"{id}","type":"payment_captured","data":{{"id":"{payment_id}","status":"Captured","amount":{amount},"currency":"{currency}","reference":"{ref_}"}}}}"#
        )
        .into_bytes()
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/checkoutcom_test.go (webhook section).

    #[test]
    fn success() {
        let body = captured_event("evt_1", "pay_1", "ord_1", "USD", 5000);
        let sig = sign(SECRET, &body);
        let event = verify_and_parse(SECRET, &body, &sig).unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "USD");
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.event_id, "evt_1");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = captured_event("evt_1", "pay_1", "ord_1", "USD", 5000);
        assert_eq!(
            verify_and_parse(SECRET, &body, ""),
            Err(CheckoutComWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = captured_event("evt_1", "pay_1", "ord_1", "USD", 5000);
        let sig = sign(SECRET, &body);
        let tampered = captured_event("evt_1", "pay_1", "ord_1", "USD", 1);
        assert_eq!(
            verify_and_parse(SECRET, &tampered, &sig),
            Err(CheckoutComWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = captured_event("evt_1", "pay_1", "ord_1", "USD", 5000);
        let sig = sign("wrong-secret", &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig),
            Err(CheckoutComWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json".to_vec();
        let sig = sign(SECRET, &body);
        assert!(matches!(
            verify_and_parse(SECRET, &body, &sig),
            Err(CheckoutComWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"id":"evt_1","type":"payment_approved","data":{"id":"pay_1","status":"Authorized","amount":5000,"currency":"USD","reference":"ord_1"}}"#.to_vec();
        let sig = sign(SECRET, &body);
        assert_eq!(
            verify_and_parse(SECRET, &body, &sig),
            Err(CheckoutComWebhookError::UnhandledEvent(
                "payment_approved".to_string()
            ))
        );
    }

    #[test]
    fn replayed_event_produces_stable_event_id() {
        let body = captured_event("evt_1", "pay_1", "ord_1", "USD", 5000);
        let sig = sign(SECRET, &body);
        let first = verify_and_parse(SECRET, &body, &sig).unwrap();
        let second = verify_and_parse(SECRET, &body, &sig).unwrap();
        assert_eq!(first.event_id, second.event_id);
        assert!(!first.event_id.is_empty());
    }
}
