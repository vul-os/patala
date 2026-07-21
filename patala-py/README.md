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
  `quote()`, `charge()`, `verify()`. Today its only constructor is
  `PatalaRail.new_mock(...)`, built on `patala_core::MockRail`.

## Adding a real rail later (no redesign)

`PatalaRail` wraps the trait object, not a concrete type. When
`patala-solana` / `patala-stellar` / `patala-hyperswitch` exist, this crate
gains one constructor per rail — e.g. a feature-gated
`PatalaRail::new_solana(rpc_url, keypair_bytes)` — that builds the real rail
and wraps it exactly the way `new_mock` does today:
`Arc::new(Self { inner: Arc::new(real_rail) })`. Every method Python already
calls is unchanged; only the constructor list grows. The generated Python API
surface (`rail.quote()`, `rail.charge()`, `rail.verify()`, `rail.id()`,
`rail.capabilities()`) does not change shape when a rail is added.

## Build & run

No `maturin` and no separately-installed `uniffi-bindgen` are required. This
crate carries its own tiny bindgen binary (`src/bin/uniffi_bindgen.rs`, a
one-liner calling `uniffi::uniffi_bindgen_main()`), so bindings are generated
straight from `cargo`:

```bash
# From the workspace root.

# 1. Build the cdylib Python will load.
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
checked in. A real packaging story (a `pyproject.toml` + `maturin build
--features uniffi` producing an installable wheel with the bindings bundled)
is a natural next step once this crate has more than one rail to ship; the
manual flow above is what this wave actually built and verified.

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
