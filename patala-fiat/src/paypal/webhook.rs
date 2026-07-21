//! PayPal webhook types and pure parsing — ported from cackle's
//! `internal/payments/paypal.go`'s `Webhook` method
//! (<https://developer.paypal.com/api/rest/webhooks/rest/>).
//!
//! **A deliberate, disclosed deviation from this crate's usual `webhook.rs`
//! shape ("a free function... NOT a trait method" — `PORTING.md` §1) —
//! unique to PayPal among this crate's adapters, not an oversight.** Every
//! other webhook module in this crate (`stripe`/`paystack`'s local HMAC,
//! `btcpay`/`lnbits`/`opennode`/`coinbasecommerce`'s local HMAC/
//! shared-secret) verifies a signature with a PURE, synchronous, no-network
//! function. PayPal's own webhook verification is NOT a local check at
//! all — cackle's own file doc comment is explicit: *"PayPal does not use a
//! simple local HMAC — verification is a server-to-server round trip"*
//! against PayPal's own `/v1/notifications/verify-webhook-signature`
//! endpoint (which itself requires a fresh OAuth2 access token). A function
//! in this module cannot perform that round trip without either
//! duplicating [`crate::paypal::rail::PayPalRail`]'s private HTTP/token
//! machinery or exposing it as `pub(crate)` — neither is worth the
//! indirection for one adapter. So the actual verification entrypoint is
//! [`crate::paypal::rail::PayPalRail::handle_webhook`] (an inherent method,
//! not a trait method — `PaymentRail` still has no webhook concept at all,
//! satisfying the SAME underlying rule `PORTING.md` §1 exists to enforce).
//! This module holds everything that CAN stay pure: the header/event wire
//! shapes and [`parse_capture_completed`], which every unit test here
//! exercises directly without a network call.

use serde::Deserialize;

/// The five `PAYPAL-TRANSMISSION-*`/`PAYPAL-*` headers PayPal signs a
/// webhook delivery with — mirrors cackle's five `r.Header.Get(...)` calls
/// in `Webhook`.
#[derive(Clone, Copy, Debug)]
pub struct PayPalWebhookHeaders<'a> {
    pub transmission_id: &'a str,
    pub transmission_time: &'a str,
    pub cert_url: &'a str,
    pub auth_algo: &'a str,
    pub transmission_sig: &'a str,
}

impl PayPalWebhookHeaders<'_> {
    /// Mirrors cackle's all-five-required check.
    pub fn all_present(&self) -> bool {
        !self.transmission_id.is_empty()
            && !self.transmission_time.is_empty()
            && !self.cert_url.is_empty()
            && !self.auth_algo.is_empty()
            && !self.transmission_sig.is_empty()
    }
}

/// Sentinel errors specific to PayPal webhook handling — mirrors cackle's
/// `ErrPayPalMissingSignatureHeaders` / `ErrPayPalInvalidSignature` /
/// `ErrPayPalUnexpectedStatus` / `ErrPayPalMalformedResponse` /
/// `ErrUnhandledEvent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayPalWebhookError {
    #[error("payments: paypal: missing one or more PAYPAL-TRANSMISSION-* headers")]
    MissingSignatureHeaders,
    #[error("payments: paypal: webhook signature verification failed")]
    InvalidSignature,
    #[error("payments: paypal: unexpected API response status: {0}")]
    UnexpectedStatus(String),
    #[error("payments: paypal: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
}

/// The verified, settled outcome of a PayPal `PAYMENT.CAPTURE.COMPLETED`
/// webhook — built by [`crate::paypal::rail::PayPalRail::handle_webhook`]
/// after PayPal's own server-side signature verification has succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayPalWebhookEvent {
    /// Mirrors cackle's `eventID := event.ID` (falling back to the capture
    /// id) — for replay protection, the caller's job (see `PORTING.md` §6).
    pub event_id: String,
    /// The `custom_id` on the capture resource — expected to equal the
    /// `PayRequest::reference`/`Receipt::reference` this webhook is about.
    pub reference: String,
    pub amount_minor: u64,
    pub currency: String,
}

#[derive(Deserialize, Default)]
struct WebhookEnvelope {
    #[serde(default)]
    id: String,
    #[serde(default)]
    event_type: String,
    #[serde(default)]
    resource: serde_json::Value,
}

#[derive(Deserialize, Default)]
struct CaptureResource {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    custom_id: String,
    #[serde(default)]
    amount: CaptureAmount,
}

#[derive(Deserialize, Default)]
struct CaptureAmount {
    #[serde(default)]
    currency_code: String,
    #[serde(default)]
    value: String,
}

/// A parsed `PAYMENT.CAPTURE.COMPLETED` event body, prior to currency
/// conversion (done by the caller via
/// `models::paypal_amount_value_to_minor`, since that needs `Error`
/// plumbing this pure parsing step shouldn't own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCaptureEvent {
    pub event_id: String,
    pub custom_id: String,
    pub currency_code: String,
    pub value: String,
}

/// Parse (but do not currency-convert) a PayPal webhook body already known
/// to carry a valid signature (see module docs on why verification itself
/// lives in `rail.rs`). Mirrors cackle's `Webhook`'s parsing logic exactly:
/// only `PAYMENT.CAPTURE.COMPLETED` is handled; a resource whose own
/// `status` disagrees with the event type is rejected as inconsistent
/// rather than trusted; a missing `custom_id` is rejected (nothing to
/// reconcile against).
pub fn parse_capture_completed(raw_body: &[u8]) -> Result<ParsedCaptureEvent, PayPalWebhookError> {
    let envelope: WebhookEnvelope = serde_json::from_slice(raw_body)
        .map_err(|e| PayPalWebhookError::MalformedResponse(e.to_string()))?;
    if envelope.event_type != "PAYMENT.CAPTURE.COMPLETED" {
        return Err(PayPalWebhookError::UnhandledEvent(envelope.event_type));
    }
    let capture: CaptureResource = serde_json::from_value(envelope.resource)
        .map_err(|e| PayPalWebhookError::MalformedResponse(format!("event resource: {e}")))?;
    if !capture.status.is_empty() && capture.status != "COMPLETED" {
        return Err(PayPalWebhookError::MalformedResponse(format!(
            "PAYMENT.CAPTURE.COMPLETED event carried resource.status={:?}",
            capture.status
        )));
    }
    if capture.custom_id.is_empty() {
        return Err(PayPalWebhookError::MalformedResponse(
            "capture resource has no custom_id to reconcile against".to_string(),
        ));
    }
    let event_id = if !envelope.id.is_empty() {
        envelope.id
    } else {
        capture.id
    };
    Ok(ParsedCaptureEvent {
        event_id,
        custom_id: capture.custom_id,
        currency_code: capture.amount.currency_code,
        value: capture.amount.value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_body(
        id: &str,
        capture_id: &str,
        custom_id: &str,
        currency: &str,
        value: &str,
    ) -> Vec<u8> {
        format!(
            r#"{{"id":{id:?},"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{{"id":{capture_id:?},"status":"COMPLETED","custom_id":{custom_id:?},"amount":{{"currency_code":{currency:?},"value":{value:?}}}}}}}"#
        )
        .into_bytes()
    }

    // Ported from cackle's internal/payments/paypal_test.go (webhook
    // parsing section -- signature verification itself is tested in
    // rail.rs since it requires the mocked HTTP round trip).

    #[test]
    fn headers_all_present() {
        let h = PayPalWebhookHeaders {
            transmission_id: "tx-1",
            transmission_time: "2026-07-20T10:00:00Z",
            cert_url: "https://api.paypal.com/cert.pem",
            auth_algo: "SHA256withRSA",
            transmission_sig: "fake-sig",
        };
        assert!(h.all_present());
        let mut missing = h;
        missing.transmission_id = "";
        assert!(!missing.all_present());
    }

    #[test]
    fn parses_capture_completed_event() {
        let body = event_body("WH-EVT-1", "CAP1", "ord_1", "USD", "50.00");
        let parsed = parse_capture_completed(&body).unwrap();
        assert_eq!(parsed.event_id, "WH-EVT-1");
        assert_eq!(parsed.custom_id, "ord_1");
        assert_eq!(parsed.currency_code, "USD");
        assert_eq!(parsed.value, "50.00");
    }

    #[test]
    fn unhandled_event_type() {
        let body = br#"{"id":"WH-EVT-1","event_type":"PAYMENT.CAPTURE.REFUNDED","resource":{}}"#;
        assert_eq!(
            parse_capture_completed(body),
            Err(PayPalWebhookError::UnhandledEvent(
                "PAYMENT.CAPTURE.REFUNDED".to_string()
            ))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        assert!(matches!(
            parse_capture_completed(b"not json"),
            Err(PayPalWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn missing_custom_id_fails_closed() {
        let body = event_body("WH-EVT-1", "CAP1", "", "USD", "50.00");
        assert!(matches!(
            parse_capture_completed(&body),
            Err(PayPalWebhookError::MalformedResponse(_))
        ));
    }
}
