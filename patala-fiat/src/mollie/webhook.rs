//! Mollie's classic webhook — ported from cackle's
//! `internal/payments/mollie.go`'s `Webhook` method
//! (<https://docs.mollie.com/reference/webhooks>).
//!
//! **Structural divergence from `stripe::webhook`/`paystack::webhook`,
//! flagged per `PORTING.md`**: every other adapter's `webhook.rs` in this
//! crate exposes a pure, network-free function (verify a signature, decode
//! a payload). Mollie's webhook design is unusually — and deliberately —
//! simple, ported verbatim from cackle's own file doc comment: the classic
//! `webhookUrl` field POSTs a SINGLE form-encoded parameter, `id`, and
//! carries NO signature at all. Mollie's own docs are explicit about why
//! this is safe: *"the script behind your webhook URL should use that ID to
//! fetch the payment status and act accordingly"* — i.e. verification IS the
//! authenticated server-to-server re-fetch of `GET /v2/payments/{id}`, not a
//! signature check. A forged webhook call can, at most, make an integration
//! re-check a real payment's real status early — it can never fabricate a
//! paid result, because the body of the webhook is never trusted for
//! anything beyond "go look up this id".
//!
//! Because that re-fetch needs an authenticated HTTP client (the API key,
//! the base URL), this module's own free function only does the
//! network-free half (extracting `id` from the form body). The actual
//! re-fetch-and-evaluate composition lives on
//! [`crate::mollie::rail::MollieRail::handle_webhook`], which needs `&self`
//! for HTTP access — mirroring cackle's own `Webhook`, which is a method on
//! `*MollieProvider` for the exact same reason, calling straight into
//! `Verify`.
//!
//! (Mollie has since added an opt-in "next-gen webhooks" beta with a real
//! HMAC signature over a richer payload
//! (<https://docs.mollie.com/reference/webhooks-new>) — a different,
//! separate subscription mechanism from the classic `webhookUrl` field this
//! adapter uses, not built here, exactly as cackle's own file doc comment
//! notes.)

/// Sentinel error for Mollie webhook handling — mirrors cackle's
/// `ErrMollieMissingID`. Every other webhook failure in this adapter
/// (malformed API response, HTTP failure) is a plain `patala_core::Error`
/// from the same re-fetch path `verify()` uses — see module docs on why a
/// dedicated error taxonomy (like `stripe`/`paystack`'s `...WebhookError`
/// enums) isn't needed here: there is no signature-verification step to
/// enumerate failure modes for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MollieWebhookError {
    #[error("payments: mollie: webhook body has no id parameter")]
    MissingId,
    #[error("payments: mollie: body is not valid form encoding: {0}")]
    MalformedForm(String),
}

/// The settlement outcome of a webhook delivery, once
/// [`crate::mollie::rail::MollieRail::handle_webhook`] has re-fetched and
/// evaluated the payment `extract_payment_id` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MollieWebhookEvent {
    /// Mollie's own payment id — mirrors cackle's `Result.EventID`, used for
    /// webhook replay dedup (a re-delivery of the same webhook call always
    /// names the same payment id).
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's own `Webhook` reading the classic form-encoded body:
/// `url.ParseQuery(string(body))` then `values.Get("id")`. Pure and
/// network-free — the network half is
/// [`crate::mollie::rail::MollieRail::handle_webhook`].
pub fn extract_payment_id(raw_body: &[u8]) -> Result<String, MollieWebhookError> {
    let body_str = std::str::from_utf8(raw_body)
        .map_err(|e| MollieWebhookError::MalformedForm(e.to_string()))?;
    let id = form_urlencoded::parse(body_str.as_bytes())
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    if id.is_empty() {
        return Err(MollieWebhookError::MissingId);
    }
    Ok(id)
}

/// A tiny, dependency-free `application/x-www-form-urlencoded` parser
/// (this crate deliberately avoids pulling in the `url`/`form_urlencoded`
/// crates just for one field — see `Cargo.toml`'s `mollie` feature, which
/// needs only `reqwest`). Handles the one shape this webhook ever sends:
/// `id=tr_....` (percent-decoding is not required for Mollie's own id
/// alphabet, but is still applied for defence-in-depth against a malformed
/// or padded body).
mod form_urlencoded {
    pub fn parse(input: &[u8]) -> impl Iterator<Item = (String, String)> + '_ {
        let s = std::str::from_utf8(input).unwrap_or("");
        s.split('&').filter(|pair| !pair.is_empty()).map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            (percent_decode(k), percent_decode(v))
        })
    }

    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                b'%' if i + 2 < bytes.len() => {
                    if let Ok(byte) = u8::from_str_radix(
                        std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                        16,
                    ) {
                        out.push(byte);
                        i += 3;
                    } else {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/mollie_test.go (webhook section).

    #[test]
    fn extracts_id_from_form_body() {
        assert_eq!(extract_payment_id(b"id=tr_test1").unwrap(), "tr_test1");
    }

    #[test]
    fn missing_id_fails_closed() {
        assert_eq!(extract_payment_id(b""), Err(MollieWebhookError::MissingId));
        assert_eq!(
            extract_payment_id(b"other=value"),
            Err(MollieWebhookError::MissingId)
        );
    }
}
