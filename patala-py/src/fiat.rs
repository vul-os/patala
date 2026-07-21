//! `patala-fiat` exposed through UniFFI via a single by-name registry
//! constructor ([`crate::PatalaRail::new_fiat`]), not one typed constructor
//! per adapter.
//!
//! ## Why by-name + string config, not 20 typed constructors
//!
//! `patala-fiat` ships 20 feature-gated processor adapters
//! (`patala-fiat/Cargo.toml`'s `[features]` table: `stripe`, `paystack`,
//! `adyen`, `checkoutcom`, `mollie`, `mercadopago`, `flutterwave`, `iyzico`,
//! `midtrans`, `yoco`, `payfast`, `payu`, `razorpay`, `square`, `xendit`,
//! `btcpay`, `lnbits`, `opennode`, `coinbasecommerce`, `paypal`) plus the
//! always-on `manual` rail, each with its own `<Provider>Config` struct --
//! different field sets, different requiredness, different defaults (see
//! each adapter's own `config.rs` in `patala-fiat`). Mirroring
//! `patala-py`'s existing per-rail pattern (`new_solana`, `new_stellar`,
//! `new_hyperswitch` in `lib.rs`) would mean 20 more
//! `#[uniffi::constructor]`s, each with its own bespoke argument list, each
//! needing its own `#[cfg]`-gated `impl` block (see `lib.rs`'s comment on
//! why every real-rail constructor lives in its own feature-gated block --
//! the same constraint applies here, just times twenty). That is a lot of
//! generated FFI surface for very little differentiation: from the
//! caller's side every one of these rails is
//! `RailClass::CustodialReversible`, built from a flat bag of
//! strings/bools/integers, and reused through the exact same
//! `quote`/`charge`/`verify` trio every other `PatalaRail` already exposes.
//!
//! `patala-fiat` already solved "select a rail by provider name + config"
//! once, in Rust: that is precisely `patala_fiat::registry::Registry`
//! (`PORTING.md`'s own framing). This module is the FFI-facing
//! continuation of that same idea, not a second design: ONE exported
//! constructor, [`crate::PatalaRail::new_fiat`], takes a provider name
//! (`"stripe"`, `"paystack"`, `"manual"`, ...) and a `HashMap<String,
//! String>` of that provider's own config fields -- keyed by the exact same
//! field names as its `<Provider>Config` struct (see the `build_<name>`
//! functions below and the table in `patala-py/README.md`) -- and returns
//! the same [`crate::PatalaRail`] object every other constructor does. This
//! keeps the generated binding small (one constructor, not twenty) and
//! matches the shape `patala-fiat` itself already committed to at the
//! registry layer, rather than inventing a second one at the FFI layer.
//!
//! ## What this module does NOT do
//!
//! No adapter logic, no HTTP, no signature verification, and no
//! re-validation of what the adapter itself already validates: every
//! `build_<name>` function below only builds a `<Provider>Config` and calls
//! that adapter's own `<Provider>Rail::new`, exactly like `patala-fiat`'s
//! own `from_env()` does, just reading a `HashMap<String, String>` instead
//! of `std::env::var`. A config map missing a required key is passed
//! through as an empty string and rejected by the adapter's own `new()`
//! (every adapter in `patala-fiat` already fails closed on an empty
//! required field -- see `PORTING.md`), exactly as if that key had been an
//! empty/unset env var. The handful of provider-specific defaults
//! `from_env()` would otherwise apply (Paystack's hardcoded currency list,
//! iyzico's production base URL, PayPal's live/sandbox base URL, ...) reuse
//! `patala-fiat`'s own `pub const`s directly -- never a duplicated literal.
//!
//! ## Feature gating
//!
//! `fiat` (this module's own gate) pulls in `patala-fiat` with ITS default
//! features -- currency table + registry + `manual`, no network/crypto deps
//! at all (see `patala-fiat/src/lib.rs`'s own "offline-by-default" docs) --
//! so `new_fiat("manual", ...)` works the moment `--features fiat` is on,
//! with zero new dependencies beyond `patala-fiat` itself. Each of the 20
//! processor adapters needs its OWN additional Cargo feature on THIS crate
//! (`fiat-stripe`, `fiat-paystack`, ... `fiat-yoco`, see `Cargo.toml`), each
//! enabling the matching `patala-fiat/<name>` feature -- so `fiat` alone
//! never pulls in `reqwest`/`hmac`/`sha2`/etc. for any specific processor;
//! callers opt into each one individually, or all at once via the
//! `fiat-all` umbrella feature (meant for this crate's own
//! `--all-features`-style tests and the Go binding's regeneration step --
//! see `patala-go/Makefile`). This mirrors `patala-fiat/Cargo.toml`'s own
//! per-adapter feature list exactly, just namespaced under `fiat-` on this
//! crate's side to avoid colliding with `patala-py`'s own
//! `solana`/`stellar`/`hyperswitch` feature names.
//!
//! Requesting a provider whose feature was not compiled in (e.g. calling
//! `new_fiat("stripe", ...)` in a build with `fiat` but not `fiat-stripe`)
//! is a `PatalaError::InvalidRequest` naming the missing feature -- never a
//! panic, never a silent fallback to a different rail.

use std::collections::HashMap;
use std::sync::Arc;

use patala_core::PaymentRail;

use crate::{PatalaError, PatalaRail};

// Only ever called from a `#[cfg(not(feature = "fiat-<name>"))]` stub below,
// so with every `fiat-*` feature enabled at once (`--features fiat-all`,
// e.g. this crate's own CI/regeneration build) NONE of those stubs compile
// in and this function is genuinely unused -- an honest artifact of feature
// permutations, not dead code to clean up.
#[allow(dead_code)]
fn not_compiled_in(provider: &str, feature: &str) -> PatalaError {
    PatalaError::InvalidRequest {
        message: format!(
            "patala-py was built without --features {feature}; the {provider:?} fiat rail is not available in this build"
        ),
    }
}

// ---- generic string-keyed config helpers --------------------------------
//
// `config: &HashMap<String, String>` stands in for `std::env::var` here --
// every helper below mirrors the exact parsing/defaulting `patala-fiat`'s
// own `<Provider>Config::from_env` does for the equivalent env var (see
// that crate's `config.rs` files), just reading a map key instead.

/// A required (or "let the adapter's own `new()` reject it") string field.
/// Missing key -> empty string, which every adapter in `patala-fiat`
/// already treats as "not configured" and refuses with
/// `PatalaError::InvalidRequest`.
fn get_string(config: &HashMap<String, String>, key: &str) -> String {
    config.get(key).cloned().unwrap_or_default()
}

/// A string field with a non-secret, sensible default (e.g. a processor's
/// public API base URL) -- mirrors `from_env`'s `.unwrap_or_else(|| DEFAULT.to_string())`.
fn get_string_or(config: &HashMap<String, String>, key: &str, default: &str) -> String {
    config
        .get(key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// An optional string field with no default at all (`None` if absent/blank).
fn get_optional_string(config: &HashMap<String, String>, key: &str) -> Option<String> {
    config
        .get(key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Mirrors every `<PROVIDER>_REQUIRES_KYC`-style boolean: absent -> `default`;
/// present -> `true` iff case-insensitively `"true"` (anything else, `false`
/// -- same permissive parsing `from_env` itself uses).
fn get_bool(config: &HashMap<String, String>, key: &str, default: bool) -> bool {
    config
        .get(key)
        .map(|s| s.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

/// Mirrors every `<PROVIDER>_CURRENCIES`-style comma-separated list: absent
/// or blank -> `default` (each adapter's own unrestricted-empty or
/// hardcoded-list default, passed in by the caller below); present ->
/// split/trim/uppercase, matching `from_env` exactly.
fn get_currencies(config: &HashMap<String, String>, key: &str, default: &[&str]) -> Vec<String> {
    match config.get(key) {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())
            .collect(),
        _ => default.iter().map(|s| s.to_string()).collect(),
    }
}

/// A numeric field. Unlike `from_env` (which silently falls back to
/// `default` on an unparsable value -- a legacy env-var-typo convenience),
/// an explicitly-supplied-but-malformed map value is rejected as
/// `InvalidRequest` here: this is a programmatic config map, not a
/// typo-prone shell env var, so failing closed on bad explicit input is the
/// more honest choice (`PATALA.md` §8).
fn get_u8(
    config: &HashMap<String, String>,
    key: &str,
    default: u8,
    provider: &str,
) -> Result<u8, PatalaError> {
    match config.get(key) {
        None => Ok(default),
        Some(s) if s.trim().is_empty() => Ok(default),
        Some(s) => s
            .trim()
            .parse::<u8>()
            .map_err(|_| PatalaError::InvalidRequest {
                message: format!("{provider}: {key:?} must be an integer between 0 and 255"),
            }),
    }
}

fn get_optional_u32(
    config: &HashMap<String, String>,
    key: &str,
    provider: &str,
) -> Result<Option<u32>, PatalaError> {
    match config.get(key) {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => s
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| PatalaError::InvalidRequest {
                message: format!("{provider}: {key:?} must be a non-negative integer"),
            }),
    }
}

fn get_u64(
    config: &HashMap<String, String>,
    key: &str,
    default: u64,
    provider: &str,
) -> Result<u64, PatalaError> {
    match config.get(key) {
        None => Ok(default),
        Some(s) if s.trim().is_empty() => Ok(default),
        Some(s) => s
            .trim()
            .parse::<u64>()
            .map_err(|_| PatalaError::InvalidRequest {
                message: format!("{provider}: {key:?} must be a non-negative integer"),
            }),
    }
}

// ---- manual: always available once `fiat` is on, zero network ever -----

fn build_manual() -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Ok(Arc::new(patala_fiat::ManualRail::default()))
}

// ---- stripe --------------------------------------------------------------

#[cfg(feature = "fiat-stripe")]
fn build_stripe(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::StripeConfig {
        secret_key: get_string(config, "secret_key"),
        webhook_secret: get_string(config, "webhook_secret"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "stripe")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "stripe")?,
    };
    Ok(Arc::new(patala_fiat::StripeRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-stripe"))]
fn build_stripe(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("stripe", "fiat-stripe"))
}

// ---- paystack -------------------------------------------------------------

#[cfg(feature = "fiat-paystack")]
fn build_paystack(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::PaystackConfig {
        secret_key: get_string(config, "secret_key"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(
            config,
            "currencies",
            patala_fiat::paystack::config::DEFAULT_CURRENCIES,
        ),
        settlement_days: get_u8(config, "settlement_days", 2, "paystack")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "paystack")?,
    };
    Ok(Arc::new(patala_fiat::PaystackRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-paystack"))]
fn build_paystack(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("paystack", "fiat-paystack"))
}

// ---- adyen -----------------------------------------------------------------

#[cfg(feature = "fiat-adyen")]
fn build_adyen(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::AdyenConfig {
        api_key: get_string(config, "api_key"),
        merchant_account: get_string(config, "merchant_account"),
        hmac_key_hex: get_string(config, "hmac_key_hex"),
        api_base_url: get_string(config, "api_base_url"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "adyen")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "adyen")?,
    };
    Ok(Arc::new(patala_fiat::AdyenRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-adyen"))]
fn build_adyen(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("adyen", "fiat-adyen"))
}

// ---- btcpay -----------------------------------------------------------------

#[cfg(feature = "fiat-btcpay")]
fn build_btcpay(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::BTCPayConfig {
        base_url: get_string(config, "base_url"),
        api_key: get_string(config, "api_key"),
        store_id: get_string(config, "store_id"),
        webhook_secret: get_string(config, "webhook_secret"),
        requires_kyc: get_bool(config, "requires_kyc", false),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_seconds: get_optional_u32(config, "settlement_seconds", "btcpay")?,
        timeout_secs: get_u64(config, "timeout_secs", 20, "btcpay")?,
    };
    Ok(Arc::new(patala_fiat::BTCPayRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-btcpay"))]
fn build_btcpay(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("btcpay", "fiat-btcpay"))
}

// ---- checkoutcom -----------------------------------------------------------

#[cfg(feature = "fiat-checkoutcom")]
fn build_checkoutcom(
    config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::CheckoutComConfig {
        secret_key: get_string(config, "secret_key"),
        webhook_secret: get_string(config, "webhook_secret"),
        api_base_url: get_string(config, "api_base_url"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "checkoutcom")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "checkoutcom")?,
    };
    Ok(Arc::new(patala_fiat::CheckoutComRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-checkoutcom"))]
fn build_checkoutcom(
    _config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("checkoutcom", "fiat-checkoutcom"))
}

// ---- coinbasecommerce -------------------------------------------------------

#[cfg(feature = "fiat-coinbasecommerce")]
fn build_coinbasecommerce(
    config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::CoinbaseCommerceConfig {
        api_key: get_string(config, "api_key"),
        webhook_secret: get_string(config, "webhook_secret"),
        base_url: get_string_or(
            config,
            "base_url",
            patala_fiat::coinbasecommerce::config::DEFAULT_BASE_URL,
        ),
        requires_kyc: get_bool(config, "requires_kyc", false),
        currencies: get_currencies(config, "currencies", &[]),
        timeout_secs: get_u64(config, "timeout_secs", 20, "coinbasecommerce")?,
    };
    Ok(Arc::new(patala_fiat::CoinbaseCommerceRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-coinbasecommerce"))]
fn build_coinbasecommerce(
    _config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("coinbasecommerce", "fiat-coinbasecommerce"))
}

// ---- flutterwave -------------------------------------------------------------

#[cfg(feature = "fiat-flutterwave")]
fn build_flutterwave(
    config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::FlutterwaveConfig {
        secret_key: get_string(config, "secret_key"),
        webhook_hash: get_string(config, "webhook_hash"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(
            config,
            "currencies",
            patala_fiat::flutterwave::config::DEFAULT_CURRENCIES,
        ),
        settlement_days: get_u8(config, "settlement_days", 2, "flutterwave")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "flutterwave")?,
    };
    Ok(Arc::new(patala_fiat::FlutterwaveRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-flutterwave"))]
fn build_flutterwave(
    _config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("flutterwave", "fiat-flutterwave"))
}

// ---- iyzico -------------------------------------------------------------------

#[cfg(feature = "fiat-iyzico")]
fn build_iyzico(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::IyzicoConfig {
        api_key: get_string(config, "api_key"),
        secret_key: get_string(config, "secret_key"),
        base_url: get_string_or(
            config,
            "base_url",
            patala_fiat::iyzico::config::PRODUCTION_BASE_URL,
        ),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(
            config,
            "currencies",
            patala_fiat::iyzico::config::DEFAULT_CURRENCIES,
        ),
        settlement_days: get_u8(config, "settlement_days", 2, "iyzico")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "iyzico")?,
    };
    Ok(Arc::new(patala_fiat::IyzicoRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-iyzico"))]
fn build_iyzico(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("iyzico", "fiat-iyzico"))
}

// ---- lnbits -------------------------------------------------------------------

#[cfg(feature = "fiat-lnbits")]
fn build_lnbits(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::LNbitsConfig {
        base_url: get_string(config, "base_url"),
        api_key: get_string(config, "api_key"),
        webhook_secret: get_string(config, "webhook_secret"),
        webhook_url: get_optional_string(config, "webhook_url"),
        quote_ttl_secs: match config.get("quote_ttl_secs") {
            None => patala_fiat::lnbits::config::DEFAULT_QUOTE_TTL_SECS,
            Some(s) if s.trim().is_empty() => patala_fiat::lnbits::config::DEFAULT_QUOTE_TTL_SECS,
            Some(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|_| PatalaError::InvalidRequest {
                    message:
                        "lnbits: \"quote_ttl_secs\" must be a positive integer number of seconds"
                            .to_string(),
                })?,
        },
        requires_kyc: get_bool(config, "requires_kyc", false),
        currencies: get_currencies(config, "currencies", &[]),
        timeout_secs: get_u64(config, "timeout_secs", 20, "lnbits")?,
    };
    Ok(Arc::new(patala_fiat::LNbitsRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-lnbits"))]
fn build_lnbits(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("lnbits", "fiat-lnbits"))
}

// ---- mercadopago ----------------------------------------------------------------

#[cfg(feature = "fiat-mercadopago")]
fn build_mercadopago(
    config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::MercadoPagoConfig {
        access_token: get_string(config, "access_token"),
        webhook_secret: get_string(config, "webhook_secret"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(
            config,
            "currencies",
            patala_fiat::mercadopago::config::DEFAULT_CURRENCIES,
        ),
        settlement_days: get_u8(config, "settlement_days", 2, "mercadopago")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "mercadopago")?,
    };
    Ok(Arc::new(patala_fiat::MercadoPagoRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-mercadopago"))]
fn build_mercadopago(
    _config: &HashMap<String, String>,
) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("mercadopago", "fiat-mercadopago"))
}

// ---- midtrans -----------------------------------------------------------------

#[cfg(feature = "fiat-midtrans")]
fn build_midtrans(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::MidtransConfig {
        server_key: get_string(config, "server_key"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        settlement_days: get_u8(config, "settlement_days", 2, "midtrans")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "midtrans")?,
    };
    Ok(Arc::new(patala_fiat::MidtransRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-midtrans"))]
fn build_midtrans(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("midtrans", "fiat-midtrans"))
}

// ---- mollie -----------------------------------------------------------------

#[cfg(feature = "fiat-mollie")]
fn build_mollie(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::MollieConfig {
        api_key: get_string(config, "api_key"),
        webhook_url: get_string(config, "webhook_url"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "mollie")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "mollie")?,
    };
    Ok(Arc::new(patala_fiat::MollieRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-mollie"))]
fn build_mollie(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("mollie", "fiat-mollie"))
}

// ---- opennode -----------------------------------------------------------------

#[cfg(feature = "fiat-opennode")]
fn build_opennode(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::OpenNodeConfig {
        api_key: get_string(config, "api_key"),
        base_url: get_string_or(
            config,
            "base_url",
            patala_fiat::opennode::config::DEFAULT_BASE_URL,
        ),
        requires_kyc: get_bool(config, "requires_kyc", false),
        currencies: get_currencies(config, "currencies", &[]),
        timeout_secs: get_u64(config, "timeout_secs", 20, "opennode")?,
    };
    Ok(Arc::new(patala_fiat::OpenNodeRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-opennode"))]
fn build_opennode(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("opennode", "fiat-opennode"))
}

// ---- payfast -----------------------------------------------------------------

#[cfg(feature = "fiat-payfast")]
fn build_payfast(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::PayFastConfig {
        merchant_id: get_string(config, "merchant_id"),
        merchant_key: get_string(config, "merchant_key"),
        passphrase: get_string(config, "passphrase"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        settlement_days: get_u8(config, "settlement_days", 2, "payfast")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "payfast")?,
    };
    Ok(Arc::new(patala_fiat::PayFastRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-payfast"))]
fn build_payfast(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("payfast", "fiat-payfast"))
}

// ---- paypal -----------------------------------------------------------------

#[cfg(feature = "fiat-paypal")]
fn build_paypal(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    // Mirrors `PayPalConfig::from_env`'s own `PAYPAL_ENV` resolution exactly
    // (required explicitly, no default either way, to avoid ever silently
    // pointing at the wrong PayPal environment -- see
    // `patala-fiat/src/paypal/config.rs`'s module docs) -- reusing its own
    // `LIVE_BASE_URL`/`SANDBOX_BASE_URL` constants rather than duplicating
    // them.
    let env = get_string(config, "env").trim().to_ascii_lowercase();
    let base_url = match env.as_str() {
        "live" => patala_fiat::paypal::config::LIVE_BASE_URL.to_string(),
        "sandbox" => patala_fiat::paypal::config::SANDBOX_BASE_URL.to_string(),
        other => {
            return Err(PatalaError::InvalidRequest {
                message: format!(
                "paypal: config key \"env\" must be exactly \"live\" or \"sandbox\", got {other:?}"
            ),
            })
        }
    };
    let cfg = patala_fiat::PayPalConfig {
        client_id: get_string(config, "client_id"),
        client_secret: get_string(config, "client_secret"),
        webhook_id: get_string(config, "webhook_id"),
        base_url,
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "paypal")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "paypal")?,
    };
    Ok(Arc::new(patala_fiat::PayPalRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-paypal"))]
fn build_paypal(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("paypal", "fiat-paypal"))
}

// ---- payu -----------------------------------------------------------------

#[cfg(feature = "fiat-payu")]
fn build_payu(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::PayUConfig {
        merchant_key: get_string(config, "merchant_key"),
        salt: get_string(config, "salt"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        settlement_days: get_u8(config, "settlement_days", 2, "payu")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "payu")?,
    };
    Ok(Arc::new(patala_fiat::PayURail::new(cfg)?))
}
#[cfg(not(feature = "fiat-payu"))]
fn build_payu(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("payu", "fiat-payu"))
}

// ---- razorpay -----------------------------------------------------------------

#[cfg(feature = "fiat-razorpay")]
fn build_razorpay(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::RazorpayConfig {
        key_id: get_string(config, "key_id"),
        key_secret: get_string(config, "key_secret"),
        webhook_secret: get_string(config, "webhook_secret"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        settlement_days: get_u8(config, "settlement_days", 2, "razorpay")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "razorpay")?,
    };
    Ok(Arc::new(patala_fiat::RazorpayRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-razorpay"))]
fn build_razorpay(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("razorpay", "fiat-razorpay"))
}

// ---- square -----------------------------------------------------------------

#[cfg(feature = "fiat-square")]
fn build_square(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::SquareConfig {
        access_token: get_string(config, "access_token"),
        webhook_signature_key: get_string(config, "webhook_signature_key"),
        location_id: get_string(config, "location_id"),
        notification_url: get_string(config, "notification_url"),
        api_base_url: get_string(config, "api_base_url"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(config, "currencies", &[]),
        settlement_days: get_u8(config, "settlement_days", 2, "square")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "square")?,
    };
    Ok(Arc::new(patala_fiat::SquareRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-square"))]
fn build_square(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("square", "fiat-square"))
}

// ---- xendit -----------------------------------------------------------------

#[cfg(feature = "fiat-xendit")]
fn build_xendit(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::XenditConfig {
        secret_key: get_string(config, "secret_key"),
        webhook_token: get_string(config, "webhook_token"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        currencies: get_currencies(
            config,
            "currencies",
            patala_fiat::xendit::config::DEFAULT_CURRENCIES,
        ),
        settlement_days: get_u8(config, "settlement_days", 2, "xendit")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "xendit")?,
    };
    Ok(Arc::new(patala_fiat::XenditRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-xendit"))]
fn build_xendit(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("xendit", "fiat-xendit"))
}

// ---- yoco -----------------------------------------------------------------

#[cfg(feature = "fiat-yoco")]
fn build_yoco(config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    let cfg = patala_fiat::YocoConfig {
        secret_key: get_string(config, "secret_key"),
        webhook_secret: get_string(config, "webhook_secret"),
        requires_kyc: get_bool(config, "requires_kyc", true),
        settlement_days: get_u8(config, "settlement_days", 2, "yoco")?,
        timeout_secs: get_u64(config, "timeout_secs", 15, "yoco")?,
    };
    Ok(Arc::new(patala_fiat::YocoRail::new(cfg)?))
}
#[cfg(not(feature = "fiat-yoco"))]
fn build_yoco(_config: &HashMap<String, String>) -> Result<Arc<dyn PaymentRail>, PatalaError> {
    Err(not_compiled_in("yoco", "fiat-yoco"))
}

// ---- the exported surface --------------------------------------------------

#[uniffi::export]
impl PatalaRail {
    /// Build a `patala-fiat` rail by provider name + a string-keyed config
    /// map, reusing `patala-fiat`'s own registry of adapters (see this
    /// module's docs for why by-name+config was chosen over one typed
    /// constructor per adapter). `provider` is matched case-insensitively
    /// against: `"manual"` (always available once `fiat` is on) and the 20
    /// feature-gated processor names `patala-fiat` ships --
    /// `"stripe"`, `"paystack"`, `"adyen"`, `"checkoutcom"`, `"mollie"`,
    /// `"mercadopago"`, `"flutterwave"`, `"iyzico"`, `"midtrans"`,
    /// `"yoco"`, `"payfast"`, `"payu"`, `"razorpay"`, `"square"`,
    /// `"xendit"`, `"btcpay"`, `"lnbits"`, `"opennode"`,
    /// `"coinbasecommerce"`, `"paypal"`. Every one of the 20 is
    /// `RailClass::CustodialReversible` (`patala-fiat`'s own
    /// `holds_funds: true` on every one, describing the PROCESSOR's
    /// custody, never patala's -- `PATALA.md` §1, §8).
    ///
    /// `config` keys are the exact field names of that provider's own
    /// `<Provider>Config` struct in `patala-fiat` (e.g. stripe wants
    /// `secret_key`/`webhook_secret`; see `patala-py/README.md`'s table for
    /// the full per-provider key list, or that adapter's own `config.rs`
    /// doc comment). A missing REQUIRED key is passed through as an empty
    /// string and rejected by that adapter's own `new()` constructor with a
    /// `PatalaError::InvalidRequest` naming the missing field -- this
    /// module never duplicates that validation. Numeric/boolean/list
    /// fields get the same defaults `patala-fiat`'s own `from_env()` would
    /// apply (e.g. `requires_kyc` defaults `true` for every card/bank rail,
    /// `settlement_days` defaults `2` -- card-network T+2).
    ///
    /// Requesting a provider whose Cargo feature was not compiled into
    /// this build (see this module's "Feature gating" docs) is also a
    /// `PatalaError::InvalidRequest`, never a panic.
    ///
    /// Building a rail never dials the network -- only the returned
    /// [`PatalaRail`]'s `quote`/`charge`/`verify` do that (except
    /// `"manual"`, which never dials the network at all -- it is the
    /// fiat-side equivalent of [`patala_core::MockRail`], see
    /// `patala-fiat`'s own `manual` module docs).
    #[uniffi::constructor]
    pub fn new_fiat(
        provider: String,
        config: HashMap<String, String>,
    ) -> Result<Arc<Self>, PatalaError> {
        let inner: Arc<dyn PaymentRail> = match provider.to_ascii_lowercase().as_str() {
            "manual" => build_manual()?,
            "stripe" => build_stripe(&config)?,
            "paystack" => build_paystack(&config)?,
            "adyen" => build_adyen(&config)?,
            "btcpay" => build_btcpay(&config)?,
            "checkoutcom" => build_checkoutcom(&config)?,
            "coinbasecommerce" => build_coinbasecommerce(&config)?,
            "flutterwave" => build_flutterwave(&config)?,
            "iyzico" => build_iyzico(&config)?,
            "lnbits" => build_lnbits(&config)?,
            "mercadopago" => build_mercadopago(&config)?,
            "midtrans" => build_midtrans(&config)?,
            "mollie" => build_mollie(&config)?,
            "opennode" => build_opennode(&config)?,
            "payfast" => build_payfast(&config)?,
            "paypal" => build_paypal(&config)?,
            "payu" => build_payu(&config)?,
            "razorpay" => build_razorpay(&config)?,
            "square" => build_square(&config)?,
            "xendit" => build_xendit(&config)?,
            "yoco" => build_yoco(&config)?,
            other => {
                return Err(PatalaError::InvalidRequest {
                    message: format!(
                        "unknown fiat provider {other:?}; see patala-fiat's registry (PORTING.md) for the supported list"
                    ),
                })
            }
        };
        Ok(Arc::new(Self { inner }))
    }
}

/// Every fiat provider name this specific build of `patala-py` can actually
/// construct via [`PatalaRail::new_fiat`] -- `"manual"` always, plus
/// whichever `fiat-<name>` Cargo features were compiled in. Lets a caller
/// (e.g. cackle) discover what is available at runtime instead of
/// hardcoding a list that might not match this build, sorted for a
/// stable/testable order.
///
/// A free function, not a `PatalaRail` method: UniFFI does not currently
/// support exporting a plain associated function (one with no `&self` and
/// no `#[uniffi::constructor]`) from inside an `impl` block, so this lives
/// at module scope instead and is generated as a top-level function in
/// every target language (e.g. `patala.PatalaFiatProviders()` in Go).
#[uniffi::export]
pub fn patala_fiat_providers() -> Vec<String> {
    let mut names = vec!["manual".to_string()];
    #[cfg(feature = "fiat-adyen")]
    names.push("adyen".to_string());
    #[cfg(feature = "fiat-btcpay")]
    names.push("btcpay".to_string());
    #[cfg(feature = "fiat-checkoutcom")]
    names.push("checkoutcom".to_string());
    #[cfg(feature = "fiat-coinbasecommerce")]
    names.push("coinbasecommerce".to_string());
    #[cfg(feature = "fiat-flutterwave")]
    names.push("flutterwave".to_string());
    #[cfg(feature = "fiat-iyzico")]
    names.push("iyzico".to_string());
    #[cfg(feature = "fiat-lnbits")]
    names.push("lnbits".to_string());
    #[cfg(feature = "fiat-mercadopago")]
    names.push("mercadopago".to_string());
    #[cfg(feature = "fiat-midtrans")]
    names.push("midtrans".to_string());
    #[cfg(feature = "fiat-mollie")]
    names.push("mollie".to_string());
    #[cfg(feature = "fiat-opennode")]
    names.push("opennode".to_string());
    #[cfg(feature = "fiat-payfast")]
    names.push("payfast".to_string());
    #[cfg(feature = "fiat-paypal")]
    names.push("paypal".to_string());
    #[cfg(feature = "fiat-payu")]
    names.push("payu".to_string());
    #[cfg(feature = "fiat-paystack")]
    names.push("paystack".to_string());
    #[cfg(feature = "fiat-razorpay")]
    names.push("razorpay".to_string());
    #[cfg(feature = "fiat-square")]
    names.push("square".to_string());
    #[cfg(feature = "fiat-stripe")]
    names.push("stripe".to_string());
    #[cfg(feature = "fiat-xendit")]
    names.push("xendit".to_string());
    #[cfg(feature = "fiat-yoco")]
    names.push("yoco".to_string());
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(amount: u64, reference: &str) -> crate::PayRequest {
        crate::PayRequest {
            amount_minor: amount,
            currency: "ZAR".into(),
            destination: "buyer@example.org".into(),
            reference: reference.into(),
        }
    }

    /// `PatalaRail` (via `Arc<Self>`) does not implement `Debug`, so plain
    /// `.expect_err(...)`/`.unwrap_err()` (which require the `Ok` side to be
    /// `Debug` too) don't work here -- this asserts `Err` the long way
    /// instead of adding a `Debug` impl just for tests.
    fn expect_err(result: Result<Arc<PatalaRail>, PatalaError>, msg: &str) -> PatalaError {
        match result {
            Err(e) => e,
            Ok(_) => panic!("{msg}"),
        }
    }

    #[test]
    fn new_fiat_manual_is_always_available_and_offline() {
        let rail = PatalaRail::new_fiat("manual".into(), HashMap::new())
            .expect("manual never needs config and never touches the network");
        assert_eq!(rail.id(), "manual");
        let caps = rail.capabilities();
        assert!(
            !caps.holds_funds,
            "manual has no processor -- nothing custodies anything"
        );
    }

    #[test]
    fn new_fiat_manual_charge_reports_honestly_pending_through_the_ffi() {
        // `manual` is cackle's "bank transfer, a human confirms it later"
        // rail (see `patala_fiat::manual`'s own module docs): `charge()`
        // returns instructions, never settled money (`amount_minor: 0`),
        // and the ONLY way to actually mark it paid is `ManualRail`'s own
        // inherent `mark_paid` method -- which is NOT part of the
        // `PaymentRail` trait, so it is unreachable through this generic
        // by-name-provider FFI surface (which only ever holds a
        // `Arc<dyn PaymentRail>`, not a concrete `ManualRail`). So through
        // `new_fiat`, a manual charge honestly verifies `false` until some
        // OTHER, direct-Rust caller marks it paid -- this is
        // `patala_core::Receipt`'s pending-vs-settled contract working
        // exactly as intended, not a bug in this binding.
        let rail = PatalaRail::new_fiat("MANUAL".into(), HashMap::new())
            .expect("provider name is matched case-insensitively");
        let receipt = rail.charge(req(1500, "fiat-order-1")).expect("charge");
        assert_eq!(receipt.amount_minor, 0, "nothing has settled yet");
        assert!(
            !rail.verify(receipt).expect("verify"),
            "an unconfirmed manual instruction must never report settled"
        );
    }

    #[test]
    fn new_fiat_rejects_unknown_provider() {
        let err = expect_err(
            PatalaRail::new_fiat("not-a-real-processor".into(), HashMap::new()),
            "an unrecognised provider name must never silently fall back",
        );
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));
    }

    #[test]
    fn fiat_providers_always_includes_manual() {
        assert!(patala_fiat_providers().contains(&"manual".to_string()));
    }

    // The tests below only run when the matching feature is enabled
    // (`cargo test -p patala-py --features fiat-stripe,fiat-paystack,...`)
    // and only ever CONSTRUCT a rail (never `charge`/`verify`, which would
    // dial a real processor) -- exactly the same offline-construction-only
    // precedent `new_solana`/`new_stellar`/`new_hyperswitch`'s own tests in
    // `lib.rs` already set for real (non-mock) rails.

    #[cfg(feature = "fiat-stripe")]
    #[test]
    fn new_fiat_stripe_builds_offline_and_reports_custodial_reversible() {
        let mut config = HashMap::new();
        config.insert("secret_key".into(), "sk_test_x".into());
        config.insert("webhook_secret".into(), "whsec_x".into());
        let rail = PatalaRail::new_fiat("stripe".into(), config)
            .expect("constructing a StripeRail must not require network access");
        assert_eq!(rail.id(), "stripe");
        let caps = rail.capabilities();
        assert_eq!(caps.class, crate::RailClass::CustodialReversible);
        assert!(caps.holds_funds);
    }

    #[cfg(feature = "fiat-stripe")]
    #[test]
    fn new_fiat_stripe_rejects_missing_webhook_secret() {
        let mut config = HashMap::new();
        config.insert("secret_key".into(), "sk_test_x".into());
        let err = expect_err(
            PatalaRail::new_fiat("stripe".into(), config),
            "stripe requires a webhook secret up front",
        );
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));
    }

    #[cfg(not(feature = "fiat-stripe"))]
    #[test]
    fn new_fiat_stripe_reports_feature_not_compiled_in() {
        let err = expect_err(
            PatalaRail::new_fiat("stripe".into(), HashMap::new()),
            "fiat-stripe is not enabled in this test run",
        );
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));
    }

    #[cfg(feature = "fiat-paystack")]
    #[test]
    fn new_fiat_paystack_uses_default_currency_list() {
        let mut config = HashMap::new();
        config.insert("secret_key".into(), "sk_test_x".into());
        let rail = PatalaRail::new_fiat("paystack".into(), config)
            .expect("paystack's own DEFAULT_CURRENCIES makes currencies non-empty");
        let caps = rail.capabilities();
        assert!(caps.currencies.contains(&"ZAR".to_string()));
    }

    #[cfg(feature = "fiat-paypal")]
    #[test]
    fn new_fiat_paypal_requires_a_valid_env() {
        let mut config = HashMap::new();
        config.insert("client_id".into(), "cid".into());
        config.insert("client_secret".into(), "secret".into());
        config.insert("webhook_id".into(), "WH-1".into());
        config.insert("env".into(), "production".into()); // not "live"/"sandbox"
        let err = expect_err(
            PatalaRail::new_fiat("paypal".into(), config),
            "an env value other than live/sandbox must be refused",
        );
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));

        let mut config = HashMap::new();
        config.insert("client_id".into(), "cid".into());
        config.insert("client_secret".into(), "secret".into());
        config.insert("webhook_id".into(), "WH-1".into());
        config.insert("env".into(), "sandbox".into());
        let rail = PatalaRail::new_fiat("paypal".into(), config).expect("sandbox is valid");
        assert_eq!(
            rail.capabilities().class,
            crate::RailClass::CustodialReversible
        );
    }

    #[cfg(feature = "fiat-btcpay")]
    #[test]
    fn new_fiat_btcpay_rejects_non_numeric_settlement_seconds() {
        let mut config = HashMap::new();
        config.insert("base_url".into(), "https://btcpay.example.com".into());
        config.insert("api_key".into(), "key".into());
        config.insert("store_id".into(), "store1".into());
        config.insert("webhook_secret".into(), "secret".into());
        config.insert("settlement_seconds".into(), "not-a-number".into());
        let err = expect_err(
            PatalaRail::new_fiat("btcpay".into(), config),
            "a malformed numeric field must fail closed, not silently default",
        );
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));
    }
}
