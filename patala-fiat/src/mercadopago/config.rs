//! Configuration for [`crate::mercadopago::MercadoPagoRail`].
//!
//! Mirrors cackle's `NewMercadoPago` (`internal/payments/mercadopago.go`):
//! access token and webhook secret are both required, no default. Fixed
//! API base (`https://api.mercadopago.com`), same pattern as
//! `stripe`/`paystack`/`mollie`.

use patala_core::Error;

/// Mercado Pago's fixed API base -- mirrors cackle's `mercadoPagoAPIBase`.
pub const MERCADOPAGO_API_BASE: &str = "https://api.mercadopago.com";

/// Mirrors cackle's `mercadoPagoCurrencies`: the currencies cackle's own
/// adapter hardcodes support for, matching Mercado Pago's real Latin
/// America footprint. Unlike Stripe/Adyen/Checkout.com (broad/
/// unrestricted), Mercado Pago's cackle adapter hardcodes a real, specific
/// list -- so this port's *default* mirrors that list exactly, same
/// precedent as `paystack::config::DEFAULT_CURRENCIES`.
pub const DEFAULT_CURRENCIES: &[&str] = &["ARS", "BRL", "CLP", "COP", "MXN", "PEN", "UYU"];

/// Everything [`crate::mercadopago::MercadoPagoRail`] needs to talk to
/// Mercado Pago's API and describe itself honestly to `patala-core`.
#[derive(Clone)]
pub struct MercadoPagoConfig {
    /// Mercado Pago access token (`APP_USR-...`). Mirrors cackle's
    /// `accessToken` / `CACKLE_MERCADOPAGO_ACCESS_TOKEN`.
    pub access_token: String,
    /// Mercado Pago webhook signing secret. Mirrors cackle's
    /// `webhookSecret` / `CACKLE_MERCADOPAGO_WEBHOOK_SECRET`.
    pub webhook_secret: String,
    /// **Gap vs cackle** (see `PORTING.md` §4): default `true`, same
    /// reasoning as `stripe/config.rs`'s identical gap.
    pub requires_kyc: bool,
    /// Currencies this rail accepts. Defaults to [`DEFAULT_CURRENCIES`]
    /// (cackle's real, hardcoded list) rather than unrestricted -- same
    /// pattern as `paystack::config::PaystackConfig::currencies`.
    pub currencies: Vec<String>,
    /// **Gap vs cackle**: days until final settlement. Default `2`, same
    /// reasoning as `stripe/config.rs`. (**Gap vs cackle also drops
    /// `Countries`** -- see `PORTING.md` §4, `registry.rs`'s
    /// `CapabilityFilter` doc for the identical point at the registry
    /// layer -- `RailCapabilities` has no country field at all.)
    pub settlement_days: u8,
    /// HTTP request timeout in seconds. Mirrors cackle's
    /// `mercadoPagoHTTPTimeout` (15s).
    pub timeout_secs: u64,
}

impl MercadoPagoConfig {
    /// Read configuration from environment variables. Mirrors cackle's
    /// `NewMercadoPago` requiring both the access token and the webhook
    /// secret.
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `MERCADOPAGO_ACCESS_TOKEN` | yes | see [`Self::access_token`] |
    /// | `MERCADOPAGO_WEBHOOK_SECRET` | yes | see [`Self::webhook_secret`] |
    /// | `MERCADOPAGO_REQUIRES_KYC` | no (default `true`) | `"true"`/`"false"` |
    /// | `MERCADOPAGO_CURRENCIES` | no (default [`DEFAULT_CURRENCIES`]) | comma-separated |
    /// | `MERCADOPAGO_SETTLEMENT_DAYS` | no (default `2`) | integer |
    /// | `MERCADOPAGO_TIMEOUT_SECS` | no (default `15`) | integer |
    pub fn from_env() -> Result<Self, Error> {
        let access_token = non_empty_env("MERCADOPAGO_ACCESS_TOKEN")?;
        let webhook_secret = non_empty_env("MERCADOPAGO_WEBHOOK_SECRET")?;
        let requires_kyc = std::env::var("MERCADOPAGO_REQUIRES_KYC")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let currencies = std::env::var("MERCADOPAGO_CURRENCIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| DEFAULT_CURRENCIES.iter().map(|s| s.to_string()).collect());
        let settlement_days = std::env::var("MERCADOPAGO_SETTLEMENT_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let timeout_secs = std::env::var("MERCADOPAGO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);

        Ok(Self {
            access_token,
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
            "MERCADOPAGO_ACCESS_TOKEN",
            "MERCADOPAGO_WEBHOOK_SECRET",
            "MERCADOPAGO_REQUIRES_KYC",
            "MERCADOPAGO_CURRENCIES",
            "MERCADOPAGO_SETTLEMENT_DAYS",
            "MERCADOPAGO_TIMEOUT_SECS",
        ] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn from_env_requires_token_and_secret() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        assert!(MercadoPagoConfig::from_env().is_err());

        std::env::set_var("MERCADOPAGO_ACCESS_TOKEN", "APP_USR-x");
        assert!(
            MercadoPagoConfig::from_env().is_err(),
            "webhook secret missing"
        );

        std::env::set_var("MERCADOPAGO_WEBHOOK_SECRET", "secret");
        let cfg = MercadoPagoConfig::from_env().expect("both required vars set");
        assert_eq!(cfg.access_token, "APP_USR-x");
        assert_eq!(cfg.webhook_secret, "secret");
        assert!(cfg.requires_kyc);
        assert_eq!(
            cfg.currencies,
            vec!["ARS", "BRL", "CLP", "COP", "MXN", "PEN", "UYU"]
        );
        assert_eq!(cfg.settlement_days, 2);
        assert_eq!(cfg.timeout_secs, 15);
        clear_env();
    }

    #[test]
    fn from_env_reads_optional_fields() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("MERCADOPAGO_ACCESS_TOKEN", "APP_USR-x");
        std::env::set_var("MERCADOPAGO_WEBHOOK_SECRET", "secret");
        std::env::set_var("MERCADOPAGO_CURRENCIES", "ars");
        std::env::set_var("MERCADOPAGO_SETTLEMENT_DAYS", "1");

        let cfg = MercadoPagoConfig::from_env().unwrap();
        assert_eq!(cfg.currencies, vec!["ARS".to_string()]);
        assert_eq!(cfg.settlement_days, 1);
        clear_env();
    }
}
