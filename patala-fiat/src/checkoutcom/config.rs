//! Configuration for [`crate::checkoutcom::CheckoutComRail`].
//!
//! Mirrors cackle's `NewCheckoutCom` (`internal/payments/checkoutcom.go`):
//! secret key, webhook secret, and API base URL are all required, no
//! default for any — Checkout.com's own base URL must be explicitly either
//! `https://api.checkout.com` (live) or `https://api.sandbox.checkout.com`
//! (sandbox), so a misconfiguration can never silently point at the wrong
//! environment (cackle's own reasoning, ported verbatim).

use patala_core::Error;

/// Everything [`crate::checkoutcom::CheckoutComRail`] needs to talk to
/// Checkout.com's API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct CheckoutComConfig {
    /// Checkout.com secret key (`Authorization: Bearer ...`). Mirrors
    /// cackle's `secretKey` / `CACKLE_CHECKOUTCOM_SECRET_KEY`.
    pub secret_key: String,
    /// Checkout.com webhook signing secret. Mirrors cackle's
    /// `webhookSecret` / `CACKLE_CHECKOUTCOM_WEBHOOK_SECRET`.
    pub webhook_secret: String,
    /// Checkout.com's API base URL — required, no default, see module docs.
    /// Mirrors cackle's `CACKLE_CHECKOUTCOM_API_BASE_URL`.
    pub api_base_url: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`, same
    /// reasoning as `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: currencies this deployment accepts. Cackle's
    /// Checkout.com `Capabilities.Currencies` is `nil` (broad/unrestricted).
    /// An empty `Vec` here means the same thing, mirroring
    /// `stripe/config.rs`'s identical pattern.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `checkoutComHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl CheckoutComConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewCheckoutCom` requiring all three of secret key / webhook secret /
    /// API base URL.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `CHECKOUTCOM_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `CHECKOUTCOM_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `CHECKOUTCOM_API_BASE_URL` | yes | see [`Self::api_base_url`] |
    /// | `CHECKOUTCOM_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `CHECKOUTCOM_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `CHECKOUTCOM_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `CHECKOUTCOM_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("CHECKOUTCOM_SECRET_KEY")?;
        let webhook_secret = non_empty_env("CHECKOUTCOM_WEBHOOK_SECRET")?;
        let api_base_url = non_empty_env("CHECKOUTCOM_API_BASE_URL")?;
        let requires_kyc = std::env::var("CHECKOUTCOM_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("CHECKOUTCOM_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("CHECKOUTCOM_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("CHECKOUTCOM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
            webhook_secret,
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
            "CHECKOUTCOM_SECRET_KEY",
            "CHECKOUTCOM_WEBHOOK_SECRET",
            "CHECKOUTCOM_API_BASE_URL",
            "CHECKOUTCOM_REQUIRES_KYC",
            "CHECKOUTCOM_CURRENCIES",
            "CHECKOUTCOM_SETTLEMENT_DAYS",
            "CHECKOUTCOM_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_all_three_vars() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(CheckoutComConfig::from_env().is_err());

        std::env::set_var("CHECKOUTCOM_SECRET_KEY", "sk_test_x");
        assert!(
            CheckoutComConfig::from_env().is_err(),
            "webhook secret missing"
        );

        std::env::set_var("CHECKOUTCOM_WEBHOOK_SECRET", "wh_x");
        assert!(CheckoutComConfig::from_env().is_err(), "base url missing");

        std::env::set_var(
            "CHECKOUTCOM_API_BASE_URL",
            "https://api.sandbox.checkout.com",
        );
        let cfg = CheckoutComConfig::from_env().expect("all three vars set");
        assert_eq!(cfg.secret_key, "sk_test_x");
        assert_eq!(cfg.webhook_secret, "wh_x");
        assert_eq!(cfg.api_base_url, "https://api.sandbox.checkout.com");
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
        std::env::set_var("CHECKOUTCOM_SECRET_KEY", "sk_test_x");
        std::env::set_var("CHECKOUTCOM_WEBHOOK_SECRET", "wh_x");
        std::env::set_var(
            "CHECKOUTCOM_API_BASE_URL",
            "https://api.sandbox.checkout.com",
        );
        std::env::set_var("CHECKOUTCOM_REQUIRES_KYC", "false");
        std::env::set_var("CHECKOUTCOM_CURRENCIES", "usd, eur");
        std::env::set_var("CHECKOUTCOM_SETTLEMENT_DAYS", "3");

        let cfg = CheckoutComConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.currencies, vec!["USD".to_string(), "EUR".to_string()]);
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
