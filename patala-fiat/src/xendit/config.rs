//! Configuration for [`crate::xendit::XenditRail`].
//!
//! Mirrors cackle's `NewXendit` (`internal/payments/xendit.go`): both the
//! secret key and the webhook callback token are REQUIRED, refusing to
//! build a half-configured adapter — cackle's own comment: *"a provider
//! that can't verify webhooks must not be constructible at all."* Same
//! reasoning as `stripe::config`'s identical two-required-fields contract.

use patala_core::Error;

/// Mirrors cackle's `xenditCurrencies`: the currencies Xendit's Invoices API
/// documents support for as of cackle's authoring
/// (<https://developers.xendit.co/api-reference/#create-invoice>). Like
/// Paystack (see `paystack::config::DEFAULT_CURRENCIES`) and unlike Stripe,
/// cackle's Xendit adapter hardcodes a real, specific list — so this port's
/// *default* mirrors that list exactly, verbatim, while still allowing an
/// operator to override it via env (same precedent as
/// `paystack::config::PaystackConfig::from_env`'s `PAYSTACK_CURRENCIES`).
pub const DEFAULT_CURRENCIES: &[&str] = &["IDR", "PHP", "VND", "THB", "MYR"];

/// Everything [`crate::xendit::XenditRail`] needs to talk to Xendit's API
/// and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct XenditConfig {
    /// Xendit secret key. Mirrors cackle's `secretKey` /
    /// `CACKLE_XENDIT_SECRET_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub secret_key: String,
    /// Xendit's static per-account webhook callback token. Mirrors cackle's
    /// `webhookToken` / `CACKLE_XENDIT_WEBHOOK_TOKEN`. Required for the same
    /// reason cackle's `NewXendit` requires it.
    pub webhook_token: String,
    /// **Gap vs cackle** (see `PORTING.md`): cackle's `Capabilities` struct
    /// has no KYC field at all. Default `true`, same reasoning as
    /// `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Defaults to [`DEFAULT_CURRENCIES`]
    /// (cackle's real, hardcoded list). **Gap vs cackle**: cackle's
    /// `Capabilities.Countries` (`["ID","PH","VN","TH","MY"]`) has no
    /// `RailCapabilities` field to port to at all — dropped, per
    /// `PORTING.md` §4 (same as every other adapter's `Countries` field).
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Cackle's
    /// `Capabilities` has no settlement-time field. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `xenditHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl XenditConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `XENDIT_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `XENDIT_WEBHOOK_TOKEN` | yes | see [`Self::webhook_token`] |
    /// | `XENDIT_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `XENDIT_CURRENCIES` | no (default [`DEFAULT_CURRENCIES`]) | comma-separated |
    /// | `XENDIT_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `XENDIT_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("XENDIT_SECRET_KEY")?;
        let webhook_token = non_empty_env("XENDIT_WEBHOOK_TOKEN")?;
        let requires_kyc = std::env::var("XENDIT_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("XENDIT_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| DEFAULT_CURRENCIES.iter().map(|s| s.to_string()).collect());
        let settlement_days = std::env::var("XENDIT_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("XENDIT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
            webhook_token,
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
            "XENDIT_SECRET_KEY",
            "XENDIT_WEBHOOK_TOKEN",
            "XENDIT_REQUIRES_KYC",
            "XENDIT_CURRENCIES",
            "XENDIT_SETTLEMENT_DAYS",
            "XENDIT_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    // Ported from cackle's internal/payments/xendit_test.go
    // (TestNewXendit_RequiresSecretAndToken).

    #[test]
    fn from_env_requires_secret_key_and_webhook_token() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(XenditConfig::from_env().is_err());

        std::env::set_var("XENDIT_SECRET_KEY", "xnd_x");
        assert!(
            XenditConfig::from_env().is_err(),
            "webhook token still missing"
        );

        std::env::set_var("XENDIT_WEBHOOK_TOKEN", "test-callback-token");
        let cfg = XenditConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.secret_key, "xnd_x");
        assert_eq!(cfg.webhook_token, "test-callback-token");
        assert!(cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            vec![
                "IDR".to_string(),
                "PHP".to_string(),
                "VND".to_string(),
                "THB".to_string(),
                "MYR".to_string()
            ]
        );
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("XENDIT_SECRET_KEY", "xnd_x");
        std::env::set_var("XENDIT_WEBHOOK_TOKEN", "test-callback-token");
        std::env::set_var("XENDIT_CURRENCIES", "idr");
        std::env::set_var("XENDIT_SETTLEMENT_DAYS", "1");
        let cfg = XenditConfig::from_env().unwrap();
        assert_eq!(cfg.currencies, vec!["IDR".to_string()]);
        assert_eq!(cfg.settlement_days, 1);
        clear_env();
    }
}
