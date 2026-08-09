//! Shared HTTP-safety and webhook-HMAC infrastructure used by every network
//! adapter in this crate — all twenty of them, which is what the private
//! `_adapter` marker feature means.
//!
//! Ported from cackle's `internal/payments/httpshared.go` (the bounded-read
//! discipline) plus the common hex-encoded-HMAC-over-a-signed-payload
//! verification shape that stripe.go's and paystack.go's own `Webhook`
//! methods each hand-roll independently (HMAC-SHA256 for Stripe,
//! HMAC-SHA512 for Paystack) — factored out here as one generic helper since
//! the decode/compute/constant-time-compare mechanics are identical; only
//! the digest algorithm and the signed-payload construction (Paystack signs
//! the raw body directly; Stripe signs `"{timestamp}.{raw body}"`) differ
//! and stay in each adapter's own `webhook` module.

// The SAME gate `lib.rs` puts on `pub mod httpshared;`, and it has to be: this
// module is `bounded_len_check`'s only home, and fifteen of the twenty
// adapters call it from a `rail.rs` that is compiled whenever their own
// feature is on. When this said `any(stripe, paystack, adyen, checkoutcom,
// mollie, mercadopago)` and `lib.rs` said `_adapter`, the module was DECLARED
// for all twenty and EMPTIED for fourteen of them, so `cargo check -p
// patala-fiat --features yoco` — and thirteen more single-processor builds —
// failed with `cannot find httpshared in crate`. An operator who wants one
// processor was pushed to `--all-features`, which links twenty processors'
// worth of adapter code into a payments binary. `scripts/check-features.sh`
// now builds each processor feature on its own so that cannot come back.
//
// The dependency-bearing helpers below keep their own narrower `cfg`s: this
// gate says "at least one adapter", not "hmac, sha2 and hex are linked".
#![cfg(feature = "_adapter")]

/// Cackle's `maxResponseBodyBytes` (`internal/payments/paystack.go`) and
/// `stripeMaxBodyBytes` (`stripe.go`) are both `1 << 20` (1 MiB) — cackle
/// keeps two separate constants (one per adapter file, by its own
/// no-shared-symbols convention for that codebase's build-order reasons);
/// this crate has no such constraint, so both adapters here use this one
/// value.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1 << 20;

/// Mirrors cackle's `boundedRead` (`httpshared.go`) / `paystackReadLimited`
/// / `stripeReadLimited`'s CONTRACT: an oversized body is always rejected,
/// never silently truncated or accepted.
///
/// **Honest deviation from the Go MECHANISM, not the contract:** cackle's
/// `boundedRead` wraps the reader in `io.LimitReader(r, limit+1)` so a
/// response is never fully read into memory before the size check — a true
/// streaming cap. `reqwest::Response::bytes()` (used by every adapter here,
/// exactly as `patala-hyperswitch`'s `rail.rs` already does) has no
/// equivalent without pulling in `futures-util` for `bytes_stream()`, which
/// this crate deliberately avoids to keep the `stripe`/`paystack` feature
/// builds as lean as `patala-hyperswitch`'s. This function instead checks an
/// ALREADY-MATERIALIZED buffer's length: a weaker DoS guard (a huge body is
/// still fully read before rejection) but the identical fail-closed
/// contract — oversized is always an error. Every adapter routes its
/// provider API responses AND incoming webhook bodies through this before
/// any `serde_json` parse or signature check, exactly as cackle's own
/// adapters do.
pub fn bounded_len_check(body: &[u8], limit: usize) -> Result<(), &'static str> {
    if body.len() > limit {
        return Err("payments: body exceeds size limit");
    }
    Ok(())
}

/// Verify `hex_signature` is a valid hex-encoded HMAC-SHA256 of
/// `signed_payload` under `secret`, constant-time, failing closed on
/// anything malformed. Used by:
/// - the `stripe` feature's webhook module (Stripe signs
///   `"{t}.{raw_body}"`, per <https://docs.stripe.com/webhooks/signatures>);
/// - the `checkoutcom` feature's webhook module (Checkout.com signs the raw
///   body directly, header `Cko-Signature`, per
///   <https://checkout.com/docs/developer-resources/webhooks/manage-webhooks/set-up-your-webhook-receiver>);
/// - the `mercadopago` feature's webhook module (Mercado Pago signs a
///   constructed manifest string, not the raw body -- see that module's own
///   doc comment -- header `x-signature`, per
///   <https://www.mercadopago.com/developers/en/docs/checkout-api/additional-content/security/signature>).
/// - the `razorpay`, `btcpay`, `opennode` and `coinbasecommerce` features'
///   webhook modules, each signing the raw body.
///
/// Gated on `_hmac_hex` — the marker feature that pulls in the very
/// `hmac`/`sha2`/`hex` this body needs — and not on a hand-kept list of
/// callers, which is what it was, and which is what left four of the seven
/// callers above unable to compile on their own.
#[cfg(feature = "_hmac_hex")]
pub fn verify_hmac_sha256_hex(secret: &[u8], signed_payload: &[u8], hex_signature: &str) -> bool {
    verify_hmac_hex::<hmac::Hmac<sha2::Sha256>>(secret, signed_payload, hex_signature)
}

/// Verify `hex_signature` is a valid hex-encoded HMAC-SHA512 of
/// `signed_payload` (the raw body) under `secret`, constant-time, failing
/// closed on anything malformed. Used by the `paystack` feature's webhook
/// module (Paystack signs the raw request body directly, per
/// <https://paystack.com/docs/payments/webhooks/>).
#[cfg(feature = "_hmac_hex")]
pub fn verify_hmac_sha512_hex(secret: &[u8], signed_payload: &[u8], hex_signature: &str) -> bool {
    verify_hmac_hex::<hmac::Hmac<sha2::Sha512>>(secret, signed_payload, hex_signature)
}

/// Verify `base64_signature` is a valid base64-encoded HMAC-SHA256 of
/// `signed_payload` under `secret`, constant-time, failing closed on
/// anything malformed (including undecodable base64 -- mirrors cackle's own
/// `verifyAdyenHMAC`, which treats a base64 decode failure the same as a
/// signature mismatch: `ErrAdyenInvalidSignature` either way). Used by the
/// `adyen` feature's webhook module -- Adyen is the only adapter in this
/// crate whose signature is base64, not hex, per
/// <https://docs.adyen.com/development-resources/webhooks/secure-webhooks/verify-hmac-signatures>.
#[cfg(feature = "_hmac_base64")]
pub fn verify_hmac_sha256_base64(
    secret: &[u8],
    signed_payload: &[u8],
    base64_signature: &str,
) -> bool {
    use base64::Engine;
    use hmac::Mac;
    if secret.is_empty() || base64_signature.trim().is_empty() {
        return false;
    }
    let Ok(expected_bytes) =
        base64::engine::general_purpose::STANDARD.decode(base64_signature.trim())
    else {
        return false;
    };
    let Ok(mut mac) = <hmac::Hmac<sha2::Sha256> as hmac::digest::KeyInit>::new_from_slice(secret)
    else {
        return false;
    };
    mac.update(signed_payload);
    mac.verify_slice(&expected_bytes).is_ok()
}

#[cfg(feature = "_hmac_hex")]
fn verify_hmac_hex<M>(secret: &[u8], signed_payload: &[u8], hex_signature: &str) -> bool
where
    M: hmac::Mac + hmac::digest::KeyInit,
{
    if secret.is_empty() || hex_signature.trim().is_empty() {
        return false;
    }
    let Ok(expected_bytes) = hex::decode(hex_signature.trim()) else {
        return false;
    };
    let Ok(mut mac) = <M as hmac::digest::KeyInit>::new_from_slice(secret) else {
        return false;
    };
    hmac::Mac::update(&mut mac, signed_payload);
    // `verify_slice` is constant-time and fails closed on any length or
    // content mismatch -- never a manual byte-by-byte `==`/`bytes::Equal`.
    mac.verify_slice(&expected_bytes).is_ok()
}

/// Constant-time byte comparison — for verifying static webhook tokens/secrets
/// (e.g. Xendit, Flutterwave) without leaking length-independent timing.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_len_check_accepts_under_limit_rejects_over() {
        assert!(bounded_len_check(b"short", 10).is_ok());
        assert!(bounded_len_check(&[0u8; 11], 10).is_err());
    }

    #[cfg(feature = "_hmac_hex")]
    #[test]
    fn hmac_sha256_genuine_verifies_tampered_fails_closed() {
        use hmac::Mac;
        let secret = b"whsec_test";
        let payload = b"1700000000.{\"id\":\"evt_1\"}";
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        assert!(verify_hmac_sha256_hex(secret, payload, &sig));
        assert!(!verify_hmac_sha256_hex(secret, b"tampered", &sig));
        assert!(!verify_hmac_sha256_hex(b"wrong-secret", payload, &sig));
        assert!(!verify_hmac_sha256_hex(secret, payload, "not-hex!!"));
        assert!(!verify_hmac_sha256_hex(b"", payload, &sig));
        assert!(!verify_hmac_sha256_hex(secret, payload, ""));
    }

    #[cfg(feature = "_hmac_hex")]
    #[test]
    fn hmac_sha512_genuine_verifies_tampered_fails_closed() {
        use hmac::Mac;
        let secret = b"sk_test";
        let payload = br#"{"event":"charge.success"}"#;
        let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(secret).unwrap();
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        assert!(verify_hmac_sha512_hex(secret, payload, &sig));
        assert!(!verify_hmac_sha512_hex(secret, b"tampered", &sig));
        assert!(!verify_hmac_sha512_hex(b"wrong-secret", payload, &sig));
        assert!(!verify_hmac_sha512_hex(secret, payload, "not-hex!!"));
    }

    #[cfg(feature = "_hmac_base64")]
    #[test]
    fn hmac_sha256_base64_genuine_verifies_tampered_fails_closed() {
        use base64::Engine;
        use hmac::Mac;
        let secret = b"adyen-test-hmac-key";
        let payload = b"psp_1:::ord_1:5000:EUR:AUTHORISATION:true";
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
        mac.update(payload);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(verify_hmac_sha256_base64(secret, payload, &sig));
        assert!(!verify_hmac_sha256_base64(secret, b"tampered", &sig));
        assert!(!verify_hmac_sha256_base64(b"wrong-secret", payload, &sig));
        assert!(!verify_hmac_sha256_base64(
            secret,
            payload,
            "not-valid-base64!!"
        ));
        assert!(!verify_hmac_sha256_base64(b"", payload, &sig));
        assert!(!verify_hmac_sha256_base64(secret, payload, ""));
    }
}
