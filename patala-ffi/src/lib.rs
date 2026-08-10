//! # patala-ffi
//!
//! A plain `extern "C"` shared library over `patala-core`. JSON in, JSON out,
//! `uint64` handles, five exported symbols. No UniFFI, no code generation, no
//! per-language runtime.
//!
//! ## Who this is for
//!
//! `patala-uniffi` covers the languages UniFFI has a backend for — Python, Go,
//! Swift, Kotlin, Ruby. It has no backend for **C, C++, Node/Deno/Bun, PHP or
//! Elixir**, and those are not small constituencies. Each of them can load a
//! C shared library: `dlopen`, `ffi-napi`/`bun:ffi`/`node-api`, FFI/Zend
//! extensions, `:erlang.load_nif` or a Port. So they load this, and get the
//! same rails through the same `patala_core::PaymentRail` trait objects.
//!
//! It is not a competitor to the UniFFI surface and does not reimplement it:
//! `patala_new`'s fiat and real-rail configurations are built by
//! `patala-uniffi`'s own constructors (see `Cargo.toml` for why that is a
//! feature-gated, optional dependency and not a mandatory one).
//!
//! ## No runtime in the host process
//!
//! This is the property that makes patala's C ABI different from the other two
//! products in this suite, and it is worth stating precisely rather than
//! vaguely.
//!
//! `llmux` and `openrate` are Go, so their C ABIs are produced with
//! `go build -buildmode=c-shared`. That puts the **Go runtime** inside the
//! host process: a garbage collector, a preemptive goroutine scheduler, and
//! Go's own `SIGSEGV`/`SIGPROF` handlers, installed at load time. It is not
//! fork-safe — after `fork()` without `exec()` the runtime in the child is
//! broken, which breaks Python `multiprocessing` with the default `fork` start
//! method and pre-fork servers like uWSGI and Unicorn.
//!
//! patala is Rust. Loading `libpatala_ffi` runs no initialiser of ours, starts
//! no thread, installs no signal handler, and registers no `atfork` hook.
//! There is no GC and no scheduler. The library is inert until called and
//! holds nothing between calls except the handles you opened.
//!
//! The one honest caveat, stated because it is the only thing in the
//! neighbourhood of a runtime: `patala_core::PaymentRail`'s methods are
//! `async fn`, so each handle owns a **current-thread** `tokio::runtime`
//! (a private field of [`Instance`]) created in `patala_new`. A current-thread runtime
//! spawns no threads: it drives futures on whichever thread called
//! `block_on`, and is dropped with the handle. `patala_close` on the last
//! handle leaves nothing of ours running in the process.
//!
//! Do not copy the Go-runtime caveats out of the other products' docs into
//! this one. They are true there and false here.
//!
//! ## Threads
//!
//! A handle is safe to use from several threads at once. Calls on **one**
//! handle serialise, because that handle's runtime is behind a mutex; calls on
//! different handles run concurrently. Opening and closing handles from any
//! thread is safe. Nothing in this library is `thread_local`.
//!
//! ## The wire format is the sidecar's
//!
//! Every request and response is the same JSON `patala-sidecar` already
//! serves, built from the same `patala-core` types. A body that works against
//! `POST /v1/rails/:id/charge` works against `patala_call(h, "charge", …)`
//! unchanged, and the response is what that endpoint returns. That is
//! deliberate: it reuses a wire contract that already has round-trip tests
//! over a real socket, and it keeps the surface small enough to bind from a
//! dozen languages.
//!
//! ## Layering
//!
//! This file is ordinary Rust and holds everything worth testing: the handle
//! registry, the rail configuration, the method dispatch. [`mod@crate::abi`]
//! is the `extern "C"` shell — pointer marshalling, `CString` ownership,
//! panic containment — and nothing else. `ctest/smoke.c` is what proves the
//! shell, by `dlopen`ing the real artifact and calling it through
//! `include/patala.h`; no Rust test can see a missing `#[no_mangle]` or a
//! header that has drifted from the library.

pub mod abi;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use patala_core::{
    DestinationVerdict, MockRail, PayRequest, PaymentRail, RailClass, Receipt, WebhookDelivery,
};

/// The version this library reports through `patala_abi_version`.
///
/// Pinned to the workspace `VERSION` file by this crate's own
/// `abi_version_matches_the_workspace_version` test (a `#[cfg(test)]` item,
/// hence not a doc link), because the whole point of the probe is that a host can compare the loaded
/// library against the version it generated its bindings for. A shared library
/// is resolved off a load path the host may not control; without the probe a
/// stale `libpatala_ffi` earlier on that path is called silently.
pub const ABI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every method [`call`] accepts. A closed set of short strings rather than
/// one C function per operation, so `include/patala.h` stays stable as patala
/// grows methods.
pub const METHODS: &[&str] = &[
    "id",
    "capabilities",
    "quote",
    "charge",
    "verify",
    "validate-destination",
    "webhook",
    "caveat",
    "providers",
];

// ---------------------------------------------------------------------------
// Handle registry
// ---------------------------------------------------------------------------

/// One live rail behind one handle.
pub struct Instance {
    rail: Arc<dyn PaymentRail>,
    /// A **current-thread** tokio runtime: it spawns no threads and drives
    /// futures on the caller's own thread inside `block_on`. See the crate
    /// docs' "No runtime in the host process".
    ///
    /// Behind a mutex because `block_on` needs `&mut`-style exclusivity in
    /// practice and a C host may well call one handle from two threads. The
    /// consequence — calls on one handle serialise — is documented in
    /// `include/patala.h` rather than left for someone to discover under load.
    runtime: Mutex<tokio::runtime::Runtime>,
}

impl Instance {
    fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        // A rail that panicked mid-call poisons this mutex. Recovering the
        // guard is right here: the runtime itself is not left in a broken
        // state by a panicking future, and refusing every subsequent call on
        // the handle would turn one failed charge into a dead rail.
        let rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.block_on(fut)
    }
}

/// Handles are integers in this registry, never pointers. A host calling with
/// a closed or invented handle gets a clean error string; handing a stale
/// `uintptr` back into Rust would be a segfault in the host's process, and the
/// host would blame patala for a crash it could not read.
///
/// Handles are never reused: the counter only increases, so a use-after-close
/// is reported as such rather than silently landing on a different rail that
/// happened to take the slot.
fn registry() -> &'static Mutex<HashMap<u64, Arc<Instance>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, Arc<Instance>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn lookup(handle: u64) -> Result<Arc<Instance>, String> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&handle)
        .cloned()
        .ok_or_else(|| {
            format!("patala: unknown handle {handle} (never created, or already closed)")
        })
}

/// Builds a rail from a JSON configuration document and returns its handle.
///
/// See [`RailConfig`] for the accepted documents. An empty or whitespace-only
/// string means the offline default: a `MockRail` on `USDC`.
pub fn open(config_json: &str) -> Result<u64, String> {
    let config: RailConfig = if config_json.trim().is_empty() {
        RailConfig::default_mock()
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| format!("patala: invalid configuration document: {e}"))?
    };
    let rail = config.build()?;

    // `enable_all()` turns on whichever drivers this build's tokio actually
    // has. In the default (mock-only) build tokio is compiled with just `rt`,
    // so this enables nothing and costs nothing; in a build with a network
    // rail, feature unification with reqwest gives tokio its io and time
    // drivers and this is what makes them available to the rail's own calls.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("patala: could not create the current-thread runtime: {e}"))?;

    let instance = Arc::new(Instance {
        rail,
        runtime: Mutex::new(runtime),
    });
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle, instance);
    Ok(handle)
}

/// Drops the handle. Closing an unknown or already-closed handle is a no-op,
/// so a host's cleanup path can be idempotent and a double close is not a
/// crash.
pub fn close(handle: u64) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&handle);
}

/// How many handles are open, across the whole process.
pub fn live_handles() -> usize {
    registry().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Whether this specific handle is open. Tests use it to prove [`close`]
/// actually removes the entry rather than merely returning — a per-handle
/// question, not a count, because the registry is process-global and Rust
/// tests run in parallel: a total that moved could have moved for someone
/// else's reason.
pub fn handle_is_live(handle: u64) -> bool {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&handle)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// `RailClass` as it is spelled in a configuration document. Mirrors
/// [`patala_core::RailClass`] one for one — never a bool, for the reason that
/// type's own docs give: the class changes the trust contract the payer is
/// shown.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigRailClass {
    CustodialReversible,
    NonCustodialFinal,
}

impl From<ConfigRailClass> for RailClass {
    fn from(c: ConfigRailClass) -> Self {
        match c {
            ConfigRailClass::CustodialReversible => RailClass::CustodialReversible,
            ConfigRailClass::NonCustodialFinal => RailClass::NonCustodialFinal,
        }
    }
}

fn default_currencies() -> Vec<String> {
    vec!["USDC".to_string()]
}

fn default_true() -> bool {
    true
}

fn default_mock_id() -> String {
    "mock".to_string()
}

/// The document `patala_new` takes, tagged by `"rail"`.
///
/// `deny_unknown_fields` is deliberate and fail-closed. A
/// caller who writes `"fee_minor_"` or `"currency"` gets a refusal naming the
/// field, rather than a rail quietly built with a default they did not mean —
/// on a substrate where the defaulted field might be the destination currency.
///
/// `Debug` is written by hand below rather than derived. This one type is
/// where **every** credential the C ABI accepts lands: the whole fiat config
/// map (any of twenty processors' secret keys), a raw Ed25519 seed as hex, a
/// Hyperswitch API key and its webhook secret. A derived `Debug` puts all of
/// them in one line reachable from `{:?}` in any consumer and from a single
/// future `tracing::debug!(?config)` in [`open`].
#[derive(Deserialize)]
#[serde(tag = "rail", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RailConfig {
    /// `{"rail":"mock", "class":"non-custodial-final", "currencies":["USDC"]}`
    ///
    /// The offline default. Deterministic, no network, available in every
    /// build — so a consumer in any language can drive a full
    /// charge → verify round trip before a single credential exists.
    Mock {
        #[serde(default = "default_mock_id")]
        id: String,
        #[serde(default = "non_custodial_final")]
        class: ConfigRailClass,
        #[serde(default = "default_currencies")]
        currencies: Vec<String>,
        #[serde(default)]
        fee_minor: u64,
        /// Make every operation fail, for exercising a consumer's error path.
        #[serde(default)]
        failing: bool,
        /// `false` gives a rail that answers `Unknown` to every destination —
        /// the offline stand-in for a fiat rail, whose destination is an
        /// opaque processor-side token. It exists so the "a human must decide"
        /// branch of a payout UI is reachable in the default build; see
        /// `patala_core::MockRail::without_destination_checks`.
        #[serde(default = "default_true")]
        destination_checks: bool,
    },

    /// `{"rail":"fiat", "provider":"stripe", "config":{"secret_key":"…"}}`
    ///
    /// One of `patala-fiat`'s 20 processor adapters, or the always-on
    /// `manual` rail, by name. `config` keys are that provider's own field
    /// names — identical to the `HashMap<String, String>` the UniFFI
    /// `PatalaRail::new_fiat` takes, because it is literally that call.
    /// Requires `--features fiat` plus the matching `fiat-<provider>`.
    Fiat {
        provider: String,
        #[serde(default)]
        config: HashMap<String, String>,
    },

    /// `{"rail":"solana", "rpc_url":"…", "cluster":"devnet"}`
    ///
    /// `keypair_seed_hex`, if given, is exactly 32 raw Ed25519 seed bytes as
    /// 64 hex characters — JSON has no byte type and a hex string beats a
    /// number array. Omit it for a verify-only rail. Requires
    /// `--features solana`.
    Solana {
        rpc_url: String,
        cluster: String,
        #[serde(default)]
        keypair_seed_hex: Option<String>,
    },

    /// `{"rail":"stellar", "horizon_url":"…", "network":"testnet", "usdc_issuer":"…"}`
    ///
    /// `testnet` requires `usdc_issuer` (the testnet issuer rotates and has no
    /// fixed value); `public`/`mainnet` ignores it. Requires
    /// `--features stellar`.
    Stellar {
        horizon_url: String,
        network: String,
        #[serde(default)]
        usdc_issuer: Option<String>,
        #[serde(default)]
        keypair_seed_hex: Option<String>,
    },

    /// `{"rail":"hyperswitch", "base_url":"…", "api_key":"…"}`
    ///
    /// A self-hosted Hyperswitch instance, presenting its whole processor set
    /// as one rail. Requires `--features hyperswitch`.
    Hyperswitch {
        base_url: String,
        api_key: String,
        #[serde(default)]
        connector: Option<String>,
        #[serde(default)]
        webhook_secret: Option<String>,
        #[serde(default = "default_true")]
        requires_kyc: bool,
        #[serde(default)]
        currencies: Vec<String>,
        #[serde(default = "two")]
        settlement_days: u8,
        #[serde(default = "thirty")]
        timeout_secs: u64,
    },
}

/// Names the rail and the non-secret shape of its configuration, and nothing
/// else. Every credential-bearing field renders as `<redacted>`/`<unset>`, and
/// the fiat `config` map shows its **keys** only — which is what a reader
/// debugging a rejected configuration actually needs, and is exactly the
/// information `patala-uniffi`'s own errors already give (they interpolate
/// the key name, never the value).
impl std::fmt::Debug for RailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redacted(present: bool) -> &'static str {
            if present {
                "<redacted>"
            } else {
                "<unset>"
            }
        }
        match self {
            RailConfig::Mock {
                id,
                class,
                currencies,
                fee_minor,
                failing,
                destination_checks,
            } => f
                .debug_struct("Mock")
                .field("id", id)
                .field("class", class)
                .field("currencies", currencies)
                .field("fee_minor", fee_minor)
                .field("failing", failing)
                .field("destination_checks", destination_checks)
                .finish(),
            RailConfig::Fiat { provider, config } => {
                let mut keys: Vec<&str> = config.keys().map(String::as_str).collect();
                keys.sort_unstable();
                f.debug_struct("Fiat")
                    .field("provider", provider)
                    .field("config_keys", &keys)
                    .finish()
            }
            RailConfig::Solana {
                rpc_url,
                cluster,
                keypair_seed_hex,
            } => f
                .debug_struct("Solana")
                .field("rpc_url", rpc_url)
                .field("cluster", cluster)
                .field("keypair_seed_hex", &redacted(keypair_seed_hex.is_some()))
                .finish(),
            RailConfig::Stellar {
                horizon_url,
                network,
                usdc_issuer,
                keypair_seed_hex,
            } => f
                .debug_struct("Stellar")
                .field("horizon_url", horizon_url)
                .field("network", network)
                .field("usdc_issuer", usdc_issuer)
                .field("keypair_seed_hex", &redacted(keypair_seed_hex.is_some()))
                .finish(),
            RailConfig::Hyperswitch {
                base_url,
                api_key,
                connector,
                webhook_secret,
                requires_kyc,
                currencies,
                settlement_days,
                timeout_secs,
            } => f
                .debug_struct("Hyperswitch")
                .field("base_url", base_url)
                .field("api_key", &redacted(!api_key.is_empty()))
                .field("connector", connector)
                .field("webhook_secret", &redacted(webhook_secret.is_some()))
                .field("requires_kyc", requires_kyc)
                .field("currencies", currencies)
                .field("settlement_days", settlement_days)
                .field("timeout_secs", timeout_secs)
                .finish(),
        }
    }
}

fn non_custodial_final() -> ConfigRailClass {
    ConfigRailClass::NonCustodialFinal
}

fn two() -> u8 {
    2
}

fn thirty() -> u64 {
    30
}

impl RailConfig {
    fn default_mock() -> Self {
        RailConfig::Mock {
            id: default_mock_id(),
            class: non_custodial_final(),
            currencies: default_currencies(),
            fee_minor: 0,
            failing: false,
            destination_checks: true,
        }
    }

    fn build(self) -> Result<Arc<dyn PaymentRail>, String> {
        match self {
            RailConfig::Mock {
                id,
                class,
                currencies,
                fee_minor,
                failing,
                destination_checks,
            } => {
                // Built straight off patala-core, with no UniFFI in the path,
                // so the default cdylib links neither UniFFI nor a network
                // client — see Cargo.toml.
                let mut rail =
                    MockRail::new(id, class.into(), currencies).with_fee_minor(fee_minor);
                if !destination_checks {
                    rail = rail.without_destination_checks();
                }
                if failing {
                    rail = rail.failing();
                }
                Ok(Arc::new(rail))
            }

            #[cfg(feature = "fiat")]
            RailConfig::Fiat { provider, config } => {
                patala_uniffi::PatalaRail::new_fiat(provider, config)
                    .map(|r| r.as_payment_rail())
                    .map_err(|e| format!("patala: {e}"))
            }
            #[cfg(not(feature = "fiat"))]
            RailConfig::Fiat { provider, .. } => Err(not_compiled_in("fiat", &provider)),

            #[cfg(feature = "solana")]
            RailConfig::Solana {
                rpc_url,
                cluster,
                keypair_seed_hex,
            } => {
                let seed = decode_seed(keypair_seed_hex.as_deref(), "solana")?;
                patala_uniffi::PatalaRail::new_solana(rpc_url, cluster, seed)
                    .map(|r| r.as_payment_rail())
                    .map_err(|e| format!("patala: {e}"))
            }
            #[cfg(not(feature = "solana"))]
            RailConfig::Solana { .. } => Err(not_compiled_in("solana", "solana")),

            #[cfg(feature = "stellar")]
            RailConfig::Stellar {
                horizon_url,
                network,
                usdc_issuer,
                keypair_seed_hex,
            } => {
                let seed = decode_seed(keypair_seed_hex.as_deref(), "stellar")?;
                patala_uniffi::PatalaRail::new_stellar(horizon_url, network, usdc_issuer, seed)
                    .map(|r| r.as_payment_rail())
                    .map_err(|e| format!("patala: {e}"))
            }
            #[cfg(not(feature = "stellar"))]
            RailConfig::Stellar { .. } => Err(not_compiled_in("stellar", "stellar")),

            #[cfg(feature = "hyperswitch")]
            RailConfig::Hyperswitch {
                base_url,
                api_key,
                connector,
                webhook_secret,
                requires_kyc,
                currencies,
                settlement_days,
                timeout_secs,
            } => patala_uniffi::PatalaRail::new_hyperswitch(
                base_url,
                api_key,
                connector,
                webhook_secret,
                requires_kyc,
                currencies,
                settlement_days,
                timeout_secs,
            )
            .map(|r| r.as_payment_rail())
            .map_err(|e| format!("patala: {e}")),
            #[cfg(not(feature = "hyperswitch"))]
            RailConfig::Hyperswitch { .. } => Err(not_compiled_in("hyperswitch", "hyperswitch")),
        }
    }
}

/// The refusal a build without the matching Cargo feature gives. It names the
/// feature, because "unknown rail" would send the reader looking for a typo in
/// their own JSON when the real cause is which library they loaded.
#[allow(dead_code)] // every arm that calls it is behind a `not(feature)` cfg
fn not_compiled_in(feature: &str, what: &str) -> String {
    format!(
        "patala: this libpatala_ffi was built without --features {feature}, so the {what:?} rail \
         is not available in it. Rebuild with `cargo build -p patala-ffi --features {feature}` \
         (see patala-ffi/README.md)."
    )
}

/// 64 hex characters -> the 32 raw seed bytes, or a refusal. Never truncates,
/// never pads: a wrong-length key is a wrong key.
#[cfg(any(feature = "solana", feature = "stellar"))]
fn decode_seed(hex: Option<&str>, rail: &str) -> Result<Option<Vec<u8>>, String> {
    match hex {
        None => Ok(None),
        Some(s) => {
            let bytes = decode_hex(s)
                .map_err(|e| format!("patala: {rail} keypair_seed_hex is not valid hex: {e}"))?;
            Ok(Some(bytes))
        }
    }
}

/// Hex -> bytes. Hand-rolled rather than adding a dependency: this is the only
/// place the library needs it, and a `hex` crate in the tree would be one more
/// thing linked into an artifact whose size is a stated feature.
fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(format!("odd number of hex digits ({})", bytes.len()));
    }
    // The offending character is NOT quoted back. The only string that reaches
    // here is a keypair seed, and its error message travels out through
    // `patala_new`'s `*err` into whatever the host logs — so echoing even one
    // character of a private key writes part of it somewhere it was never
    // meant to be, and 64 malformed attempts differing in one position would
    // read it out entirely. The POSITION is what a caller needs to fix a typo,
    // and it discloses nothing.
    let nibble = |c: u8, at: usize| -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!(
                "the character at position {at} is not a hex digit (it is not quoted \
                 back here: this is a private key)"
            )),
        }
    };
    bytes
        .chunks(2)
        .enumerate()
        .map(|(i, pair)| Ok((nibble(pair[0], i * 2)? << 4) | nibble(pair[1], i * 2 + 1)?))
        .collect()
}

// ---------------------------------------------------------------------------
// Request / response bodies that are not already patala-core types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct IdResponse {
    rail_id: String,
}

#[derive(Serialize)]
struct VerifyResponse {
    valid: bool,
}

#[derive(Serialize)]
struct CaveatResponse {
    exchange_deposit_caveat: &'static str,
}

#[cfg(feature = "fiat")]
#[derive(Serialize)]
struct ProvidersResponse {
    providers: Vec<String>,
}

/// The body of `validate-destination`. `deny_unknown_fields` for the same
/// fail-closed reason the sidecar's identical route uses it: a caller who
/// POSTs a whole `PayRequest` here, or misspells `destination`, is told so
/// rather than handed a verdict about a field they did not mean.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateDestinationRequest {
    destination: String,
}

/// Mirrors the sidecar's response: the whole verdict, flattened, plus
/// `is_refusal`.
///
/// `is_refusal` is a **method** on the core type and a method does not survive
/// JSON. Leaving it out would make every consumer re-derive it from `status`
/// in its own language, and a `switch` that has not heard of a status added
/// later falls through to its default — which a caller writes as "not a
/// refusal". That fails OPEN, on the one question that decides whether a
/// customer's money goes to an address the rail already knows is wrong.
#[derive(Serialize)]
struct ValidateDestinationResponse {
    #[serde(flatten)]
    verdict: DestinationVerdict,
    is_refusal: bool,
}

/// The body of `webhook` — one inbound delivery, as received.
///
/// Exactly one of `body` / `body_hex` must be given. Every scheme signs the
/// bytes the processor actually sent, so a body that has been through a JSON
/// round-trip on the caller's side is no longer the thing that was signed:
/// forward it verbatim. `body` is the usual case (every processor in
/// `patala-fiat` sends UTF-8) and preserves the bytes exactly; `body_hex` is
/// there so a genuinely binary body is still expressible over a JSON ABI.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookRequest {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    body_hex: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    /// Only schemes that put their secret in the URL rather than a header
    /// (LNbits) read this.
    #[serde(default)]
    query: HashMap<String, String>,
    /// Unix seconds to check replay windows against. An explicit parameter and
    /// not a clock read, so a caller can reproduce a delivery exactly.
    now_unix: u64,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn unknown_method(method: &str) -> String {
    format!(
        "patala: unknown method {method:?} (want one of: {})",
        METHODS.join(", ")
    )
}

fn bad_request(method: &str, e: serde_json::Error) -> String {
    format!("patala: {method}: invalid request JSON: {e}")
}

fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("patala: could not encode the response: {e}"))
}

/// One unary call: the body of `patala_call`.
///
/// `request_json` may be empty for methods that ignore it (`id`,
/// `capabilities`, `caveat`, `providers`).
pub fn call(handle: u64, method: &str, request_json: &str) -> Result<String, String> {
    let inst = lookup(handle)?;
    let rail = &inst.rail;

    match method {
        "id" => encode(&IdResponse {
            rail_id: rail.id().to_string(),
        }),

        "capabilities" => encode(rail.capabilities()),

        "quote" => {
            let req: PayRequest =
                serde_json::from_str(request_json).map_err(|e| bad_request(method, e))?;
            let quote = inst
                .block_on(rail.quote(&req))
                .map_err(|e| format!("patala: {e}"))?;
            encode(&quote)
        }

        "charge" => {
            let req: PayRequest =
                serde_json::from_str(request_json).map_err(|e| bad_request(method, e))?;
            let receipt = inst
                .block_on(rail.charge(&req))
                .map_err(|e| format!("patala: {e}"))?;
            encode(&receipt)
        }

        // A receipt that does not verify is `{"valid": false}` and NOT an
        // error, exactly as the sidecar returns `200 {"valid": false}` and the
        // trait returns `Ok(false)`. A caller must never be able to confuse
        // "the rail says this did not settle" with "the call broke": those
        // demand different handling, and collapsing them is how an unpaid
        // order gets treated as a transient failure and retried into an
        // entitlement.
        "verify" => {
            let receipt: Receipt =
                serde_json::from_str(request_json).map_err(|e| bad_request(method, e))?;
            let valid = inst
                .block_on(rail.verify(&receipt))
                .map_err(|e| format!("patala: {e}"))?;
            encode(&VerifyResponse { valid })
        }

        // Infallible on purpose: "I could not check" is the `Unknown` verdict,
        // not an error. Raising there would let a caller's error handling
        // swallow the one answer that most needs a human.
        "validate-destination" => {
            let req: ValidateDestinationRequest =
                serde_json::from_str(request_json).map_err(|e| bad_request(method, e))?;
            let verdict = rail.validate_destination(&req.destination);
            encode(&ValidateDestinationResponse {
                is_refusal: verdict.is_refusal(),
                verdict,
            })
        }

        "webhook" => {
            let req: WebhookRequest =
                serde_json::from_str(request_json).map_err(|e| bad_request(method, e))?;
            let raw_body = match (req.body, req.body_hex) {
                (Some(_), Some(_)) => {
                    return Err(
                        "patala: webhook: give exactly one of \"body\" or \"body_hex\", \
                                not both — they would disagree about what the processor signed"
                            .to_string(),
                    )
                }
                (Some(b), None) => b.into_bytes(),
                (None, Some(h)) => decode_hex(&h)
                    .map_err(|e| format!("patala: webhook: body_hex is not valid hex: {e}"))?,
                (None, None) => {
                    return Err(
                        "patala: webhook: one of \"body\" or \"body_hex\" is required".to_string(),
                    )
                }
            };
            let delivery = WebhookDelivery::new(raw_body, req.now_unix)
                .with_headers(req.headers)
                .with_query(req.query);
            let event = inst
                .block_on(rail.verify_webhook(&delivery))
                .map_err(|e| format!("patala: {e}"))?;
            encode(&event)
        }

        "caveat" => encode(&CaveatResponse {
            exchange_deposit_caveat: patala_core::EXCHANGE_DEPOSIT_CAVEAT,
        }),

        "providers" => {
            #[cfg(feature = "fiat")]
            {
                encode(&ProvidersResponse {
                    providers: patala_uniffi::patala_fiat_providers(),
                })
            }
            #[cfg(not(feature = "fiat"))]
            {
                Err(not_compiled_in("fiat", "fiat"))
            }
        }

        other => Err(unknown_method(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_mock() -> u64 {
        open(r#"{"rail":"mock","currencies":["USDC","USD"]}"#).expect("mock rail")
    }

    #[test]
    fn abi_version_matches_the_workspace_version() {
        // The probe is worthless if it reports something the tree does not
        // agree with: a host comparing versions would reject a correct library
        // or accept a stale one.
        let file = include_str!("../../VERSION").trim();
        assert_eq!(
            ABI_VERSION, file,
            "patala-ffi's package version and ./VERSION disagree"
        );
    }

    #[test]
    fn charge_then_verify_round_trips_as_json() {
        let h = open_mock();
        let receipt = call(
            h,
            "charge",
            r#"{"amount_minor":1250,"currency":"USDC","destination":"dest","reference":"ffi-1"}"#,
        )
        .expect("charge");
        let parsed: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(parsed["amount_minor"], 1250);
        assert_eq!(parsed["rail_id"], "mock");

        let verified = call(h, "verify", &receipt).expect("verify");
        assert_eq!(verified, r#"{"valid":true}"#);
        close(h);
    }

    #[test]
    fn a_tampered_receipt_is_valid_false_and_not_an_error() {
        // The distinction the whole ABI is arranged around: a rail's honest
        // "this did not settle" is data, never a failure return.
        let h = open_mock();
        let receipt = call(
            h,
            "charge",
            r#"{"amount_minor":1250,"currency":"USDC","destination":"dest","reference":"ffi-2"}"#,
        )
        .unwrap();
        let mut parsed: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        parsed["amount_minor"] = serde_json::json!(999_999);
        let verified = call(h, "verify", &parsed.to_string()).expect("verify must not error");
        assert_eq!(verified, r#"{"valid":false}"#);
        close(h);
    }

    #[test]
    fn an_unsupported_currency_is_an_error_with_a_readable_message() {
        let h = open_mock();
        let err = call(
            h,
            "charge",
            r#"{"amount_minor":100,"currency":"EUR","destination":"d","reference":"ffi-3"}"#,
        )
        .expect_err("EUR is not supported by this mock rail");
        assert!(err.contains("EUR"), "unhelpful error: {err}");
        close(h);
    }

    #[test]
    fn every_destination_status_is_reachable_over_json_and_distinct() {
        let h = open_mock();
        let mut seen = Vec::new();
        for dest in [
            "mock:wallet:alice",
            "mock:program:vault",
            "stellar:wallet:alice",
            "definitely-not-an-address",
            "",
        ] {
            let out = call(
                h,
                "validate-destination",
                &serde_json::json!({ "destination": dest }).to_string(),
            )
            .expect("validate-destination never errors");
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert!(
                v["human_must_confirm"].as_bool().unwrap(),
                "{dest:?} must still require a human to confirm"
            );
            assert!(!v["exchange_deposit_caveat"].as_str().unwrap().is_empty());
            seen.push(v["status"].as_str().unwrap().to_string());
        }
        assert_eq!(seen.len(), 5);
        close(h);

        // Unknown needs a rail that declines to check at all — the fiat shape.
        let opaque = open(r#"{"rail":"mock","destination_checks":false}"#).unwrap();
        let out = call(
            opaque,
            "validate-destination",
            r#"{"destination":"cus_opaque_token"}"#,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "Unknown");
        assert_eq!(
            v["is_refusal"], false,
            "Unknown is not a refusal — and is not a green light either"
        );
        close(opaque);
    }

    #[test]
    fn is_refusal_crosses_as_data_and_matches_the_core_types_own_answer() {
        let h = open_mock();
        let core = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        for dest in [
            "mock:wallet:alice",
            "mock:program:vault",
            "stellar:wallet:alice",
            "junk",
            "",
        ] {
            let out = call(
                h,
                "validate-destination",
                &serde_json::json!({ "destination": dest }).to_string(),
            )
            .unwrap();
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(
                v["is_refusal"].as_bool().unwrap(),
                core.validate_destination(dest).is_refusal(),
                "is_refusal disagrees with patala_core for {dest:?}"
            );
        }
        close(h);
    }

    #[test]
    fn a_rail_with_no_push_delivery_reports_unsupported_rather_than_an_event() {
        let h = open_mock();
        let err = call(
            h,
            "webhook",
            r#"{"body":"{}","headers":{},"now_unix":1700000000}"#,
        )
        .expect_err("the mock has no processor and so no push delivery");
        assert!(err.contains("verify_webhook"), "unhelpful error: {err}");
        close(h);
    }

    #[test]
    fn a_webhook_body_must_be_given_exactly_once() {
        let h = open_mock();
        let both = call(
            h,
            "webhook",
            r#"{"body":"{}","body_hex":"7b7d","now_unix":1}"#,
        )
        .expect_err("two bodies cannot both be what was signed");
        assert!(both.contains("exactly one"), "unhelpful error: {both}");

        let neither = call(h, "webhook", r#"{"now_unix":1}"#).expect_err("no body at all");
        assert!(neither.contains("required"), "unhelpful error: {neither}");
        close(h);
    }

    #[test]
    fn an_unknown_method_is_refused_and_lists_the_known_ones() {
        let h = open_mock();
        let err = call(h, "no-such-method", "{}").expect_err("unknown method");
        assert!(err.contains("unknown method"), "{err}");
        assert!(
            err.contains("charge"),
            "the refusal must list what IS allowed"
        );
        close(h);
    }

    #[test]
    fn an_invented_handle_is_a_clean_error() {
        let err = call(999_999, "capabilities", "").expect_err("no such handle");
        assert!(err.contains("unknown handle"), "{err}");
    }

    #[test]
    fn handles_are_removed_on_close_and_never_reused() {
        let a = open_mock();
        let b = open_mock();
        assert_ne!(a, b);
        assert!(handle_is_live(a) && handle_is_live(b));
        assert!(live_handles() >= 2);

        close(a);
        assert!(
            !handle_is_live(a),
            "close must remove the entry, not just return"
        );
        assert!(
            handle_is_live(b),
            "closing one handle must not touch another"
        );
        close(a); // idempotent
        assert!(!handle_is_live(a));
        assert!(
            call(a, "capabilities", "").is_err(),
            "a closed handle must not answer"
        );

        // Never reused: the counter only increases, so a use-after-close is
        // reported as such rather than landing on whatever took the slot.
        let c = open_mock();
        assert!(c > b, "handles must only ever increase, never be reused");
        assert_ne!(c, a);
        close(b);
        close(c);
    }

    #[test]
    fn an_empty_configuration_means_the_offline_default() {
        let h = open("").expect("empty config is the offline mock");
        let caps: serde_json::Value =
            serde_json::from_str(&call(h, "capabilities", "").unwrap()).unwrap();
        assert_eq!(caps["class"], "NonCustodialFinal");
        assert_eq!(caps["holds_funds"], false);
        close(h);
    }

    #[test]
    fn a_malformed_configuration_is_refused() {
        let err = open("{\"rail\": ").expect_err("truncated JSON");
        assert!(err.contains("invalid configuration"), "{err}");
    }

    #[test]
    fn a_misspelled_configuration_field_is_refused_rather_than_defaulted() {
        // Fail-closed: `deny_unknown_fields`. A silently-defaulted currency
        // list on a payment rail is exactly the kind of thing that must not be
        // a shrug.
        let err = open(r#"{"rail":"mock","currencys":["USDC"]}"#)
            .expect_err("a misspelled field must not be ignored");
        assert!(err.contains("currencys"), "{err}");
    }

    #[test]
    fn an_unknown_rail_kind_is_refused() {
        let err = open(r#"{"rail":"dogecoin"}"#).expect_err("no such rail");
        assert!(err.contains("invalid configuration"), "{err}");
    }

    #[cfg(not(feature = "fiat"))]
    #[test]
    fn a_fiat_rail_in_a_build_without_the_feature_names_the_feature() {
        let err = open(r#"{"rail":"fiat","provider":"stripe"}"#)
            .expect_err("this build has no fiat support");
        assert!(err.contains("--features fiat"), "{err}");
    }

    #[cfg(feature = "fiat")]
    #[test]
    fn the_manual_fiat_rail_round_trips_offline_and_reports_honestly_pending() {
        let h = open(r#"{"rail":"fiat","provider":"manual"}"#).expect("manual needs no config");
        let receipt = call(
            h,
            "charge",
            r#"{"amount_minor":1500,"currency":"ZAR","destination":"buyer@example.org","reference":"ffi-manual-1"}"#,
        )
        .expect("charge");
        let parsed: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(parsed["amount_minor"], 0, "nothing has settled yet");
        assert_eq!(
            call(h, "verify", &receipt).unwrap(),
            r#"{"valid":false}"#,
            "an unconfirmed manual instruction must never report settled"
        );
        close(h);
    }

    #[cfg(feature = "fiat")]
    #[test]
    fn providers_lists_what_this_build_can_actually_construct() {
        let h = open_mock();
        let out = call(h, "providers", "").expect("providers");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let names: Vec<&str> = v["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert!(names.contains(&"manual"), "manual is always available");
        close(h);
    }

    #[cfg(feature = "fiat-stripe")]
    #[test]
    fn a_feature_gated_processor_is_constructible_from_json() {
        // Construction only — charging or verifying would dial a real
        // processor. Same precedent as patala-uniffi's own fiat tests.
        let h = open(
            r#"{"rail":"fiat","provider":"stripe","config":{"secret_key":"sk_test_x","webhook_secret":"whsec_x"}}"#,
        )
        .expect("constructing a StripeRail must not require network access");
        let caps: serde_json::Value =
            serde_json::from_str(&call(h, "capabilities", "").unwrap()).unwrap();
        assert_eq!(caps["class"], "CustodialReversible");
        assert_eq!(caps["holds_funds"], true);
        close(h);
    }

    #[test]
    fn hex_decoding_fails_closed() {
        assert!(decode_hex("7b7d").is_ok());
        assert!(decode_hex("7b7").is_err(), "odd length must be refused");
        assert!(decode_hex("zz").is_err(), "non-hex must be refused");
    }

    /// The only string `decode_hex` is ever handed is a keypair seed, and its
    /// error travels out through `patala_new`'s `*err` into whatever the host
    /// logs. It used to say `'z' is not a hex digit`, quoting the offending
    /// character back — so a caller feeding 64 near-miss seeds that differ in
    /// one position each would read a private key out of its own log file, one
    /// character at a time. Restore the `format!("{:?} is not a hex digit",
    /// c as char)` arm and this reports which secret character it leaked.
    #[test]
    fn a_malformed_seed_is_never_quoted_back() {
        // A plausible seed with one non-hex character among real ones.
        // `decode_hex` directly: `decode_seed`, its only caller for a seed, is
        // behind the solana/stellar features and this must hold in every build.
        // 'z' rather than 'q', because the refusal below says "cannot be
        // quoted" and this assertion must not match its own explanation.
        let seed = "0123456789abcdef0123456789abcdef0123456789abcdez0123456789abcdef";
        let message = decode_hex(seed).unwrap_err();
        assert!(
            !message.contains('z'),
            "the error message quotes the offending character of a private key \
             back at the caller: {message}"
        );
        // Nor any run of the surrounding key material. Four is short enough to
        // catch a `{:?}` of a slice or a context window, and long enough that
        // the position number cannot trip it.
        for w in seed.as_bytes().windows(4) {
            let run = std::str::from_utf8(w).unwrap();
            assert!(
                !message.contains(run),
                "the error message leaks {run:?} from a private key: {message}"
            );
        }
        assert!(
            message.contains("position 47"),
            "and it must still say WHERE, or a typo is unfixable: {message}"
        );
    }

    #[test]
    fn the_method_list_and_the_dispatcher_agree() {
        // A method named in METHODS but not handled would be advertised in
        // every error message and every header, and refused when called.
        let h = open_mock();
        for m in METHODS {
            let out = call(h, m, "{}");
            if let Err(e) = &out {
                assert!(
                    !e.contains("unknown method"),
                    "METHODS advertises {m:?} but the dispatcher does not handle it"
                );
            }
        }
        close(h);
    }
}
