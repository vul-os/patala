//! Verify and parse a PayFast ITN (Instant Transaction Notification) —
//! ported from cackle's `internal/payments/payfast.go`'s `Webhook` method
//! (<https://developers.payfast.co.za/docs#step_5_confirm_payment>).
//!
//! **Reached through the trait** by
//! [`patala_core::PaymentRail::verify_webhook`] on this adapter's rail —
//! that wrapper is what makes this verification usable from the UniFFI
//! binding and the sidecar, and not only from Rust. What the rail method
//! delegates to is the rail's own inherent handler (this scheme needs an
//! authenticated round trip, so it cannot be a free function), which in
//! turn calls the pure half below.
//!
//! **A genuine, protocol-driven divergence from `stripe::webhook`/
//! `paystack::webhook`/`midtrans::webhook`/`yoco::webhook`'s pure-function
//! shape** (same class of divergence as `iyzico::webhook` — see that
//! module's docs): PayFast's own documented anti-fraud checklist requires,
//! in addition to the signature check this module performs, a
//! server-to-server round trip back to PayFast's `validate` endpoint
//! before trusting the notification at all. That round trip needs an HTTP
//! client, so it cannot live in a network-free function here — it is
//! [`crate::payfast::PayFastRail::handle_itn`], which calls
//! [`verify_and_parse`] (this module, pure: signature + field parsing)
//! FIRST, then performs the validate round trip, mirroring cackle's
//! `Webhook` exactly (signature check, then `confirmWithPayFast`, then
//! field extraction/mapping — this port reorders the OUTPUT step relative
//! to cackle's for clarity, but the confirm-with-PayFast call still gates
//! whether the caller ever sees a `settled: true` outcome, exactly as in
//! cackle).

use std::collections::HashMap;

use crate::payfast::models;

/// Sentinel errors specific to PayFast ITN handling — mirrors cackle's
/// `ErrPayFastMissingSignature` / `ErrPayFastInvalidSignature` /
/// `ErrPayFastMalformedNotification`. `ErrPayFastNotValidated`/
/// `ErrPayFastUnexpectedStatus` (the network-round-trip half) live on
/// [`crate::payfast::PayFastRail::handle_itn`] instead, since only that
/// method performs the call that can produce them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayFastWebhookError {
    #[error("payments: payfast: missing signature field")]
    MissingSignature,
    #[error("payments: payfast: invalid signature")]
    InvalidSignature,
    #[error("payments: payfast: malformed ITN payload: {0}")]
    MalformedNotification(String),
}

/// The settlement outcome of a signature-verified (but NOT yet
/// PayFast-`validate`-confirmed) ITN — see module docs: a caller must still
/// treat this as untrusted until [`crate::payfast::PayFastRail::handle_itn`]
/// (which wraps this) has also completed the validate round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayFastNotificationEvent {
    pub reference: String,
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
}

/// Verify the `signature` field embedded in `raw_body` (MD5 over the
/// ordered, url-encoded field set plus optional passphrase), then parse the
/// settlement outcome, failing closed at every step — mirrors the
/// signature-check + field-extraction halves of cackle's
/// `PayFastProvider.Webhook` (the `confirmWithPayFast` half is NOT here —
/// see module docs).
pub fn verify_and_parse(
    passphrase: &str,
    raw_body: &[u8],
) -> Result<PayFastNotificationEvent, PayFastWebhookError> {
    let values: HashMap<String, String> = models::parse_query_map(raw_body)
        .map_err(|e| PayFastWebhookError::MalformedNotification(e.to_string()))?;
    let given_sig = values.get("signature").cloned().unwrap_or_default();
    if given_sig.is_empty() {
        return Err(PayFastWebhookError::MissingSignature);
    }
    if !models::verify_signature(raw_body, &given_sig, passphrase) {
        return Err(PayFastWebhookError::InvalidSignature);
    }

    let outcome = models::evaluate_notification(&values)
        .map_err(|e| PayFastWebhookError::MalformedNotification(e.to_string()))?;

    Ok(PayFastNotificationEvent {
        reference: outcome.reference,
        event_id: outcome.event_id,
        settled: outcome.settled,
        amount_minor: outcome.amount_minor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payfast::models::Kv;

    const PASSPHRASE: &str = "test-passphrase";

    fn signed_body(fields: &[Kv], passphrase: &str) -> Vec<u8> {
        let sig = models::compute_signature(fields, passphrase);
        let mut s = String::new();
        for kv in fields {
            if !s.is_empty() {
                s.push('&');
            }
            s.push_str(&kv.key);
            s.push('=');
            s.push_str(&kv.value);
        }
        s.push_str("&signature=");
        s.push_str(&sig);
        s.into_bytes()
    }

    // Ported from cackle's internal/payments/payfast_test.go (signature/parsing section).

    #[test]
    fn valid_signature_succeeds() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "pf_payment_id".into(),
                value: "pf_123".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "COMPLETE".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let body = signed_body(&fields, PASSPHRASE);
        let event = verify_and_parse(PASSPHRASE, &body).unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 10000);
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = b"m_payment_id=ord_1&payment_status=COMPLETE&amount_gross=100.00";
        assert_eq!(
            verify_and_parse(PASSPHRASE, body),
            Err(PayFastWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_field_fails_closed() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "COMPLETE".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let sig = models::compute_signature(&fields, PASSPHRASE);
        // Attacker changes amount_gross after the signature was computed.
        let tampered = format!(
            "m_payment_id=ord_1&payment_status=COMPLETE&amount_gross=999999.00&signature={sig}"
        );
        assert_eq!(
            verify_and_parse(PASSPHRASE, tampered.as_bytes()),
            Err(PayFastWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_passphrase_fails_closed() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "COMPLETE".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let body = signed_body(&fields, "different-passphrase");
        assert_eq!(
            verify_and_parse(PASSPHRASE, &body),
            Err(PayFastWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn failed_status_is_not_paid() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "FAILED".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
            Kv {
                key: "pf_payment_id".into(),
                value: "pf_1".into(),
            },
        ];
        let body = signed_body(&fields, PASSPHRASE);
        let event = verify_and_parse(PASSPHRASE, &body).unwrap();
        assert!(!event.settled);
        assert_eq!(
            event.event_id, "pf_1",
            "a non-settling ITN still has to be nameable"
        );
    }

    /// The same defect one layer up, through the signature check: a
    /// CORRECTLY SIGNED failure ITN with no `pf_payment_id` used to be
    /// accepted with `event_id: ""`.
    #[test]
    fn a_correctly_signed_itn_with_no_pf_payment_id_is_refused() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "FAILED".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let body = signed_body(&fields, PASSPHRASE);
        match verify_and_parse(PASSPHRASE, &body) {
            Err(e) => assert!(
                e.to_string().contains("pf_payment_id"),
                "refused, but not for the missing id: {e}"
            ),
            Ok(ev) => panic!(
                "a signed ITN with no pf_payment_id reached Ok with event_id {:?}",
                ev.event_id
            ),
        }
    }
}
