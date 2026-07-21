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
//! [`runtime`]). This is the same trade patala-sidecar makes in the other
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

use std::sync::{Arc, OnceLock};

use patala_core::{
    Error as CoreError, MockRail, PayRequest as CorePayRequest, PaymentRail, Quote as CoreQuote,
    RailCapabilities as CoreRailCapabilities, RailClass as CoreRailClass, Receipt as CoreReceipt,
    Settlement as CoreSettlement,
};

uniffi::setup_scaffolding!();

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
}
