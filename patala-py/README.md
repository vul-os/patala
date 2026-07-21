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
  `quote()`, `charge()`, `verify()`. `PatalaRail.new_mock(...)`, built on
  `patala_core::MockRail`, is always available — no feature flag needed, and
  this is what CI and a bare `pip install patala-py` get by default.

## Real rails (TASK 1: not just MockRail anymore)

`PatalaRail` wraps the trait object, not a concrete type, so adding a real
rail never changes the shape of `id()`/`capabilities()`/`quote()`/`charge()`/
`verify()` — only the constructor list grows. Three more constructors exist
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
