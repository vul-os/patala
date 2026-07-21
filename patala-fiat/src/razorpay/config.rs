//! Configuration for [`crate::razorpay::RazorpayRail`].
//!
//! Mirrors cackle's `NewRazorpay` (`internal/payments/razorpay.go`): all
//! three credentials are required up front, checked in the SAME order
//! cackle checks them — `key_id`/`key_secret` together first (cackle's own
//! `NewRazorpay` returns a single combined `ErrRazorpayCredentialsNotConfigured`
//! for either being empty, not two separate errors), THEN `webhook_secret`
//! separately (`ErrRazorpayWebhookSecretNotConfigured`) — mirroring the same
//! "a provider that can Begin a charge but never verify a webhook is a
//! footgun" reasoning `stripe/config.rs` cites for requiring both up front.

use patala_core::Error;

/// Everything [`crate::razorpay::RazorpayRail`] needs to talk to Razorpay's
/// API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct RazorpayConfig {
    /// Razorpay key id (`rzp_...`). Mirrors cackle's `keyID` /
    /// `CACKLE_RAZORPAY_KEY_ID`. Used as the HTTP Basic Auth username.
    pub key_id: String,
    /// Razorpay key secret. Mirrors cackle's `keySecret` /
    /// `CACKLE_RAZORPAY_KEY_SECRET`. Used as the HTTP Basic Auth password.
    /// Never logged, never `Debug`-printed in full.
    pub key_secret: String,
    /// Razorpay webhook signing secret. Mirrors cackle's `webhookSecret` /
    /// `CACKLE_RAZORPAY_WEBHOOK_SECRET`. Required for the same reason
    /// cackle's `NewRazorpay` requires it up front. Never logged.
    pub webhook_secret: String,
    /// **Gap vs cackle** (see `PORTING.md` and `razorpay/rail.rs`'s module
    /// docs): whether this rail reports `requires_kyc: true`.
    /// `patala_core::RailCapabilities::requires_kyc` has no field in
    /// cackle's `Capabilities` struct at all to port a value from — this
    /// mirrors `stripe/paystack::Config::requires_kyc`'s same gap and
    /// reasoning (default `true`, the honest assumption for a custodial
    /// card rail).
    pub requires_kyc: bool,
    /// **Gap vs cackle**: days until final settlement, surfaced as
    /// `Settlement::Days(n)`. Cackle's `Capabilities` has no settlement-time
    /// field. Default `2`, matching card-network T+2
    /// (`PATALA.md` §3's own T+2 example, and every other rail in this
    /// crate's identical default).
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `razorpayHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl RazorpayConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewRazorpay` requirement ordering: `RAZORPAY_KEY_ID`/
    /// `RAZORPAY_KEY_SECRET` are checked TOGETHER first (a single combined
    /// error if either is missing, exactly like cackle), THEN
    /// `RAZORPAY_WEBHOOK_SECRET` is checked separately.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `RAZORPAY_KEY_ID` | yes (with `RAZORPAY_KEY_SECRET`) | see [`Self::key_id`] |
    /// | `RAZORPAY_KEY_SECRET` | yes (with `RAZORPAY_KEY_ID`) | see [`Self::key_secret`] |
    /// | `RAZORPAY_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `RAZORPAY_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `RAZORPAY_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `RAZORPAY_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let key_id = std::env::var("RAZORPAY_KEY_ID").unwrap_or_default();
        let key_secret = std::env::var("RAZORPAY_KEY_SECRET").unwrap_or_default();
        if key_id.trim().is_empty() || key_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "razorpay: RAZORPAY_KEY_ID and RAZORPAY_KEY_SECRET must both be set".into(),
            ));
        }
        let webhook_secret = non_empty_env("RAZORPAY_WEBHOOK_SECRET")?;

        let requires_kyc = std::env::var("RAZORPAY_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let settlement_days = std::env::var("RAZORPAY_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("RAZORPAY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            key_id: key_id.trim().to_string(),
            key_secret: key_secret.trim().to_string(),
            webhook_secret,
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
            "RAZORPAY_KEY_ID",
            "RAZORPAY_KEY_SECRET",
            "RAZORPAY_WEBHOOK_SECRET",
            "RAZORPAY_REQUIRES_KYC",
            "RAZORPAY_SETTLEMENT_DAYS",
            "RAZORPAY_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    // Ported from cackle's internal/payments/razorpay_test.go
    // (TestNewRazorpay_RequiresCredentials).
    #[test]
    fn from_env_requires_credentials_then_webhook_secret() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(RazorpayConfig::from_env().is_err(), "nothing set");

        std::env::set_var("RAZORPAY_KEY_ID", "rzp_test_x");
        assert!(
            RazorpayConfig::from_env().is_err(),
            "key_secret still missing"
        );

        std::env::set_var("RAZORPAY_KEY_SECRET", "secret");
        assert!(
            RazorpayConfig::from_env().is_err(),
            "webhook_secret still missing"
        );

        std::env::set_var("RAZORPAY_WEBHOOK_SECRET", "whsec_x");
        let cfg = RazorpayConfig::from_env().expect("all three required vars set");
        assert_eq!(cfg.key_id, "rzp_test_x");
        assert_eq!(cfg.key_secret, "secret");
        assert_eq!(cfg.webhook_secret, "whsec_x");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("RAZORPAY_KEY_ID", "rzp_test_x");
        std::env::set_var("RAZORPAY_KEY_SECRET", "secret");
        std::env::set_var("RAZORPAY_WEBHOOK_SECRET", "whsec_x");
        std::env::set_var("RAZORPAY_REQUIRES_KYC", "false");
        std::env::set_var("RAZORPAY_SETTLEMENT_DAYS", "3");

        let cfg = RazorpayConfig::from_env().unwrap();
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 3);
        clear_env();
    }
}
