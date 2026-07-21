//! Configuration for [`crate::square::SquareRail`].
//!
//! Mirrors cackle's `NewSquare` (`internal/payments/square.go`): all FIVE
//! fields below are required, checked in this EXACT sequential order,
//! refusing to build a half-configured adapter.

use patala_core::Error;

/// Everything [`crate::square::SquareRail`] needs to talk to Square's API
/// and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct SquareConfig {
    /// Square access token. Mirrors cackle's `accessToken` /
    /// `CACKLE_SQUARE_ACCESS_TOKEN`. Never logged, never `Debug`-printed in
    /// full.
    pub access_token: String,
    /// Square webhook signature key. Mirrors cackle's
    /// `webhookSignatureKey` / `CACKLE_SQUARE_WEBHOOK_SIGNATURE_KEY`.
    pub webhook_signature_key: String,
    /// Square location id used on every created Order. Mirrors cackle's
    /// `locationID` / `CACKLE_SQUARE_LOCATION_ID`.
    pub location_id: String,
    /// The EXACT URL configured for this webhook subscription in Square's
    /// dashboard -- Square's signature covers this URL, so a mismatch here
    /// makes every signature check fail closed (safe, but useless) rather
    /// than silently skip verification. Mirrors cackle's `notificationURL`
    /// / `CACKLE_SQUARE_NOTIFICATION_URL`.
    pub notification_url: String,
    /// Cackle's own doc comment says this must be either
    /// `https://connect.squareup.com` (production) or
    /// `https://connect.squareupsandbox.com` (sandbox) -- but exactly like
    /// cackle's `NewSquare`, this is NOT enforced in code, only documented
    /// convention: only non-emptiness is checked here, matching cackle
    /// literally rather than inventing enforcement cackle doesn't have.
    /// Mirrors cackle's `baseURL` / `CACKLE_SQUARE_API_BASE_URL`.
    pub api_base_url: String,
    /// **Gap vs cackle** (see `PORTING.md`): cackle's `Capabilities` struct
    /// has no KYC field at all. Default `true`, same reasoning as
    /// `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Empty means unrestricted, mirroring
    /// cackle's `Capabilities.Currencies: nil` -- Square's own Payment
    /// Links API is not documented as restricted to any specific currency
    /// list. Overridable via env for an operator who wants to narrow it,
    /// same pattern as `stripe::config` (cackle's own adapter does not
    /// offer this but does not contradict it either).
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Cackle's
    /// `Capabilities` has no settlement-time field. Default `2`, same
    /// reasoning as `stripe/config.rs`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `squareHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl SquareConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewSquare`: all five required vars are checked in this EXACT
    /// sequential order (access token, then webhook key, then location,
    /// then notification URL, then base URL) -- see this module's own
    /// test, which ports cackle's `TestNewSquare_RequiresAllFiveEnvVars`.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `SQUARE_ACCESS_TOKEN` | yes | see [`Self::access_token`] |
    /// | `SQUARE_WEBHOOK_SIGNATURE_KEY` | yes | see [`Self::webhook_signature_key`] |
    /// | `SQUARE_LOCATION_ID` | yes | see [`Self::location_id`] |
    /// | `SQUARE_NOTIFICATION_URL` | yes | see [`Self::notification_url`] |
    /// | `SQUARE_API_BASE_URL` | yes | see [`Self::api_base_url`] |
    /// | `SQUARE_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `SQUARE_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `SQUARE_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `SQUARE_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let access_token = non_empty_env("SQUARE_ACCESS_TOKEN")?;
        let webhook_signature_key = non_empty_env("SQUARE_WEBHOOK_SIGNATURE_KEY")?;
        let location_id = non_empty_env("SQUARE_LOCATION_ID")?;
        let notification_url = non_empty_env("SQUARE_NOTIFICATION_URL")?;
        let api_base_url = non_empty_env("SQUARE_API_BASE_URL")?;
        let requires_kyc = std::env::var("SQUARE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("SQUARE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let settlement_days = std::env::var("SQUARE_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("SQUARE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            access_token,
            webhook_signature_key,
            location_id,
            notification_url,
            api_base_url: api_base_url.trim_end_matches('/').to_string(),
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
            "SQUARE_ACCESS_TOKEN",
            "SQUARE_WEBHOOK_SIGNATURE_KEY",
            "SQUARE_LOCATION_ID",
            "SQUARE_NOTIFICATION_URL",
            "SQUARE_API_BASE_URL",
            "SQUARE_REQUIRES_KYC",
            "SQUARE_CURRENCIES",
            "SQUARE_SETTLEMENT_DAYS",
            "SQUARE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    // Ports cackle's TestNewSquare_RequiresAllFiveEnvVars: the exact
    // sequential ordering in which each of the five required vars is
    // checked.
    #[test]
    fn from_env_requires_all_five_vars_in_order() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        assert!(SquareConfig::from_env().is_err(), "access token missing");
        std::env::set_var("SQUARE_ACCESS_TOKEN", "tok");

        assert!(SquareConfig::from_env().is_err(), "webhook key missing");
        std::env::set_var("SQUARE_WEBHOOK_SIGNATURE_KEY", "key");

        assert!(SquareConfig::from_env().is_err(), "location id missing");
        std::env::set_var("SQUARE_LOCATION_ID", "L1");

        assert!(
            SquareConfig::from_env().is_err(),
            "notification url missing"
        );
        std::env::set_var("SQUARE_NOTIFICATION_URL", "https://example.com/webhook");

        assert!(SquareConfig::from_env().is_err(), "base url missing");
        std::env::set_var("SQUARE_API_BASE_URL", "https://connect.squareupsandbox.com");

        let cfg = SquareConfig::from_env().expect("all five now set");
        assert_eq!(cfg.access_token, "tok");
        assert_eq!(cfg.webhook_signature_key, "key");
        assert_eq!(cfg.location_id, "L1");
        assert_eq!(cfg.notification_url, "https://example.com/webhook");
        assert_eq!(cfg.api_base_url, "https://connect.squareupsandbox.com");
        assert!(cfg.requires_kyc);
        assert!(cfg.currencies.is_empty());
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields_and_strips_trailing_slash() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("SQUARE_ACCESS_TOKEN", "tok");
        std::env::set_var("SQUARE_WEBHOOK_SIGNATURE_KEY", "key");
        std::env::set_var("SQUARE_LOCATION_ID", "L1");
        std::env::set_var("SQUARE_NOTIFICATION_URL", "https://example.com/webhook");
        std::env::set_var("SQUARE_API_BASE_URL", "https://connect.squareup.com/");
        std::env::set_var("SQUARE_REQUIRES_KYC", "false");
        std::env::set_var("SQUARE_CURRENCIES", "usd, eur");
        std::env::set_var("SQUARE_SETTLEMENT_DAYS", "1");

        let cfg = SquareConfig::from_env().unwrap();
        assert_eq!(cfg.api_base_url, "https://connect.squareup.com");
        assert!(!cfg.requires_kyc);
        assert_eq!(cfg.currencies, vec!["USD".to_string(), "EUR".to_string()]);
        assert_eq!(cfg.settlement_days, 1);
        clear_env();
    }
}
