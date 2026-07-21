//! Wire shapes and the hash sequences for the PayU adapter — ported from
//! cackle's `internal/payments/payu.go`.
//!
//! Reference: <https://docs.payu.in/docs/hash-generation> (request/response
//! hash sequences) and <https://docs.payu.in/reference/verify-payment-api>
//! (server-to-server Verify Payment API). Confidence: MEDIUM, per cackle's
//! own file header — the hash sequences are corroborated across PayU
//! India's docs and third-party integration guides, but this has NOT been
//! re-verified live from this environment (see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note).
#![allow(dead_code)]

use patala_core::Error;
use std::collections::HashMap;

/// Mirrors cackle's `requestHash`: PayU's request hash sequence —
/// `key|txnid|amount|productinfo|firstname|email|udf1|udf2|udf3|udf4|udf5||||||SALT`
/// — a raw SHA-512 digest, hex-encoded. This is NOT a keyed HMAC: the salt
/// is simply one of the pipe-joined fields being hashed, exactly as cackle
/// computes it via `crypto/sha512` directly (no `crypto/hmac` involved).
pub fn request_hash(
    merchant_key: &str,
    salt: &str,
    txnid: &str,
    amount: &str,
    productinfo: &str,
    firstname: &str,
    email: &str,
) -> String {
    let joined = [
        merchant_key,
        txnid,
        amount,
        productinfo,
        firstname,
        email,
        "",
        "",
        "",
        "",
        "", // udf1..udf5, always empty -- this port never sets them, see rail.rs
        "",
        "",
        "",
        "",
        "", // the five trailing empty fields cackle's own sequence pads with
        salt,
    ]
    .join("|");
    sha512_hex(joined.as_bytes())
}

/// Mirrors cackle's `responseHash`: PayU's reverse (response verification)
/// hash sequence —
/// `SALT|status|udf5|udf4|udf3|udf2|udf1|email|firstname|productinfo|amount|txnid|key`
/// — note the REVERSED field order vs [`request_hash`]. Also a raw SHA-512
/// digest, not a keyed HMAC.
#[allow(clippy::too_many_arguments)] // mirrors cackle's own responseHash signature field-for-field
pub fn response_hash(
    merchant_key: &str,
    salt: &str,
    status: &str,
    txnid: &str,
    amount: &str,
    productinfo: &str,
    firstname: &str,
    email: &str,
) -> String {
    let joined = [
        salt,
        status,
        "",
        "",
        "",
        "",
        "", // udf5..udf1, always empty -- see request_hash
        email,
        firstname,
        productinfo,
        amount,
        txnid,
        merchant_key,
    ]
    .join("|");
    sha512_hex(joined.as_bytes())
}

fn sha512_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha512::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// `transaction_details` entry from PayU's Verify Payment API response —
/// mirrors cackle's `payUVerifyTransactionDetail`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PayUVerifyTransactionDetail {
    #[serde(default)]
    pub mihpayid: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "txnid")]
    pub txn_id: String,
    #[serde(default, rename = "amt")]
    pub amount: String,
    #[serde(default)]
    pub addedon: String,
}

/// `POST` response from PayU's Verify Payment API (`command=verify_payment`)
/// — mirrors cackle's anonymous struct in `Verify`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PayUVerifyResponse {
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub transaction_details: HashMap<String, PayUVerifyTransactionDetail>,
}

/// Mirrors cackle's `ErrPayUMalformedResponse`-wrapping pattern.
pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("payu: malformed API response: {detail}"))
}

/// Mirrors cackle's `ErrPayUUnexpectedStatus`-wrapping pattern.
pub fn unexpected_status(status: u16) -> Error {
    Error::Rail(format!(
        "payu: unexpected API response status: http {status}"
    ))
}

/// Mirrors the fail-closed `switch`/`strings.ToLower` status mapping shared
/// by cackle's `Webhook` and `Verify`: only `"success"` (case-insensitively
/// for `Verify`, exactly `"success"` for the webhook's raw `status` field,
/// matching cackle's own asymmetry -- `Webhook`'s `switch status` is NOT
/// lower-cased, `Verify`'s IS via `strings.ToLower(detail.Status)`) is ever
/// settled; everything else fails closed as not-paid.
pub fn is_settled_success_ci(status: &str) -> bool {
    status.eq_ignore_ascii_case("success")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/payu_test.go: the hash
    // sequences themselves aren't unit-tested directly in cackle (only
    // indirectly, via Begin/Webhook round-trips), so these tests instead
    // assert the round-trip property + the documented field order.
    #[test]
    fn request_and_response_hash_are_deterministic_and_differ() {
        let req = request_hash(
            "gtKFFx",
            "eCwWELxi",
            "txn_1",
            "100.00",
            "Order txn_1",
            "Jane",
            "a@b.com",
        );
        let req2 = request_hash(
            "gtKFFx",
            "eCwWELxi",
            "txn_1",
            "100.00",
            "Order txn_1",
            "Jane",
            "a@b.com",
        );
        assert_eq!(req, req2);
        assert_eq!(req.len(), 128, "sha512 hex digest is 128 hex chars");

        let resp = response_hash(
            "gtKFFx",
            "eCwWELxi",
            "success",
            "txn_1",
            "100.00",
            "Order txn_1",
            "Jane",
            "a@b.com",
        );
        assert_ne!(
            req, resp,
            "request and response hash sequences differ in field order"
        );
        assert_eq!(resp.len(), 128);
    }

    #[test]
    fn settled_status_mapping() {
        assert!(is_settled_success_ci("success"));
        assert!(is_settled_success_ci("SUCCESS"));
        for s in ["failure", "failed", "pending", "some-new-status"] {
            assert!(!is_settled_success_ci(s), "{s}");
        }
    }
}
