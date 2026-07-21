//! Verify a BTCPay webhook signature and extract which invoice it's about —
//! ported from cackle's `internal/payments/btcpay.go`'s `Webhook` method
//! (<https://docs.btcpayserver.org/Webhooks/>).
//!
//! **Not part of [`patala_core::PaymentRail`]**: same reasoning as
//! `stripe/webhook.rs`/`paystack/webhook.rs` — the trait has no webhook
//! method at all.
//!
//! **A deliberate, disclosed narrowing vs `stripe`/`paystack`'s webhook
//! modules — preserving cackle's own refetch-required security property,
//! not a regression**: `stripe.go`'s and `paystack.go`'s `Webhook` methods
//! trust the HMAC'd webhook body's own settlement fields directly (no
//! refetch), and this crate's `stripe::webhook`/`paystack::webhook` port
//! that faithfully. cackle's `btcpay.go` `Webhook` does NOT do this — its
//! own doc comment is explicit: *"rather than trust any settlement fields
//! that may or may not be present in the webhook payload itself,
//! [it refetches] the invoice from BTCPay's API and reports ITS
//! authoritative status... a forged webhook POST can, at worst, trigger one
//! extra authenticated GET; it can never fabricate a settlement."* This
//! function preserves exactly that property: it verifies the `BTCPay-Sig`
//! signature (proving the request genuinely came from this store's BTCPay
//! instance) and extracts ONLY the invoice id — it makes NO settlement
//! claim and performs NO network call itself. The caller MUST take the
//! returned `invoice_id`, find (or reconstruct) the [`patala_core::Receipt`]
//! whose [`super::proof::ChargeProof::invoice_id`] matches it, and call
//! [`patala_core::PaymentRail::verify`] on that receipt — which DOES
//! refetch from BTCPay and DOES perform the amount/currency reconciliation
//! — to get the authoritative answer. This keeps `webhook.rs` a pure,
//! synchronously-testable function (matching this crate's established
//! `webhook.rs` shape) while never regressing cackle's "webhook body is
//! never the settlement source of truth" guarantee for this adapter.

use serde::Deserialize;

/// Sentinel errors specific to BTCPay webhook handling — mirrors cackle's
/// `ErrBTCPayMissingSignature` / `ErrBTCPayInvalidSignature` /
/// `ErrBTCPayMalformedResponse` / `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BTCPayWebhookError {
    #[error("payments: btcpay: missing BTCPay-Sig webhook header")]
    MissingSignature,
    #[error("payments: btcpay: invalid BTCPay-Sig webhook signature")]
    InvalidSignature,
    #[error("payments: btcpay: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// A signature-verified BTCPay webhook, naming which invoice to re-verify.
/// Carries NO settlement claim — see module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BTCPayWebhookEvent {
    /// Mirrors cackle's choice of `EventID: inv.ID` ("one invoice settles at
    /// most once; the invoice id is a stable dedupe key") — for replay
    /// protection, the caller's job (see `PORTING.md` §6).
    pub event_id: String,
    /// The BTCPay invoice id this webhook is about — look up (or
    /// reconstruct) the matching [`patala_core::Receipt`] and call
    /// [`patala_core::PaymentRail::verify`] on it.
    pub invoice_id: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(rename = "invoiceId", default)]
    invoice_id: String,
}

/// Verify `signature_header` (the raw `BTCPay-Sig` header value, expected
/// shape `"sha256=<hex>"`) against `raw_body` under `secret`, then extract
/// the invoice id — mirrors cackle's `BTCPayProvider.Webhook` up to (but not
/// including) its refetch, which this port defers to `verify()` — see
/// module docs.
pub fn verify_and_extract(
    secret: &str,
    raw_body: &[u8],
    signature_header: &str,
) -> Result<BTCPayWebhookEvent, BTCPayWebhookError> {
    let signature_header = signature_header.trim();
    if signature_header.is_empty() {
        return Err(BTCPayWebhookError::MissingSignature);
    }
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return Err(BTCPayWebhookError::InvalidSignature);
    };
    if !crate::httpshared::verify_hmac_sha256_hex(secret.as_bytes(), raw_body, hex_sig) {
        return Err(BTCPayWebhookError::InvalidSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| BTCPayWebhookError::MalformedResponse(e.to_string()))?;
    if !envelope.event_type.starts_with("Invoice") {
        return Err(BTCPayWebhookError::UnhandledEvent(envelope.event_type));
    }
    if envelope.invoice_id.is_empty() {
        return Err(BTCPayWebhookError::MalformedResponse(
            "missing invoiceId".to_string(),
        ));
    }
    Ok(BTCPayWebhookEvent {
        event_id: envelope.invoice_id.clone(),
        invoice_id: envelope.invoice_id,
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
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    // Ported from cackle's internal/payments/btcpay_test.go (webhook section).

    #[test]
    fn valid_signature_extracts_invoice_id() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh","storeId":"store1"}"#;
        let sig = sign(body);
        let event = verify_and_extract(SECRET, body, &sig).unwrap();
        assert_eq!(event.invoice_id, "inv_wh");
        assert_eq!(event.event_id, "inv_wh");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh"}"#;
        assert_eq!(
            verify_and_extract(SECRET, body, ""),
            Err(BTCPayWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh"}"#;
        let tampered = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh_evil"}"#;
        let sig = sign(body);
        assert_eq!(
            verify_and_extract(SECRET, tampered, &sig),
            Err(BTCPayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh"}"#;
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"wrong-secret").unwrap();
        mac.update(body);
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert_eq!(
            verify_and_extract(SECRET, body, &sig),
            Err(BTCPayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn missing_sha256_prefix_fails_closed() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":"inv_wh"}"#;
        assert_eq!(
            verify_and_extract(SECRET, body, "deadbeef"),
            Err(BTCPayWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json at all";
        let sig = sign(body);
        assert!(matches!(
            verify_and_extract(SECRET, body, &sig),
            Err(BTCPayWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"type":"SomeUnrelatedEvent","invoiceId":"inv_wh"}"#;
        let sig = sign(body);
        assert_eq!(
            verify_and_extract(SECRET, body, &sig),
            Err(BTCPayWebhookError::UnhandledEvent(
                "SomeUnrelatedEvent".to_string()
            ))
        );
    }

    #[test]
    fn missing_invoice_id_fails_closed() {
        let body = br#"{"type":"InvoiceSettled","invoiceId":""}"#;
        let sig = sign(body);
        assert!(matches!(
            verify_and_extract(SECRET, body, &sig),
            Err(BTCPayWebhookError::MalformedResponse(_))
        ));
    }
}
