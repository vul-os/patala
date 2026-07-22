//! # patala-py
//!
//! The Python binding over `patala-core` (`PATALA.md` §5: "adapters are
//! written ONCE in Rust; Python and any other language consume that one
//! core"). This crate never reimplements a rail — it wraps whatever
//! `Box<dyn patala_core::PaymentRail>` already exists and exposes it to
//! Python (and, later, any other UniFFI target: Swift, Kotlin, ...) through
//! one generated interface.
//!
//! ## Why UniFFI over PyO3
//!
//! See `patala-py/README.md` for the full justification; short version:
//! `PATALA.md` §5 explicitly names UniFFI as "likely the better call" because
//! the suite wants more than Python (wasm/napi for JS is called out
//! explicitly, and Swift/Kotlin are free with UniFFI once this IDL exists).
//! One `#[uniffi::export]` surface here generates bindings for every target
//! language from a single definition — that is the literal "M×1, never M×N"
//! principle this crate exists to satisfy. PyO3 would give slightly nicer
//! Python ergonomics (real Python classes, no ctypes indirection) but is
//! Python-only: a second language would mean a second binding crate, i.e.
//! exactly the M×N this file is supposed to prevent.
//!
//! ## Async boundary
//!
//! [`patala_core::PaymentRail`]'s methods are `async fn`. UniFFI can export
//! `async fn`s to Python (it drives them off Python's own `asyncio` loop),
//! but that would force every caller — including a simple one-shot script —
//! to run an event loop just to call `charge()`. Since this binding's whole
//! job is to make the rail reachable from *any* Python code, not just async
//! code, [`PatalaRail`]'s exported methods are **synchronous**: each one
//! blocks on the underlying async call using a single lazily-created
//! multi-thread [`tokio::runtime::Runtime`] owned by this crate (see
//! `runtime`). This is the same trade patala-sidecar makes in the other
//! direction (it stays async because its whole existence *is* an async HTTP
//! server) — here the goal is a plain blocking call, so `block_on` inside a
//! dedicated runtime is the right shape, not a leaked requirement that the
//! Python caller manage async themselves. A future async-Python surface
//! could be added alongside this one without redesigning the object model,
//! since it would wrap the exact same `Arc<dyn PaymentRail>`.
//!
//! ## Adding a real rail later
//!
//! [`PatalaRail`] wraps `Arc<dyn patala_core::PaymentRail>` — the trait
//! object, not a concrete type. Today the only constructor is
//! [`PatalaRail::new_mock`], built on [`patala_core::MockRail`]. When
//! `patala-solana`/`patala-stellar`/`patala-hyperswitch` exist, this crate
//! adds one constructor per rail (e.g. `new_solana(rpc_url, keypair_bytes)`,
//! feature-gated the same way the Rust crates are) that builds the real rail
//! and wraps it in the same `PatalaRail { inner: Arc::new(real_rail) }`.
//! Every method Python already calls — `id`, `capabilities`, `quote`,
//! `charge`, `verify` — is unchanged: **the generated Python API surface
//! does not grow or change shape when a real rail is added**, only the
//! constructor list does. That is what "structured so real rails can be
//! exposed later without redesign" means concretely here.
//!
//! ## `patala-fiat` (20 processor adapters, one constructor)
//!
//! Unlike the one-constructor-per-rail pattern above, `patala-fiat`'s 20
//! feature-gated processor adapters (Stripe, Paystack, Adyen, ...) are
//! exposed through a single by-name registry constructor,
//! `PatalaRail::new_fiat`, defined in `fiat.rs` (gated behind
//! `--features fiat`, with each adapter behind its own additional
//! `fiat-<name>` feature). See that file's module docs for the full
//! justification of why by-name+config was chosen over 20 more typed
//! constructors.

use std::sync::{Arc, OnceLock};

use patala_core::{
    Error as CoreError, MockRail, PayRequest as CorePayRequest, PaymentRail, Quote as CoreQuote,
    RailCapabilities as CoreRailCapabilities, RailClass as CoreRailClass, Receipt as CoreReceipt,
    Settlement as CoreSettlement,
};

#[cfg(feature = "solana")]
use patala_solana::{
    keys::Keypair as SolanaKeypair, rpc::HttpRpc as SolanaHttpRpc, SolanaConfig, SolanaRail,
};

#[cfg(feature = "stellar")]
use patala_stellar::{
    keys::Keypair as StellarKeypair, rpc::HorizonRpc, StellarConfig, StellarRail,
};

#[cfg(feature = "hyperswitch")]
use patala_hyperswitch::{HyperswitchConfig, HyperswitchRail};

// `patala-fiat`'s 20 processor adapters, exposed via ONE by-name registry
// constructor (`PatalaRail::new_fiat`) rather than 20 typed ones -- see
// `fiat.rs`'s module docs for the full justification. Declared as its own
// file (not inline here) purely for size: twenty adapters' worth of
// config-mapping code would otherwise dwarf this file.
#[cfg(feature = "fiat")]
mod fiat;

uniffi::setup_scaffolding!();

/// Parse a caller-supplied 32-byte seed into a fixed-size array, failing
/// closed (as `PatalaError::InvalidRequest`, never a panic) on any other
/// length. Shared by every real-rail constructor that accepts a raw seed.
#[cfg(any(feature = "solana", feature = "stellar"))]
fn seed32(bytes: &[u8], rail: &str) -> Result<[u8; 32], PatalaError> {
    bytes.try_into().map_err(|_| PatalaError::InvalidRequest {
        message: format!(
            "{rail} keypair seed must be exactly 32 bytes, got {}",
            bytes.len()
        ),
    })
}

/// The shared runtime every [`PatalaRail`] method blocks on. One process-wide
/// multi-thread runtime, created on first use — see the module docs' "Async
/// boundary" section for why this exists at all.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("patala-py: failed to start the internal tokio runtime")
    })
}

/// `PATALA.md` §3's [`patala_core::RailClass`], mirrored 1:1 for UniFFI.
/// Never flattened to a bool on this side of the boundary either — see the
/// core type's docs for why.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailClass {
    CustodialReversible,
    NonCustodialFinal,
}

impl From<CoreRailClass> for RailClass {
    fn from(c: CoreRailClass) -> Self {
        match c {
            CoreRailClass::CustodialReversible => RailClass::CustodialReversible,
            CoreRailClass::NonCustodialFinal => RailClass::NonCustodialFinal,
        }
    }
}

impl From<RailClass> for CoreRailClass {
    fn from(c: RailClass) -> Self {
        match c {
            RailClass::CustodialReversible => CoreRailClass::CustodialReversible,
            RailClass::NonCustodialFinal => CoreRailClass::NonCustodialFinal,
        }
    }
}

/// Mirrors [`patala_core::Settlement`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    Instant,
    Seconds { secs: u32 },
    Days { days: u8 },
}

impl From<CoreSettlement> for Settlement {
    fn from(s: CoreSettlement) -> Self {
        match s {
            CoreSettlement::Instant => Settlement::Instant,
            CoreSettlement::Seconds(secs) => Settlement::Seconds { secs },
            CoreSettlement::Days(days) => Settlement::Days { days },
        }
    }
}

/// Mirrors [`patala_core::RailCapabilities`]. Readable from Python exactly as
/// the doc comment on the core type demands: a caller can branch on `class`
/// without knowing which provider is behind a given [`PatalaRail`].
#[derive(uniffi::Record, Clone, Debug)]
pub struct RailCapabilities {
    pub class: RailClass,
    pub reversible: bool,
    pub requires_kyc: bool,
    pub holds_funds: bool,
    pub currencies: Vec<String>,
    pub settlement: Settlement,
}

impl From<&CoreRailCapabilities> for RailCapabilities {
    fn from(c: &CoreRailCapabilities) -> Self {
        Self {
            class: c.class.into(),
            reversible: c.reversible,
            requires_kyc: c.requires_kyc,
            holds_funds: c.holds_funds,
            currencies: c.currencies.clone(),
            settlement: c.settlement.into(),
        }
    }
}

/// Mirrors [`patala_core::PayRequest`]. Amount stays a `u64` minor-units
/// integer across the FFI boundary too — never a float (`PATALA.md` §3, §8).
#[derive(uniffi::Record, Clone, Debug)]
pub struct PayRequest {
    pub amount_minor: u64,
    pub currency: String,
    pub destination: String,
    pub reference: String,
}

impl From<PayRequest> for CorePayRequest {
    fn from(r: PayRequest) -> Self {
        CorePayRequest {
            amount_minor: r.amount_minor,
            currency: r.currency,
            destination: r.destination,
            reference: r.reference,
        }
    }
}

/// Mirrors [`patala_core::Quote`].
#[derive(uniffi::Record, Clone, Debug)]
pub struct Quote {
    pub rail_id: String,
    pub amount_minor: u64,
    pub currency: String,
    pub fee_minor: u64,
    pub total_minor: u64,
    pub settlement: Settlement,
    pub expires_at_unix: u64,
}

impl From<CoreQuote> for Quote {
    fn from(q: CoreQuote) -> Self {
        Self {
            rail_id: q.rail_id,
            amount_minor: q.amount_minor,
            currency: q.currency,
            fee_minor: q.fee_minor,
            total_minor: q.total_minor,
            settlement: q.settlement.into(),
            expires_at_unix: q.expires_at_unix,
        }
    }
}

/// Mirrors [`patala_core::Receipt`]. `proof` stays opaque bytes — this crate
/// never interprets it, exactly like `patala-core` itself.
#[derive(uniffi::Record, Clone, Debug)]
pub struct Receipt {
    pub rail_id: String,
    pub amount_minor: u64,
    pub currency: String,
    pub reference: String,
    pub proof: Vec<u8>,
    pub settled_at_unix: u64,
}

impl From<CoreReceipt> for Receipt {
    fn from(r: CoreReceipt) -> Self {
        Self {
            rail_id: r.rail_id,
            amount_minor: r.amount_minor,
            currency: r.currency,
            reference: r.reference,
            proof: r.proof,
            settled_at_unix: r.settled_at_unix,
        }
    }
}

impl From<Receipt> for CoreReceipt {
    fn from(r: Receipt) -> Self {
        CoreReceipt {
            rail_id: r.rail_id,
            amount_minor: r.amount_minor,
            currency: r.currency,
            reference: r.reference,
            proof: r.proof,
            settled_at_unix: r.settled_at_unix,
        }
    }
}

/// Mirrors [`patala_core::Error`]. `verify` failing closed is expressed the
/// same way it is in the core crate: as `Ok(false)`, never as a variant
/// here — see `patala_core::error` module docs.
#[derive(uniffi::Error, thiserror::Error, Debug)]
pub enum PatalaError {
    #[error("{operation} is not supported by this rail")]
    Unsupported { operation: String },
    #[error("rail error: {message}")]
    Rail { message: String },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error(
        "failover from a {from:?} rail to a {to:?} rail would cross the settlement-class boundary"
    )]
    CrossClassFailover { from: RailClass, to: RailClass },
    #[error("no rail satisfied the request")]
    AllRailsFailed,
}

impl From<CoreError> for PatalaError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::Unsupported(op) => PatalaError::Unsupported {
                operation: op.to_string(),
            },
            CoreError::Rail(message) => PatalaError::Rail { message },
            CoreError::InvalidRequest(message) => PatalaError::InvalidRequest { message },
            CoreError::CrossClassFailover { from, to } => PatalaError::CrossClassFailover {
                from: from.into(),
                to: to.into(),
            },
            CoreError::AllRailsFailed => PatalaError::AllRailsFailed,
        }
    }
}

/// The one object type Python (or Swift, or Kotlin) ever sees. It wraps
/// whatever concrete [`patala_core::PaymentRail`] its constructor built, so
/// every real rail added later reuses this exact type and these exact
/// methods — see the module docs' "Adding a real rail later" section.
#[derive(uniffi::Object)]
pub struct PatalaRail {
    inner: Arc<dyn PaymentRail>,
}

#[uniffi::export]
impl PatalaRail {
    /// Build a rail backed by [`patala_core::MockRail`] — the offline
    /// default, and today the only rail this crate can construct (real
    /// rails land as additional constructors, not additional types; see the
    /// module docs).
    #[uniffi::constructor]
    pub fn new_mock(
        id: String,
        class: RailClass,
        currencies: Vec<String>,
        fee_minor: u64,
        failing: bool,
    ) -> Arc<Self> {
        let mut rail = MockRail::new(id, class.into(), currencies).with_fee_minor(fee_minor);
        if failing {
            rail = rail.failing();
        }
        Arc::new(Self {
            inner: Arc::new(rail),
        })
    }

    /// Stable rail id — see [`patala_core::PaymentRail::id`].
    pub fn id(&self) -> String {
        self.inner.id().to_string()
    }

    /// The capability descriptor — see [`patala_core::PaymentRail::capabilities`].
    /// Readable from Python without needing to know which rail is behind it.
    pub fn capabilities(&self) -> RailCapabilities {
        RailCapabilities::from(self.inner.capabilities())
    }

    /// Fees, fx and expiry for a prospective payment. Blocks the calling
    /// Python thread for the duration of the underlying async call — see the
    /// module docs' "Async boundary" section.
    pub fn quote(&self, req: PayRequest) -> Result<Quote, PatalaError> {
        let core_req: CorePayRequest = req.into();
        runtime()
            .block_on(self.inner.quote(&core_req))
            .map(Quote::from)
            .map_err(PatalaError::from)
    }

    /// Initiate/settle a payment, returning the [`Receipt`] entitlement.
    pub fn charge(&self, req: PayRequest) -> Result<Receipt, PatalaError> {
        let core_req: CorePayRequest = req.into();
        runtime()
            .block_on(self.inner.charge(&core_req))
            .map(Receipt::from)
            .map_err(PatalaError::from)
    }

    /// Verify a receipt was actually issued by this rail. Fails closed — see
    /// [`patala_core::PaymentRail::verify`]'s docs: any doubt is `Ok(false)`,
    /// never assumed valid.
    pub fn verify(&self, receipt: Receipt) -> Result<bool, PatalaError> {
        let core_receipt: CoreReceipt = receipt.into();
        runtime()
            .block_on(self.inner.verify(&core_receipt))
            .map_err(PatalaError::from)
    }
}

// The three real-rail constructors below each live in their OWN
// `#[uniffi::export] impl PatalaRail` block, with `#[cfg(feature = "...")]`
// on the *block* itself rather than on individual methods inside the shared
// block above. This is deliberate, not stylistic: `cfg` resolves before an
// outer attribute macro like `#[uniffi::export]` runs when they are stacked
// on the same item, so a `#[cfg]`-gated whole impl block is cleanly absent
// from the macro's input in a feature-off build. `#[uniffi::export]` does
// NOT reliably do the equivalent per-method inside one shared block — every
// method in a single `#[uniffi::export] impl` contributes to that macro's
// generated scaffolding regardless of any `#[cfg]` on the individual `fn`,
// which breaks a real feature-gated default build with an E0599 ("function
// not found") pointing at the constructor's own definition. Splitting into
// one block per feature is what actually keeps the feature-free build
// offline and green (verified in this environment) while still adding one
// constructor per rail, exactly as the module docs describe.

/// Build a rail backed by the real [`patala_solana::SolanaRail`]
/// (`--features solana`; PATALA.md §4, §7). `NonCustodialFinal` —
/// wallet-to-wallet SPL-USDC. `cluster` is `"devnet"` or
/// `"mainnet"`/`"mainnet-beta"`; anything else is a
/// `PatalaError::InvalidRequest`, never a silent default (`SolanaConfig`
/// itself has no "unknown cluster" fallback, so neither does this
/// constructor). `keypair_seed`, if given, must be exactly 32 raw Ed25519
/// seed bytes — per `PATALA.md` §6 this is simultaneously the signing
/// identity and the wallet funds move from, no separate mapping table. Omit
/// it to build a verify-only rail (one that can `quote`/`verify` but not
/// `charge`). This constructor only builds the rail object and talks to no
/// network itself — `quote`/`charge`/`verify` are what actually hit
/// `rpc_url`.
#[cfg(feature = "solana")]
#[uniffi::export]
impl PatalaRail {
    #[uniffi::constructor]
    pub fn new_solana(
        rpc_url: String,
        cluster: String,
        keypair_seed: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, PatalaError> {
        let cfg = match cluster.as_str() {
            "devnet" => SolanaConfig::devnet(rpc_url.clone()),
            "mainnet" | "mainnet-beta" => SolanaConfig::mainnet(rpc_url.clone()),
            other => {
                return Err(PatalaError::InvalidRequest {
                    message: format!(
                        "unknown solana cluster {other:?}; use \"devnet\" or \"mainnet\""
                    ),
                })
            }
        };
        let rpc: Arc<dyn patala_solana::rpc::SolanaRpc> = Arc::new(SolanaHttpRpc::new(rpc_url));
        let mut rail = SolanaRail::new(cfg, rpc);
        if let Some(seed) = keypair_seed {
            rail = rail.with_signer(SolanaKeypair::from_seed(seed32(&seed, "solana")?));
        }
        Ok(Arc::new(Self {
            inner: Arc::new(rail),
        }))
    }
}

/// Build a rail backed by the real [`patala_stellar::StellarRail`]
/// (`--features stellar`; PATALA.md §4, §6 — **UNVERIFIED AGAINST LIVE
/// STELLAR**, see `patala-stellar`'s own README). `NonCustodialFinal` —
/// wallet-to-wallet native Circle USDC. `network` is `"testnet"` (which
/// *requires* `usdc_issuer`, since the testnet issuer rotates and has no
/// fixed well-known value — `StellarConfig::testnet` takes it explicitly) or
/// `"public"`/`"mainnet"` (which ignores `usdc_issuer` and uses the
/// well-known Circle mainnet issuer baked into `patala-stellar`).
/// `keypair_seed`, if given, must be exactly 32 raw Ed25519 seed bytes
/// (StrKey-encoded on-chain) — same "identity key doubles as wallet key"
/// rule as Solana. Omit it for a verify-only rail.
#[cfg(feature = "stellar")]
#[uniffi::export]
impl PatalaRail {
    #[uniffi::constructor]
    pub fn new_stellar(
        horizon_url: String,
        network: String,
        usdc_issuer: Option<String>,
        keypair_seed: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, PatalaError> {
        let cfg = match network.as_str() {
            "testnet" => {
                let issuer = usdc_issuer.ok_or_else(|| PatalaError::InvalidRequest {
                    message:
                        "stellar network \"testnet\" requires usdc_issuer (the testnet issuer rotates and has no fixed default)"
                            .to_string(),
                })?;
                StellarConfig::testnet(issuer)
            }
            "public" | "mainnet" => StellarConfig::public(),
            other => {
                return Err(PatalaError::InvalidRequest {
                    message: format!(
                        "unknown stellar network {other:?}; use \"testnet\" or \"public\""
                    ),
                })
            }
        };
        let rpc: Arc<dyn patala_stellar::rpc::StellarRpc> = Arc::new(HorizonRpc::new(horizon_url));
        let mut rail = StellarRail::new(cfg, rpc);
        if let Some(seed) = keypair_seed {
            rail = rail.with_signer(StellarKeypair::from_seed(seed32(&seed, "stellar")?));
        }
        Ok(Arc::new(Self {
            inner: Arc::new(rail),
        }))
    }
}

/// Build a rail backed by the real [`patala_hyperswitch::HyperswitchRail`]
/// (`--features hyperswitch`; PATALA.md §4 — **UNVERIFIED AGAINST LIVE**, see
/// `patala-hyperswitch`'s own README). `CustodialReversible` — one HTTP
/// client to a **self-hosted** Hyperswitch instance, presenting its whole
/// processor set (Stripe/Paystack/Xendit/...) as a single rail; this crate
/// never talks to a processor directly (`PATALA.md` §2, §4). `base_url`/
/// `api_key` are required (no hardcoded endpoint — same invariant
/// `HyperswitchConfig` itself enforces); `connector` pins one
/// Hyperswitch-configured processor by name (e.g. `"paystack"`), `None` lets
/// Hyperswitch's own merchant-account routing decide.
#[cfg(feature = "hyperswitch")]
#[uniffi::export]
impl PatalaRail {
    #[allow(clippy::too_many_arguments)]
    #[uniffi::constructor]
    pub fn new_hyperswitch(
        base_url: String,
        api_key: String,
        connector: Option<String>,
        webhook_secret: Option<String>,
        requires_kyc: bool,
        currencies: Vec<String>,
        settlement_days: u8,
        timeout_secs: u64,
    ) -> Result<Arc<Self>, PatalaError> {
        let config = HyperswitchConfig {
            base_url,
            api_key,
            connector,
            webhook_secret,
            requires_kyc,
            currencies,
            settlement_days,
            timeout_secs,
        };
        let rail = HyperswitchRail::new(config).map_err(PatalaError::from)?;
        Ok(Arc::new(Self {
            inner: Arc::new(rail),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(amount: u64, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: "USDC".into(),
            destination: "dest-anything".into(),
            reference: reference.into(),
        }
    }

    #[test]
    fn charge_then_verify_round_trips_through_the_ffi_types() {
        let rail = PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            false,
        );

        assert_eq!(rail.id(), "mock");
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::NonCustodialFinal);
        assert!(!caps.holds_funds);
        assert_eq!(caps.currencies, vec!["USDC".to_string()]);

        let quote = rail.quote(req(500, "order-py-1")).expect("quote");
        assert_eq!(quote.total_minor, 500);

        let receipt = rail.charge(req(500, "order-py-1")).expect("charge");
        assert_eq!(receipt.amount_minor, 500);

        assert!(
            rail.verify(receipt).expect("verify"),
            "a genuine receipt must verify through the FFI boundary"
        );
    }

    #[test]
    fn tampered_receipt_fails_closed_through_the_ffi_types() {
        let rail = PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            false,
        );
        let mut receipt = rail.charge(req(500, "order-py-2")).expect("charge");
        receipt.amount_minor = 999_999;
        assert!(
            !rail.verify(receipt).expect("verify"),
            "a tampered receipt must never verify, even through FFI"
        );
    }

    #[test]
    fn unsupported_currency_is_reported_as_invalid_request() {
        let rail = PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            false,
        );
        let err = rail
            .charge(PayRequest {
                amount_minor: 100,
                currency: "EUR".into(),
                destination: "dest".into(),
                reference: "order-py-3".into(),
            })
            .expect_err("EUR is not a supported currency on this mock rail");
        assert!(matches!(err, PatalaError::InvalidRequest { .. }));
    }

    #[test]
    fn failing_rail_is_reported_as_a_rail_error() {
        let rail = PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            true,
        );
        let err = rail
            .charge(req(100, "order-py-4"))
            .expect_err("this rail is configured to always fail");
        assert!(matches!(err, PatalaError::Rail { .. }));
    }

    // The three tests below exercise the real-rail constructors added for
    // TASK 1 (patala-py exposing SolanaRail/StellarRail/HyperswitchRail, not
    // just MockRail). They only run when the matching feature is enabled
    // (`cargo test -p patala-py --features solana,stellar,hyperswitch`) and
    // are entirely offline: constructing a rail never dials the network —
    // only `quote`/`charge`/`verify` would, and none of those are called
    // here. They prove the capability/class model (`RailClass`,
    // `RailCapabilities`) is reachable through `PatalaRail` for a *real*
    // rail, not just `MockRail`.

    #[cfg(feature = "solana")]
    #[test]
    fn new_solana_builds_offline_and_reports_non_custodial_final() {
        let rail = PatalaRail::new_solana(
            "http://127.0.0.1:1".into(), // never dialed by construction alone
            "devnet".into(),
            None,
        )
        .expect("constructing a SolanaRail must not require network access");
        assert_eq!(rail.id(), "solana");
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::NonCustodialFinal);
        assert!(
            !caps.holds_funds,
            "a wallet-to-wallet rail never custodies funds"
        );
        assert_eq!(caps.currencies, vec!["USDC".to_string()]);
    }

    #[cfg(feature = "solana")]
    #[test]
    fn new_solana_rejects_unknown_cluster() {
        let result =
            PatalaRail::new_solana("http://127.0.0.1:1".into(), "totally-bogus".into(), None);
        match result {
            Err(PatalaError::InvalidRequest { .. }) => {}
            _ => panic!("an unknown cluster name must be refused, never silently defaulted"),
        }
    }

    #[cfg(feature = "solana")]
    #[test]
    fn new_solana_rejects_wrong_length_seed() {
        let result = PatalaRail::new_solana(
            "http://127.0.0.1:1".into(),
            "devnet".into(),
            Some(vec![0u8; 4]),
        );
        match result {
            Err(PatalaError::InvalidRequest { .. }) => {}
            _ => panic!("a non-32-byte seed must be refused, never truncated/padded"),
        }
    }

    #[cfg(feature = "stellar")]
    #[test]
    fn new_stellar_builds_offline_and_reports_non_custodial_final() {
        let rail = PatalaRail::new_stellar(
            "http://127.0.0.1:1".into(),
            "testnet".into(),
            Some("GATESTISSUERPLACEHOLDER00000000000000000000000000000000".into()),
            None,
        )
        .expect("constructing a StellarRail must not require network access");
        assert_eq!(rail.id(), "stellar");
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::NonCustodialFinal);
        assert!(!caps.holds_funds);
        assert_eq!(caps.currencies, vec!["USDC".to_string()]);
    }

    #[cfg(feature = "stellar")]
    #[test]
    fn new_stellar_testnet_requires_usdc_issuer() {
        let result =
            PatalaRail::new_stellar("http://127.0.0.1:1".into(), "testnet".into(), None, None);
        match result {
            Err(PatalaError::InvalidRequest { .. }) => {}
            _ => panic!("testnet has no fixed issuer, so omitting it must be a hard error"),
        }
    }

    #[cfg(feature = "stellar")]
    #[test]
    fn new_stellar_public_network_does_not_need_usdc_issuer() {
        let rail =
            PatalaRail::new_stellar("http://127.0.0.1:1".into(), "public".into(), None, None)
                .expect("public network uses the well-known Circle mainnet issuer");
        assert_eq!(rail.capabilities().class, RailClass::NonCustodialFinal);
    }

    #[cfg(feature = "hyperswitch")]
    #[test]
    fn new_hyperswitch_builds_offline_and_reports_custodial_reversible() {
        let rail = PatalaRail::new_hyperswitch(
            "https://hyperswitch.internal.example.org".into(),
            "snd_test_abc".into(),
            Some("paystack".into()),
            None,
            true,
            vec!["USD".into(), "NGN".into()],
            2,
            30,
        )
        .expect("constructing a HyperswitchRail must not require network access");
        assert_eq!(rail.id(), "hyperswitch");
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(
            caps.holds_funds,
            "the fronted PROCESSOR custodies funds, even though patala itself never does"
        );
        assert_eq!(caps.currencies, vec!["USD".to_string(), "NGN".to_string()]);
    }

    #[cfg(feature = "hyperswitch")]
    #[test]
    fn new_hyperswitch_rejects_empty_base_url() {
        let result = PatalaRail::new_hyperswitch(
            "".into(),
            "snd_test_abc".into(),
            None,
            None,
            true,
            vec!["USD".into()],
            2,
            30,
        );
        match result {
            Err(PatalaError::InvalidRequest { .. }) => {}
            _ => panic!("an empty base_url must be refused, never a silent no-op endpoint"),
        }
    }
}
