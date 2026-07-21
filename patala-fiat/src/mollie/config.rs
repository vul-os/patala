//! Configuration for [`crate::mollie::MollieRail`].
//!
//! Mirrors cackle's `NewMollie` (`internal/payments/mollie.go`): a single
//! API key covers both test (`test_...`) and live (`live_...`) modes --
//! Mollie encodes environment in the key prefix itself, not a separate base
//! URL, so unlike Adyen/Checkout.com there is only one required env var for
//! credentials (plus the webhook URL). `api_base_url` is fixed
//! (`https://api.mollie.com/v2`), same pattern as `stripe`/`paystack`.

use patala_core::Error;

/// Mollie's fixed API base -- mirrors cackle's `mollieAPIBase`.
pub const MOLLIE_API_BASE: &str = "https://api.mollie.com/v2";

/// Everything [`crate::mollie::MollieRail`] needs to talk to Mollie's API
/// and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct MollieConfig {
    /// Mollie API key (`test_...` or `live_...`). Mirrors cackle's `apiKey`
    /// / `CACKLE_MOLLIE_API_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub api_key: String,
    /// The absolute, public HTTPS URL Mollie should call back on payment
    /// status changes (the Create Payment `webhookUrl` field). Mirrors
    /// cackle's `webhookURL` / `CACKLE_MOLLIE_WEBHOOK_URL` -- required, no
    /// default, since a missing webhook URL would silently create payments
    /// this rail is never told about (cackle's own reasoning, ported
    /// verbatim).
    pub webhook_url: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`, same
    /// reasoning as `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: currencies this deployment accepts. Cackle's
    /// Mollie `Capabilities.Currencies` is `nil` (broad) -- the currency-level
    /// restriction cackle actually applies (non-2-decimal refused) happens
    /// inside [`crate::mollie::models::mollie_amount_value`], not here, same
    /// layering cackle itself uses. An empty `Vec` here means unrestricted,
    /// mirroring `stripe/config.rs`'s identical pattern.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `mollieHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl MollieConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewMollie` requiring both the API key and the webhook URL.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `MOLLIE_API_KEY` | yes | see [`Self::api_key`] |
    /// | `MOLLIE_WEBHOOK_URL` | yes | see [`Self::webhook_url`] |
    /// | `MOLLIE_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `MOLLIE_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `MOLLIE_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `MOLLIE_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let api_key = non_empty_env("MOLLIE_API_KEY")?;
        let webhook_url = non_empty_env("MOLLIE_WEBHOOK_URL")?;
        let requires_kyc = std::env::var("MOLLIE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("MOLLIE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("MOLLIE_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("MOLLIE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            api_key,
            webhook_url,
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
    Ok(v)
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
            "MOLLIE_API_KEY",
            "MOLLIE_WEBHOOK_URL",
            "MOLLIE_REQUIRES_KYC",
            "MOLLIE_CURRENCIES",
            "MOLLIE_SETTLEMENT_DAYS",
            "MOLLIE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_api_key_and_webhook_url() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(MollieConfig::from_env().is_err());

        std::env::set_var("MOLLIE_API_KEY", "test_x");
        assert!(MollieConfig::from_env().is_err(), "webhook url missing");

        std::env::set_var("MOLLIE_WEBHOOK_URL", "https://example.com/hook");
        let cfg = MollieConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.api_key, "test_x");
        assert_eq!(cfg.webhook_url, "https://example.com/hook");
        assert!(cfg.requires_kyc);
        assert!(cfg.currencies.is_empty());
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("MOLLIE_API_KEY", "test_x");
        std::env::set_var("MOLLIE_WEBHOOK_URL", "https://example.com/hook");
        std::env::set_var("MOLLIE_REQUIRES_KYC", "false");
        std::env::set_var("MOLLIE_SETTLEMENT_DAYS", "1");

        let cfg = MollieConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 1);
        clear_env();
    }
}
