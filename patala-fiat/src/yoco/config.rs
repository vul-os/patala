//! Configuration for [`crate::yoco::YocoRail`].
//!
//! Mirrors cackle's `NewYoco` (`internal/payments/yoco.go`): both the
//! secret key and the webhook secret (in Yoco/Svix's `whsec_<base64>`
//! format) are required.

use patala_core::Error;

/// Everything [`crate::yoco::YocoRail`] needs to talk to Yoco's API and
/// describe itself honestly to `patala-core`.
///
/// **No `currencies` field** — Yoco is hardcoded ZAR-only both here and in
/// cackle (`Capabilities{Currencies: []string{"ZAR"}}`), same reasoning as
/// `midtrans::config`'s identical omission.
#[derive(Clone)]
pub struct YocoConfig {
    /// Yoco secret key. Mirrors cackle's `secretKey` /
    /// `CACKLE_YOCO_SECRET_KEY`. Never logged, never `Debug`-printed in
    /// full.
    pub secret_key: String,
    /// Yoco/Svix webhook secret in `whsec_<base64>` format. Mirrors
    /// cackle's `webhookSecret` (already decoded there at construction) /
    /// `CACKLE_YOCO_WEBHOOK_SECRET`. Decoded (and validated) by
    /// [`crate::yoco::YocoRail::new`], not here — this field carries the
    /// raw env-var form.
    pub webhook_secret: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`.
    pub requires_kyc: bool,
    /// **Gap vs cackle**: days until final settlement. Default `2`.
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's `yocoHTTPTimeout`
    /// (15s).
    pub timeout_secs: u64,
}

impl YocoConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `YOCO_SECRET_KEY` | yes | see [`Self::secret_key`] |
    /// | `YOCO_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `YOCO_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `YOCO_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `YOCO_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = non_empty_env("YOCO_SECRET_KEY")?;
        let webhook_secret = non_empty_env("YOCO_WEBHOOK_SECRET")?;
        let requires_kyc = std::env::var("YOCO_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let settlement_days = std::env::var("YOCO_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("YOCO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            secret_key,
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
            "YOCO_SECRET_KEY",
            "YOCO_WEBHOOK_SECRET",
            "YOCO_REQUIRES_KYC",
            "YOCO_SETTLEMENT_DAYS",
            "YOCO_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_secret_and_webhook_secret() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(YocoConfig::from_env().is_err());

        std::env::set_var("YOCO_SECRET_KEY", "sk_test_x");
        assert!(
            YocoConfig::from_env().is_err(),
            "webhook secret still missing"
        );

        std::env::set_var("YOCO_WEBHOOK_SECRET", "whsec_MDEyMw==");
        let cfg = YocoConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.secret_key, "sk_test_x");
        assert!(cfg.requires_kyc);
        assert_eq!(cfg.settlement_days, 2);
        clear_env();
    }
}
