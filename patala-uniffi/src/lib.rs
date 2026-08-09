//! # patala-uniffi
//!
//! **The** UniFFI surface over `patala-core` (`PATALA.md` §5: "adapters are
//! written ONCE in Rust; Python and any other language consume that one
//! core"). This crate never reimplements a rail — it wraps whatever
//! `Box<dyn patala_core::PaymentRail>` already exists and exposes it to every
//! UniFFI target language (Python, Go, Swift, Kotlin, ...) through one
//! generated interface.
//!
//! ## Why this crate is not `patala-py`
//!
//! It used to be. The whole `#[uniffi::export]` surface lived in `patala-py`,
//! which was the only cdylib, so UniFFI derived the binding *namespace* from
//! that crate name: every generator emitted a module called `patala_py`.
//! `uniffi-bindgen-go`'s output literally began `package patala_py`, and
//! `patala-go` had to alias its import to read naturally. That was tolerable
//! for one extra language and indefensible for ten — none of this surface is
//! Python-specific, and a Swift or PHP consumer should not import something
//! named after Python.
//!
//! So the surface lives here, and this crate calls
//! `uniffi::setup_scaffolding!("patala")` — an *explicit* namespace rather
//! than one derived from whichever crate happened to be built first.
//! `patala-py` and `patala-go` are now both consumers of this crate:
//! `patala-py` links it into the cdylib the Python wheel ships, `patala-go`
//! generates Go from this crate's own cdylib. Adding an eleventh language
//! adds a directory, never another copy of these type definitions.
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
//! ## Languages UniFFI does not reach
//!
//! UniFFI has no C, C++, Node/Deno/Bun, PHP or Elixir backend. Those load
//! `patala-ffi` instead — a plain `extern "C"` cdylib in this same workspace,
//! JSON in and JSON out, built on the same `patala_core::PaymentRail` trait
//! objects this crate wraps. Neither surface is a reimplementation of the
//! other; both are thin skins over the one core.
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
//!
//! ## Naming
//!
//! `uniffi::setup_scaffolding!("patala")` below is the whole reason this
//! crate is separate — see "Why this crate is not `patala-py`" above. The
//! namespace is spelled out rather than derived, so it does not silently
//! change if this crate is ever renamed.

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
pub mod fiat;

#[cfg(feature = "fiat")]
pub use fiat::patala_fiat_providers;

uniffi::setup_scaffolding!("patala");

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
            .expect("patala-uniffi: failed to start the internal tokio runtime")
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
    /// Mirrors [`patala_core::RailCapabilities::atomic_multi_party`] — see
    /// that field's docs. `false` for every fiat processor, structurally;
    /// `false` for every crypto rail exposed through this binding today,
    /// because none has an atomic multi-party operation wired up yet (B3,
    /// `docs/shared-economics.md` §5).
    pub atomic_multi_party: bool,
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
            atomic_multi_party: c.atomic_multi_party,
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

/// Mirrors [`patala_core::WebhookDelivery`] — one inbound webhook delivery,
/// as received.
///
/// `raw_body` is bytes, not a string, and must be the **literal** request
/// body: every scheme signs what was actually sent, so a body that has been
/// through a JSON round-trip on the caller's side is no longer the thing the
/// processor signed. Header names are matched case-insensitively, so a caller
/// can forward whatever casing its own HTTP stack produced.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WebhookDelivery {
    pub raw_body: Vec<u8>,
    pub headers: std::collections::HashMap<String, String>,
    /// Query-string parameters. Only schemes that put their secret in the
    /// URL rather than a header (LNbits) read this; pass an empty map
    /// otherwise.
    #[uniffi(default = None)]
    pub query: Option<std::collections::HashMap<String, String>>,
    /// Unix seconds to check replay windows against. Pass the current time;
    /// it is an explicit parameter, not a system-clock read, so a caller can
    /// reproduce a delivery exactly.
    pub now_unix: u64,
}

impl From<WebhookDelivery> for patala_core::WebhookDelivery {
    fn from(d: WebhookDelivery) -> Self {
        let core =
            patala_core::WebhookDelivery::new(d.raw_body, d.now_unix).with_headers(d.headers);
        match d.query {
            Some(q) => core.with_query(q),
            None => core,
        }
    }
}

/// Mirrors [`patala_core::WebhookStatus`]. Three states, never a bool — see
/// the core type's docs for why "the rail says this did not settle" and "the
/// rail cannot say" must not collapse.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebhookStatus {
    Settled,
    NotSettled,
    Unconfirmed,
}

impl From<patala_core::WebhookStatus> for WebhookStatus {
    fn from(s: patala_core::WebhookStatus) -> Self {
        match s {
            patala_core::WebhookStatus::Settled => WebhookStatus::Settled,
            patala_core::WebhookStatus::NotSettled => WebhookStatus::NotSettled,
            patala_core::WebhookStatus::Unconfirmed => WebhookStatus::Unconfirmed,
        }
    }
}

/// Mirrors [`patala_core::WebhookEvent`]. Receiving one at all means the rail
/// authenticated the delivery; `status` is what the delivery then claims.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WebhookEvent {
    pub rail_id: String,
    pub event_id: String,
    pub reference: String,
    pub object_id: String,
    pub status: WebhookStatus,
    pub amount_minor: u64,
    pub currency: String,
}

impl From<patala_core::WebhookEvent> for WebhookEvent {
    fn from(e: patala_core::WebhookEvent) -> Self {
        Self {
            rail_id: e.rail_id,
            event_id: e.event_id,
            reference: e.reference,
            object_id: e.object_id,
            status: e.status.into(),
            amount_minor: e.amount_minor,
            currency: e.currency,
        }
    }
}

/// Mirrors [`patala_core::DestinationStatus`] — all five variants, one for one.
///
/// **Never flattened to a bool at this boundary**, for the same reason
/// [`WebhookStatus`] is not and [`RailClass`] is not: a caller has to render
/// each of these differently. "you mistyped that", "that is a Stellar address
/// and this is a Solana payout", "that is a contract, not a wallet", "this
/// looks well-formed — now confirm you control it" and "this rail cannot tell
/// you anything" are five different things to say to a person, and a binding
/// that collapsed them would leave every non-Rust consumer unable to say any
/// of them.
///
/// **No variant means "safe to send to."** See
/// [`DestinationVerdict::human_must_confirm`] and
/// [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestinationStatus {
    /// Structurally invalid for this rail — wrong alphabet, length, checksum,
    /// or empty. A refusal: do not charge to it.
    Malformed,
    /// Well-formed, but for a different network than this rail pays on. A
    /// refusal: the money would land on a chain nobody is watching.
    WrongNetwork,
    /// Valid on this network but not a plain wallet — a program/contract
    /// account, a Solana PDA, a token mint. A refusal: nobody holds a key for
    /// it.
    NotAWallet,
    /// Every offline check this rail can make passed. **Not "valid", not
    /// "safe"** — the absence of a decidable defect. A human still confirms.
    StructurallyValid,
    /// This rail cannot check this destination offline and says so rather than
    /// guessing — the honest answer for a fiat rail, whose destination is an
    /// opaque processor-side token. **Never treat this as valid.**
    Unknown,
}

impl From<patala_core::DestinationStatus> for DestinationStatus {
    fn from(s: patala_core::DestinationStatus) -> Self {
        // Exhaustive, not a catch-all: a status added to the core enum must be
        // mapped here deliberately rather than silently folding into one of
        // these five at the FFI boundary.
        match s {
            patala_core::DestinationStatus::Malformed => DestinationStatus::Malformed,
            patala_core::DestinationStatus::WrongNetwork => DestinationStatus::WrongNetwork,
            patala_core::DestinationStatus::NotAWallet => DestinationStatus::NotAWallet,
            patala_core::DestinationStatus::StructurallyValid => {
                DestinationStatus::StructurallyValid
            }
            patala_core::DestinationStatus::Unknown => DestinationStatus::Unknown,
        }
    }
}

/// Mirrors [`patala_core::DestinationVerdict`] — what one rail could decide
/// about one address, offline, plus what it could not.
///
/// The two boolean fields are the reason this is a Record with six fields and
/// not just `(status, reason)`. `patala_core::DestinationVerdict` carries
/// `is_refusal()` and `requires_human_confirmation()` as **Rust methods**, and
/// a UniFFI record has no methods — so on the far side of this boundary they
/// would not exist at all. Re-deriving them in each consuming language from
/// `status` is exactly the wrong answer: a `switch` in Go or Python that has
/// not heard of a status added later falls through to its default, and the
/// default a caller writes is "not a refusal". So both cross as data, computed
/// on the Rust side by the core type itself.
#[derive(uniffi::Record, Clone, Debug)]
pub struct DestinationVerdict {
    /// The rail that formed this verdict (matches [`PatalaRail::id`]). A
    /// verdict is only ever about the network *that* rail pays on.
    pub rail_id: String,
    /// What was established. See [`DestinationStatus`].
    pub status: DestinationStatus,
    /// Why, in one sentence, for a person to read. Never empty.
    pub reason: String,
    /// `true` for **every** status, including
    /// [`DestinationStatus::StructurallyValid`]. There is no verdict this
    /// binding can produce that lets a caller skip asking a human — see
    /// [`Self::exchange_deposit_caveat`].
    pub human_must_confirm: bool,
    /// [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`], verbatim, on every verdict:
    /// the sentence a UI shows a person next to [`Self::reason`]. It travels
    /// as data because the consumers that most need it — a Go merchant
    /// backend, a Python script, a Swift app — cannot read Rust doc comments.
    pub exchange_deposit_caveat: String,
    /// `true` when this rail positively established a defect
    /// ([`DestinationStatus::Malformed`], [`DestinationStatus::WrongNetwork`],
    /// [`DestinationStatus::NotAWallet`]). Computed by
    /// [`patala_core::DestinationVerdict::is_refusal`], never re-derived here.
    ///
    /// **Guards fail closed**: do not charge to a destination whose verdict is
    /// a refusal, and do not offer a human the option to confirm it anyway.
    /// `false` does *not* mean "go ahead" — it is also `false` for
    /// [`DestinationStatus::Unknown`], where nothing was established.
    pub is_refusal: bool,
}

impl From<patala_core::DestinationVerdict> for DestinationVerdict {
    fn from(v: patala_core::DestinationVerdict) -> Self {
        Self {
            status: v.status.into(),
            // Read from the core type's own methods rather than recomputed
            // from `status` — see the struct docs.
            is_refusal: v.is_refusal(),
            human_must_confirm: v.human_must_confirm,
            rail_id: v.rail_id,
            reason: v.reason,
            exchange_deposit_caveat: v.exchange_deposit_caveat,
        }
    }
}

/// [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`] — the one thing no rail can decide
/// offline, in a sentence a UI can show verbatim.
///
/// Every [`DestinationVerdict`] already carries this string, so a caller
/// rendering a verdict does not need this function. It exists for the caller
/// that wants the wording *before* there is a verdict to render — on the form
/// where a customer is first asked for a payout address, which is the moment
/// the warning is most useful.
#[uniffi::export]
pub fn exchange_deposit_caveat() -> String {
    patala_core::EXCHANGE_DEPOSIT_CAVEAT.to_string()
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

// A plain Rust impl block, deliberately NOT `#[uniffi::export]`ed: nothing
// here crosses a UniFFI boundary or appears in any generated binding.
impl PatalaRail {
    /// The `Arc<dyn patala_core::PaymentRail>` this object wraps.
    ///
    /// This exists for **`patala-ffi`**, the plain C-ABI cdylib in this
    /// workspace, and for no other reason. UniFFI has no C, C++, Node, PHP or
    /// Elixir backend, so those languages load a hand-written `extern "C"`
    /// library instead — and that library needs to build rails from the same
    /// inputs this crate already knows how to build them from. Every
    /// constructor above (`new_mock`, `new_mock_without_destination_checks`,
    /// `new_solana`, `new_stellar`, `new_hyperswitch`, `new_fiat`) is ordinary
    /// Rust; only the `#[uniffi::constructor]` attribute makes it *also* a
    /// binding entry point. So `patala-ffi` reuses them through this accessor
    /// rather than carrying a second, drifting copy of twenty processor
    /// adapters' worth of config mapping (`fiat.rs` alone is ~1000 lines).
    ///
    /// It returns the trait object, never a concrete rail, so no caller can
    /// reach a provider-specific type through it (`PATALA.md` §3).
    pub fn as_payment_rail(&self) -> Arc<dyn PaymentRail> {
        Arc::clone(&self.inner)
    }
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

    /// A [`patala_core::MockRail`] that reports
    /// [`DestinationStatus::Unknown`] for every destination — the offline
    /// stand-in for a rail that cannot check an address at all.
    ///
    /// This exists so `Unknown` is reachable from **outside Rust in the
    /// default build**, and it is not a convenience. `Unknown` is the trait's
    /// default verdict and the honest answer for every fiat rail, whose
    /// `destination` is an opaque processor-side token; it is also the verdict
    /// a consumer is most likely to get wrong, because the safe handling of it
    /// ("a human must decide") looks nothing like the handling of
    /// [`DestinationStatus::StructurallyValid`] and is easy to collapse into
    /// "not a refusal, therefore fine".
    ///
    /// Without this constructor, a Go, Python, Swift or Kotlin consumer could
    /// only produce an `Unknown` verdict by compiling in a feature-gated real
    /// rail — so the branch of its payout UI that matters most could not be
    /// tested in the offline default build at all. Same reasoning as
    /// [`patala_core::MockRail::without_destination_checks`], which this wraps.
    ///
    /// Everything else about the returned rail matches [`Self::new_mock`]:
    /// `quote`/`charge`/`verify` behave identically. Only the destination
    /// verdict changes.
    #[uniffi::constructor]
    pub fn new_mock_without_destination_checks(
        id: String,
        class: RailClass,
        currencies: Vec<String>,
        fee_minor: u64,
        failing: bool,
    ) -> Arc<Self> {
        let mut rail = MockRail::new(id, class.into(), currencies)
            .with_fee_minor(fee_minor)
            .without_destination_checks();
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

    /// Authenticate an inbound webhook delivery from this rail's processor —
    /// the push counterpart to [`Self::verify`], and the reason this method
    /// exists at all: webhook verification used to live only in
    /// provider-specific free functions, which a binding cannot reach, so a
    /// consumer on this side of the FFI could confirm a payment **only** by
    /// polling `verify`.
    ///
    /// Fails closed: a missing, malformed, stale or mismatched signature
    /// raises, and a rail whose processor has no push delivery (the mock,
    /// `manual`) raises `Unsupported`. A returned [`WebhookEvent`] means the
    /// rail is satisfied the delivery genuinely came from its processor;
    /// gate entitlement on `status == Settled`, and reconcile
    /// `amount_minor`/`currency` against your own stored order first.
    pub fn verify_webhook(&self, delivery: WebhookDelivery) -> Result<WebhookEvent, PatalaError> {
        let core_delivery: patala_core::WebhookDelivery = delivery.into();
        runtime()
            .block_on(self.inner.verify_webhook(&core_delivery))
            .map(WebhookEvent::from)
            .map_err(PatalaError::from)
    }

    /// Check a payout destination as far as this rail can, **offline**, before
    /// any money moves — see
    /// [`patala_core::PaymentRail::validate_destination`].
    ///
    /// This is the pre-flight half of the two-party payout flow: on a
    /// `NonCustodialFinal` rail there is no reversal, so giving a customer
    /// their money back is a second, independent [`Self::charge`] to an
    /// address **the customer supplies** — never the address the payment came
    /// from, which is very often an exchange withdrawal address where the
    /// funds cannot be credited back to them. This call is what lets a
    /// consumer tell a person "that is not a valid Solana address" at the
    /// moment they type it rather than at charge time.
    ///
    /// # Three things a caller must not mistake
    ///
    /// - **It returns a [`DestinationVerdict`], never an error.** "I could not
    ///   check" is [`DestinationStatus::Unknown`], a verdict, because a caller
    ///   has to handle it as carefully as a refusal — raising there would let
    ///   a `try`/`except` swallow it.
    /// - **No verdict means "safe to send to."** `human_must_confirm` is
    ///   `true` on every one of them, including
    ///   [`DestinationStatus::StructurallyValid`]. patala does not detect
    ///   exchange-owned addresses and will not guess: that needs commercial
    ///   address-attribution data this workspace refuses to depend on, and a
    ///   heuristic would be worse than nothing. Show
    ///   `exchange_deposit_caveat` and make a human tick the box.
    /// - **`is_refusal` is not a warning to click past.** Those three statuses
    ///   are defects the rail *knows about*; stop there.
    ///
    /// Unlike every other method on this object, this one is genuinely pure:
    /// no network, no clock, no filesystem, and it does not touch the internal
    /// tokio runtime at all, so it is safe to call from a UI thread on every
    /// keystroke.
    pub fn validate_destination(&self, destination: String) -> DestinationVerdict {
        DestinationVerdict::from(self.inner.validate_destination(&destination))
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
    fn verify_webhook_on_a_rail_without_push_delivery_is_unsupported_not_ok() {
        // The mock has no processor and so no push delivery. It must raise
        // Unsupported across the FFI boundary -- never return an event a
        // caller could mistake for an authenticated delivery.
        let rail = PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            false,
        );
        let err = rail
            .verify_webhook(WebhookDelivery {
                raw_body: b"{}".to_vec(),
                headers: std::collections::HashMap::new(),
                query: None,
                now_unix: 1_700_000_000,
            })
            .expect_err("mock has no webhook surface");
        assert!(matches!(err, PatalaError::Unsupported { .. }));
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

    // ── validate_destination across the FFI boundary ──────────────────────
    //
    // MockRail's synthetic `<network>:<kind>:<label>` grammar is what makes
    // every DestinationStatus reachable with no chain and no feature flags, so
    // the *binding* for each variant is tested in the default build rather
    // than only when a real rail is compiled in.

    fn mock_rail() -> Arc<PatalaRail> {
        PatalaRail::new_mock(
            "mock".into(),
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
            0,
            false,
        )
    }

    #[test]
    fn every_destination_status_survives_the_ffi_boundary_distinctly() {
        // The whole point of task (a): a verdict that flattened to a bool here
        // would defeat the design. Five inputs, five *different* statuses.
        let rail = mock_rail();
        let cases = [
            ("mock:wallet:alice", DestinationStatus::StructurallyValid),
            ("mock:program:vault", DestinationStatus::NotAWallet),
            ("stellar:wallet:alice", DestinationStatus::WrongNetwork),
            ("definitely-not-an-address", DestinationStatus::Malformed),
            ("", DestinationStatus::Malformed),
        ];
        for (dest, want) in cases {
            let v = rail.validate_destination(dest.to_string());
            assert_eq!(v.status, want, "validate_destination({dest:?})");
        }

        // Unknown needs a rail that declines to check at all — the fiat shape.
        // Built through the exported constructor, not by hand, so this covers
        // the path a non-Rust consumer actually has.
        let opaque = PatalaRail::new_mock_without_destination_checks(
            "opaque".into(),
            RailClass::CustodialReversible,
            vec!["USD".into()],
            0,
            false,
        );
        assert_eq!(
            opaque
                .validate_destination("cus_opaque_processor_token".into())
                .status,
            DestinationStatus::Unknown
        );
        assert!(
            !opaque
                .validate_destination("cus_opaque_processor_token".into())
                .is_refusal,
            "Unknown is not a refusal — and is not a green light either"
        );

        // And the five are distinct values on this side, not aliases that
        // happen to print differently.
        let all = [
            DestinationStatus::Malformed,
            DestinationStatus::WrongNetwork,
            DestinationStatus::NotAWallet,
            DestinationStatus::StructurallyValid,
            DestinationStatus::Unknown,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two DestinationStatus variants compare equal");
            }
        }
    }

    #[test]
    fn every_verdict_carries_the_caveat_and_the_confirmation_flag_across_ffi() {
        // These two fields exist *because* they have to survive this boundary:
        // the core type's `requires_human_confirmation()` is a Rust method and
        // does not exist in Python, Go, Swift or Kotlin.
        let rail = mock_rail();
        for dest in [
            "mock:wallet:alice",
            "mock:program:vault",
            "stellar:wallet:alice",
            "junk",
            "",
        ] {
            let v = rail.validate_destination(dest.to_string());
            assert!(
                v.human_must_confirm,
                "{dest:?} must still require a human to confirm"
            );
            assert_eq!(
                v.exchange_deposit_caveat,
                patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                "{dest:?} must carry the caveat verbatim"
            );
            assert!(
                !v.reason.trim().is_empty(),
                "{dest:?} needs a reason a UI can show"
            );
            assert_eq!(v.rail_id, "mock", "a verdict names whose opinion it is");
        }
    }

    #[test]
    fn is_refusal_crosses_as_data_and_matches_the_core_types_own_answer() {
        // A consumer must never have to re-derive this from `status`: a `switch`
        // that has not heard of a status added later defaults to "not a
        // refusal", which fails OPEN. So it is computed in Rust and compared
        // here against the core method it is computed from.
        let rail = mock_rail();
        let core = MockRail::new(
            "mock",
            CoreRailClass::NonCustodialFinal,
            vec!["USDC".into()],
        );
        for dest in [
            "mock:wallet:alice",
            "mock:program:vault",
            "stellar:wallet:alice",
            "junk",
            "",
        ] {
            let ffi = rail.validate_destination(dest.to_string());
            assert_eq!(
                ffi.is_refusal,
                core.validate_destination(dest).is_refusal(),
                "is_refusal disagrees with patala_core for {dest:?}"
            );
        }

        // Spelled out, so the mapping is pinned and not merely self-consistent.
        assert!(rail.validate_destination("junk".into()).is_refusal);
        assert!(
            rail.validate_destination("mock:program:vault".into())
                .is_refusal
        );
        assert!(
            rail.validate_destination("stellar:wallet:alice".into())
                .is_refusal
        );
        assert!(
            !rail
                .validate_destination("mock:wallet:alice".into())
                .is_refusal
        );
    }

    #[test]
    fn validate_destination_is_pure_across_the_boundary() {
        // The contract that lets this be called on a UI thread on every
        // keystroke, in a browser, and on a gate device with no uplink.
        let rail = mock_rail();
        let once = rail.validate_destination("mock:wallet:alice".into());
        let twice = rail.validate_destination("mock:wallet:alice".into());
        assert_eq!(once.status, twice.status);
        assert_eq!(once.reason, twice.reason);
        assert_eq!(once.is_refusal, twice.is_refusal);
    }

    #[test]
    fn the_caveat_is_reachable_before_there_is_a_verdict_to_render() {
        // The free function exists for the form where a customer is first asked
        // for an address — the moment the warning matters most.
        assert_eq!(
            exchange_deposit_caveat(),
            patala_core::EXCHANGE_DEPOSIT_CAVEAT
        );
        assert!(exchange_deposit_caveat().contains("exchange"));
    }

    #[test]
    fn refund_is_still_unreachable_here_and_that_is_deliberate() {
        // Audited, not changed. `PatalaRail` exposes no `refund` method, and
        // this asserts the reason is honest rather than an oversight: the
        // underlying MockRail is NonCustodialFinal and its `refund` is
        // Unsupported, so a binding method would only ever raise. Paying a
        // customer back on such a rail is a compensating `charge` to a
        // validated, customer-supplied destination — see
        // `validate_destination`'s docs and docs/compensating-payments.md.
        let rail = MockRail::new(
            "mock",
            CoreRailClass::NonCustodialFinal,
            vec!["USDC".into()],
        );
        runtime().block_on(async {
            let receipt = rail
                .charge(&CorePayRequest {
                    amount_minor: 500,
                    currency: "USDC".into(),
                    destination: "dest".into(),
                    reference: "order-py-refund".into(),
                })
                .await
                .expect("charge");
            assert!(matches!(
                rail.refund(&receipt).await,
                Err(CoreError::Unsupported("refund"))
            ));
        });
    }

    // The three tests below exercise the real-rail constructors added for
    // TASK 1 (this crate exposing SolanaRail/StellarRail/HyperswitchRail, not
    // just MockRail). They only run when the matching feature is enabled
    // (`cargo test -p patala-uniffi --features solana,stellar,hyperswitch`) and
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
