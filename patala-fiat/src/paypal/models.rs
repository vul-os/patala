//! Wire shapes and money conversion for the PayPal adapter — ported from
//! cackle's `internal/payments/paypal.go`.
//!
//! Reference: <https://developer.paypal.com/docs/api/orders/v2/> (Orders
//! v2), <https://developer.paypal.com/api/rest/authentication/> (OAuth2
//! client credentials), <https://developer.paypal.com/api/rest/reference/currency-codes/>
//! (currency codes), <https://developer.paypal.com/api/rest/webhooks/rest/>
//! (webhook signature verification). Cackle's own file doc comment says
//! this file was verified live against developer.paypal.com's DOCS (not a
//! live/sandbox account) during its authoring — see this crate's
//! `PORTING.md` "UNVERIFIED AGAINST LIVE" note: no live/sandbox PayPal
//! account has been reachable from this environment either.
//!
//! **HONESTY notes, carried over from cackle's file doc comment
//! verbatim:**
//! 1. The full Get-Order status enum (`CREATED`, `PAYER_ACTION_REQUIRED`,
//!    `APPROVED`, `COMPLETED` — and possibly `SAVED`/`VOIDED`) was not
//!    confirmed exhaustively against a fetched schema page. This adapter
//!    only special-cases the values confirmed with reasonable confidence
//!    and treats everything else — recognised or not — as "not yet paid"
//!    (never as paid), so an incomplete enum can only make this adapter too
//!    conservative, never wrongly permissive.
//! 2. The webhook event JSON shape (`{id, event_type, resource: {...}}`) is
//!    PayPal's long-standing documented convention, corroborated via
//!    secondary summaries rather than a freshly fetched official sample —
//!    confirm against PayPal's Webhooks Simulator before production use.
//! 3. Three-decimal ISO-4217 currencies (KWD, BHD, JOD, OMR, TND) are not
//!    mentioned anywhere in PayPal's fetched currency-codes reference —
//!    this adapter refuses them rather than guessing at a decimal-string
//!    format PayPal might reject or misinterpret.
//!
//! **A provider-specific EXCEPTION to `crate::currency`'s general ISO-4217
//! table — ported exactly, NOT routed through `crate::currency`, per
//! `PORTING.md` §8's own guidance (see `stripe::models`'s identical
//! ISK/UGX exception for the worked example this mirrors):** PayPal treats
//! JPY/HUF/TWD as zero-decimal ON THE WIRE (`amount.value` has no decimal
//! point at all), even though ISO-4217 itself (and `crate::currency`'s
//! table) says HUF/TWD have exponent 2. And PayPal's docs never mention
//! three-decimal currencies (KWD/BHD/JOD/OMR/TND) at all — cackle refuses
//! them outright rather than guess, and this port does too, even though
//! `crate::currency` itself DOES know about them (it just isn't consulted
//! here). This is why `paypal` is the one adapter in this crate whose
//! amount conversion does NOT call into `crate::currency` at all.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `paypalZeroDecimalCurrencies`.
pub const ZERO_DECIMAL_CURRENCIES: &[&str] = &["JPY", "HUF", "TWD"];
/// Mirrors cackle's `paypalThreeDecimalCurrencies`.
pub const THREE_DECIMAL_CURRENCIES: &[&str] = &["KWD", "BHD", "JOD", "OMR", "TND"];

fn is_zero_decimal(cur: &str) -> bool {
    ZERO_DECIMAL_CURRENCIES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(cur))
}

fn is_three_decimal(cur: &str) -> bool {
    THREE_DECIMAL_CURRENCIES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(cur))
}

/// Mirrors cackle's `paypalAmountValue`: formats `amount_minor` (patala's
/// ISO-4217 minor-unit integer) as the decimal string PayPal's
/// `amount.value` field expects. `amount_minor` is already scaled to the
/// currency's real exponent, so this is purely string formatting, not a
/// numeric conversion. Unlike cackle's signed `int64`, patala's
/// `amount_minor: u64` is never negative, so cackle's negative-amount
/// branch is structurally impossible here and not ported (same convention
/// as `crate::currency`'s own doc comment on this point).
pub fn paypal_amount_value(amount_minor: u64, currency: &str) -> Result<String, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if is_three_decimal(&cur) {
        return Err(unsupported_currency(&cur));
    }
    if is_zero_decimal(&cur) {
        return Ok(amount_minor.to_string());
    }
    let whole = amount_minor / 100;
    let frac = amount_minor % 100;
    Ok(format!("{whole}.{frac:02}"))
}

/// Mirrors cackle's `paypalAmountValueToMinor`: the inverse of
/// [`paypal_amount_value`], for reconciling a settled amount PayPal reports
/// back into patala's `amount_minor`.
pub fn paypal_amount_value_to_minor(value: &str, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if is_three_decimal(&cur) {
        return Err(unsupported_currency(&cur));
    }
    let value = value.trim();
    if is_zero_decimal(&cur) {
        return value
            .parse::<u64>()
            .map_err(|_| malformed(&format!("zero-decimal amount {value:?} is not an integer")));
    }
    let mut parts = value.splitn(2, '.');
    let whole_part = parts.next().unwrap_or("");
    let whole: u64 = whole_part
        .parse()
        .map_err(|_| malformed(&format!("amount {value:?} has an invalid whole part")))?;
    let frac: u64 = match parts.next() {
        Some(f) => {
            let mut f = f.to_string();
            while f.len() < 2 {
                f.push('0');
            }
            f.truncate(2);
            f.parse().map_err(|_| {
                malformed(&format!("amount {value:?} has an invalid fractional part"))
            })?
        }
        None => 0,
    };
    Ok(whole * 100 + frac)
}

fn unsupported_currency(cur: &str) -> Error {
    Error::InvalidRequest(format!(
        "paypal: three-decimal ISO-4217 currency {cur:?} is not verified against PayPal's \
         documented amount semantics; refusing rather than guessing"
    ))
}

/// Mirrors cackle's `paypalErrorEnvelope` / `classifyPayPalError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        name: String,
        #[serde(default)]
        message: String,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if !env.message.is_empty() {
        env.message
    } else if !env.name.is_empty() {
        env.name
    } else {
        "no message".to_string()
    };
    Error::Rail(format!(
        "paypal: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("paypal: malformed API response: {detail}"))
}

/// `POST /v1/oauth2/token` response. Mirrors cackle's anonymous struct in
/// `fetchAccessToken`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenResponse {
    #[serde(default)]
    pub access_token: String,
}

/// One entry in an Order's `links` array.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Link {
    #[serde(default)]
    pub rel: String,
    #[serde(default)]
    pub href: String,
}

/// `POST /v2/checkout/orders` response. Mirrors cackle's anonymous struct in
/// `Begin`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CreateOrderResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CaptureAmount {
    #[serde(default)]
    pub currency_code: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Capture {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount: CaptureAmount,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Payments {
    #[serde(default)]
    pub captures: Vec<Capture>,
}

/// Mirrors cackle's `paypalPurchaseUnit` — the subset of an Order's
/// `purchase_units` entry this adapter reads, across both Get Order and
/// Capture Order responses.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PurchaseUnit {
    #[serde(default)]
    pub reference_id: String,
    #[serde(default)]
    pub custom_id: String,
    #[serde(default)]
    pub payments: Payments,
}

/// `GET`/`POST .../capture` Order response. Mirrors cackle's anonymous
/// structs in `Verify`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct OrderResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub purchase_units: Vec<PurchaseUnit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/paypal_test.go
    // (currency-formatting section).

    #[test]
    fn amount_value_ordinary_currency() {
        assert_eq!(paypal_amount_value(5000, "USD").unwrap(), "50.00");
    }

    #[test]
    fn amount_value_zero_decimal_currency() {
        assert_eq!(paypal_amount_value(1000, "JPY").unwrap(), "1000");
    }

    #[test]
    fn amount_value_three_decimal_currency_refused() {
        assert!(paypal_amount_value(1000, "KWD").is_err());
    }

    #[test]
    fn amount_value_to_minor_round_trips() {
        assert_eq!(paypal_amount_value_to_minor("50.00", "USD").unwrap(), 5000);
        assert_eq!(paypal_amount_value_to_minor("1000", "JPY").unwrap(), 1000);
    }
}
