//! Wire shapes and the currency-bucket amount handling for the Adyen
//! adapter — ported from cackle's `internal/payments/adyen.go`.
//!
//! Doc sources (cited verbatim from adyen.go, verified live against
//! docs.adyen.com by cackle's own author; not re-verified from this
//! environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note):
//! - Pay by Link: <https://docs.adyen.com/api-explorer/Checkout/71/post/paymentLinks>
//! - Webhook types: <https://docs.adyen.com/development-resources/webhooks/webhook-types>
//! - Currency codes / minor units: <https://docs.adyen.com/development-resources/currency-codes/>
#![allow(dead_code)]

use patala_core::Error;

/// Mirrors cackle's `adyenZeroDecimalCurrencies`: ISO-4217 zero-exponent
/// currencies Adyen's `amount.value` ALSO treats as zero-decimal. Kept for
/// documentation parity with cackle's own file (which defines this var but
/// never actually branches on it in `adyenAmount` either — see
/// [`adyen_amount`]'s doc comment): every bucket except
/// [`ADYEN_NON_ISO_STANDARD`] is a straight passthrough for Adyen, so this
/// list is informational, not consulted.
const ADYEN_ZERO_DECIMAL: &[&str] = &[
    "JPY", "KRW", "DJF", "GNF", "KMF", "PYG", "RWF", "UGX", "VND", "VUV", "XAF", "XOF", "XPF",
];

/// Mirrors cackle's `adyenThreeDecimalCurrencies`: informational only, same
/// reasoning as [`ADYEN_ZERO_DECIMAL`] — these also pass straight through.
const ADYEN_THREE_DECIMAL: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

/// Mirrors cackle's `adyenNonISOStandardCurrencies`: currencies Adyen's own
/// currency-codes doc singles out as diverging from the plain ISO-4217
/// exponent (similar in spirit to Stripe's ISK/UGX exception, but cackle's
/// own author could not confirm the EXACT adjustment Adyen applies to each
/// of these four). Rather than guess a multiplier and risk a silent
/// under/over-charge, this port refuses orders in these currencies exactly
/// as cackle's `adyenAmount` does — see [`unsupported_currency`].
const ADYEN_NON_ISO_STANDARD: &[&str] = &["CLP", "CVE", "IDR", "ISK"];

fn contains(list: &[&str], code: &str) -> bool {
    list.iter().any(|c| c.eq_ignore_ascii_case(code))
}

/// Mirrors cackle's `adyenAmount`: converts `amount_minor` into Adyen's
/// `amount.value` field. Zero-decimal, three-decimal, and the ordinary
/// two-decimal default are all a direct passthrough — Adyen's own minor-unit
/// table and `patala_core`'s ISO-4217 minor-unit representation agree for
/// every currency in those buckets. Only [`ADYEN_NON_ISO_STANDARD`] is
/// refused.
pub fn adyen_amount(amount_minor: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(ADYEN_NON_ISO_STANDARD, &cur) {
        return Err(unsupported_currency(&cur));
    }
    Ok(amount_minor)
}

/// Mirrors cackle's `ErrAdyenUnsupportedCurrency`.
pub fn unsupported_currency(cur: &str) -> Error {
    Error::InvalidRequest(format!(
        "adyen: {cur} needs Adyen's non-ISO-standard minor-unit handling, which this adapter has not verified precisely; refusing rather than guessing"
    ))
}

/// Mirrors cackle's `ErrAdyenMalformedResponse`.
pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("adyen: malformed API response: {detail}"))
}

/// The response body of `POST /paymentLinks` this adapter reads — mirrors
/// cackle's anonymous `Begin` response struct.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct PaymentLinkResponse {
    pub id: String,
    pub url: String,
}

/// Adyen's documented error response shape:
/// `{"status":..., "errorCode":"...", "message":"..."}`. Mirrors cackle's
/// `adyenErrorEnvelope`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct AdyenErrorEnvelope {
    #[serde(default)]
    pub status: i64,
    #[serde(default, rename = "errorCode")]
    pub error_code: String,
    #[serde(default)]
    pub message: String,
}

/// Mirrors cackle's `classifyAdyenError`: builds an error for a non-2xx
/// Adyen response, best-effort including Adyen's own message and errorCode,
/// never the API key.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: AdyenErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "adyen: unexpected API response status: http {status}: {msg} (errorCode={})",
        env.error_code
    ))
}

/// The subset of Adyen's amount object (`{value, currency}`) this adapter
/// reads/writes — mirrors cackle's `adyenAmountObj`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AmountObj {
    pub value: u64,
    pub currency: String,
}

/// The subset of Adyen's `NotificationRequestItem`
/// (<https://docs.adyen.com/development-resources/webhooks/webhook-types>)
/// this adapter reads — mirrors cackle's `adyenNotificationRequestItem`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct NotificationRequestItem {
    #[serde(default, rename = "additionalData")]
    pub additional_data: AdditionalData,
    #[serde(default)]
    pub amount: AmountObj,
    #[serde(default, rename = "eventCode")]
    pub event_code: String,
    #[serde(default, rename = "merchantReference")]
    pub merchant_reference: String,
    #[serde(default, rename = "originalReference")]
    pub original_reference: String,
    #[serde(default, rename = "pspReference")]
    pub psp_reference: String,
    /// `"true"` / `"false"` — a STRING, not a bool, exactly as cackle's own
    /// field comment calls out.
    #[serde(default)]
    pub success: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct AdditionalData {
    #[serde(default, rename = "hmacSignature")]
    pub hmac_signature: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct NotificationItemWrapper {
    #[serde(rename = "NotificationRequestItem")]
    pub notification_request_item: NotificationRequestItem,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct NotificationEnvelope {
    #[serde(default)]
    pub live: String,
    #[serde(default, rename = "notificationItems")]
    pub notification_items: Vec<NotificationItemWrapper>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/adyen_test.go.

    #[test]
    fn ordinary_zero_and_three_decimal_pass_through() {
        for (amount, currency) in [(5000u64, "USD"), (1000, "JPY"), (1000, "KWD")] {
            assert_eq!(adyen_amount(amount, currency).unwrap(), amount);
        }
    }

    #[test]
    fn refuses_non_iso_standard_currencies() {
        for cur in ["CLP", "CVE", "IDR", "ISK"] {
            assert!(adyen_amount(1000, cur).is_err(), "{cur}");
        }
    }
}
