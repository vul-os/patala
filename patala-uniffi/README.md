# patala-uniffi

The **one** UniFFI surface over `patala-core`. Every UniFFI-reachable
language — Python, Go, Swift, Kotlin, Ruby, and whatever comes next — is
generated from the `#[uniffi::export]` definitions in `src/lib.rs` and
`src/fiat.rs`. Nothing in this crate implements a rail; it wraps
`Arc<dyn patala_core::PaymentRail>` and nothing else (`PATALA.md` §5:
"adapters are written ONCE in Rust; every other language consumes that one
core").

```
patala-core  ── the trait, the types, MockRail
     │
     ├── patala-uniffi ── the one #[uniffi::export] surface, namespace "patala"
     │        ├── patala-py   (wheel: libpatala_py + generated patala.py)
     │        └── patala-go   (generated: bindings/patala/patala.go)
     │
     ├── patala-ffi    ── plain extern "C", JSON in/JSON out, for the languages
     │                    UniFFI has no backend for (C, C++, Node, PHP, Elixir)
     └── patala-sidecar ── loopback HTTP, for anything that wants no FFI at all
```

## Why the namespace is spelled out

```rust
uniffi::setup_scaffolding!("patala");
```

That string is the reason this crate exists as something separate from
`patala-py`.

UniFFI derives a binding's *namespace* from the crate that calls
`setup_scaffolding!()` unless you name it, and every generator turns that
namespace into the module or package name it emits. The whole surface used to
live in `patala-py` — the only cdylib in the workspace — so the namespace was
`patala_py`. `uniffi-bindgen-go`'s output literally began `package patala_py`;
`patala-go` carried a `uniffi.toml` that renamed only the output *directory*,
and every Go call site needed an import alias to read naturally. That was a
documented wrinkle for one extra language. With ten more languages being
generated from this surface, all ten would have inherited a Python-flavoured
name for a surface with nothing Python-specific in it.

So the surface moved here and names its namespace explicitly. The generated
artefacts are now `patala.py`, `package patala`, `Patala.swift`, and so on.
Naming it explicitly rather than relying on the crate name also means renaming
this crate would not silently rename every binding in the suite.

Two gates hold it in place, because a namespace regression is silent at the
Rust level and only shows up as broken imports downstream:

- `patala-py/tests/scaffolding.rs` references the
  `UNIFFI_META_NAMESPACE_PATALA` symbol. If `setup_scaffolding!("patala")`
  became `setup_scaffolding!()`, the symbol would be
  `UNIFFI_META_NAMESPACE_PATALA_UNIFFI` and that test target would fail to
  **link**.
- `patala-go`'s `make generate` asserts the generated file begins
  `package patala`, and says which Rust line to look at when it does not.

## What is exported

One object, `PatalaRail`, wrapping `Arc<dyn PaymentRail>`; the records and
enums that mirror `patala-core` one for one (`RailClass`, `Settlement`,
`RailCapabilities`, `PayRequest`, `Quote`, `Receipt`, `WebhookDelivery`,
`WebhookEvent`, `WebhookStatus`, `DestinationStatus`, `DestinationVerdict`);
the error type `PatalaError`; and the free functions
`exchange_deposit_caveat()` and `patala_fiat_providers()`.

Nothing here is flattened for the convenience of the boundary. `RailClass` is
not a bool, `WebhookStatus` is three states and not a bool, and
`DestinationStatus` is five variants and not a bool — each of those
distinctions changes what a consumer must show a person, and a binding that
collapsed one would leave every non-Rust consumer unable to say it. See the
per-type doc comments in `src/lib.rs` for the argument in each case.

`DestinationVerdict` additionally carries `is_refusal` and
`human_must_confirm` **as data**, computed on the Rust side, because they are
methods on the core type and a method does not survive a UniFFI record. A
consumer re-deriving `is_refusal` from `status` in its own language writes a
`switch` whose default is "not a refusal" — which fails open on the one
question that decides whether a customer's money goes to an address the rail
already knows is wrong.

## Features

`default = []`. Everything real is opt-in, and a plain
`cargo build -p patala-uniffi` links no network client at all:

| Feature | Adds |
|---|---|
| `solana` | `PatalaRail::new_solana` (`patala-solana`) |
| `stellar` | `PatalaRail::new_stellar` (`patala-stellar`) |
| `hyperswitch` | `PatalaRail::new_hyperswitch` (`patala-hyperswitch`) |
| `fiat` | `PatalaRail::new_fiat` + `patala_fiat_providers()`, `manual` rail only |
| `fiat-<provider>` | one of the 20 `patala-fiat` processor adapters |
| `fiat-all` | all twenty at once |

`patala-py` and `patala-ffi` re-declare the same names as pure forwarders;
`scripts/check-features.sh` fails the build if any of them drifts, and if
`fiat-all` misses a processor that exists in `patala-fiat/src/`. Since 0.1.1 it
additionally builds and lints each of the twenty features **alone** — fourteen
of them did not compile, which pushed operators to `fiat-all` and linked every
processor into their binary.

## Generating bindings

The cdylib carries the metadata, so every generator runs in `--library` mode
against the built artefact. There is no `.udl` file in this tree.

```bash
# From the workspace root.
cargo build -p patala-uniffi                 # or --features fiat-all
cargo run -p patala-uniffi --bin uniffi-bindgen -- generate \
    --library target/debug/libpatala_uniffi.dylib \
    --language python \
    --out-dir /tmp/patala-python
#   (Linux: libpatala_uniffi.so)
```

`--language` also accepts `swift` and `kotlin`. Go has its own out-of-tree
generator (`uniffi-bindgen-go`, pinned to `v0.5.0+v0.29.5` to match
`uniffi = 0.29.5`) — see `patala-go/README.md`.

The generated wrapper loads its native library **by the file name of the
cdylib the metadata was read from**. Generating from
`target/debug/libpatala_uniffi.dylib` produces a wrapper that looks for
`libpatala_uniffi`; generating from `patala-py`'s cdylib produces one that
looks for `libpatala_py`. Both carry the same scaffolding — `patala-py` links
this crate as an rlib and rustc re-exports its `#[no_mangle]` symbols — so the
choice is only about which file has to sit next to the wrapper.

## Tests

```bash
cargo test -p patala-uniffi                  # 11 tests, offline
cargo test -p patala-uniffi --features fiat-all
```

Every test is offline. The real-rail tests only *construct* a rail — no
`quote`/`charge`/`verify` against `patala-solana`/`patala-stellar`/
`patala-hyperswitch` or any fiat processor, because those dial a network. The
`manual` fiat rail is the exception and is exercised end to end, since it
never touches a network by design.
