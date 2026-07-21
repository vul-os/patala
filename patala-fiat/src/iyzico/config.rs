//! Configuration for [`crate::iyzico::IyzicoRail`].
//!
//! Mirrors cackle's `NewIyzico` (`internal/payments/iyzico.go`): both the
//! API key and secret key are required.

use patala_core::Error;

/// Mirrors cackle's `iyzicoProductionBase`.
pub const PRODUCTION_BASE_URL: &str = "https://api.iyzipay.com";

/// Mirrors cackle's hardcoded `Capabilities().Currencies` for iyzico.
pub const DEFAULT_CURRENCIES: &[&str] = &["TRY", "USD", "EUR", "GBP"];

/// Everything [`crate::iyzico::IyzicoRail`] needs to talk to iyzico's API
/// and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct IyzicoConfig {
    /// iyzico API key. Mirrors cackle's `apiKey` / `CACKLE_IYZICO_API_KEY`.
    /// Never logged, never `Debug`-printed in full.
    pub api_key: String,
    /// iyzico secret key. Mirrors cackle's `secretKey` /
    /// `CACKLE_IYZICO_SECRET_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub secret_key: String,
    /// API host. Mirrors cackle's `EnvIyzicoBaseURL` /
    /// `iyzicoProductionBase` — e.g. `https://sandbox-api.iyzipay.com` for
    /// testing. Defaults to [`PRODUCTION_BASE_URL`].
    pub base_url: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`, same
    /// reasoning as every other rail in this crate.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Defaults to [`DEFAULT_CURRENCIES`]
    /// (cackle's real, hardcoded list).
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `iyzicoHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl IyzicoConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `IYZICO_API_KEY` | yes | see [`Self::api_key`] |
    /// | `IYZICO_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `IYZICO_BASE_URL` | no (default [`PRODUCTION_BASE_URL`]) | API host override |
    /// | `IYZICO_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `IYZICO_CURRENCIES` | no (default [`DEFAULT_CURRENCIES`]) | comma-separated |
    /// | `IYZICO_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `IYZICO_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let api_key = non_empty_env("IYZICO_API_KEY")?;
        let secret_key = non_empty_env("IYZICO_SECRET_KEY")?;
        let base_url = std::env::var("IYZICO_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| PRODUCTION_BASE_URL.to_string());
        let requires_kyc = std::env::var("IYZICO_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("IYZICO_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| DEFAULT_CURRENCIES.iter().map(|s| s.to_string()).collect());
        let settlement_days = std::env::var("IYZICO_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("IYZICO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            api_key,
            secret_key,
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
            "IYZICO_API_KEY",
            "IYZICO_SECRET_KEY",
            "IYZICO_BASE_URL",
            "IYZICO_REQUIRES_KYC",
            "IYZICO_CURRENCIES",
            "IYZICO_SETTLEMENT_DAYS",
            "IYZICO_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_both_credentials() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(IyzicoConfig::from_env().is_err());

        std::env::set_var("IYZICO_API_KEY", "key_x");
        assert!(IyzicoConfig::from_env().is_err(), "secret still missing");

        std::env::set_var("IYZICO_SECRET_KEY", "secret_x");
        let cfg = IyzicoConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.api_key, "key_x");
        assert_eq!(cfg.secret_key, "secret_x");
        assert_eq!(cfg.base_url, PRODUCTION_BASE_URL);
        assert!(cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            DEFAULT_CURRENCIES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(cfg.settlement_days, 2);
        clear_env();
    }

    #[test]
    fn from_env_reads_base_url_override() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("IYZICO_API_KEY", "key_x");
        std::env::set_var("IYZICO_SECRET_KEY", "secret_x");
        std::env::set_var("IYZICO_BASE_URL", "https://sandbox-api.iyzipay.com");
        let cfg = IyzicoConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://sandbox-api.iyzipay.com");
        clear_env();
    }
}
