//! Configuration for [`crate::payfast::PayFastRail`].
//!
//! Mirrors cackle's `NewPayFast` (`internal/payments/payfast.go`): merchant
//! id and merchant key are required; the passphrase is optional but
//! strongly recommended by PayFast.

use patala_core::Error;

/// Everything [`crate::payfast::PayFastRail`] needs to talk to PayFast's
/// API and describe itself honestly to `patala-core`.
///
/// **No `currencies` field** — PayFast is hardcoded ZAR-only both here and
/// in cackle, same reasoning as `midtrans::config`/`yoco::config`'s
/// identical omission.
#[derive(Clone)]
pub struct PayFastConfig {
    /// PayFast merchant id. Mirrors cackle's `merchantID` /
    /// `CACKLE_PAYFAST_MERCHANT_ID`.
    pub merchant_id: String,
    /// PayFast merchant key. Mirrors cackle's `merchantKey` /
    /// `CACKLE_PAYFAST_MERCHANT_KEY`. Never logged, never `Debug`-printed
    /// in full.
    pub merchant_key: String,
    /// Optional but strongly recommended by PayFast (appended to the
    /// signed field set if non-empty). Mirrors cackle's `passphrase` /
    /// `CACKLE_PAYFAST_PASSPHRASE`. Never logged, never `Debug`-printed in
    /// full.
    pub passphrase: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: days until final settlement. Default `2`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `payFastHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl PayFastConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `PAYFAST_MERCHANT_ID` | yes | see [`Self::merchant_id`] |
    /// | `PAYFAST_MERCHANT_KEY` | yes | see [`Self::merchant_key`] |
    /// | `PAYFAST_PASSPHRASE` | no (default empty) | see [`Self::passphrase`] |
    /// | `PAYFAST_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `PAYFAST_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `PAYFAST_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let merchant_id = non_empty_env("PAYFAST_MERCHANT_ID")?;
        let merchant_key = non_empty_env("PAYFAST_MERCHANT_KEY")?;
        let passphrase = std::env::var("PAYFAST_PASSPHRASE").unwrap_or_default();
        let requires_kyc = std::env::var("PAYFAST_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let settlement_days = std::env::var("PAYFAST_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("PAYFAST_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            merchant_id,
            merchant_key,
            passphrase,
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
            "PAYFAST_MERCHANT_ID",
            "PAYFAST_MERCHANT_KEY",
            "PAYFAST_PASSPHRASE",
            "PAYFAST_REQUIRES_KYC",
            "PAYFAST_SETTLEMENT_DAYS",
            "PAYFAST_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_merchant_id_and_key() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(PayFastConfig::from_env().is_err());

        std::env::set_var("PAYFAST_MERCHANT_ID", "10000100");
        assert!(
            PayFastConfig::from_env().is_err(),
            "merchant key still missing"
        );

        std::env::set_var("PAYFAST_MERCHANT_KEY", "46f0cd694581a");
        let cfg = PayFastConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.merchant_id, "10000100");
        assert_eq!(cfg.merchant_key, "46f0cd694581a");
        assert_eq!(cfg.passphrase, "");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 2);
        clear_env();
    }

    #[test]
    fn from_env_reads_passphrase() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("PAYFAST_MERCHANT_ID", "10000100");
        std::env::set_var("PAYFAST_MERCHANT_KEY", "46f0cd694581a");
        std::env::set_var("PAYFAST_PASSPHRASE", "test-passphrase");
        let cfg = PayFastConfig::from_env().unwrap();
        assert_eq!(cfg.passphrase, "test-passphrase");
        clear_env();
    }
}
