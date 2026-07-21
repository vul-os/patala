//! Configuration for [`crate::stripe::StripeRail`].
//!
//! Mirrors cackle's `NewStripe` (`internal/payments/stripe.go`): both the
//! secret key and the webhook signing secret are REQUIRED, refusing to build
//! a half-configured adapter — cackle's own comment: *"a provider that can
//! Begin a charge but never verify a webhook is a footgun."*

use patala_core::Error;

/// Everything [`crate::stripe::StripeRail`] needs to talk to Stripe's API
/// and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct StripeConfig {
    /// Stripe secret key (`sk_...`). Mirrors cackle's `secretKey` /
    /// `CACKLE_STRIPE_SECRET_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub secret_key: String,
    /// Stripe webhook signing secret (`whsec_...`). Mirrors cackle's
    /// `webhookSecret` / `CACKLE_STRIPE_WEBHOOK_SECRET`. Required for the
    /// same reason cackle's `NewStripe` requires it.
    pub webhook_secret: String,
    /// **Gap vs cackle** (see `PORTING.md` and `stripe/rail.rs`'s module
    /// docs): whether this rail reports `requires_kyc: true`.
    /// `patala_core::RailCapabilities::requires_kyc` has no field in
    /// cackle's `Capabilities` struct at all to port a value from — this
    /// mirrors `patala-hyperswitch::HyperswitchConfig::requires_kyc`'s same
    /// gap and its same reasoning (default `true`, the honest assumption for
    /// a custodial card rail).
    pub requires_kyc: bool,
    /// **Gap vs cackle**: currencies this deployment accepts. Cackle's
    /// Stripe `Capabilities.Currencies` is `nil` (unrestricted — Stripe
    /// supports 135+ presentment currencies that cackle's own comment
    /// declines to freeze into a maintained list). An empty `Vec` here
    /// means the identical "unrestricted" thing; a non-empty `Vec` lets an
    /// operator restrict to a known subset if they want to, which cackle's
    /// adapter does not offer but does not contradict either.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement, surfaced as
    /// `Settlement::Days(n)`. Cackle's `Capabilities` has no settlement-time
    /// field. Default `2`, matching card-network T+2
    /// (`patala-hyperswitch::HyperswitchConfig::settlement_days`'s same
    /// default, `PATALA.md` §3's own T+2 example).
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `stripeHTTPTimeout` (15s), applied even if the caller's own context
    /// has no deadline.
    pub timeout_secs: u64,
}

impl StripeConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `EnvStripeSecretKey`/`EnvStripeWebhookSecret` requirement: both are
    /// required, and this returns an error if either is missing or empty.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `STRIPE_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `STRIPE_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `STRIPE_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `STRIPE_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `STRIPE_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `STRIPE_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("STRIPE_SECRET_KEY")?;
        let webhook_secret = non_empty_env("STRIPE_WEBHOOK_SECRET")?;
        let requires_kyc = std::env::var("STRIPE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("STRIPE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("STRIPE_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("STRIPE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
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

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for var in [
            "STRIPE_SECRET_KEY",
            "STRIPE_WEBHOOK_SECRET",
            "STRIPE_REQUIRES_KYC",
            "STRIPE_CURRENCIES",
            "STRIPE_SETTLEMENT_DAYS",
            "STRIPE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_secret_key_and_webhook_secret() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(StripeConfig::from_env().is_err());

        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        assert!(
            StripeConfig::from_env().is_err(),
            "webhook secret still missing"
        );

        std::env::set_var("STRIPE_WEBHOOK_SECRET", "whsec_x");
        let cfg = StripeConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.secret_key, "sk_test_x");
        assert_eq!(cfg.webhook_secret, "whsec_x");
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
        std::env::set_var("STRIPE_SECRET_KEY", "sk_test_x");
        std::env::set_var("STRIPE_WEBHOOK_SECRET", "whsec_x");
        std::env::set_var("STRIPE_REQUIRES_KYC", "false");
        std::env::set_var("STRIPE_CURRENCIES", "usd, eur ,zar");
        std::env::set_var("STRIPE_SETTLEMENT_DAYS", "3");

        let cfg = StripeConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            vec!["USD".to_string(), "EUR".to_string(), "ZAR".to_string()]
        );
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
