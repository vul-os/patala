//! Verify and parse a Xendit webhook — ported from cackle's
//! `internal/payments/xendit.go`'s `Webhook` method
//! (<https://developers.xendit.co/docs/webhooks/>).
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
//! **Intentionally the WEAKEST webhook scheme in this crate, faithfully
//! preserved, not strengthened** -- port faithfully, do not redesign
//! applies to Xendit's real (weaker) security model too. Xendit's
//! `x-callback-token` header is a STATIC per-account shared secret, echoed
//! back verbatim on every callback -- it is NOT a MAC over the request
//! body (unlike every other adapter here: Stripe/Razorpay HMAC-SHA256,
//! Paystack HMAC-SHA512, Square HMAC-SHA256-base64, PayU a keyed SHA-512
//! digest). The body itself is never cryptographically bound to the
//! token at all, exactly as cackle's own `hmac.Equal([]byte(given),
//! []byte(p.webhookToken))` implements it -- a constant-time STRING
//! compare, not a digest verification. `crate::httpshared::constant_time_eq`
//! is used here for the identical reason cackle uses `crypto/hmac.Equal`
//! for this comparison even though there's no MAC involved: leaking
//! equality-comparison timing on a bare shared secret would leak the token
//! byte-by-byte.

use crate::xendit::models::{self, XenditInvoice};

/// Sentinel errors specific to Xendit webhook handling — mirrors cackle's
/// `ErrXenditMissingSignature` / `ErrXenditInvalidSignature` /
/// `ErrXenditMalformedResponse` / `ErrUnhandledEvent` /
/// `ErrXenditResponseTooLarge`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XenditWebhookError {
    #[error("payments: xendit: missing x-callback-token header")]
    MissingToken,
    #[error("payments: xendit: invalid x-callback-token")]
    InvalidToken,
    #[error("payments: xendit: malformed API response: {0}")]
    MalformedResponse(String),
    #[error("payments: unhandled webhook event type: {0}")]
    UnhandledEvent(String),
    #[error("payments: xendit: response body exceeds size limit")]
    ResponseTooLarge,
}

/// The settlement outcome of a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XenditWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub amount_minor: u64,
    pub currency: String,
}

/// Verify `callback_token_header` against `webhook_token` (a constant-time
/// STRING compare, not a body signature -- see module docs), then parse the
/// invoice payload, failing closed at every step -- mirrors cackle's
/// `XenditProvider.Webhook`.
///
/// Unlike every other adapter here, Xendit's webhook payload IS the invoice
/// object itself (no separate `{event, data}` envelope) -- mirrors cackle's
/// `Webhook` decoding `body` directly into an `xenditInvoice`.
pub fn verify_and_parse(
    webhook_token: &str,
    raw_body: &[u8],
    callback_token_header: &str,
) -> Result<XenditWebhookEvent, XenditWebhookError> {
    let given = callback_token_header.trim();
    if given.is_empty() {
        return Err(XenditWebhookError::MissingToken);
    }
    crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
        .map_err(|_| XenditWebhookError::ResponseTooLarge)?;
    if !crate::httpshared::constant_time_eq(given.as_bytes(), webhook_token.as_bytes()) {
        return Err(XenditWebhookError::InvalidToken);
    }

    let inv: XenditInvoice = serde_json::from_slice(raw_body)
        .map_err(|e| XenditWebhookError::MalformedResponse(e.to_string()))?;

    if inv.status != "PAID" && inv.status != "SETTLED" {
        return Err(XenditWebhookError::UnhandledEvent(inv.status.clone()));
    }

    let outcome = models::invoice_to_outcome(&inv)
        .map_err(|e| XenditWebhookError::MalformedResponse(e.to_string()))?;

    Ok(XenditWebhookEvent {
        event_id: outcome.event_id,
        reference: outcome.reference,
        amount_minor: outcome.amount_minor,
        currency: outcome.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "test-callback-token";

    // Ported from cackle's internal/payments/xendit_test.go (webhook section).

    #[test]
    fn valid_token_succeeds() {
        let body = br#"{"id":"inv_9","external_id":"ord_1","status":"PAID","amount":10000,"paid_amount":10000,"currency":"IDR"}"#;
        let event = verify_and_parse(TOKEN, body, TOKEN).unwrap();
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 1_000_000);
        assert_eq!(event.currency, "IDR");
        assert_eq!(event.event_id, "inv_9");
    }

    #[test]
    fn missing_token_fails_closed() {
        let body = br#"{"id":"inv_9","external_id":"ord_1","status":"PAID","amount":10000,"currency":"IDR"}"#;
        assert_eq!(
            verify_and_parse(TOKEN, body, ""),
            Err(XenditWebhookError::MissingToken)
        );
    }

    #[test]
    fn wrong_token_fails_closed() {
        let body = br#"{"id":"inv_9","external_id":"ord_1","status":"PAID","amount":10000,"currency":"IDR"}"#;
        assert_eq!(
            verify_and_parse(TOKEN, body, "wrong-token"),
            Err(XenditWebhookError::InvalidToken)
        );
    }

    #[test]
    fn pending_status_unhandled() {
        let body = br#"{"id":"inv_9","external_id":"ord_1","status":"PENDING","amount":10000,"currency":"IDR"}"#;
        assert_eq!(
            verify_and_parse(TOKEN, body, TOKEN),
            Err(XenditWebhookError::UnhandledEvent("PENDING".to_string()))
        );
    }

    #[test]
    fn malformed_json_fails_closed() {
        let body = b"{not valid";
        assert!(matches!(
            verify_and_parse(TOKEN, body, TOKEN),
            Err(XenditWebhookError::MalformedResponse(_))
        ));
    }

    #[test]
    fn oversized_body_rejected() {
        let junk = "a".repeat(crate::httpshared::DEFAULT_MAX_BODY_BYTES + 1024);
        let body = format!(
            r#"{{"id":"inv_9","external_id":"ord_1","status":"PAID","amount":10000,"currency":"IDR","note":"{junk}"}}"#
        );
        assert_eq!(
            verify_and_parse(TOKEN, body.as_bytes(), TOKEN),
            Err(XenditWebhookError::ResponseTooLarge)
        );
    }
}
