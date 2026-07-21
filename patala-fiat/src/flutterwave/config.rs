//! Configuration for [`crate::flutterwave::FlutterwaveRail`].
//!
//! Mirrors cackle's `NewFlutterwave` (`internal/payments/flutterwave.go`):
//! both the secret key AND the webhook hash are REQUIRED, refusing to build
//! a half-configured adapter — cackle's own `NewFlutterwave` doc comment
//! requires both up front for the identical reason Stripe's does ("a
//! provider that can Begin a charge but never verify a webhook is a
//! footgun").

use patala_core::Error;

/// Mirrors cackle's `flutterwaveCurrencies`: the currencies cackle's own
/// adapter hardcodes support for (`internal/payments/flutterwave.go`).
/// Unlike Stripe's broad/unrestricted list, Flutterwave's cackle adapter
/// (like Paystack's) hardcodes a real, specific list — this port's default
/// mirrors it exactly, verbatim, same order.
pub const DEFAULT_CURRENCIES: &[&str] = &[
    "NGN", "GHS", "KES", "UGX", "TZS", "ZAR", "USD", "XOF", "XAF", "RWF",
];

/// Everything [`crate::flutterwave::FlutterwaveRail`] needs to talk to
/// Flutterwave's API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct FlutterwaveConfig {
    /// Flutterwave secret key (Bearer token for the API). Mirrors cackle's
    /// `secretKey` / `CACKLE_FLUTTERWAVE_SECRET_KEY`. Never logged, never
    /// `Debug`-printed in full.
    pub secret_key: String,
    /// The static "hash" value configured in the Flutterwave dashboard's
    /// webhook settings, echoed back verbatim in the `verif-hash` header on
    /// every webhook delivery. Mirrors cackle's `webhookHash` /
    /// `CACKLE_FLUTTERWAVE_WEBHOOK_HASH`. This is NOT a keyed MAC secret —
    /// see `webhook.rs`'s module docs.
    pub webhook_hash: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): cackle's `Capabilities` has
    /// no KYC field at all. Default `true`, same reasoning as every other
    /// rail in this crate.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Defaults to [`DEFAULT_CURRENCIES`]
    /// (cackle's real, hardcoded list).
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`, same
    /// reasoning as every other rail in this crate.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `flutterwaveHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl FlutterwaveConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `FLUTTERWAVE_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `FLUTTERWAVE_WEBHOOK_HASH` | yes | see [`Self::webhook_hash`] |
    /// | `FLUTTERWAVE_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `FLUTTERWAVE_CURRENCIES` | no (default [`DEFAULT_CURRENCIES`]) | comma-separated |
    /// | `FLUTTERWAVE_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `FLUTTERWAVE_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("FLUTTERWAVE_SECRET_KEY")?;
        let webhook_hash = non_empty_env("FLUTTERWAVE_WEBHOOK_HASH")?;
        let requires_kyc = std::env::var("FLUTTERWAVE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("FLUTTERWAVE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| DEFAULT_CURRENCIES.iter().map(|s| s.to_string()).collect());
        let settlement_days = std::env::var("FLUTTERWAVE_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("FLUTTERWAVE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
            webhook_hash,
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
            "FLUTTERWAVE_SECRET_KEY",
            "FLUTTERWAVE_WEBHOOK_HASH",
            "FLUTTERWAVE_REQUIRES_KYC",
            "FLUTTERWAVE_CURRENCIES",
            "FLUTTERWAVE_SETTLEMENT_DAYS",
            "FLUTTERWAVE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_secret_key_and_webhook_hash() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(FlutterwaveConfig::from_env().is_err());

        std::env::set_var("FLUTTERWAVE_SECRET_KEY", "sk_test_x");
        assert!(
            FlutterwaveConfig::from_env().is_err(),
            "webhook hash still missing"
        );

        std::env::set_var("FLUTTERWAVE_WEBHOOK_HASH", "hash_x");
        let cfg = FlutterwaveConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.secret_key, "sk_test_x");
        assert_eq!(cfg.webhook_hash, "hash_x");
        assert!(cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            DEFAULT_CURRENCIES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("FLUTTERWAVE_SECRET_KEY", "sk_test_x");
        std::env::set_var("FLUTTERWAVE_WEBHOOK_HASH", "hash_x");
        std::env::set_var("FLUTTERWAVE_CURRENCIES", "ngn, zar");
        std::env::set_var("FLUTTERWAVE_SETTLEMENT_DAYS", "3");

        let cfg = FlutterwaveConfig::from_env().unwrap();
        assert_eq!(cfg.currencies, vec!["NGN".to_string(), "ZAR".to_string()]);
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
