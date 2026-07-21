//! Configuration for [`crate::adyen::AdyenRail`].
//!
//! Mirrors cackle's `NewAdyen` (`internal/payments/adyen.go`): ALL FOUR of
//! `api_key`, `merchant_account`, `hmac_key` and `api_base_url` are
//! required, with no default for any of them — unlike `stripe`/`paystack`
//! (which hardcode one fixed global API base), Adyen's live API base URL is
//! per-merchant (a unique subdomain prefix assigned in the Customer Area,
//! per <https://docs.adyen.com/development-resources/live-endpoints/>), so
//! guessing one would be wrong more often than not. This port keeps cackle's
//! choice to require it explicitly rather than default it.

use patala_core::Error;

/// Everything [`crate::adyen::AdyenRail`] needs to talk to Adyen's API and
/// describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct AdyenConfig {
    /// Adyen API key (`X-API-Key` header). Mirrors cackle's `apiKey` /
    /// `CACKLE_ADYEN_API_KEY`. Never logged, never `Debug`-printed in full.
    pub api_key: String,
    /// Adyen merchant account code. Mirrors cackle's `merchantAccount` /
    /// `CACKLE_ADYEN_MERCHANT_ACCOUNT`.
    pub merchant_account: String,
    /// The HMAC key exactly as Adyen's Customer Area presents it: a HEX
    /// STRING. Mirrors cackle's `CACKLE_ADYEN_HMAC_KEY`, which cackle's
    /// `NewAdyen` hex-decodes to raw bytes before use — see
    /// [`AdyenRail::new`](crate::adyen::rail::AdyenRail::new), which performs
    /// the identical decode, and adyen.go's own file-level HONESTY note on
    /// why hex-decode (vs. use-as-UTF8-bytes) was chosen and what direction
    /// getting it wrong would fail in (every genuine webhook's signature
    /// would then fail to verify — never the reverse).
    pub hmac_key_hex: String,
    /// Adyen's per-merchant API base URL (e.g.
    /// `https://checkout-test.adyen.com/v71`). Mirrors cackle's
    /// `CACKLE_ADYEN_API_BASE_URL` — required, no default, see module docs.
    pub api_base_url: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): cackle's `Capabilities` has
    /// no KYC field at all. Default `true`, same reasoning as
    /// `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: currencies this deployment accepts. Cackle's Adyen
    /// `Capabilities.Currencies` is `nil` (broad — "Adyen supports a large,
    /// evolving currency set"). An empty `Vec` here means the identical
    /// "unrestricted" thing, mirroring `stripe/config.rs`'s identical
    /// pattern.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's `adyenHTTPTimeout`
    /// (15s).
    pub timeout_secs: u64,
}

impl AdyenConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewAdyen` requiring all four of api key / merchant account / HMAC
    /// key / API base URL.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `ADYEN_API_KEY` | yes | see [`Self::api_key`] |
    /// | `ADYEN_MERCHANT_ACCOUNT` | yes | see [`Self::merchant_account`] |
    /// | `ADYEN_HMAC_KEY` | yes | see [`Self::hmac_key_hex`] |
    /// | `ADYEN_API_BASE_URL` | yes | see [`Self::api_base_url`] |
    /// | `ADYEN_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `ADYEN_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `ADYEN_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `ADYEN_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let api_key = non_empty_env("ADYEN_API_KEY")?;
        let merchant_account = non_empty_env("ADYEN_MERCHANT_ACCOUNT")?;
        let hmac_key_hex = non_empty_env("ADYEN_HMAC_KEY")?;
        let api_base_url = non_empty_env("ADYEN_API_BASE_URL")?;
        let requires_kyc = std::env::var("ADYEN_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("ADYEN_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("ADYEN_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("ADYEN_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            api_key,
            merchant_account,
            hmac_key_hex,
            api_base_url,
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
            "ADYEN_API_KEY",
            "ADYEN_MERCHANT_ACCOUNT",
            "ADYEN_HMAC_KEY",
            "ADYEN_API_BASE_URL",
            "ADYEN_REQUIRES_KYC",
            "ADYEN_CURRENCIES",
            "ADYEN_SETTLEMENT_DAYS",
            "ADYEN_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_all_four_vars() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(AdyenConfig::from_env().is_err());

        std::env::set_var("ADYEN_API_KEY", "k");
        assert!(AdyenConfig::from_env().is_err(), "merchant account missing");

        std::env::set_var("ADYEN_MERCHANT_ACCOUNT", "TestMerchant");
        assert!(AdyenConfig::from_env().is_err(), "hmac key missing");

        std::env::set_var("ADYEN_HMAC_KEY", "deadbeef");
        assert!(AdyenConfig::from_env().is_err(), "base url missing");

        std::env::set_var("ADYEN_API_BASE_URL", "https://checkout-test.adyen.com/v71");
        let cfg = AdyenConfig::from_env().expect("all four vars set");
        assert_eq!(cfg.api_key, "k");
        assert_eq!(cfg.merchant_account, "TestMerchant");
        assert_eq!(cfg.hmac_key_hex, "deadbeef");
        assert_eq!(cfg.api_base_url, "https://checkout-test.adyen.com/v71");
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
        std::env::set_var("ADYEN_API_KEY", "k");
        std::env::set_var("ADYEN_MERCHANT_ACCOUNT", "TestMerchant");
        std::env::set_var("ADYEN_HMAC_KEY", "deadbeef");
        std::env::set_var("ADYEN_API_BASE_URL", "https://checkout-test.adyen.com/v71");
        std::env::set_var("ADYEN_REQUIRES_KYC", "false");
        std::env::set_var("ADYEN_CURRENCIES", "eur, usd");
        std::env::set_var("ADYEN_SETTLEMENT_DAYS", "3");

        let cfg = AdyenConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.currencies, vec!["EUR".to_string(), "USD".to_string()]);
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
