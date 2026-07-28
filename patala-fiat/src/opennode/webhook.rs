//! Verify an OpenNode webhook signature and extract which charge it's about
//! — ported from cackle's `internal/payments/opennode.go`'s `Webhook`
//! method (<https://developers.opennode.com/docs/webhooks>).
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
//! **Signs only the charge id, not the whole body — ported exactly, not a
//! simplification**: unlike every other HMAC'd webhook in this crate
//! (Stripe/Paystack/BTCPay/Coinbase Commerce all sign the raw request
//! body), OpenNode's documented `hashed_order` field is
//! `HMAC-SHA256(key=API key, message=charge id)` — the charge id string
//! ALONE, not the request body. Cackle's own file doc comment rates this
//! MODERATE confidence (callback verification schemes have shifted across
//! OpenNode API versions historically) and, as defense in depth, never
//! trusts the webhook body for settlement data regardless — see below.
//!
//! **Same deliberate narrowing as `btcpay`/`lnbits`/`coinbasecommerce`'s
//! webhook modules, preserving cackle's own refetch-required security
//! property**: cackle's `opennode.go` `Webhook` verifies `hashed_order`,
//! then ALWAYS refetches the charge from OpenNode's authenticated API
//! rather than trust the (also form-encoded, attacker-influenceable)
//! `status` field in the same POST body — its own doc comment: *"even a
//! subtly-wrong signature construction cannot fabricate a settlement — at
//! worst it would... cause an extra authenticated read."* This function
//! preserves exactly that: it verifies `hashed_order` and extracts ONLY the
//! charge id; it makes NO settlement claim and performs NO network call
//! itself. The caller MUST take the returned `charge_id`, find the
//! [`patala_core::Receipt`] whose [`super::proof::ChargeProof::charge_id`]
//! matches it, and call [`patala_core::PaymentRail::verify`] on that
//! receipt (which DOES refetch) to get the authoritative answer.

use std::collections::HashMap;

/// Sentinel errors specific to OpenNode webhook handling — mirrors cackle's
/// `ErrOpenNodeMissingSignature` / `ErrOpenNodeInvalidSignature` /
/// `ErrOpenNodeMalformedResponse`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenNodeWebhookError {
    #[error("payments: opennode: missing hashed_order in webhook callback")]
    MissingSignature,
    #[error("payments: opennode: invalid hashed_order in webhook callback")]
    InvalidSignature,
    #[error("payments: opennode: malformed API response: {0}")]
    MalformedResponse(String),
}

/// A signature-verified OpenNode webhook, naming which charge to re-verify.
/// Carries NO settlement claim — see module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenNodeWebhookEvent {
    /// Mirrors cackle's choice of dedupe key: a charge settles at most
    /// once, so its id is stable.
    pub event_id: String,
    /// The OpenNode charge id this webhook is about.
    pub charge_id: String,
}

/// Verify a form-encoded OpenNode webhook body: `id` (the charge id) and
/// `hashed_order` = hex `HMAC-SHA256(key=api_key, message=id)`. `form` is
/// the already-`x-www-form-urlencoded`-parsed body (caller's job to parse
/// the raw body into key/value pairs, e.g. via any form decoder — this
/// function takes the parsed map so it stays a pure, dependency-free
/// function, mirroring `verify_and_extract`'s shape in the other crypto
/// adapters' `webhook.rs` modules).
pub fn verify_and_extract(
    api_key: &str,
    form: &HashMap<String, String>,
) -> Result<OpenNodeWebhookEvent, OpenNodeWebhookError> {
    let id = form.get("id").map(String::as_str).unwrap_or("");
    if id.is_empty() {
        return Err(OpenNodeWebhookError::MalformedResponse(
            "missing id".to_string(),
        ));
    }
    let hashed_order = form.get("hashed_order").map(String::as_str).unwrap_or("");
    if hashed_order.is_empty() {
        return Err(OpenNodeWebhookError::MissingSignature);
    }
    if !crate::httpshared::verify_hmac_sha256_hex(api_key.as_bytes(), id.as_bytes(), hashed_order) {
        return Err(OpenNodeWebhookError::InvalidSignature);
    }
    Ok(OpenNodeWebhookEvent {
        event_id: id.to_string(),
        charge_id: id.to_string(),
    })
}

/// Minimal `application/x-www-form-urlencoded` body decoder, used by tests
/// and available to callers who don't already have a form-decoding crate on
/// hand. Not a general-purpose decoder (no `+`-as-space handling beyond the
/// common case, no repeated-key semantics) — sufficient for OpenNode's own
/// simple `id=...&hashed_order=...` callback shape.
pub fn parse_form_body(body: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let k = parts.next().unwrap_or("");
        let v = parts.next().unwrap_or("");
        let decode = |s: &str| -> String {
            let s = s.replace('+', " ");
            let mut bytes = Vec::with_capacity(s.len());
            let mut chars = s.bytes();
            while let Some(b) = chars.next() {
                if b == b'%' {
                    let hi = chars.next();
                    let lo = chars.next();
                    if let (Some(hi), Some(lo)) = (hi, lo) {
                        let hex = [hi, lo];
                        if let Ok(hex_str) = std::str::from_utf8(&hex) {
                            if let Ok(byte) = u8::from_str_radix(hex_str, 16) {
                                bytes.push(byte);
                                continue;
                            }
                        }
                    }
                    bytes.push(b);
                } else {
                    bytes.push(b);
                }
            }
            String::from_utf8_lossy(&bytes).into_owned()
        };
        out.insert(decode(k), decode(v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(api_key: &str, id: &str) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(api_key.as_bytes()).unwrap();
        mac.update(id.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    // Ported from cackle's internal/payments/opennode_test.go (webhook section).

    #[test]
    fn valid_signature_extracts_charge_id() {
        let mut form = HashMap::new();
        form.insert("id".to_string(), "charge_1".to_string());
        form.insert("hashed_order".to_string(), sign("test-api-key", "charge_1"));
        let event = verify_and_extract("test-api-key", &form).unwrap();
        assert_eq!(event.charge_id, "charge_1");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let mut form = HashMap::new();
        form.insert("id".to_string(), "charge_1".to_string());
        assert_eq!(
            verify_and_extract("test-api-key", &form),
            Err(OpenNodeWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_signature_fails_closed() {
        // hashed_order computed for a DIFFERENT charge id than submitted.
        let mut form = HashMap::new();
        form.insert("id".to_string(), "charge_1".to_string());
        form.insert(
            "hashed_order".to_string(),
            sign("test-api-key", "charge_evil"),
        );
        assert_eq!(
            verify_and_extract("test-api-key", &form),
            Err(OpenNodeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_api_key_fails_closed() {
        let mut form = HashMap::new();
        form.insert("id".to_string(), "charge_1".to_string());
        form.insert("hashed_order".to_string(), sign("wrong-key", "charge_1"));
        assert_eq!(
            verify_and_extract("test-api-key", &form),
            Err(OpenNodeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn malformed_hex_signature_fails_closed() {
        let mut form = HashMap::new();
        form.insert("id".to_string(), "charge_1".to_string());
        form.insert("hashed_order".to_string(), "not-hex!!".to_string());
        assert_eq!(
            verify_and_extract("test-api-key", &form),
            Err(OpenNodeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn parse_form_body_roundtrip() {
        let parsed = parse_form_body("id=charge_1&hashed_order=abc123");
        assert_eq!(parsed.get("id").unwrap(), "charge_1");
        assert_eq!(parsed.get("hashed_order").unwrap(), "abc123");
    }
}
