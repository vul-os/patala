//! Configuration for [`crate::midtrans::MidtransRail`].
//!
//! Mirrors cackle's `NewMidtrans` (`internal/payments/midtrans.go`): the
//! server key is required, no default, never logged.

use patala_core::Error;

/// Mirrors cackle's `midtransSnapAPIBase`.
pub const SNAP_API_BASE: &str = "https://app.midtrans.com/snap/v1";
/// Mirrors cackle's `midtransCoreAPIBase`.
pub const CORE_API_BASE: &str = "https://api.midtrans.com/v2";

/// Everything [`crate::midtrans::MidtransRail`] needs to talk to Midtrans's
/// API and describe itself honestly to `patala-core`.
///
/// **No `currencies` field** — unlike every other rail in this crate,
/// Midtrans is hardcoded to IDR-only both here and in cackle
/// (`Capabilities{Currencies: []string{"IDR"}}`, not configurable): cackle's
/// own file header says Midtrans "does not document broad multi-currency
/// support", so this port does not expose a knob that could misconfigure
/// the single-currency invariant cackle itself never allowed to be
/// disabled.
#[derive(Clone)]
pub struct MidtransConfig {
    /// Midtrans server key. Mirrors cackle's `serverKey` /
    /// `CACKLE_MIDTRANS_SERVER_KEY`. Used both as HTTP Basic auth for
    /// outbound calls AND as the webhook `signature_key` ingredient. Never
    /// logged, never `Debug`-printed in full.
    pub server_key: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: days until final settlement. Default `2`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `midtransHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl MidtransConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `MIDTRANS_SERVER_KEY` | yes | see [`Self::server_key`] |
    /// | `MIDTRANS_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `MIDTRANS_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `MIDTRANS_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let server_key = non_empty_env("MIDTRANS_SERVER_KEY")?;
        let requires_kyc = std::env::var("MIDTRANS_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let settlement_days = std::env::var("MIDTRANS_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("MIDTRANS_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            server_key,
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
            "MIDTRANS_SERVER_KEY",
            "MIDTRANS_REQUIRES_KYC",
            "MIDTRANS_SETTLEMENT_DAYS",
            "MIDTRANS_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_server_key() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(MidtransConfig::from_env().is_err());

        std::env::set_var("MIDTRANS_SERVER_KEY", "SB-Mid-server-x");
        let cfg = MidtransConfig::from_env().unwrap();
        assert_eq!(cfg.server_key, "SB-Mid-server-x");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }
}
