//! Configuration for [`crate::coinbasecommerce::CoinbaseCommerceRail`].
//!
//! Mirrors cackle's `NewCoinbaseCommerce`
//! (`internal/payments/coinbasecommerce.go`): both the API key and webhook
//! secret are required; the API base is a public hostname (not a secret),
//! so — like `opennode` — it has a sensible default.

use patala_core::Error;

/// Mirrors cackle's `coinbaseCommerceDefaultBaseURL`.
pub const DEFAULT_BASE_URL: &str = "https://api.commerce.coinbase.com";

/// Mirrors cackle's `coinbaseCommerceAPIVersion` — sent as the required
/// `X-CC-Version` header. Coinbase Commerce versions its API by date; this
/// is cackle's own last-known value. NEEDS-CONFIRMATION against current
/// docs (see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE" note).
pub const API_VERSION: &str = "2018-03-22";

/// Everything [`crate::coinbasecommerce::CoinbaseCommerceRail`] needs to
/// talk to Coinbase Commerce's hosted checkout API and describe itself
/// honestly to `patala-core`.
#[derive(Clone)]
pub struct CoinbaseCommerceConfig {
    /// Coinbase Commerce API key. Mirrors cackle's `apiKey` /
    /// `CACKLE_COINBASECOMMERCE_API_KEY`. Never logged, never
    /// `Debug`-printed in full.
    pub api_key: String,
    /// Per-endpoint webhook shared secret from the Coinbase Commerce
    /// dashboard. Mirrors cackle's `webhookSecret` /
    /// `CACKLE_COINBASECOMMERCE_WEBHOOK_SECRET`.
    pub webhook_secret: String,
    /// API base URL, trailing slash trimmed. Mirrors cackle's `baseURL` /
    /// `CACKLE_COINBASECOMMERCE_BASE_URL`, defaulting to
    /// [`DEFAULT_BASE_URL`].
    pub base_url: String,
    /// **Gap vs cackle** (see `PORTING.md`): defaults `false` — same
    /// reasoning as `opennode::config::OpenNodeConfig::requires_kyc`.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Cackle's `Capabilities.Currencies` is
    /// `nil` (unrestricted) — an empty `Vec` here is the identical thing.
    pub currencies: Vec<String>,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `cryptoDefaultHTTPTimeout` (20s).
    pub timeout_secs: u64,
}

impl CoinbaseCommerceConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `COINBASECOMMERCE_API_KEY` | yes | see [`Self::api_key`] |
    /// | `COINBASECOMMERCE_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `COINBASECOMMERCE_BASE_URL` | no (default [`DEFAULT_BASE_URL`]) | see [`Self::base_url`] |
    /// | `COINBASECOMMERCE_REQUIRES_KYC` | no (default `false`) | `"true"`/`"false"` |
    /// | `COINBASECOMMERCE_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `COINBASECOMMERCE_TIMEOUT_SECS` | no (default `20`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let api_key = non_empty_env("COINBASECOMMERCE_API_KEY")?;
        let webhook_secret = non_empty_env("COINBASECOMMERCE_WEBHOOK_SECRET")?;
        let base_url = std::env::var("COINBASECOMMERCE_BASE_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let requires_kyc = std::env::var("COINBASECOMMERCE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let currencies = std::env::var("COINBASECOMMERCE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout_secs = std::env::var("COINBASECOMMERCE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        Ok(Self {
            api_key,
            webhook_secret,
            base_url,
            requires_kyc,
            currencies,
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
            "COINBASECOMMERCE_API_KEY",
            "COINBASECOMMERCE_WEBHOOK_SECRET",
            "COINBASECOMMERCE_BASE_URL",
            "COINBASECOMMERCE_REQUIRES_KYC",
            "COINBASECOMMERCE_CURRENCIES",
            "COINBASECOMMERCE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_env_vars() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(CoinbaseCommerceConfig::from_env().is_err());

        std::env::set_var("COINBASECOMMERCE_API_KEY", "key");
        assert!(CoinbaseCommerceConfig::from_env().is_err());
        std::env::set_var("COINBASECOMMERCE_WEBHOOK_SECRET", "secret");

        let cfg = CoinbaseCommerceConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        clear_env();
    }
}
