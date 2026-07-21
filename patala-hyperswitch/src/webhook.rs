//! Verify a Hyperswitch outgoing-webhook signature, fail-closed.
//!
//! **Scheme, as implemented, with sources** (see this crate's `README.md`
//! "Sources" section for exact repo paths/lines):
//! - Header name: `X-Webhook-Signature-512` -- Hyperswitch's own constant
//!   `router::headers::X_WEBHOOK_SIGNATURE`.
//! - Algorithm: **HMAC-SHA512**, hex-encoded -- Hyperswitch's own
//!   `OutgoingWebhookType::get_outgoing_webhooks_signature` signs with
//!   `common_utils::crypto::HmacSha512` and hex-encodes the output
//!   (`.map(hex::encode)`).
//! - Message: the **raw JSON body bytes** of the outgoing webhook request,
//!   exactly as Hyperswitch serialized and sent them (Hyperswitch signs
//!   `self.encode_to_string_of_json()` and sends that same string as the
//!   body) -- so the caller of [`verify_webhook_signature`] MUST pass the
//!   literal bytes received on the wire, never a value re-serialized by this
//!   crate or any other JSON library (key order / whitespace differences
//!   would break the HMAC).
//! - Key: the merchant's own `payment_response_hash_key`, a secret configured
//!   on the Hyperswitch merchant account / business profile -- there is no
//!   fixed or discoverable value; the operator must configure this crate
//!   with whatever they set on their Hyperswitch instance
//!   ([`crate::HyperswitchConfig::webhook_secret`]).
//!
//! **NEEDS-CONFIRMATION:** this reflects Hyperswitch's `main`-branch source
//! as of this crate's authoring; it has not been exercised against a live
//! self-hosted instance's actual webhook delivery from here (no live
//! Hyperswitch is reachable in this environment -- see the crate README).
//! If the target deployment runs an older/patched Hyperswitch build, or a
//! `v2`-API deployment with a different outgoing-webhook signer, confirm
//! against that instance's own behavior before relying on this in
//! production. This function still fails closed regardless: a mismatch,
//! malformed hex, or missing secret is always rejection, never acceptance.

use hmac::{Hmac, Mac};
use sha2::Sha512;

type HmacSha512 = Hmac<Sha512>;

/// Verify a Hyperswitch outgoing-webhook.
///
/// - `secret`: the merchant's `payment_response_hash_key`
///   ([`crate::HyperswitchConfig::webhook_secret`]).
/// - `raw_body`: the exact bytes of the HTTP request body as received (not
///   re-serialized).
/// - `signature_header`: the raw value of the `X-Webhook-Signature-512`
///   header (hex-encoded HMAC-SHA512).
///
/// Fails closed: an empty secret, an empty/non-hex signature header, a
/// wrong-length digest, or a mismatched HMAC all return `false`. This
/// function never panics on attacker-controlled input.
pub fn verify_webhook_signature(secret: &[u8], raw_body: &[u8], signature_header: &str) -> bool {
    if secret.is_empty() || signature_header.trim().is_empty() {
        return false;
    }

    let Ok(expected_bytes) = hex::decode(signature_header.trim()) else {
        return false;
    };

    // HMAC keys of any length are valid for HMAC-SHA512 (RFC 2104 permits
    // short keys, though a strong random secret is the operator's
    // responsibility, not this function's). `new_from_slice` never fails for
    // a `Hmac<Sha512>`.
    let Ok(mut mac) = HmacSha512::new_from_slice(secret) else {
        return false;
    };
    mac.update(raw_body);

    // `verify_slice` is constant-time and fails closed on any length or
    // content mismatch -- never a manual byte-by-byte `==` on a MAC.
    mac.verify_slice(&expected_bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha512::new_from_slice(secret).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn genuine_signature_verifies() {
        let secret = b"payment_response_hash_key_example";
        let body = br#"{"event_type":"payment_succeeded","payment_id":"pay_abc"}"#;
        let sig = sign(secret, body);
        assert!(verify_webhook_signature(secret, body, &sig));
    }

    #[test]
    fn tampered_body_fails_closed() {
        let secret = b"payment_response_hash_key_example";
        let body = br#"{"event_type":"payment_succeeded","payment_id":"pay_abc"}"#;
        let sig = sign(secret, body);
        let tampered = br#"{"event_type":"payment_succeeded","payment_id":"pay_XXX"}"#;
        assert!(!verify_webhook_signature(secret, tampered, &sig));
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = br#"{"event_type":"payment_succeeded"}"#;
        let sig = sign(b"the_real_secret", body);
        assert!(!verify_webhook_signature(b"a_wrong_secret", body, &sig));
    }

    #[test]
    fn malformed_hex_fails_closed_not_panics() {
        assert!(!verify_webhook_signature(
            b"secret",
            b"body",
            "not-hex-at-all!!"
        ));
    }

    #[test]
    fn empty_secret_or_signature_fails_closed() {
        assert!(!verify_webhook_signature(b"", b"body", "aabb"));
        assert!(!verify_webhook_signature(b"secret", b"body", ""));
        assert!(!verify_webhook_signature(b"secret", b"body", "   "));
    }
}
