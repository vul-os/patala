//! Verify and parse a PayU callback (`surl`/`furl` POST) — ported from
//! cackle's `internal/payments/payu.go`'s `Webhook` method
//! (<https://docs.payu.in/docs/hash-generation>).
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
//! Verification, exactly as cackle's `Webhook`:
//! 1. Bound the raw body length (`crate::httpshared::bounded_len_check`) --
//!    cackle's own webhook applies `boundedRead` to the incoming request
//!    body; this port adapts that to an already-materialized `&[u8]`, the
//!    same adaptation `httpshared.rs`'s own module docs describe for the
//!    outbound-response case.
//! 2. Parse as `application/x-www-form-urlencoded`.
//! 3. Recompute PayU's response hash
//!    (`crate::payu::models::response_hash`) and compare against the given
//!    `hash` field using [`crate::httpshared::constant_time_eq`] -- mirrors
//!    cackle's own `constantTimeEqualString`.
//! 4. Map `status` to settled/not-settled -- only the literal `"success"`
//!    (cackle's `Webhook` does NOT lower-case this field, unlike `Verify`'s
//!    `strings.ToLower`; this port preserves that exact asymmetry) is ever
//!    settled.

use std::collections::HashMap;

/// Sentinel errors specific to PayU webhook handling — mirrors cackle's
/// `ErrPayUMissingHash` / `ErrPayUInvalidHash` / `ErrPayUMalformedResponse`
/// / `ErrPayUResponseTooLarge`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayUWebhookError {
    #[error("payments: payu: missing hash field")]
    MissingHash,
    #[error("payments: payu: invalid response hash")]
    InvalidHash,
    #[error("payments: payu: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: payu: response body exceeds size limit")]
    ResponseTooLarge,
}

/// The settlement outcome of a PayU callback delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayUWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Verify `raw_body` (a `surl`/`furl` callback POST body) under
/// `merchant_key`/`salt`, then parse the event, failing closed at every
/// step -- mirrors cackle's `PayUProvider.Webhook`.
pub fn verify_and_parse(
    merchant_key: &str,
    salt: &str,
    raw_body: &[u8],
) -> Result<PayUWebhookEvent, PayUWebhookError> {
    crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
        .map_err(|_| PayUWebhookError::ResponseTooLarge)?;

    let values: HashMap<String, String> =
        url::form_urlencoded::parse(raw_body).into_owned().collect();

    let given = values.get("hash").map(String::as_str).unwrap_or("");
    if given.is_empty() {
        return Err(PayUWebhookError::MissingHash);
    }

    let status = values.get("status").map(String::as_str).unwrap_or("");
    let txnid = values.get("txnid").map(String::as_str).unwrap_or("");
    let amount = values.get("amount").map(String::as_str).unwrap_or("");
    let productinfo = values.get("productinfo").map(String::as_str).unwrap_or("");
    let firstname = values.get("firstname").map(String::as_str).unwrap_or("");
    let email = values.get("email").map(String::as_str).unwrap_or("");
    let mihpayid = values.get("mihpayid").map(String::as_str).unwrap_or("");

    if txnid.is_empty() || amount.is_empty() {
        return Err(PayUWebhookError::MalformedResponse(
            "missing txnid or amount".to_string(),
        ));
    }

    let expected = crate::payu::models::response_hash(
        merchant_key,
        salt,
        status,
        txnid,
        amount,
        productinfo,
        firstname,
        email,
    );
    if !crate::httpshared::constant_time_eq(expected.as_bytes(), given.as_bytes()) {
        return Err(PayUWebhookError::InvalidHash);
    }

    // `mihpayid` is this event's id, and `WebhookEvent::event_id` is documented
    // "Never empty: a caller cannot suppress a duplicate it cannot name." It
    // was checked only inside the `"success"` arm, so a SIGNED failure
    // redelivery — PayU retries those — arrived with no dedup key at all.
    // Checked before the status is even looked at, because the status is not
    // what makes an event nameable.
    if mihpayid.is_empty() {
        return Err(PayUWebhookError::MalformedResponse(
            "no mihpayid: this delivery carries no id to deduplicate on".to_string(),
        ));
    }

    let amount_minor = crate::currency::major_string_to_minor(amount, "INR")
        .map_err(|e| PayUWebhookError::MalformedResponse(format!("amount {amount:?}: {e}")))?;

    let settled = match status {
        "success" => {
            if amount_minor == 0 {
                return Err(PayUWebhookError::MalformedResponse(
                    "success status with non-positive amount".to_string(),
                ));
            }
            true
        }
        // "failure" | "failed" | anything else: fail closed -- never
        // treated as paid. Mirrors cackle's `default` arm.
        _ => false,
    };

    Ok(PayUWebhookEvent {
        event_id: mihpayid.to_string(),
        reference: txnid.to_string(),
        settled,
        amount_minor: if settled { amount_minor } else { 0 },
        currency: "INR".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERCHANT_KEY: &str = "gtKFFx";
    const SALT: &str = "eCwWELxi";

    fn signed_body(status: &str, mihpayid: &str) -> Vec<u8> {
        let hash = crate::payu::models::response_hash(
            MERCHANT_KEY,
            SALT,
            status,
            "txn_1",
            "100.00",
            "Order txn_1",
            "Jane",
            "a@b.com",
        );
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("status", status)
            .append_pair("txnid", "txn_1")
            .append_pair("amount", "100.00")
            .append_pair("productinfo", "Order txn_1")
            .append_pair("firstname", "Jane")
            .append_pair("email", "a@b.com")
            .append_pair("mihpayid", mihpayid)
            .append_pair("hash", &hash)
            .finish()
            .into_bytes()
    }

    // Ported from cackle's internal/payments/payu_test.go (webhook section).

    #[test]
    fn valid_hash_succeeds() {
        let body = signed_body("success", "mihpay123");
        let event = verify_and_parse(MERCHANT_KEY, SALT, &body).unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "txn_1");
        assert_eq!(event.amount_minor, 10000);
        assert_eq!(event.currency, "INR");
        assert_eq!(event.event_id, "mihpay123");
    }

    #[test]
    fn missing_hash_fails_closed() {
        let body = b"status=success&txnid=txn_1&amount=100.00".to_vec();
        assert_eq!(
            verify_and_parse(MERCHANT_KEY, SALT, &body),
            Err(PayUWebhookError::MissingHash)
        );
    }

    #[test]
    fn tampered_amount_fails_closed() {
        let hash = crate::payu::models::response_hash(
            MERCHANT_KEY,
            SALT,
            "success",
            "txn_1",
            "100.00",
            "Order txn_1",
            "Jane",
            "a@b.com",
        );
        // Attacker changes amount after the hash was computed.
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("status", "success")
            .append_pair("txnid", "txn_1")
            .append_pair("amount", "999999.00")
            .append_pair("productinfo", "Order txn_1")
            .append_pair("firstname", "Jane")
            .append_pair("email", "a@b.com")
            .append_pair("mihpayid", "mihpay123")
            .append_pair("hash", &hash)
            .finish()
            .into_bytes();
        assert_eq!(
            verify_and_parse(MERCHANT_KEY, SALT, &body),
            Err(PayUWebhookError::InvalidHash)
        );
    }

    #[test]
    fn wrong_salt_fails_closed() {
        let body = signed_body("success", "mihpay123"); // signed with SALT
        assert_eq!(
            verify_and_parse(MERCHANT_KEY, "different-salt", &body),
            Err(PayUWebhookError::InvalidHash)
        );
    }

    #[test]
    fn failure_status_is_not_paid() {
        let body = signed_body("failure", "mihpay123");
        let event = verify_and_parse(MERCHANT_KEY, SALT, &body).unwrap();
        assert!(!event.settled);
        assert_eq!(event.amount_minor, 0);
        assert_eq!(
            event.event_id, "mihpay123",
            "a non-settling delivery still has to be nameable"
        );
    }

    /// `WebhookEvent::event_id` is documented "Never empty: a caller cannot
    /// suppress a duplicate it cannot name", and `mihpayid` is where this
    /// rail's comes from. The check used to live inside the `"success"` arm
    /// only, so this exact body — CORRECTLY SIGNED, PayU's own hash over it —
    /// was accepted with `event_id: ""`. Delete the guard and this reports:
    /// `failure: reached Ok with event_id "" -- a correctly signed delivery
    /// with no mihpayid has no dedup key`.
    #[test]
    fn a_correctly_signed_delivery_with_no_mihpayid_is_refused() {
        for status in ["failure", "pending", "success"] {
            let body = signed_body(status, "");
            match verify_and_parse(MERCHANT_KEY, SALT, &body) {
                Err(e) => assert!(
                    e.to_string().contains("mihpayid"),
                    "{status}: refused, but not for the missing id: {e}"
                ),
                Ok(ev) => panic!(
                    "{status}: reached Ok with event_id {:?} -- a correctly signed \
                     delivery with no mihpayid has no dedup key",
                    ev.event_id
                ),
            }
        }
    }

    #[test]
    fn malformed_body_fails_closed() {
        // A body that decodes fine as form-urlencoded (it always does -- the
        // format tolerates almost anything) but is missing txnid/amount.
        let body = b"hash=deadbeef".to_vec();
        assert!(matches!(
            verify_and_parse(MERCHANT_KEY, SALT, &body),
            Err(PayUWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn oversized_body_rejected() {
        let junk = "a".repeat(crate::httpshared::DEFAULT_MAX_BODY_BYTES + 1024);
        let body = format!("status=success&txnid=txn_1&amount=100.00&note={junk}&hash=irrelevant")
            .into_bytes();
        assert_eq!(
            verify_and_parse(MERCHANT_KEY, SALT, &body),
            Err(PayUWebhookError::ResponseTooLarge)
        );
    }

    // TestPayUWebhook_ReplayedThroughHandleWebhook and
    // TestPayUWebhook_AmountMismatchFailsClosed are NOT ported: cackle's
    // `HandleWebhook`/`Reconcile`/replay-dedup registry-layer machinery is
    // out of scope for this crate's `PaymentRail` seam (`PORTING.md` §6) --
    // replay/reconcile dedup is the caller's job, keyed on
    // (rail_id, event_id).
}
