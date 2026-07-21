//! Configuration for [`crate::paystack::PaystackRail`].
//!
//! Mirrors cackle's `NewPaystack` (`internal/payments/paystack.go`): the
//! secret key is required, no default, never logged.

use patala_core::Error;

/// Mirrors cackle's `paystackCurrencies`: the currencies Paystack publicly
/// documents support for as of cackle's authoring
/// (<https://paystack.com/docs/payments/accept-payments/>). Unlike Stripe
/// (broad/unrestricted, see `stripe/config.rs`), Paystack's cackle adapter
/// hardcodes a real, specific list — so this port's *default* mirrors that
/// list exactly, verbatim.
pub const DEFAULT_CURRENCIES: &[&str] = &["NGN", "GHS", "ZAR", "KES", "USD"];

/// Everything [`crate::paystack::PaystackRail`] needs to talk to Paystack's
/// API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct PaystackConfig {
    /// Paystack secret key. Mirrors cackle's `secretKey` /
    /// `CACKLE_PAYSTACK_SECRET_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub secret_key: String,
    /// **Gap vs cackle** (see `PORTING.md`): cackle's `Capabilities` struct
    /// has no KYC field at all. Default `true`, same reasoning as
    /// `stripe/config.rs`'s identical gap and `patala-hyperswitch`'s
    /// precedent.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Defaults to
    /// [`DEFAULT_CURRENCIES`] (cackle's real, hardcoded list) rather than
    /// unrestricted — this is the one pilot where cackle itself narrows the
    /// list, so the port narrows it too.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Cackle's
    /// `Capabilities` has no settlement-time field. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `defaultHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl PaystackConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `PAYSTACK_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `PAYSTACK_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `PAYSTACK_CURRENCIES` | no (default [`DEFAULT_CURRENCIES`]) | comma-separated |
    /// | `PAYSTACK_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `PAYSTACK_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("PAYSTACK_SECRET_KEY")?;
        let requires_kyc = std::env::var("PAYSTACK_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("PAYSTACK_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| DEFAULT_CURRENCIES.iter().map(|s| s.to_string()).collect());
        let settlement_days = std::env::var("PAYSTACK_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("PAYSTACK_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
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
            "PAYSTACK_SECRET_KEY",
            "PAYSTACK_REQUIRES_KYC",
            "PAYSTACK_CURRENCIES",
            "PAYSTACK_SETTLEMENT_DAYS",
            "PAYSTACK_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_secret_key() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(PaystackConfig::from_env().is_err());

        std::env::set_var("PAYSTACK_SECRET_KEY", "sk_test_x");
        let cfg = PaystackConfig::from_env().unwrap();
        assert_eq!(cfg.secret_key, "sk_test_x");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.currencies, vec!["NGN", "GHS", "ZAR", "KES", "USD"]);
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("PAYSTACK_SECRET_KEY", "sk_test_x");
        std::env::set_var("PAYSTACK_CURRENCIES", "zar");
        std::env::set_var("PAYSTACK_SETTLEMENT_DAYS", "1");
        let cfg = PaystackConfig::from_env().unwrap();
        assert_eq!(cfg.currencies, vec!["ZAR".to_string()]);
        assert_eq!(cfg.settlement_days, 1);
        clear_env();
    }
}
