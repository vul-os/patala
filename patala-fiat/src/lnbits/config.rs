//! Configuration for [`crate::lnbits::LNbitsRail`].
//!
//! Mirrors cackle's `NewLNbits` (`internal/payments/lnbits.go`):
//! `base_url`, `api_key` (an invoice/read key, never an admin key) and
//! `webhook_secret` (cackle's own compensating shared secret — see
//! `webhook.rs`'s module docs) are all required; `webhook_url` and the quote
//! TTL are optional.

use patala_core::Error;

/// Mirrors cackle's `lnbitsDefaultQuoteTTLSecs`.
pub const DEFAULT_QUOTE_TTL_SECS: u64 = 900;

/// Everything [`crate::lnbits::LNbitsRail`] needs to talk to a self-hosted
/// (or self-operated) LNbits wallet and describe itself honestly to
/// `patala-core`.
#[derive(Clone)]
pub struct LNbitsConfig {
    /// The organiser's own LNbits base URL, trailing slash trimmed. Mirrors
    /// cackle's `baseURL` / `CACKLE_LNBITS_BASE_URL`.
    pub base_url: String,
    /// An LNbits invoice/read key — never an admin key. Mirrors cackle's
    /// `apiKey` / `CACKLE_LNBITS_API_KEY`. Never logged, never
    /// `Debug`-printed in full.
    pub api_key: String,
    /// Cackle's own compensating shared secret (LNbits' native webhook
    /// delivery has no built-in signature at all — see `webhook.rs`'s
    /// module docs). Mirrors cackle's `webhookSecret` /
    /// `CACKLE_LNBITS_WEBHOOK_SECRET`.
    pub webhook_secret: String,
    /// Optional: registered with LNbits as the invoice's webhook target,
    /// with `?secret=<webhook_secret>` appended. Mirrors cackle's
    /// `webhookURL` / `CACKLE_LNBITS_WEBHOOK_URL`.
    pub webhook_url: Option<String>,
    /// How long a BOLT11 invoice this rail creates stays payable, in
    /// seconds. Mirrors cackle's `quoteTTL` /
    /// `CACKLE_LNBITS_QUOTE_TTL_SECONDS` (default 900 = 15 minutes) —
    /// requested as the invoice's own `expiry` AND enforced independently by
    /// this adapter (see `rail.rs`'s module docs).
    pub quote_ttl_secs: u64,
    /// **Gap vs cackle** (see `PORTING.md`): defaults `false`, same
    /// reasoning as `btcpay::config::BTCPayConfig::requires_kyc` — a
    /// self-hosted, non-custodial crypto rail has no processor account
    /// requiring KYC of the payer.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Cackle's LNbits `Capabilities.Currencies`
    /// is `nil` ("whatever fiat currencies your LNbits instance's rate
    /// source supports") — an empty `Vec` here is the identical
    /// "unrestricted" thing.
    pub currencies: Vec<String>,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `cryptoDefaultHTTPTimeout` (20s, shared by the whole crypto adapter
    /// group in cackle).
    pub timeout_secs: u64,
}

impl LNbitsConfig {
    /// Read configuration from environment variables.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `LNBITS_BASE_URL` | yes | see [`Self::base_url`] |
    /// | `LNBITS_API_KEY` | yes | see [`Self::api_key`] |
    /// | `LNBITS_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `LNBITS_WEBHOOK_URL` | no | see [`Self::webhook_url`] |
    /// | `LNBITS_QUOTE_TTL_SECONDS` | no (default 900) | positive integer |
    /// | `LNBITS_REQUIRES_KYC` | no (default `false`) | `"true"`/`"false"` |
    /// | `LNBITS_CURRENCIES` | no (default empty/unrestricted) | comma-separated |
    /// | `LNBITS_TIMEOUT_SECS` | no (default `20`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let base_url = non_empty_env("LNBITS_BASE_URL")?
            .trim_end_matches('/')
            .to_string();
        let api_key = non_empty_env("LNBITS_API_KEY")?;
        let webhook_secret = non_empty_env("LNBITS_WEBHOOK_SECRET")?;
        let webhook_url = std::env::var("LNBITS_WEBHOOK_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let quote_ttl_secs = match std::env::var("LNBITS_QUOTE_TTL_SECONDS") {
            Ok(v) if !v.trim().is_empty() => {
                let n: u64 = v.trim().parse().map_err(|_| {
                    Error::InvalidRequest(
                        "LNBITS_QUOTE_TTL_SECONDS must be a positive integer number of seconds"
                            .into(),
                    )
                })?;
                if n == 0 {
                    return Err(Error::InvalidRequest(
                        "LNBITS_QUOTE_TTL_SECONDS must be a positive integer number of seconds"
                            .into(),
                    ));
                }
                n
            }
            _ => DEFAULT_QUOTE_TTL_SECS,
        };
        let requires_kyc = std::env::var("LNBITS_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let currencies = std::env::var("LNBITS_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout_secs = std::env::var("LNBITS_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        Ok(Self {
            base_url,
            api_key,
            webhook_secret,
            webhook_url,
            quote_ttl_secs,
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
            "LNBITS_BASE_URL",
            "LNBITS_API_KEY",
            "LNBITS_WEBHOOK_SECRET",
            "LNBITS_WEBHOOK_URL",
            "LNBITS_QUOTE_TTL_SECONDS",
            "LNBITS_REQUIRES_KYC",
            "LNBITS_CURRENCIES",
            "LNBITS_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_env_vars() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(LNbitsConfig::from_env().is_err());

        std::env::set_var("LNBITS_BASE_URL", "https://lnbits.example.com");
        assert!(LNbitsConfig::from_env().is_err());
        std::env::set_var("LNBITS_API_KEY", "key");
        assert!(LNbitsConfig::from_env().is_err());
        std::env::set_var("LNBITS_WEBHOOK_SECRET", "secret");

        let cfg = LNbitsConfig::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://lnbits.example.com");
        assert_eq!(cfg.quote_ttl_secs, DEFAULT_QUOTE_TTL_SECS);
        assert!(!cfg.requires_kyc);
        clear_env();
    }

    #[test]
    fn from_env_rejects_non_positive_ttl() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LNBITS_BASE_URL", "https://lnbits.example.com");
        std::env::set_var("LNBITS_API_KEY", "key");
        std::env::set_var("LNBITS_WEBHOOK_SECRET", "secret");
        std::env::set_var("LNBITS_QUOTE_TTL_SECONDS", "0");
        assert!(LNbitsConfig::from_env().is_err());
        clear_env();
    }
}
