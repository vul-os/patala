//! Configuration for [`crate::payu::PayURail`].
//!
//! Mirrors cackle's `NewPayU` (`internal/payments/payu.go`): both
//! `merchant_key` and `salt` are required together, no default, never
//! logged.

use patala_core::Error;

/// Everything [`crate::payu::PayURail`] needs to talk to PayU's API and
/// describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct PayUConfig {
    /// PayU merchant key. Mirrors cackle's `merchantKey` /
    /// `CACKLE_PAYU_MERCHANT_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub merchant_key: String,
    /// PayU salt, used both in the request hash and the response-hash
    /// verification. Mirrors cackle's `salt` / `CACKLE_PAYU_SALT`. Never
    /// logged.
    pub salt: String,
    /// **Gap vs cackle** (see `PORTING.md`): cackle's `Capabilities` struct
    /// has no KYC field at all. Default `true`, same reasoning as
    /// `stripe/config.rs`/`paystack/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: days until final settlement. Cackle's
    /// `Capabilities` has no settlement-time field. Default `2`, matching
    /// every other rail's card-network-T+2 default in this crate.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's `payUHTTPTimeout`
    /// (15s).
    pub timeout_secs: u64,
}

impl PayUConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `PAYU_MERCHANT_KEY` | yes | see [`Self::merchant_key`] |
    /// | `PAYU_SALT` | yes | see [`Self::salt`] |
    /// | `PAYU_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `PAYU_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `PAYU_TIMEOUT_SECS` | no (default `15`) | integer |
    ///
    /// **No `PAYU_CURRENCIES` variable exists** — unlike Paystack/Square/
    /// Xendit's config in this crate, PayU's currency list is not
    /// configurable at all. See [`crate::payu::rail::PayURail::new`]'s doc
    /// comment for why: cackle's own `Begin` hardcodes a real, functional
    /// `INR`-only check (`if !strings.EqualFold(o.Currency, "INR")`), not
    /// just an advertised capability, so there is nothing to override here.
    pub fn from_env() -> Result<Self, Error> {
        let merchant_key = non_empty_env("PAYU_MERCHANT_KEY")?;
        let salt = non_empty_env("PAYU_SALT")?;
        let requires_kyc = std::env::var("PAYU_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let settlement_days = std::env::var("PAYU_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("PAYU_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            merchant_key,
            salt,
            requires_kyc,
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
            "PAYU_MERCHANT_KEY",
            "PAYU_SALT",
            "PAYU_REQUIRES_KYC",
            "PAYU_SETTLEMENT_DAYS",
            "PAYU_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    // Ported from cackle's internal/payments/payu_test.go
    // (TestNewPayU_RequiresCredentials).
    #[test]
    fn from_env_requires_merchant_key_and_salt() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(PayUConfig::from_env().is_err());

        std::env::set_var("PAYU_MERCHANT_KEY", "gtKFFx");
        assert!(PayUConfig::from_env().is_err(), "salt still missing");

        std::env::set_var("PAYU_SALT", "eCwWELxi");
        let cfg = PayUConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.merchant_key, "gtKFFx");
        assert_eq!(cfg.salt, "eCwWELxi");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("PAYU_MERCHANT_KEY", "gtKFFx");
        std::env::set_var("PAYU_SALT", "eCwWELxi");
        std::env::set_var("PAYU_REQUIRES_KYC", "false");
        std::env::set_var("PAYU_SETTLEMENT_DAYS", "1");
        std::env::set_var("PAYU_TIMEOUT_SECS", "30");

        let cfg = PayUConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 1);
        assert_eq!(cfg.timeout_secs, 30);
        clear_env();
    }
}
