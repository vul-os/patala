//! Verify and parse an Adyen webhook notification — ported from cackle's
//! `internal/payments/adyen.go`'s `Webhook` method
//! (<https://docs.adyen.com/development-resources/webhooks/webhook-types>).
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
//! This is Adyen's AUTHORITATIVE settlement signal for this port, not just
//! an optional push-delivery convenience: `AdyenRail::verify` cannot be
//! implemented at all (see `rail.rs`'s module docs) because Adyen's Pay by
//! Link resource has no documented "look up by merchant reference" endpoint
//! — a caller integrating this rail MUST wire up this webhook, exactly as
//! Adyen's own docs and cackle's own `adyen.go` file doc comment recommend.
//!
//! Verification, exactly as cackle's `Webhook` and Adyen's own docs
//! describe (<https://docs.adyen.com/development-resources/webhooks/secure-webhooks/verify-hmac-signatures>):
//! 1. Parse the JSON envelope (`{live, notificationItems: [{NotificationRequestItem}]}`).
//! 2. For each item (Adyen's HTTP/JSON webhook sends exactly one per call in
//!    practice; this loop is defensive, mirroring cackle's own loop):
//!    recompute `HMAC-SHA256` over the colon-joined signing string
//!    `"pspReference:originalReference:merchantAccountCode:merchantReference:amount.value:amount.currency:eventCode:success"`
//!    and compare it (base64-decoded, constant-time) against
//!    `additionalData.hmacSignature`.
//! 3. Only an item whose signature verifies AND whose `eventCode` is
//!    `"AUTHORISATION"` produces a result; everything else is skipped in
//!    favour of the next item, and the loop's last error is returned if none
//!    qualifies — mirroring cackle's fail-closed loop exactly.
//!
//! **NOTE, ported verbatim from cackle's own `verifyAdyenHMAC` comment**:
//! this signing string's `merchantAccountCode` segment is always the empty
//! string here, because [`NotificationRequestItem`](crate::adyen::models::NotificationRequestItem)
//! does not carry that field from the payload (cackle's own adapter doesn't
//! either). If Adyen's real notifications carry a non-empty
//! `merchantAccountCode`, every signature check here will fail closed
//! (safe direction) until this field is read and threaded through — flagged
//! for reconciliation against a live Adyen webhook, exactly as cackle's own
//! `adyen.go` flags it.

use crate::adyen::models::NotificationRequestItem;

/// Sentinel errors specific to Adyen webhook handling — mirrors cackle's
/// `ErrAdyenMissingSignature` / `ErrAdyenInvalidSignature` /
/// `ErrAdyenMalformedResponse` / `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdyenWebhookError {
    #[error("payments: adyen: notification item has no additionalData.hmacSignature")]
    MissingSignature,
    #[error("payments: adyen: invalid HMAC signature")]
    InvalidSignature,
    #[error("payments: adyen: malformed API response: {0}")]
    MalformedResponse(String),
    /// Mirrors cackle's `ErrUnhandledEvent`: a validly-signed notification
    /// item whose `eventCode` this build does not treat as a settlement
    /// (e.g. `"REFUND"`). Callers wiring an HTTP route should treat this as
    /// "ack with `[accepted]`, do nothing" — Adyen's own docs require the
    /// webhook endpoint to always respond `[accepted]` regardless.
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The settlement outcome of a webhook delivery — mirrors the subset of
/// cackle's `Result` a webhook produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdyenWebhookEvent {
    /// Adyen's own `pspReference` for THIS notification — mirrors cackle's
    /// `Result.EventID`, used for webhook replay dedup.
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
    /// Adyen's own `pspReference`, duplicated here (identical to
    /// `event_id` for an AUTHORISATION notification) under an explicit name
    /// for callers building a [`crate::adyen::proof::ChargeProof`] with
    /// `psp_reference` populated so a later `refund()` call can work — see
    /// that proof type's own module docs.
    pub psp_reference: String,
}

/// Mirrors cackle's `verifyAdyenHMAC` — see module docs' NOTE on the
/// `merchantAccountCode` gap.
fn verify_item_hmac(item: &NotificationRequestItem, key: &[u8]) -> Result<(), AdyenWebhookError> {
    let given = item.additional_data.hmac_signature.trim();
    if given.is_empty() {
        return Err(AdyenWebhookError::MissingSignature);
    }
    let signing_string = [
        item.psp_reference.as_str(),
        item.original_reference.as_str(),
        "", // merchantAccountCode -- see module docs' NOTE
        item.merchant_reference.as_str(),
        &item.amount.value.to_string(),
        item.amount.currency.as_str(),
        item.event_code.as_str(),
        item.success.as_str(),
    ]
    .join(":");
    if crate::httpshared::verify_hmac_sha256_base64(key, signing_string.as_bytes(), given) {
        Ok(())
    } else {
        Err(AdyenWebhookError::InvalidSignature)
    }
}

/// Verify and parse a raw Adyen webhook body under `hmac_key` (already
/// hex-decoded to raw bytes — see [`crate::adyen::config::AdyenConfig::hmac_key_hex`]),
/// failing closed at every step — mirrors cackle's `AdyenProvider.Webhook`.
pub fn verify_and_parse(
    hmac_key: &[u8],
    raw_body: &[u8],
) -> Result<AdyenWebhookEvent, AdyenWebhookError> {
    let envelope: crate::adyen::models::NotificationEnvelope = serde_json::from_slice(raw_body)
        .map_err(|e| AdyenWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.notification_items.is_empty() {
        return Err(AdyenWebhookError::MalformedResponse(
            "no notificationItems".to_string(),
        ));
    }

    let mut last_err = AdyenWebhookError::MalformedResponse(
        "no notificationItems produced a settlement result".to_string(),
    );
    for wrapper in &envelope.notification_items {
        let item = &wrapper.notification_request_item;
        if let Err(e) = verify_item_hmac(item, hmac_key) {
            last_err = e;
            continue;
        }
        if item.event_code != "AUTHORISATION" {
            last_err = AdyenWebhookError::UnhandledEvent(item.event_code.clone());
            continue;
        }
        if item.merchant_reference.is_empty() {
            last_err = AdyenWebhookError::MalformedResponse("missing merchantReference".into());
            continue;
        }
        // `pspReference` becomes `WebhookEvent::event_id`, which is documented
        // "Never empty: a caller cannot suppress a duplicate it cannot name."
        // Both return arms below copied it out unchecked, so an item that
        // passed the HMAC (which is computed over a payload whose pspReference
        // field may itself be empty) produced an unnameable event. Adyen
        // redelivers until acknowledged, so this is precisely the rail where a
        // missing dedup key costs the most.
        if item.psp_reference.is_empty() {
            last_err = AdyenWebhookError::MalformedResponse(
                "no pspReference: this notificationItem carries no id to deduplicate on".into(),
            );
            continue;
        }

        let currency = item.amount.currency.trim().to_ascii_uppercase();
        if item.success == "true" {
            if item.amount.value == 0 {
                last_err = AdyenWebhookError::MalformedResponse(
                    "success=true with non-positive amount".into(),
                );
                continue;
            }
            return Ok(AdyenWebhookEvent {
                event_id: item.psp_reference.clone(),
                reference: item.merchant_reference.clone(),
                settled: true,
                amount_minor: item.amount.value, // straight passthrough -- see models::adyen_amount
                currency,
                psp_reference: item.psp_reference.clone(),
            });
        }
        return Ok(AdyenWebhookEvent {
            event_id: item.psp_reference.clone(),
            reference: item.merchant_reference.clone(),
            settled: false,
            amount_minor: 0,
            currency,
            psp_reference: item.psp_reference.clone(),
        });
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/adyen_test.go.

    const HMAC_KEY: &[u8] = b"test-adyen-hmac-key-32-bytes!!!!";

    fn sign_item(
        psp: &str,
        original: &str,
        merchant_ref: &str,
        value: u64,
        currency: &str,
        event_code: &str,
        success: &str,
    ) -> String {
        use base64::Engine;
        use hmac::Mac;
        let signing_string = [
            psp,
            original,
            "",
            merchant_ref,
            &value.to_string(),
            currency,
            event_code,
            success,
        ]
        .join(":");
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(HMAC_KEY).unwrap();
        mac.update(signing_string.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    fn notification_body(
        psp: &str,
        merchant_ref: &str,
        value: u64,
        currency: &str,
        event_code: &str,
        success: &str,
    ) -> Vec<u8> {
        let sig = sign_item(psp, "", merchant_ref, value, currency, event_code, success);
        format!(
            r#"{{"live":"false","notificationItems":[{{"NotificationRequestItem":{{"additionalData":{{"hmacSignature":"{sig}"}},"amount":{{"value":{value},"currency":"{currency}"}},"eventCode":"{event_code}","merchantReference":"{merchant_ref}","pspReference":"{psp}","success":"{success}"}}}}]}}"#
        )
        .into_bytes()
    }

    /// `WebhookEvent::event_id` is documented "Never empty: a caller cannot
    /// suppress a duplicate it cannot name", and `pspReference` is where this
    /// rail's comes from. Both return arms copied it out unchecked, so this
    /// body — whose HMAC is GENUINE, computed over a signing string with an
    /// empty pspReference exactly as Adyen constructs it — was accepted with
    /// `event_id: ""`. Adyen redelivers until acknowledged, so a consumer
    /// deduplicating on that id discards every distinct unacknowledged event
    /// after the first. Delete the `psp_reference.is_empty()` guard and this
    /// reports: `success=true: reached Ok with event_id "" -- a correctly
    /// HMAC'd notificationItem with no pspReference has no dedup key`.
    #[test]
    fn a_correctly_hmacd_item_with_no_psp_reference_is_refused() {
        for success in ["true", "false"] {
            let body = notification_body("", "ord_1", 5000, "EUR", "AUTHORISATION", success);
            match verify_and_parse(HMAC_KEY, &body) {
                Err(e) => assert!(
                    e.to_string().contains("pspReference"),
                    "success={success}: refused, but not for the missing id: {e}"
                ),
                Ok(ev) => panic!(
                    "success={success}: reached Ok with event_id {:?} -- a correctly \
                     HMAC'd notificationItem with no pspReference has no dedup key",
                    ev.event_id
                ),
            }
        }
    }

    #[test]
    fn success() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "AUTHORISATION", "true");
        let event = verify_and_parse(HMAC_KEY, &body).unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "EUR");
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.event_id, "psp_1");
        assert_eq!(event.psp_reference, "psp_1");
    }

    #[test]
    fn missing_signature_fails_closed() {
        let body = br#"{"live":"false","notificationItems":[{"NotificationRequestItem":{"amount":{"value":5000,"currency":"EUR"},"eventCode":"AUTHORISATION","merchantReference":"ord_1","pspReference":"psp_1","success":"true"}}]}"#;
        assert_eq!(
            verify_and_parse(HMAC_KEY, body),
            Err(AdyenWebhookError::MissingSignature)
        );
    }

    #[test]
    fn tampered_signature_fails_closed() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "AUTHORISATION", "true");
        let tampered = String::from_utf8(body)
            .unwrap()
            .replace("\"value\":5000", "\"value\":1");
        assert_eq!(
            verify_and_parse(HMAC_KEY, tampered.as_bytes()),
            Err(AdyenWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_key_fails_closed() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "AUTHORISATION", "true");
        let wrong_key = b"completely-different-key-material";
        assert_eq!(
            verify_and_parse(wrong_key, &body),
            Err(AdyenWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn failure_notification_is_not_settled() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "AUTHORISATION", "false");
        let event = verify_and_parse(HMAC_KEY, &body).unwrap();
        assert!(!event.settled);
    }

    #[test]
    fn unhandled_event_code() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "REFUND", "true");
        assert_eq!(
            verify_and_parse(HMAC_KEY, &body),
            Err(AdyenWebhookError::UnhandledEvent("REFUND".to_string()))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"not json at all";
        assert!(matches!(
            verify_and_parse(HMAC_KEY, body),
            Err(AdyenWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn no_notification_items_fails_closed() {
        let body = br#"{"live":"false","notificationItems":[]}"#;
        assert!(matches!(
            verify_and_parse(HMAC_KEY, body),
            Err(AdyenWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn replayed_event_produces_stable_event_id() {
        let body = notification_body("psp_1", "ord_1", 5000, "EUR", "AUTHORISATION", "true");
        let first = verify_and_parse(HMAC_KEY, &body).unwrap();
        let second = verify_and_parse(HMAC_KEY, &body).unwrap();
        assert_eq!(first.event_id, second.event_id);
        assert!(!first.event_id.is_empty());
    }
}
