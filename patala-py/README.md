# patala-py

A Python binding over `patala-core` (`PATALA.md` §5: "adapters are written
ONCE in Rust; Python and any other language consume that one core"). This
crate never reimplements a rail — it wraps whatever `PaymentRail` already
exists in Rust and exposes it to Python (and, later, any other UniFFI
target).

## UniFFI, not PyO3 — and why

`PATALA.md` §5 names UniFFI as "likely the better call" because the suite
wants more than Python — wasm/napi for JS is called out explicitly, and
Swift/Kotlin are effectively free once a UniFFI IDL exists. This crate
follows that call:

- **UniFFI** generates bindings for *every* target language from one
  `#[uniffi::export]` surface (`src/lib.rs`). Adding Swift or Kotlin later is
  a bindgen invocation with a different `--language`, not a new crate. That
  is the literal "M×1, never M×N" principle §5 states — one Rust surface,
  many language consumers, and every consumer is generated from the *same*
  definition rather than hand-written per language.
- **PyO3** would give slightly nicer Python ergonomics — real Python
  classes, direct C-API calls, no `ctypes` indirection — but it is
  Python-only. A second language would mean writing (and maintaining) a
  second binding crate: exactly the M×N this crate exists to avoid.

Given the suite's stated ambitions beyond Python, UniFFI is the pick. If
this crate ever turns out to only ever need Python, revisiting PyO3 for the
ergonomics is a legitimate future call — but that is not the situation today.

## Async boundary

`patala_core::PaymentRail`'s methods (`quote`, `charge`, `verify`, `refund`)
are `async fn`. UniFFI *can* export async functions to Python (driven off
Python's own `asyncio` event loop), but that would force every caller —
including a one-shot script — to run an event loop just to call `charge()`.

This binding instead exposes **synchronous** methods on `PatalaRail`. Each
one blocks the calling Python thread on the underlying async call using a
single lazily-created multi-thread `tokio::runtime::Runtime`, owned
process-wide by this crate (`src/lib.rs`, the `runtime()` function). The
Python caller never sees `async`/`await` at all — `rail.charge(req)` just
returns a `Receipt` or raises `PatalaError`.

This is the opposite trade `patala-sidecar` makes (that crate stays async,
because its entire existence *is* an async HTTP server). Here the goal is a
plain blocking call from arbitrary — usually synchronous — Python code, so
`block_on` inside a dedicated runtime is the right shape rather than a
leaked requirement that every Python caller manage an event loop. A future
async-Python surface (`async def charge(...)`, using UniFFI's foreign-future
support) could be added alongside the synchronous one without redesigning
`PatalaRail` — it would wrap the exact same `Arc<dyn PaymentRail>`.

## What's exposed

- `RailClass` (`CustodialReversible` / `NonCustodialFinal`) and `Settlement`
  (`Instant` / `Seconds` / `Days`) — mirrored 1:1, never flattened, exactly
  as `patala-core` insists (`PATALA.md` §3).
- `RailCapabilities`, `PayRequest`, `Quote`, `Receipt` — mirrored records.
  Amounts stay `u64` minor-units integers across the FFI boundary too — never
  a float.
- `PatalaError` — a UniFFI error enum mirroring `patala_core::Error`
  (`Unsupported`, `Rail`, `InvalidRequest`, `CrossClassFailover`,
  `AllRailsFailed`). `verify` failing closed is still expressed as `Ok(false)`
  / a Python `False`, never as an exception — exactly like the core trait's
  contract.
- `PatalaRail` — the one object type Python ever touches. It wraps
  `Arc<dyn patala_core::PaymentRail>` and exports `id()`, `capabilities()`,
  `quote()`, `charge()`, `verify()`, `verify_webhook()`.
  `PatalaRail.new_mock(...)`, built on `patala_core::MockRail`, is always
  available — no feature flag needed, and this is what CI and a bare
  `pip install patala-py` get by default.
- `WebhookDelivery` / `WebhookEvent` / `WebhookStatus` — the push side.

## Webhooks

`verify_webhook(delivery)` is the push counterpart to `verify(receipt)`.
Without it a consumer on this side of the FFI can only ever *poll* a
processor, because webhook signature verification is provider-specific Rust
that lives beside each adapter — and anything not on the `PaymentRail` trait
is invisible to UniFFI.

```python
import hashlib, hmac
from patala_py import PatalaRail, WebhookDelivery, WebhookStatus, PatalaError

rail = PatalaRail.new_fiat("stripe", {"secret_key": ..., "webhook_secret": secret, ...})

# Forward the processor's request VERBATIM: same bytes, same headers, same
# query string. Every scheme signs exactly what was sent, so a body that has
# been through a JSON round-trip on your side will not verify.
delivery = WebhookDelivery(
    raw_body=request.get_data(),          # bytes, not str, not a parsed dict
    headers=dict(request.headers),        # matched case-insensitively
    query=None,                           # only LNbits reads this (?secret=)
    now_unix=int(time.time()),            # replay windows are checked against this
)

try:
    event = rail.verify_webhook(delivery)  # raises if not authentic
except PatalaError.InvalidRequest:
    return "", 400
except PatalaError.Unsupported:
    return "", 501                         # this rail has no push delivery

if event.status == WebhookStatus.SETTLED:
    # Reconcile event.amount_minor / event.currency against your own stored
    # order before trusting them, and dedupe on (event.rail_id, event.event_id).
    ...
```

`WebhookStatus` has three values, not two. `UNCONFIRMED` means the delivery
is genuine but carries no settlement claim — BTCPay, Coinbase Commerce,
OpenNode, LNbits and Mollie all authenticate a notification that names an
object and nothing else. Look up your stored `Receipt` for `event.object_id`
and call `verify()` on it; never treat `UNCONFIRMED` as payment.

`patala-py/examples/smoke_test.py` drives this end to end from Python
(against a genuinely signed Stripe delivery, offline) when the cdylib is
built with `--features fiat-stripe`, and says so loudly when it is not.

## Real rails (TASK 1: not just MockRail anymore)

`PatalaRail` wraps the trait object, not a concrete type, so adding a real
rail never changes the shape of `id()`/`capabilities()`/`quote()`/`charge()`/
`verify()`/`verify_webhook()` — only the constructor list grows. Three more constructors exist
today, each gated behind its own Cargo feature so the **default build stays
exactly as offline as before** (`PATALA.md` §8) — `patala-solana`/
`patala-stellar`/`patala-hyperswitch` are `optional = true` dependencies of
this crate (`dep:patala-solana` etc.), pulled in only when the matching
feature is on:

| Feature | Constructor | Rail class |
|---|---|---|
| `solana` | `PatalaRail.new_solana(rpc_url, cluster, keypair_seed)` | `NonCustodialFinal` (SPL-USDC) |
| `stellar` | `PatalaRail.new_stellar(horizon_url, network, usdc_issuer, keypair_seed)` | `NonCustodialFinal` (native USDC) |
| `hyperswitch` | `PatalaRail.new_hyperswitch(base_url, api_key, connector, webhook_secret, requires_kyc, currencies, settlement_days, timeout_secs)` | `CustodialReversible` |

Details:

- **`new_solana(rpc_url, cluster, keypair_seed)`** — `cluster` is `"devnet"`
  or `"mainnet"`/`"mainnet-beta"` (anything else is a
  `PatalaError.InvalidRequest`, never a silent default — same as
  `patala_solana::Cluster::parse`). `keypair_seed` is `None` for a
  verify-only rail, or exactly 32 raw Ed25519 seed bytes for a rail that can
  also `charge()` — per `PATALA.md` §6 that same key is both the signing
  identity and the wallet the funds move from, no separate mapping table.
  Building the rail touches no network; only `quote`/`charge`/`verify` call
  `rpc_url`.
- **`new_stellar(horizon_url, network, usdc_issuer, keypair_seed)`** —
  `network` is `"testnet"` (which *requires* `usdc_issuer`, since Stellar's
  testnet USDC issuer rotates and has no fixed default) or
  `"public"`/`"mainnet"` (which ignores `usdc_issuer` and uses the
  well-known Circle mainnet issuer already baked into `patala-stellar`).
  Same seed rule as Solana. **UNVERIFIED AGAINST LIVE STELLAR** — see
  `patala-stellar`'s own README; that caveat is unchanged by this binding.
- **`new_hyperswitch(...)`** — talks to a **self-hosted** Hyperswitch
  instance (never a hardcoded endpoint — `base_url`/`api_key` are required
  arguments, exactly mirroring `HyperswitchConfig`'s own invariant).
  `connector` optionally pins one Hyperswitch-configured processor (e.g.
  `"paystack"`); `None` lets Hyperswitch's own merchant-account routing
  decide. **UNVERIFIED AGAINST LIVE** — no live Hyperswitch instance was
  reachable from this environment, matching `patala-hyperswitch`'s own
  README.

Every constructor above raises a typed `PatalaError` (never panics, never
returns a half-built rail) on bad input — see `src/lib.rs`'s
`new_solana`/`new_stellar`/`new_hyperswitch` doc comments and their
`#[cfg(test)]` unit tests for the exact validation each performs.

Reading the capability/class model from Python works identically regardless
of which rail is behind a `PatalaRail` — a caller does `rail.capabilities()`
and branches on `._class` (`RailClass.NON_CUSTODIAL_FINAL` /
`RailClass.CUSTODIAL_REVERSIBLE`) without ever needing to know or name the
concrete provider, exactly as `PATALA.md` §3 requires.

## `patala-fiat` (20 processor adapters, one by-name constructor)

`patala-fiat` ships 20 feature-gated `CustodialReversible` processor
adapters (Stripe, Paystack, Adyen, Checkout.com, Mollie, Mercado Pago,
Flutterwave, iyzico, Midtrans, Yoco, PayFast, PayU, Razorpay, Square,
Xendit, BTCPay, LNbits, OpenNode, Coinbase Commerce, PayPal) plus the
always-on, zero-network `manual` rail. Rather than adding 20 more typed
constructors (`new_stripe`, `new_paystack`, ...), these are exposed through
**one** by-name registry constructor:

```python
PatalaRail.new_fiat(provider: str, config: dict[str, str]) -> PatalaRail
```

See `src/fiat.rs`'s module docs for the full "why by-name+config, not 20
typed constructors" justification — short version: `patala-fiat` already
solved "pick a rail by name + config" once, at its own `registry` layer;
this is the FFI-facing continuation of that same design, not a second one,
and it keeps the generated binding's surface small. `provider` is matched
case-insensitively; an unknown name or a provider whose Cargo feature was
not compiled into this build both raise `PatalaError.InvalidRequest` (never
a panic, never a silent fallback to a different rail).

### Cargo features

- `fiat` — pulls in `patala-fiat` with its own default features only
  (currency table + registry + `manual`, zero network/crypto deps). Enough
  for `new_fiat("manual", ...)`.
- `fiat-<name>` (one per adapter: `fiat-stripe`, `fiat-paystack`,
  `fiat-adyen`, `fiat-btcpay`, `fiat-checkoutcom`, `fiat-coinbasecommerce`,
  `fiat-flutterwave`, `fiat-iyzico`, `fiat-lnbits`, `fiat-mercadopago`,
  `fiat-midtrans`, `fiat-mollie`, `fiat-opennode`, `fiat-payfast`,
  `fiat-paypal`, `fiat-payu`, `fiat-razorpay`, `fiat-square`,
  `fiat-xendit`, `fiat-yoco`) — each enables exactly `patala-fiat/<name>`,
  pulling in only THAT adapter's network/crypto deps (mirrors
  `patala-fiat/Cargo.toml`'s own per-adapter feature list one-to-one).
- `fiat-all` — every `fiat-<name>` feature at once. Mainly for this crate's
  own tests and for regenerating the Go binding with the full surface (see
  `../patala-go/Makefile`'s `run-example-fiat`/`test-fiat` targets).

A plain `cargo build -p patala-py` (no `--features`) is unaffected — no new
dependency, no new symbol, exactly the same offline default as before.

### `config` keys, by provider

Every key below is the EXACT field name of that provider's own
`<Provider>Config` struct in `patala-fiat` (see that adapter's own
`config.rs` doc comment for full detail on each field) — this binding does
not rename anything. A missing key for a required field is passed through
as an empty string and rejected by that adapter's own `new()` constructor
(every `patala-fiat` adapter already fails closed on an empty required
field) with a `PatalaError.InvalidRequest` naming the field. Boolean fields
are `"true"`/anything-else (case-insensitive); `currencies` is a
comma-separated list, uppercased; numeric fields (`settlement_days`,
`timeout_secs`, `settlement_seconds`, `quote_ttl_secs`) are parsed and, if
present but malformed, rejected as `InvalidRequest` rather than silently
defaulted (this binding is a programmatic config map, not a typo-prone
shell env var, so failing closed on bad explicit input is the more honest
choice — `PATALA.md` §8).

| Provider (`new_fiat` name) | Required keys | Notable optional keys / defaults |
|---|---|---|
| `manual` | *(none)* | Always available once `fiat` is on; never dials the network. |
| `stripe` | `secret_key`, `webhook_secret` | `currencies` empty = unrestricted. |
| `paystack` | `secret_key` | `currencies` defaults to Paystack's own hardcoded list (NGN/GHS/ZAR/KES/USD). |
| `adyen` | `api_key`, `merchant_account`, `hmac_key_hex`, `api_base_url` | `hmac_key_hex` must be valid hex. |
| `btcpay` | `base_url`, `api_key`, `store_id`, `webhook_secret` | `settlement_seconds` optional (unset → `Settlement::Instant`). |
| `checkoutcom` | `secret_key`, `webhook_secret`, `api_base_url` | |
| `coinbasecommerce` | `api_key`, `webhook_secret` | `base_url` defaults to `https://api.commerce.coinbase.com`. |
| `flutterwave` | `secret_key`, `webhook_hash` | `currencies` defaults to Flutterwave's own hardcoded list. |
| `iyzico` | `api_key`, `secret_key` | `base_url` defaults to iyzico's production API; `currencies` defaults to TRY/USD/EUR/GBP. |
| `lnbits` | `base_url`, `api_key`, `webhook_secret` | `quote_ttl_secs` defaults to 900s; must be a positive integer if given. |
| `mercadopago` | `access_token`, `webhook_secret` | `currencies` defaults to Mercado Pago's own hardcoded LatAm list. |
| `midtrans` | `server_key` | No `currencies` key — hardcoded IDR-only, same as `patala-fiat` itself. |
| `mollie` | `api_key`, `webhook_url` | |
| `opennode` | `api_key` | `base_url` defaults to `https://api.opennode.com`. |
| `payfast` | `merchant_id`, `merchant_key` | `passphrase` optional (empty default). No `currencies` key — ZAR-only. |
| `paypal` | `client_id`, `client_secret`, `webhook_id`, `env` (`"live"`/`"sandbox"`, exactly) | `env` has no default — a typo/other value is `InvalidRequest`, mirroring `PayPalConfig::from_env`'s own "never silently point at the wrong environment" rule. |
| `payu` | `merchant_key`, `salt` | No `currencies` key — cackle hardcodes INR. |
| `razorpay` | `key_id`, `key_secret`, `webhook_secret` | No `currencies` key — hardcoded INR. |
| `square` | `access_token`, `webhook_signature_key`, `location_id`, `notification_url`, `api_base_url` | |
| `xendit` | `secret_key`, `webhook_token` | `currencies` defaults to Xendit's own hardcoded list. |
| `yoco` | `secret_key`, `webhook_secret` | No `currencies` key — hardcoded ZAR-only. |

Every provider above also accepts `requires_kyc` (default `true`, except
`btcpay`/`lnbits`/`coinbasecommerce`/`opennode` default `false` — the
self-hosted/crypto-adjacent ones, matching `patala-fiat`'s own per-adapter
default) and `settlement_days` (default `2`, card-network T+2) or
`timeout_secs` (default `15`, or `20` for the crypto-adjacent adapters) —
see the table above and each `build_<name>` function in `src/fiat.rs` for
the exact default per field.

```python
from patala_py import PatalaRail

rail = PatalaRail.new_fiat("manual", {})
print(rail.id(), rail.capabilities())

stripe = PatalaRail.new_fiat("stripe", {
    "secret_key": "sk_live_...",
    "webhook_secret": "whsec_...",
})
```

`PatalaRail.fiat_providers()` (Python: `patala_fiat_providers()`, a free
function — UniFFI does not currently support exporting a plain associated
function with no `&self`/constructor from inside an `impl` block, see
`src/fiat.rs`) lists every provider name this specific build can actually
construct, so a caller can discover what's available instead of hardcoding
a list that might not match the build.

**UNVERIFIED AGAINST LIVE** for all 20 processor adapters — same status as
`patala-fiat` itself (see its own crate docs): every unit test here only
CONSTRUCTS a rail offline (proving the config-map → typed-Config → real-Rail
path and the capability/class model), never calls `charge`/`verify` against
a real processor. `manual`'s `charge`/`verify` round trip IS exercised for
real (it never touches the network at all), and honestly reports
`amount_minor: 0` / `verify() == false` until a separate, direct-Rust caller
of `ManualRail::mark_paid` (not part of the `PaymentRail` trait, so
unreachable through this generic by-name FFI surface) confirms it — see
`src/fiat.rs`'s test docs.

## Packaging (TASK 2: genuinely `pip install`-able)

**The shipping story is a maturin-built wheel — not the manual
`uniffi-bindgen` flow below.** Both use the *exact same* UniFFI binding
(`src/lib.rs`, `src/bin/uniffi_bindgen.rs`); maturin does not replace or
compete with UniFFI, it is the wheel-packaging frontend around it.
`pyproject.toml`'s `[tool.maturin] bindings = "uniffi"` tells maturin to
build this crate's cdylib and then run this crate's own `uniffi-bindgen`
binary target against it to generate `patala_py.py` — the same generation
step the manual flow runs by hand — and bundle the result plus the compiled
native library into a real wheel with proper metadata
(`dist-info`, platform tag, `import patala_py` from a normal `site-packages`
install). That is what makes it genuinely `pip install`-able: a wheel a user
installs with `pip install <file>.whl` (or, once published, `pip install
patala-py`) and then just `import patala_py` — no `cargo`, no Rust
toolchain, no manual bindgen invocation, no `PYTHONPATH` juggling on the
user's machine. The manual flow (previous wave, still documented below) is
kept only as the offline/no-maturin fallback and for local iteration; it is
not what an end user should be told to do.

### Build a wheel locally

```bash
# From patala-py/ (this crate's directory — pyproject.toml lives here).
cd patala-py

python3 -m venv .venv && source .venv/bin/activate
pip install maturin

# MockRail only (offline default, no rail deps):
maturin build --release
# Wheel lands in patala-py/target/wheels/patala_py-<version>-<tag>.whl

# With one or more real rails compiled in (adds patala-solana/stellar/
# hyperswitch and their deps to THIS wheel only — the workspace's other
# crates are unaffected):
maturin build --release --features solana,stellar,hyperswitch
```

### Install the wheel and use it

```bash
pip install target/wheels/patala_py-*.whl
python3 -c "
from patala_py import PatalaRail, RailClass
rail = PatalaRail.new_mock('mock', RailClass.NON_CUSTODIAL_FINAL, ['USDC'], 0, False)
print(rail.id(), rail.capabilities())
"
```

### Iterate locally without building a wheel each time

```bash
# Installs an editable/develop build straight into the active venv —
# rebuilds the extension in place, no `pip install` of a wheel file needed.
maturin develop --features solana,stellar,hyperswitch
```

### Publishing pre-built wheels to PyPI (no Rust toolchain for end users)

The point of shipping wheels (as opposed to an sdist) is that `pip install
patala-py` on an end user's machine downloads a **pre-built** binary for
their exact platform/arch/Python version — no compiler, no Rust, no
`cargo`. That means building one wheel per (OS × arch) combination you want
to support, ahead of time, in CI, then uploading all of them:

1. **Build a matrix of wheels**, one per target, using `maturin-action`
   (the official GitHub Action) or `cibuildwheel`+maturin, e.g. targets:
   - macOS: `x86_64-apple-darwin`, `aarch64-apple-darwin`
   - Linux: `x86_64-unknown-linux-gnu` (manylinux), `aarch64-unknown-linux-gnu`
   - Windows: `x86_64-pc-windows-msvc`
   Each CI job runs the same `maturin build --release [--features ...]`
   command above, cross-compiling or running on a native runner per target;
   `maturin-action` handles the manylinux container / cross toolchain
   details.
2. **Collect every wheel** from every job into one directory (`dist/` is
   already `.gitignore`d in this repo for exactly this build-output reason).
3. **Publish** with `pip install twine && twine upload dist/*.whl` (or
   `maturin publish`, which wraps the same PyPI upload API and can be run
   per-target-wheel directly from each CI job instead of a separate collect
   step). Either way the end result on PyPI is one release with several
   wheel files attached, each tagged for its platform/arch/Python ABI; `pip`
   picks the right one automatically for the installing machine.
4. Optionally also publish an **sdist** (`maturin sdist`) as a fallback for
   platforms with no pre-built wheel — that path *does* need a Rust
   toolchain on the installing machine, which is exactly what the wheel
   matrix above exists to avoid for the common platforms.

None of step 1-4 was run against the real PyPI from this environment (no
PyPI credentials here, and this crate is `publish = false` — see
`Cargo.toml`); the commands above are exact and ready to run, not
speculative, but actually publishing is a founder/CI-secrets action, not
something this wave executed.

## Manual flow (no maturin) — offline fallback / local iteration

This still works and needs neither `maturin` nor a separately-installed
`uniffi-bindgen` CLI: this crate carries its own tiny bindgen binary
(`src/bin/uniffi_bindgen.rs`, a one-liner calling
`uniffi::uniffi_bindgen_main()`), so bindings can be generated straight from
`cargo` with no other tool installed at all:

```bash
# From the workspace root.

# 1. Build the cdylib Python will load (add --features solana,stellar,hyperswitch
#    to also compile in the real rails; omit for the offline MockRail-only default).
cargo build -p patala-py

# 2. Generate the Python wrapper module from that cdylib's UniFFI metadata.
cargo run -p patala-py --bin uniffi-bindgen -- generate \
    --library target/debug/libpatala_py.dylib \
    --language python \
    --out-dir patala-py/bindings/python
# (Linux: target/debug/libpatala_py.so)

# 3. The generated `patala_py.py` loads its native library by name from its
#    own directory (see `_uniffi_load_indirect` in the generated file), so
#    copy the freshly built library next to it:
cp target/debug/libpatala_py.dylib patala-py/bindings/python/

# 4. Run the smoke test.
PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py
```

`patala-py/bindings/` is gitignored (see the workspace `.gitignore`) — like
`target/`, it is build output, reproduced by the four commands above, not
checked in.

### Rust-only checks (no Python needed)

`src/lib.rs` also carries ordinary `#[cfg(test)]` Rust unit tests that
exercise `PatalaRail` directly (charge → verify round-trip, tamper-detection,
unsupported currency, a failing rail) without going through Python or ctypes
at all:

```bash
cargo test -p patala-py
```

## Verified in this environment (2026-07-21)

Both steps were actually executed here, not just written:

- `cargo test -p patala-py` — 4/4 Rust unit tests pass.
- The full **Build & run** sequence above was run end-to-end: `cargo build`,
  `cargo run --bin uniffi-bindgen`, and then the real generated
  `patala_py.py` + compiled `.dylib` were loaded by a real `python3`
  (3.13.9) process running `examples/smoke_test.py`, which imports the built
  module and performs a genuine `MockRail` charge → verify round trip,
  asserting `capabilities()._class`, `holds_funds`, `currencies`,
  `quote().total_minor` (an `int`), and both a valid and a tampered
  `verify()` result, plus that an unsupported currency raises
  `PatalaError.InvalidRequest`. It printed
  `ALL PYTHON SMOKE ASSERTIONS PASSED` and exited `0`.

`maturin` was not installed; it was not needed for the above — see "Build &
run". `pip install maturin` was confirmed resolvable from this network
(`pip3 install --dry-run maturin` succeeded) if a packaged wheel is wanted
later, but building one was out of scope for proving the binding works.

## `patala-fiat` exposure: verified in this environment

- `cargo build -p patala-py` (default, no `--features`) — succeeds, and
  `cargo tree -p patala-py -e normal` confirms it pulls in **no**
  `patala-fiat` at all (let alone `reqwest`/`hmac`/`sha2`).
- `cargo build -p patala-py --features fiat` — succeeds; `cargo tree`
  confirms `patala-fiat` is now a dependency but still pulls in **zero**
  `reqwest`/`hmac`/`sha2`/`hex` (patala-fiat's own default features are
  just the currency table + registry + `manual`).
- `cargo build -p patala-py --features fiat-all` (all 20 processor
  adapters) and `--features fiat-all,solana,stellar,hyperswitch` (every
  feature this crate has, combined) both succeed.
- `cargo test -p patala-py --features fiat-all` — 13/13 tests pass
  (4 pre-existing MockRail tests + 9 new fiat tests: `manual`'s genuine
  charge/verify round trip reporting honestly-pending, an unknown-provider
  rejection, `fiat_providers()` always listing `manual`, and
  construction-only offline tests for `stripe` — including a missing
  required field — `paystack`'s default currency list, `paypal`'s
  `env` validation, and `btcpay`'s numeric-field validation).
  `--features fiat-all,solana,stellar,hyperswitch` together: 21/21 pass.
- `cargo clippy -p patala-py --features fiat-all --all-targets -- -D
  warnings` and the same for `fiat-all,solana,stellar,hyperswitch` — both
  clean. `cargo fmt -p patala-py -- --check` — clean.
- **UNVERIFIED AGAINST LIVE** for all 20 processor adapters (same status
  as `patala-fiat` itself) — every fiat test above either stays fully
  offline (`manual`) or only constructs a rail without calling
  `charge`/`verify`, which would dial a real processor.
