//! Extract the token from an iyzico Checkout Form callback — ported from
//! cackle's `internal/payments/iyzico.go`'s `extractIyzicoToken` /
//! `Webhook`.
//!
//! **A genuine, protocol-driven divergence from `stripe::webhook`/
//! `paystack::webhook`'s pure-function shape** (see `rail.rs`'s module
//! docs): iyzico's Checkout Form callback carries NO signature of its own —
//! the buyer's browser is redirected back with a bare `token`, and iyzico's
//! own documentation is explicit that the integrator MUST call
//! `retrieveCheckoutForm` server-to-server with that token to learn the
//! real outcome (cackle's file header, MEDIUM-HIGH confidence on this
//! half). That server-to-server call needs an HTTP client, credentials, and
//! the IYZWS request-signing this module has no access to — so unlike
//! Stripe/Paystack, this webhook's SECURITY-CRITICAL half cannot be a pure,
//! network-free function. This module only extracts the lookup key (pure,
//! mirrors `extractIyzicoToken` exactly); the actual re-confirmation is
//! [`crate::iyzico::IyzicoRail::handle_webhook`], which performs the exact
//! same authenticated `retrieveCheckoutForm` round trip `verify()` does —
//! never trusting anything else in the callback body, exactly as cackle's
//! own `Webhook` (which is *literally* `return p.Verify(ctx, token)`) does.

/// The settlement outcome of re-confirming an iyzico callback's token —
/// returned by [`crate::iyzico::IyzicoRail::handle_webhook`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IyzicoWebhookOutcome {
    /// The token this callback was about — the caller's own lookup key.
    /// **Caller's responsibility** (`PORTING.md` §6): this module has no
    /// way to know which of the caller's own stored `Receipt`s (if any)
    /// this token belongs to, or whether this event has already been
    /// processed — the same "caller keys replay-dedup on the event id
    /// itself" division of labour every other adapter's webhook module in
    /// this crate documents.
    pub token: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Sentinel errors specific to iyzico callback handling — mirrors cackle's
/// `ErrIyzicoMissingToken` / `ErrIyzicoResponseTooLarge`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IyzicoWebhookError {
    #[error("payments: iyzico: callback carried no token")]
    MissingToken,
}

/// Mirrors cackle's `extractIyzicoToken`: pulls `token` from either a
/// form-urlencoded POST body (iyzico's documented callback shape) or,
/// defensively, a JSON body, depending on the `Content-Type` header.
pub fn extract_token(content_type: &str, raw_body: &[u8]) -> Option<String> {
    if content_type.contains("application/json") {
        #[derive(serde::Deserialize)]
        struct Payload {
            #[serde(default)]
            token: String,
        }
        let payload: Payload = serde_json::from_slice(raw_body).ok()?;
        return (!payload.token.is_empty()).then_some(payload.token);
    }
    let body_str = std::str::from_utf8(raw_body).ok()?;
    for pair in body_str.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "token" {
            // iyzico's callback is application/x-www-form-urlencoded --
            // percent-decode the value the same way url-encoded form
            // fields are decoded (spaces as '+').
            return Some(percent_decode_form_value(v));
        }
    }
    None
}

/// Byte-safe percent-decoding (space treated as `+`, per form encoding):
/// decodes into a byte buffer first, then validates UTF-8, so a multi-byte
/// percent-encoded sequence round-trips correctly rather than being
/// reassembled one Latin-1 code point at a time. Falls back to the
/// original (undecoded) value if the bytes are not valid UTF-8, rather
/// than silently corrupting it.
fn percent_decode_form_value(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&v[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/iyzico_test.go (webhook/token-extraction section).

    #[test]
    fn extracts_token_from_form_body() {
        let body = b"token=tok_abc";
        assert_eq!(
            extract_token("application/x-www-form-urlencoded", body),
            Some("tok_abc".to_string())
        );
    }

    #[test]
    fn extracts_token_from_json_body() {
        let body = br#"{"token":"tok_abc"}"#;
        assert_eq!(
            extract_token("application/json", body),
            Some("tok_abc".to_string())
        );
    }

    #[test]
    fn missing_token_returns_none() {
        assert_eq!(
            extract_token("application/x-www-form-urlencoded", b""),
            None
        );
        assert_eq!(extract_token("application/json", b"{}"), None);
    }
}
