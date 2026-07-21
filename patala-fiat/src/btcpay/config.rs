//! Configuration for [`crate::btcpay::BTCPayRail`].
//!
//! Mirrors cackle's `NewBTCPay` (`internal/payments/btcpay.go`): BTCPay is
//! self-hosted, so there is no "the" BTCPay instance and no default base
//! URL — `base_url`, `api_key`, `store_id` and `webhook_secret` are all
//! required, exactly as cackle's four required env vars are.

use patala_core::Error;

/// Everything [`crate::btcpay::BTCPayRail`] needs to talk to a self-hosted
/// BTCPay Server instance and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct BTCPayConfig {
    /// The organiser's own BTCPay Server base URL (e.g.
    /// `https://btcpay.example.com`), trailing slash trimmed. Mirrors
    /// cackle's `baseURL` / `CACKLE_BTCPAY_BASE_URL`.
    pub base_url: String,
    /// BTCPay Greenfield API key. Mirrors cackle's `apiKey` /
    /// `CACKLE_BTCPAY_API_KEY`. Never logged, never `Debug`-printed in full.
    pub api_key: String,
    /// The BTCPay store to invoice against. Mirrors cackle's `storeID` /
    /// `CACKLE_BTCPAY_STORE_ID`.
    pub store_id: String,
    /// Per-webhook shared secret configured in BTCPay's own dashboard.
    /// Mirrors cackle's `webhookSecret` / `CACKLE_BTCPAY_WEBHOOK_SECRET`.
    pub webhook_secret: String,
    /// **Gap vs cackle** (see `PORTING.md`): cackle's `Capabilities` struct
    /// has no KYC field at all. Unlike the fiat pilots (Stripe/Paystack,
    /// which default `true`), BTCPay is a self-hosted, non-custodial rail —
    /// there is no processor account requiring KYC of the payer at all, so
    /// this defaults `false`, mirroring `patala_core`'s own
    /// `NonCustodialFinal` convention (see `patala-core::capabilities`
    /// tests: a crypto rail's sample capabilities set `requires_kyc: false`).
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Cackle's BTCPay `Capabilities.Currencies`
    /// is `nil` ("broad: whatever fiat currencies the store's configured
    /// rate sources support") — an empty `Vec` here is the identical
    /// "unrestricted" thing, matching `stripe::config`'s default.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: how long BTCPay takes to reach final settlement.
    /// Cackle's own file doc comment explains this adapter trusts BTCPay's
    /// per-store "SpeedPolicy" (0-conf/1-conf/N-conf, configured in BTCPay
    /// itself) rather than re-deriving a confirmation count — so there is
    /// no single honest number to hardcode. `None` (the default) reports
    /// [`patala_core::Settlement::Instant`] (Lightning genuinely settles
    /// this way, and this adapter's `verify()` never reports paid until
    /// BTCPay's own policy already confirmed it, matching the "final at
    /// broadcast/acceptance" framing loosely enough for on-site/instant use
    /// cases); `Some(seconds)` reports
    /// [`patala_core::Settlement::Seconds`] for an operator who knows their
    /// store requires N on-chain confirmations and wants a truthful ETA
    /// surfaced to callers instead.
    pub settlement_seconds: Option<u32>,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `cryptoDefaultHTTPTimeout` (20s — wider than Paystack's 15s "since
    /// crypto processors, in particular self-hosted BTCPay, warrant a wider
    /// margin").
    pub timeout_secs: u64,
}

impl BTCPayConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `BTCPAY_BASE_URL` | yes | see [`Self::base_url`] |
    /// | `BTCPAY_API_KEY` | yes | see [`Self::api_key`] |
    /// | `BTCPAY_STORE_ID` | yes | see [`Self::store_id`] |
    /// | `BTCPAY_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `BTCPAY_REQUIRES_KYC` | no (default `false`) | `"true"`/`"false"` |
    /// | `BTCPAY_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `BTCPAY_SETTLEMENT_SECONDS` | no (default unset -> Instant) | integer |
    /// | `BTCPAY_TIMEOUT_SECS` | no (default `20`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let base_url = non_empty_env("BTCPAY_BASE_URL")?
            .trim_end_matches('/')
            .to_string();
        let api_key = non_empty_env("BTCPAY_API_KEY")?;
        let store_id = non_empty_env("BTCPAY_STORE_ID")?;
        let webhook_secret = non_empty_env("BTCPAY_WEBHOOK_SECRET")?;
        let requires_kyc = std::env::var("BTCPAY_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let currencies = std::env::var("BTCPAY_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_seconds = std::env::var("BTCPAY_SETTLEMENT_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok());
        let timeout_secs = std::env::var("BTCPAY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        Ok(Self {
            base_url,
            api_key,
            store_id,
            webhook_secret,
            requires_kyc,
            currencies,
            settlement_seconds,
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
            "BTCPAY_BASE_URL",
            "BTCPAY_API_KEY",
            "BTCPAY_STORE_ID",
            "BTCPAY_WEBHOOK_SECRET",
            "BTCPAY_REQUIRES_KYC",
            "BTCPAY_CURRENCIES",
            "BTCPAY_SETTLEMENT_SECONDS",
            "BTCPAY_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_all_four_settings() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(BTCPayConfig::from_env().is_err());

        std::env::set_var("BTCPAY_BASE_URL", "https://btcpay.example.com/");
        assert!(BTCPayConfig::from_env().is_err());
        std::env::set_var("BTCPAY_API_KEY", "key");
        assert!(BTCPayConfig::from_env().is_err());
        std::env::set_var("BTCPAY_STORE_ID", "store1");
        assert!(BTCPayConfig::from_env().is_err());
        std::env::set_var("BTCPAY_WEBHOOK_SECRET", "secret");

        let cfg = BTCPayConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://btcpay.example.com");
        assert_eq!(cfg.api_key, "key");
        assert_eq!(cfg.store_id, "store1");
        assert_eq!(cfg.webhook_secret, "secret");
        assert!(!cfg.requires_kyc);
        assert!(cfg.currencies.is_empty());
        assert_eq!(cfg.settlement_seconds, None);
        assert_eq!(cfg.timeout_secs, 20);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("BTCPAY_BASE_URL", "https://btcpay.example.com");
        std::env::set_var("BTCPAY_API_KEY", "key");
        std::env::set_var("BTCPAY_STORE_ID", "store1");
        std::env::set_var("BTCPAY_WEBHOOK_SECRET", "secret");
        std::env::set_var("BTCPAY_SETTLEMENT_SECONDS", "3600");
        std::env::set_var("BTCPAY_TIMEOUT_SECS", "30");

        let cfg = BTCPayConfig::from_env().unwrap();
        assert_eq!(cfg.settlement_seconds, Some(3600));
        assert_eq!(cfg.timeout_secs, 30);
        clear_env();
    }
}
