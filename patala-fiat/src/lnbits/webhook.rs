//! Verify an LNbits webhook's compensating shared secret and extract which
//! payment it's about — ported from cackle's `internal/payments/lnbits.go`'s
//! `Webhook` method.
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
//! **No cryptographic signature at all — a compensating control, ported
//! exactly as cackle designed it**: cackle's own file doc comment explains
//! LNbits' native webhook delivery has no built-in signing scheme
//! whatsoever, so cackle requires the webhook URL registered with LNbits to
//! embed an operator-chosen shared secret as a `?secret=` query parameter,
//! checked in constant time, and — like `btcpay`/`opennode`/
//! `coinbasecommerce` in this crate — NEVER trusts the webhook body for
//! settlement data, only for WHICH payment to ask about. This function
//! preserves exactly that: it checks `?secret=` and extracts the
//! `payment_hash`; it makes NO settlement claim and performs NO network
//! call itself. The caller MUST take the returned `payment_hash`, find the
//! [`patala_core::Receipt`] whose [`super::proof::ChargeProof::payment_hash`]
//! matches it, and call [`patala_core::PaymentRail::verify`] on that receipt
//! (which DOES poll LNbits' authoritative status) to get the real answer.

use serde::Deserialize;

/// Sentinel errors specific to LNbits webhook handling — mirrors cackle's
/// `ErrLNbitsMissingSignature` / `ErrLNbitsInvalidSignature` /
/// `ErrLNbitsMalformedResponse`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LNbitsWebhookError {
    #[error("payments: lnbits: missing ?secret= query parameter on webhook request")]
    MissingSecret,
    #[error("payments: lnbits: webhook ?secret= does not match the configured webhook secret")]
    InvalidSecret,
    #[error("payments: lnbits: malformed API response: {0}")]
    MalformedResponse(String),
}

/// A shared-secret-verified LNbits webhook, naming which payment to
/// re-verify. Carries NO settlement claim — see module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LNbitsWebhookEvent {
    /// Mirrors cackle's choice of `EventID: reference` ("a BOLT11 invoice
    /// settles at most once; the payment_hash is a stable dedupe key").
    pub event_id: String,
    /// The LNbits payment hash this webhook is about.
    pub payment_hash: String,
}

#[derive(Deserialize, Default)]
struct Envelope {
    #[serde(default)]
    payment_hash: String,
}

/// Verify `given_secret` (the raw `?secret=` query parameter value) against
/// `expected_secret` in constant time, then extract the `payment_hash` from
/// `raw_body` — mirrors cackle's `LNbitsProvider.Webhook` up to (but not
/// including) its re-verify call, which this port defers to `verify()` —
/// see module docs.
pub fn verify_and_extract(
    expected_secret: &str,
    given_secret: Option<&str>,
    raw_body: &[u8],
) -> Result<LNbitsWebhookEvent, LNbitsWebhookError> {
    let Some(given) = given_secret.filter(|s| !s.is_empty()) else {
        return Err(LNbitsWebhookError::MissingSecret);
    };
    if !crate::httpshared::constant_time_eq(given.as_bytes(), expected_secret.as_bytes()) {
        return Err(LNbitsWebhookError::InvalidSecret);
    }

    let envelope: Envelope = serde_json::from_slice(raw_body)
        .map_err(|e| LNbitsWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.payment_hash.is_empty() {
        return Err(LNbitsWebhookError::MalformedResponse(
            "missing payment_hash".to_string(),
        ));
    }
    Ok(LNbitsWebhookEvent {
        event_id: envelope.payment_hash.clone(),
        payment_hash: envelope.payment_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-webhook-secret";

    // Ported from cackle's internal/payments/lnbits_test.go (webhook section).

    #[test]
    fn missing_secret_fails_closed() {
        let body = br#"{"payment_hash":"hash123"}"#;
        assert_eq!(
            verify_and_extract(SECRET, None, body),
            Err(LNbitsWebhookError::MissingSecret)
        );
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let body = br#"{"payment_hash":"hash123"}"#;
        assert_eq!(
            verify_and_extract(SECRET, Some("wrong"), body),
            Err(LNbitsWebhookError::InvalidSecret)
        );
    }

    #[test]
    fn correct_secret_extracts_payment_hash() {
        let body = br#"{"payment_hash":"hash123"}"#;
        let event = verify_and_extract(SECRET, Some(SECRET), body).unwrap();
        assert_eq!(event.payment_hash, "hash123");
        assert_eq!(event.event_id, "hash123");
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json";
        assert!(matches!(
            verify_and_extract(SECRET, Some(SECRET), body),
            Err(LNbitsWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn missing_payment_hash_fails_closed() {
        let body = br#"{}"#;
        assert!(matches!(
            verify_and_extract(SECRET, Some(SECRET), body),
            Err(LNbitsWebhookError::MalformedResponse(_))
        ));
    }
}
