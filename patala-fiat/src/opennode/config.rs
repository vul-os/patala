//! Configuration for [`crate::opennode::OpenNodeRail`].
//!
//! Mirrors cackle's `NewOpenNode` (`internal/payments/opennode.go`): only
//! the API key is required. Unlike `btcpay`/`lnbits` (genuinely self-hosted,
//! no "the" instance), OpenNode's API base is a public hostname, not a
//! secret, so it has a sensible default — mirrors cackle's own
//! `opennodeDefaultBaseURL` reasoning.
//!
//! **`RailClass`/`holds_funds`: see `rail.rs`'s module docs.** OpenNode is a
//! hosted, custodial checkout service — cackle's own file doc comment says
//! so explicitly ("OpenNode itself briefly touches the funds before paying
//! the organiser out"), unlike `btcpay`/`lnbits`.

use patala_core::Error;

/// Mirrors cackle's `opennodeDefaultBaseURL`.
pub const DEFAULT_BASE_URL: &str = "https://api.opennode.com";

/// Everything [`crate::opennode::OpenNodeRail`] needs to talk to OpenNode's
/// hosted checkout API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct OpenNodeConfig {
    /// OpenNode API key. Mirrors cackle's `apiKey` /
    /// `CACKLE_OPENNODE_API_KEY`. Never logged, never `Debug`-printed in
    /// full. Also used (raw, not hex-decoded) as the HMAC key for webhook
    /// signature verification — see `webhook.rs`.
    pub api_key: String,
    /// OpenNode API base URL, trailing slash trimmed. Mirrors cackle's
    /// `baseURL` / `CACKLE_OPENNODE_BASE_URL`, defaulting to
    /// [`DEFAULT_BASE_URL`].
    pub base_url: String,
    /// **Gap vs cackle** (see `PORTING.md`): defaults `false`. OpenNode is a
    /// hosted custodial service (the OPERATOR's merchant account may itself
    /// be subject to OpenNode's own compliance requirements), but the BUYER
    /// completing a Bitcoin/Lightning payment through it is never KYC'd —
    /// `requires_kyc` describes the payer, per
    /// `patala_core::RailCapabilities`'s own doc comment.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Cackle's OpenNode `Capabilities.Currencies`
    /// is `nil` (unrestricted) — an empty `Vec` here is the identical thing.
    pub currencies: Vec<String>,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `cryptoDefaultHTTPTimeout` (20s).
    pub timeout_secs: u64,
}

impl OpenNodeConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `OPENNODE_API_KEY` | yes | see [`Self::api_key`] |
    /// | `OPENNODE_BASE_URL` | no (default [`DEFAULT_BASE_URL`]) | see [`Self::base_url`] |
    /// | `OPENNODE_REQUIRES_KYC` | no (default `false`) | `"true"`/`"false"` |
    /// | `OPENNODE_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `OPENNODE_TIMEOUT_SECS` | no (default `20`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let api_key = non_empty_env("OPENNODE_API_KEY")?;
        let base_url = std::env::var("OPENNODE_BASE_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let requires_kyc = std::env::var("OPENNODE_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let currencies = std::env::var("OPENNODE_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout_secs = std::env::var("OPENNODE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        Ok(Self {
            api_key,
            base_url,
            requires_kyc,
            currencies,
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
            "OPENNODE_API_KEY",
            "OPENNODE_BASE_URL",
            "OPENNODE_REQUIRES_KYC",
            "OPENNODE_CURRENCIES",
            "OPENNODE_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_api_key() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(OpenNodeConfig::from_env().is_err());

        std::env::set_var("OPENNODE_API_KEY", "key");
        let cfg = OpenNodeConfig::from_env().unwrap();
        assert_eq!(cfg.api_key, "key");
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        clear_env();
    }
}
