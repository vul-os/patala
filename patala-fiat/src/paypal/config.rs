//! Configuration for [`crate::paypal::PayPalRail`].
//!
//! Mirrors cackle's `NewPayPal` (`internal/payments/paypal.go`):
//! `client_id`, `client_secret` and `webhook_id` are required, and `env`
//! must be explicitly `"live"` or `"sandbox"` — cackle's own reasoning,
//! preserved verbatim: *"required explicitly (not defaulted either way) so
//! a forgotten/misspelled value can never silently point at the wrong
//! PayPal environment, which would be a real-money-vs-play-money mistake in
//! either direction."*

use patala_core::Error;

/// Mirrors cackle's `paypalLiveBaseURL`.
pub const LIVE_BASE_URL: &str = "https://api-m.paypal.com";
/// Mirrors cackle's `paypalSandboxBaseURL`.
pub const SANDBOX_BASE_URL: &str = "https://api-m.sandbox.paypal.com";

/// Everything [`crate::paypal::PayPalRail`] needs to talk to PayPal's Orders
/// v2 API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct PayPalConfig {
    /// PayPal REST app client id. Mirrors cackle's `clientID` /
    /// `CACKLE_PAYPAL_CLIENT_ID`.
    pub client_id: String,
    /// PayPal REST app client secret. Mirrors cackle's `clientSecret` /
    /// `CACKLE_PAYPAL_CLIENT_SECRET`. Never logged, never `Debug`-printed in
    /// full.
    pub client_secret: String,
    /// The PayPal webhook id (from the PayPal developer dashboard) used to
    /// verify incoming webhook signatures. Mirrors cackle's `webhookID` /
    /// `CACKLE_PAYPAL_WEBHOOK_ID`.
    pub webhook_id: String,
    /// Resolved API base URL — [`LIVE_BASE_URL`] or [`SANDBOX_BASE_URL`],
    /// chosen by [`Self::from_env`] from `PAYPAL_ENV` (`"live"`/`"sandbox"`,
    /// required explicitly, no default either way — see module docs).
    pub base_url: String,
    /// **Gap vs cackle** (see `PORTING.md`): defaults `true` — PayPal
    /// transactions funded by a card/bank are subject to PayPal's own
    /// KYC/AML program, same reasoning as `stripe::config`/`paystack::config`.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Cackle's `Capabilities.Currencies` is
    /// `nil` (unrestricted) — an empty `Vec` here is the identical thing.
    /// Note this is independent of the currency-specific
    /// zero-/three-decimal handling in `models.rs`, which applies
    /// regardless of this list.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement to the organiser's
    /// bank. Cackle's `Capabilities` has no settlement-time field, and this
    /// author does not have confirmed data on PayPal's typical payout
    /// timing (NEEDS-CONFIRMATION) — defaults `2`, the same generic
    /// card-network-style estimate `stripe::config`/`paystack::config` use
    /// absent provider-specific data (`PATALA.md` §3's own T+2 example).
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `paypalHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl PayPalConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `PAYPAL_CLIENT_ID` | yes | see [`Self::client_id`] |
    /// | `PAYPAL_CLIENT_SECRET` | yes | see [`Self::client_secret`] |
    /// | `PAYPAL_WEBHOOK_ID` | yes | see [`Self::webhook_id`] |
    /// | `PAYPAL_ENV` | yes, exactly `"live"` or `"sandbox"` | see [`Self::base_url`] |
    /// | `PAYPAL_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `PAYPAL_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `PAYPAL_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `PAYPAL_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let client_id = non_empty_env("PAYPAL_CLIENT_ID")?;
        let client_secret = non_empty_env("PAYPAL_CLIENT_SECRET")?;
        let webhook_id = non_empty_env("PAYPAL_WEBHOOK_ID")?;
        let base_url = match std::env::var("PAYPAL_ENV")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "live" => LIVE_BASE_URL.to_string(),
            "sandbox" => SANDBOX_BASE_URL.to_string(),
            _ => {
                return Err(Error::InvalidRequest(
                    "environment variable PAYPAL_ENV must be exactly \"live\" or \"sandbox\""
                        .into(),
                ))
            }
        };
        let requires_kyc = std::env::var("PAYPAL_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("PAYPAL_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("PAYPAL_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("PAYPAL_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            client_id,
            client_secret,
            webhook_id,
            base_url,
            requires_kyc,
            currencies,
            settlement_days,
            timeout_secs,
        })
    }
}

fn non_empty_env(name: &'static str) -> Result<String, Error> {
    let v = std::env::var(name)
        .map_err(|_| Error::InvalidRequest(format!("environment variable {name} is not set")))?;
    if v.trim().is_empty() {
        return Err(Error::InvalidRequest(format!(
            "environment variable {name} is set but empty"
        )));
    }
    Ok(v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for var in [
            "PAYPAL_CLIENT_ID",
            "PAYPAL_CLIENT_SECRET",
            "PAYPAL_WEBHOOK_ID",
            "PAYPAL_ENV",
            "PAYPAL_REQUIRES_KYC",
            "PAYPAL_CURRENCIES",
            "PAYPAL_SETTLEMENT_DAYS",
            "PAYPAL_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_all_four_settings() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(PayPalConfig::from_env().is_err());

        std::env::set_var("PAYPAL_CLIENT_ID", "cid");
        assert!(PayPalConfig::from_env().is_err());
        std::env::set_var("PAYPAL_CLIENT_SECRET", "secret");
        assert!(PayPalConfig::from_env().is_err());
        std::env::set_var("PAYPAL_WEBHOOK_ID", "WH-1");
        assert!(PayPalConfig::from_env().is_err(), "PAYPAL_ENV missing");

        std::env::set_var("PAYPAL_ENV", "production"); // not "live" or "sandbox"
        assert!(PayPalConfig::from_env().is_err());

        std::env::set_var("PAYPAL_ENV", "sandbox");
        let cfg = PayPalConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, SANDBOX_BASE_URL);
        assert!(cfg.requires_kyc);
        clear_env();
    }

    #[test]
    fn from_env_accepts_live() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("PAYPAL_CLIENT_ID", "cid");
        std::env::set_var("PAYPAL_CLIENT_SECRET", "secret");
        std::env::set_var("PAYPAL_WEBHOOK_ID", "WH-1");
        std::env::set_var("PAYPAL_ENV", "LIVE");
        let cfg = PayPalConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, LIVE_BASE_URL);
        clear_env();
    }
}
