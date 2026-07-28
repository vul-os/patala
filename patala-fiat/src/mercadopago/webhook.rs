//! Verify a Mercado Pago webhook's `x-signature` header and extract its
//! payment id — ported from cackle's
//! `internal/payments/mercadopago.go`'s `Webhook` method
//! (<https://www.mercadopago.com/developers/en/docs/checkout-api/additional-content/security/signature>).
//!
//! **Reached through the trait** by
//! [`patala_core::PaymentRail::verify_webhook`] on this adapter's rail —
//! that wrapper is what makes this verification usable from the UniFFI
//! binding and the sidecar, and not only from Rust. What the rail method
//! delegates to is the rail's own inherent handler (this scheme needs an
//! authenticated round trip, so it cannot be a free function), which in
//! turn calls the pure half below.
//!
//! **Structural divergence from `stripe::webhook`/`paystack::webhook`,
//! flagged per `PORTING.md`, same reasoning as `mollie::webhook`**: Mercado
//! Pago's webhook notification body does NOT carry the settled amount —
//! only `{action, data:{id}, type}` — so, exactly as cackle's own file doc
//! comment states, "this file's `Webhook`... MUST therefore make an
//! authenticated server-to-server call (`GET /v1/payments/{id}`) to fetch
//! the actual amount/currency/status before returning any `Result` — never
//! trusts the push body for anything beyond 'go look this payment id up'."
//! This module's own free function only verifies the signature and extracts
//! that id — pure and network-free. The actual re-fetch-and-evaluate
//! composition lives on
//! [`crate::mercadopago::rail::MercadoPagoRail::handle_webhook`], which
//! needs `&self` for HTTP access.
//!
//! Verification, exactly as cackle's `Webhook` and Mercado Pago's own docs
//! describe: header `x-signature` carries `"ts=...,v1=..."`; the signed
//! manifest is the literal string
//! `"id:{data.id};request-id:{x-request-id};ts:{ts};"` — note the trailing
//! semicolon after each field, including the last — HMAC-SHA256'd with the
//! webhook secret and compared (hex, constant-time) to `v1`.

use serde::Deserialize;

/// Sentinel errors specific to Mercado Pago webhook handling — mirrors
/// cackle's `ErrMercadoPagoMissingSignature` /
/// `ErrMercadoPagoInvalidSignature` / `ErrMercadoPagoMalformedResponse` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MercadoPagoWebhookError {
    #[error("payments: mercadopago: missing x-signature/x-request-id headers")]
    MissingSignature,
    #[error("payments: mercadopago: invalid webhook signature")]
    InvalidSignature,
    #[error("payments: mercadopago: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    #[allow(dead_code)]
    action: String,
    #[serde(rename = "type")]
    event_type: String,
    data: EnvelopeData,
}

#[derive(Deserialize, Default)]
struct EnvelopeData {
    #[serde(default)]
    id: String,
}

/// Mirrors cackle's `parseMercadoPagoSignatureHeader`: splits
/// `"ts=1234567890,v1=abcdef..."` into its `ts` and `v1` parts.
fn parse_signature_header(header: &str) -> Option<(String, String)> {
    let mut ts = None;
    let mut v1 = None;
    for part in header.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            match k.trim() {
                "ts" => ts = Some(v.trim().to_string()),
                "v1" => v1 = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    match (ts, v1) {
        (Some(t), Some(v)) if !t.is_empty() && !v.is_empty() => Some((t, v)),
        _ => None,
    }
}

/// Verify `x_signature`/`x_request_id` against `raw_body` under `secret`,
/// then extract the payment id `data.id` names, failing closed at every
/// step — mirrors the signature-verification half of cackle's
/// `MercadoPagoProvider.Webhook` (the re-fetch half lives on
/// [`crate::mercadopago::rail::MercadoPagoRail::handle_webhook`]).
pub fn verify_signature_and_extract_id(
    secret: &str,
    raw_body: &[u8],
    x_signature: &str,
    x_request_id: &str,
) -> Result<String, MercadoPagoWebhookError> {
    let x_signature = x_signature.trim();
    let x_request_id = x_request_id.trim();
    if x_signature.is_empty() || x_request_id.is_empty() {
        return Err(MercadoPagoWebhookError::MissingSignature);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| MercadoPagoWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.data.id.is_empty() {
        return Err(MercadoPagoWebhookError::MalformedResponse(
            "missing data.id".to_string(),
        ));
    }

    let Some((ts, v1)) = parse_signature_header(x_signature) else {
        return Err(MercadoPagoWebhookError::InvalidSignature);
    };
    let manifest = format!(
        "id:{};request-id:{};ts:{};",
        envelope.data.id, x_request_id, ts
    );
    if !crate::httpshared::verify_hmac_sha256_hex(secret.as_bytes(), manifest.as_bytes(), &v1) {
        return Err(MercadoPagoWebhookError::InvalidSignature);
    }

    if envelope.event_type != "payment" {
        return Err(MercadoPagoWebhookError::UnhandledEvent(envelope.event_type));
    }
    Ok(envelope.data.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-mp-webhook-secret";

    fn sign_manifest(data_id: &str, request_id: &str, ts: &str) -> String {
        use hmac::Mac;
        let manifest = format!("id:{data_id};request-id:{request_id};ts:{ts};");
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(manifest.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/mercadopago_test.go (webhook section).

    #[test]
    fn valid_signature_extracts_id() {
        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        let sig = format!(
            "ts=1700000000,v1={}",
            sign_manifest("555", "req-1", "1700000000")
        );
        let id = verify_signature_and_extract_id(SECRET, body, &sig, "req-1").unwrap();
        assert_eq!(id, "555");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        assert_eq!(
            verify_signature_and_extract_id(SECRET, body, "", ""),
            Err(MercadoPagoWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_request_id_fails_closed() {
        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        let sig = format!(
            "ts=1700000000,v1={}",
            sign_manifest("555", "req-1", "1700000000")
        );
        // Attacker (or a proxy bug) changes x-request-id after the
        // signature was computed for "req-1" -- manifest no longer matches.
        assert_eq!(
            verify_signature_and_extract_id(SECRET, body, &sig, "req-EVIL"),
            Err(MercadoPagoWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        use hmac::Mac;
        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        let manifest = "id:555;request-id:req-1;ts:1700000000;";
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"some-other-secret").unwrap();
        mac.update(manifest.as_bytes());
        let sig = format!(
            "ts=1700000000,v1={}",
            hex::encode(mac.finalize().into_bytes())
        );
        assert_eq!(
            verify_signature_and_extract_id(SECRET, body, &sig, "req-1"),
            Err(MercadoPagoWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid json";
        assert!(matches!(
            verify_signature_and_extract_id(SECRET, body, "ts=1700000000,v1=irrelevant", "req-1"),
            Err(MercadoPagoWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unhandled_type() {
        let body = br#"{"action":"created","type":"merchant_order","data":{"id":"555"}}"#;
        let sig = format!(
            "ts=1700000000,v1={}",
            sign_manifest("555", "req-1", "1700000000")
        );
        assert_eq!(
            verify_signature_and_extract_id(SECRET, body, &sig, "req-1"),
            Err(MercadoPagoWebhookError::UnhandledEvent(
                "merchant_order".to_string()
            ))
        );
    }
}
