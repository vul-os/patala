//! Configuration for [`crate::HyperswitchRail`].
//!
//! **Never hardcode a base URL or an API key.** Both come from configuration
//! the caller supplies at construction time, most commonly by reading
//! environment variables via [`HyperswitchConfig::from_env`]. This crate talks
//! to a **self-hosted** Hyperswitch instance the operator controls; there is
//! no default/public endpoint baked in anywhere in this crate.

use patala_core::Error;

/// Everything [`crate::HyperswitchRail`] needs to reach a self-hosted
/// Hyperswitch instance and describe it honestly to `patala-core`.
#[derive(Clone, Debug)]
pub struct HyperswitchConfig {
    /// Base URL of the self-hosted Hyperswitch instance, e.g.
    /// `https://hyperswitch.internal.example.org`. No trailing slash required
    /// -- it is trimmed. Hyperswitch's own hosted sandbox
    /// (`https://sandbox.hyperswitch.io`, per its OpenAPI `servers` entry) is
    /// a valid value for testing against Hyperswitch's own sandbox, but
    /// `PATALA.md` §4 asks specifically for a **self-hosted** instance --
    /// this crate does not care which, it just calls whatever base URL it is
    /// given.
    pub base_url: String,

    /// The merchant API key issued by the Hyperswitch instance (Hyperswitch
    /// dashboard / merchant account). Sent as the `api-key` request header on
    /// every call -- this is the exact header name Hyperswitch's own OpenAPI
    /// spec declares for its `api_key` security scheme (see this crate's
    /// README "Sources" section). Never logged, never `Debug`-printed in
    /// full.
    pub api_key: String,

    /// Optional: manually pin which Hyperswitch-configured connector
    /// (processor) a charge should route through, e.g. `"paystack"`,
    /// `"stripe"`, `"xendit"`. Hyperswitch's `PaymentsCreateRequest.connector`
    /// field accepts an array of connector names for exactly this manual
    /// selection; when set, this crate sends `connector: [<value>]`. When
    /// `None`, Hyperswitch applies its own configured routing rules for the
    /// merchant account instead.
    ///
    /// This is what makes "charge via Paystack" (or any other Hyperswitch
    /// connector) a **configuration value**, never a hardcoded or
    /// per-processor code path in this crate -- there is exactly one
    /// `PaymentRail` implementation here, and the processor is a config
    /// field on it.
    pub connector: Option<String>,

    /// The `payment_response_hash_key` configured on the Hyperswitch merchant
    /// account / business profile, used to verify the `X-Webhook-Signature-512`
    /// header on incoming outgoing-webhooks from Hyperswitch. See
    /// [`crate::webhook::verify_webhook_signature`]. Optional because a
    /// deployment may choose not to consume Hyperswitch webhooks and instead
    /// poll `HyperswitchRail::verify`.
    pub webhook_secret: Option<String>,

    /// Whether this rail reports `requires_kyc: true` in its capabilities.
    /// Hyperswitch fronts many processors; some jurisdictions/connectors
    /// require payer KYC and some (sandbox/test connectors, some
    /// low-value flows) do not. `PATALA.md` §4 asks for this to be "true or
    /// configurable" -- default is `true` (the honest assumption for a
    /// custodial card/bank rail) and an operator who knows their specific
    /// connector mix does not requires it can set this `false`.
    pub requires_kyc: bool,

    /// Currencies this deployment accepts, e.g. `["USD", "NGN", "EUR"]`
    /// (ISO-4217 codes, matching what is passed as `PayRequest::currency`).
    /// Broad and operator-configured rather than hardcoded, because which
    /// currencies actually work depends on which connectors are configured
    /// on the Hyperswitch merchant account behind `base_url` -- this crate
    /// has no way to discover that from here.
    pub currencies: Vec<String>,

    /// Days until final settlement, surfaced as `Settlement::Days(n)`.
    /// Default `2`, matching card-network T+2 (`PATALA.md` §3 example).
    pub settlement_days: u8,

    /// HTTP request timeout in seconds. Default `30`.
    pub timeout_secs: u64,
}

impl HyperswitchConfig {
    /// Read configuration from environment variables. Never falls back to a
    /// hardcoded base URL or key -- `base_url` and `api_key` are required and
    /// this returns an error if either is missing or empty.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `HYPERSWITCH_BASE_URL` | yes | see [`Self::base_url`] |
    /// | `HYPERSWITCH_API_KEY` | yes | see [`Self::api_key`] |
    /// | `HYPERSWITCH_CONNECTOR` | no | see [`Self::connector`] |
    /// | `HYPERSWITCH_WEBHOOK_SECRET` | no | see [`Self::webhook_secret`] |
    /// | `HYPERSWITCH_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `HYPERSWITCH_CURRENCIES` | no (default `USD`) | comma-separated, e.g. `USD,NGN,EUR` |
    /// | `HYPERSWITCH_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `HYPERSWITCH_TIMEOUT_SECS` | no (default `30`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let base_url = non_empty_env("HYPERSWITCH_BASE_URL")?;
        let api_key = non_empty_env("HYPERSWITCH_API_KEY")?;
        let connector = std::env::var("HYPERSWITCH_CONNECTOR")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let webhook_secret = std::env::var("HYPERSWITCH_WEBHOOK_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let requires_kyc = std::env::var("HYPERSWITCH_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("HYPERSWITCH_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["USD".to_string()]);
        let settlement_days = std::env::var("HYPERSWITCH_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("HYPERSWITCH_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            connector,
            webhook_secret,
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

    // `std::env::set_var` mutates whole-process state, and `cargo test` runs
    // tests on multiple threads by default -- serialize the two tests below
    // so they can't interleave and read each other's half-set environment.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for var in [
            "HYPERSWITCH_BASE_URL",
            "HYPERSWITCH_API_KEY",
            "HYPERSWITCH_CONNECTOR",
            "HYPERSWITCH_WEBHOOK_SECRET",
            "HYPERSWITCH_REQUIRES_KYC",
            "HYPERSWITCH_CURRENCIES",
            "HYPERSWITCH_SETTLEMENT_DAYS",
            "HYPERSWITCH_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_base_url_and_api_key() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(HyperswitchConfig::from_env().is_err());

        std::env::set_var("HYPERSWITCH_BASE_URL", "https://hs.example.org");
        assert!(
            HyperswitchConfig::from_env().is_err(),
            "api key still missing"
        );

        std::env::set_var("HYPERSWITCH_API_KEY", "snd_test_abc");
        let cfg = HyperswitchConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.base_url, "https://hs.example.org");
        assert_eq!(cfg.api_key, "snd_test_abc");
        assert_eq!(cfg.currencies, vec!["USD".to_string()]);
        assert_eq!(cfg.settlement_days, 2);
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.connector, None);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields_and_trims_base_url_slash() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("HYPERSWITCH_BASE_URL", "https://hs.example.org/");
        std::env::set_var("HYPERSWITCH_API_KEY", "snd_test_abc");
        std::env::set_var("HYPERSWITCH_CONNECTOR", "paystack");
        std::env::set_var("HYPERSWITCH_WEBHOOK_SECRET", "whsec");
        std::env::set_var("HYPERSWITCH_REQUIRES_KYC", "false");
        std::env::set_var("HYPERSWITCH_CURRENCIES", "usd, ngn ,eur");
        std::env::set_var("HYPERSWITCH_SETTLEMENT_DAYS", "3");

        let cfg = HyperswitchConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://hs.example.org");
        assert_eq!(cfg.connector, Some("paystack".to_string()));
        assert_eq!(cfg.webhook_secret, Some("whsec".to_string()));
        assert!(!cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            vec!["USD".to_string(), "NGN".to_string(), "EUR".to_string()]
        );
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
