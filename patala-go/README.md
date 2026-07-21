# patala-go

A Go binding over `patala-core`, generated with
[`uniffi-bindgen-go`](https://github.com/NordSecurity/uniffi-bindgen-go) from
the **same** `#[uniffi::export]` surface `patala-py` already exposes
(`patala-py/src/lib.rs`) — the literal "M×1, never M×N" principle `PATALA.md`
§5 states: adapters are written once in Rust, and every language is a
generated consumer of that one definition. Nothing in this directory
reimplements `PatalaRail`, `RailClass`, `RailCapabilities`, `Quote`, or
`Receipt` — it consumes the exact same UniFFI metadata `patala-py`'s cdylib
already carries, targeted at a different output language.

**Read this before reaching for it:** this binding uses cgo. If your Go
project needs a pure-static, cross-compiles-trivially binary (see "The cgo
cost" below — `cackle` is the concrete example in this suite), reach for
[`patala-sidecar`](../patala-sidecar/) instead. That is not a caveat buried at
the bottom of this file — it is the main thing to know before choosing this
path over the sidecar.

## Which cdylib, which package name?

`patala-py`'s crate is named `patala-py` and its `Cargo.toml` builds a
`cdylib` (`libpatala_py.dylib` / `.so`) so **Python** can load it — but the
compiled artifact itself is not Python-specific. UniFFI's cdylib carries
generic C-ABI scaffolding plus embedded metadata describing the whole
`#[uniffi::export]` surface; that is precisely what lets `uniffi-bindgen-go`,
`uniffi-bindgen` (Python), and a hypothetical Swift/Kotlin bindgen all target
the *same* compiled library. So `patala-go` does not add a new Rust crate or
touch `patala-py`/`patala-core` — it points `uniffi-bindgen-go` directly at
`target/debug/libpatala_py.dylib` (built by `cargo build -p patala-py`,
unmodified) and generates a Go package from it.

**One naming wrinkle, stated plainly:** `uniffi.toml`'s
`[bindings.go] package_name = "patala"` only renames the *output directory*
(`bindings/patala/` instead of `bindings/patala_py/`) — it does **not**
rename the Go `package` clause inside the generated file. That clause is
fixed to the UniFFI *namespace*, which `patala-py`'s own
`uniffi::setup_scaffolding!()` call derives from the crate name
(`patala_py`), and this package does not touch `patala-py`'s source to change
that. So the generated file at `bindings/patala/patala_py.go` genuinely
starts with `package patala_py`. `examples/roundtrip/main.go` works around
this with an import alias — `patala "github.com/vul-os/patala/patala-go/bindings/patala"`
— which is enough for every call site to read naturally (`patala.PatalaRailNewMock(...)`,
`patala.RailClassNonCustodialFinal`, etc.) without needing to change anything
upstream. Any Go code importing this package should do the same.

If this two-name situation (directory `patala`, package clause `patala_py`)
ever feels confusing in practice, the honest fix is to extract a
neutrally-named `patala-uniffi` crate that both `patala-py` and `patala-go`
build against, with its own `uniffi::setup_scaffolding!("patala")` call —
noted here as a legitimate future cleanup, not built now (`PATALA.md`'s
convention: note deferred items, don't build them speculatively).

## What's exposed

Exactly the same surface `patala-py/README.md` documents, generated for Go
instead of Python:

- `RailClass` (`NonCustodialFinal` / `CustodialReversible`) and `Settlement`
  — mirrored 1:1 from `patala_core`, never flattened (`PATALA.md` §3).
- `RailCapabilities`, `PayRequest`, `Quote`, `Receipt` — generated Go structs.
  Amounts stay `uint64` minor-units integers, never a float.
- `PatalaError` — a Go error type per `patala_core::Error` variant
  (`Unsupported`, `Rail`, `InvalidRequest`, `CrossClassFailover`,
  `AllRailsFailed`). `Verify` failing closed is still `(false, nil)`, never an
  error — same contract as the core trait and the Python binding.
- `PatalaRail` — the one type Go code touches: `Id()`, `Capabilities()`,
  `Quote()`, `Charge()`, `Verify()`. Today its only constructor is
  `PatalaRailNewMock(...)`, built on `patala_core::MockRail`. When a real rail
  (`patala-solana`/`patala-stellar`/`patala-hyperswitch`) grows a constructor
  in `patala-py`, that constructor becomes reachable here too, the next time
  bindings are regenerated — no redesign, same as `patala-py`'s "adding a
  real rail later" story.
- `PatalaRailNewFiat(provider string, config map[string]string) (*PatalaRail,
  error)` — reaches `patala-fiat`'s 20 processor adapters (Stripe, Paystack,
  Adyen, ...) plus the always-on `manual` rail through ONE by-name registry
  constructor, generated the moment `patala-py`'s cdylib was built with
  `--features fiat` (see "`patala-fiat` (20 processor adapters)" below —
  this is generated Go, not hand-written, from the exact same
  `#[uniffi::export]` surface `patala-py`'s own `src/fiat.rs` defines).
- `PatalaFiatProviders() []string` — every fiat provider name reachable via
  `PatalaRailNewFiat` in THIS specific build (a free function, not a
  `PatalaRail` method — UniFFI does not currently support exporting a plain
  associated function with no receiver/constructor from inside an `impl`
  block, so this is generated at package scope instead).

## `patala-fiat` (20 processor adapters, one by-name constructor)

Same design `patala-py/README.md` documents in full (config-key table,
per-provider defaults, honesty notes) — this section is the Go-specific
delta only.

**Build the cdylib with the fiat feature, then regenerate bindings** (the
default `make generate`/`make build`/`make run-example`/`make test` targets
are UNCHANGED and still build a MockRail-only, fiat-free cdylib — see
"Build & run" below for what's new):

```bash
cd patala-go
make FEATURES=fiat-all generate   # or e.g. FEATURES=fiat-stripe,fiat-paystack
```

**Calling it from Go:**

```go
rail, err := patala.PatalaRailNewFiat("manual", map[string]string{})
// or, once fiat-stripe is compiled in:
rail, err := patala.PatalaRailNewFiat("stripe", map[string]string{
    "secret_key":     "sk_live_...",
    "webhook_secret": "whsec_...",
})
```

`config` keys are plain Go `map[string]string` — the exact same field
names `patala-py/README.md`'s table documents (they come from the same
Rust `#[uniffi::export]` surface, so there is nothing Go-specific to
relearn). An unknown provider name, or a provider whose Cargo feature
was not compiled into this build's cdylib, both return a Go `error`
wrapping `PatalaError.InvalidRequest` — never a panic.

**Why a Go build tag, not just a Cargo feature:** Go has no per-dependency
optional-feature mechanism the way Cargo does, so
`examples/fiatroundtrip/main.go` (which calls `PatalaRailNewFiat`/
`PatalaFiatProviders`) carries a `//go:build fiat` constraint. Without
`-tags fiat`, `go build ./...`/`go test ./...` skip that file/directory
entirely — this is what keeps the plain `make build`/`make test`/
`make run-example` targets working unchanged against a MockRail-only
cdylib (they never pass `-tags fiat`); only `make run-example-fiat`/
`make test-fiat` do. This is a Go-side concern only — `patala-py`'s own
Rust code has no equivalent tag, since Cargo features already solve this
for Rust.

## The cgo cost — read this first

This is not a footnote. `uniffi-bindgen-go`'s generated Go file contains
`import "C"` and hand-written cgo glue that calls into the UniFFI C-ABI
scaffolding compiled into `libpatala_py.{dylib,so}`. Concretely, choosing this
binding over the sidecar means:

- **`CGO_ENABLED=1` is mandatory.** The generated bindings will not compile
  with `CGO_ENABLED=0`. If your build pipeline currently sets
  `CGO_ENABLED=0` (many Go projects do, deliberately, for exactly the reasons
  below), importing `patala-go` forces you to unset it for any binary that
  imports it.
- **A C toolchain is required at build time** (`cc`/`clang`/`gcc` — whatever
  `cgo` shells out to on your platform), not just a Go toolchain. CI images
  that only install Go now need a C compiler too.
- **This breaks a pure-Go static single binary.** A binary that imports
  `patala-go` dynamically links against `libpatala_py.{dylib,so}` (see the
  `CGO_LDFLAGS`/`-L`/`-l` flags in `Makefile`) — it is no longer a single
  self-contained static executable. Verified concretely in this environment:
  `otool -L` on the built `examples/roundtrip` binary shows a direct
  `LC_LOAD_DYLIB` entry pointing at the **build-time absolute path** of
  `libpatala_py.dylib` (Rust's default dylib install name is not
  rpath-relative), alongside `CoreFoundation`/`libSystem`. The compiled Rust
  library has to travel with the binary (or the install name has to be
  fixed up with `install_name_tool`/`-rpath`, or `LD_LIBRARY_PATH`/
  `DYLD_LIBRARY_PATH` has to be set) at *runtime*, on every machine the Go
  binary runs on, matching that machine's OS/arch — it will not run
  standalone on a different machine without one of those fixes, unlike a
  static Go binary that just runs.
- **Cross-compilation gets much harder.** Plain `GOOS=linux GOARCH=arm64 go
  build` from a macOS/amd64 host, which "just works" for pure Go, now needs a
  matching cross C toolchain *and* a `libpatala_py` built for that same
  target triple. This is exactly the pain `CGO_ENABLED=0` exists to avoid.
- **Slower, more fragile builds.** cgo compilation is slower than pure Go,
  and adds a second build system's (Cargo's) failure modes to your Go build:
  if `libpatala_py.{dylib,so}` isn't already built and on the linker's
  search path, `go build` fails at the link step, not with a Go-shaped error.

**If your Go project prizes a pure-static binary — `cackle`
(`/Users/pc/code/vulos/cackle`) is the concrete example in this suite — do
not reach for `patala-go`.** Use **[`patala-sidecar`](../patala-sidecar/)**
instead: a loopback-only HTTP server over the exact same `patala-core`
surface (`quote`/`charge`/`verify` as JSON, token-authenticated,
`127.0.0.1`-only — see its README's threat model). A Go program talks to it
with `net/http` and `encoding/json` from the standard library, zero cgo, zero
FFI, and the calling binary stays pure Go and fully static. The trade is
worse latency and JSON instead of native Go types, in exchange for keeping
`CGO_ENABLED=0`, trivial cross-compilation, and one fewer thing that can break
the build. For any Go binary where "stays a single static executable" is a
hard requirement, that trade is almost always worth it.

## Build & run, step by step

### 1. Install `uniffi-bindgen-go` — pin the version to match this workspace

The workspace's `patala-py/Cargo.toml` pins `uniffi = "0.29"` (`Cargo.lock`
resolves it to `0.29.5`). `uniffi-bindgen-go` is versioned against a specific
`uniffi-rs` release (its own README documents this table), so the installed
generator **must** target `0.29.5` too, or generation can fail against
metadata this workspace's cdylib actually embeds. The matching tag is
`v0.5.0+v0.29.5`:

```bash
cargo install --locked --git https://github.com/NordSecurity/uniffi-bindgen-go \
    --tag v0.5.0+v0.29.5 uniffi-bindgen-go
```

(Do **not** `cargo install uniffi-bindgen-go --git ... ` without `--tag` —
`@latest`/the default branch currently builds `v0.7.x`, which targets
`uniffi-rs 0.31.0` and does not match this workspace's `0.29.5`.)

This is a Rust binary, installed via `cargo install`, not `go install` —
`uniffi-bindgen-go`'s own source tree is a Cargo workspace (see its
`bindgen/Cargo.toml`), not a Go module, even though what it *emits* is Go
code.

### 2. Build the Rust cdylib

```bash
# From the patala workspace root.
cargo build -p patala-py
```

This produces `target/debug/libpatala_py.dylib` (macOS) or
`target/debug/libpatala_py.so` (Linux) — see "Which cdylib, which package name?" above for why
`patala-go` reuses this rather than adding a new Rust crate.

### 3. Generate the Go bindings

```bash
# From patala-go/.
mkdir -p bindings
uniffi-bindgen-go ../target/debug/libpatala_py.dylib \
    --out-dir bindings \
    --library \
    --config uniffi.toml
cp ../target/debug/libpatala_py.dylib bindings/patala/
# (Linux: libpatala_py.so throughout.)
```

`--library` is the auto-detecting mode added for UniFFI's "library mode" —
it reads the compiled cdylib's embedded metadata directly, the same way
`patala-py`'s own `--library` invocation works for Python (see
`patala-py/README.md`'s "Build & run"); there is no `.udl` file in this
tree, only proc-macro (`#[uniffi::export]`) definitions, so `--library` mode
is required, not optional. This produces `bindings/patala/patala_py.go` +
`bindings/patala/patala.h` — see "Which cdylib, which package name?" above
for why the directory is `patala` but the file's `package` clause still
reads `patala_py`.

### 4. Build / run against it (cgo flags required — see "The cgo cost")

```bash
cd patala-go
CGO_ENABLED=1 \
  CGO_LDFLAGS="-lpatala_py -Lbindings/patala" \
  DYLD_LIBRARY_PATH="bindings/patala:$DYLD_LIBRARY_PATH" \
  LD_LIBRARY_PATH="bindings/patala:$LD_LIBRARY_PATH" \
  go run ./examples/roundtrip
```

`bindings/` is gitignored (see `.gitignore` in this directory) — like
`patala-py/bindings/`, it's build output reproduced by the four steps above,
not checked in.

### All four steps as one command

```bash
cd patala-go
make run-example   # runs steps 1(check)-4 via `make generate` + `go run`
# or:
make build         # steps 1(check)-3, then `go build ./...`
make test          # steps 1(check)-3, then `go test ./...`
```

All three targets above build a MockRail-only cdylib (`FEATURES` defaults to
empty — the exact status quo before `patala-fiat` was exposed here). To get
the fiat surface too:

```bash
make run-example-fiat   # FEATURES=fiat-all generate, then `go run -tags fiat ./examples/fiatroundtrip`
make test-fiat          # FEATURES=fiat-all generate, then `go test -tags fiat ./...`
# or by hand, any combination:
make FEATURES=fiat-stripe,fiat-paystack generate
```

`make check-uniffi-bindgen-go` only verifies the binary is on `PATH` and
prints the install command above if it's missing — `make` does not install
Rust/Go toolchain binaries on your behalf.

## Example

`examples/roundtrip/main.go` is the Go analogue of
`patala-py/examples/smoke_test.py`: it builds a `MockRail`-backed
`PatalaRail`, reads `Capabilities()` (asserting `Class`, `HoldsFunds`,
`Reversible`, `Currencies`), does a `Quote` → `Charge` → `Verify` round trip
(asserting a genuine receipt verifies `true` and a tampered one verifies
`false` — fail-closed, `PATALA.md` §3/§8), and asserts an unsupported
currency surfaces as a typed error rather than a crash.

`examples/fiatroundtrip/main.go` (`//go:build fiat`, see "`patala-fiat`"
above) is the Go analogue of `patala-py/src/fiat.rs`'s own tests: it lists
`PatalaFiatProviders()`, builds `"manual"` via `PatalaRailNewFiat` and does
a genuine, fully offline `Charge` → `Verify` round trip against it
(asserting the honestly-pending contract: `AmountMinor == 0` and
`Verify() == false` until a separate, direct-Rust caller of `ManualRail`'s
own `mark_paid` — not part of the `PaymentRail` trait, so unreachable
through this generic by-name surface — confirms it), asserts an unknown
provider name surfaces a typed `InvalidRequest` error, and CONSTRUCTS (never
charges/verifies, which would dial a real processor) a `"stripe"` rail from
a config map to prove `RailClass`/`HoldsFunds` come through correctly for a
feature-gated processor adapter too.

## Verified in this environment (2026-07-21)

Every step in "Build & run, step by step" was actually executed here, against
a real toolchain, not just written:

- **Installed** `uniffi-bindgen-go` from source: `cargo install --locked
  --git https://github.com/NordSecurity/uniffi-bindgen-go --tag
  v0.5.0+v0.29.5 uniffi-bindgen-go`. `uniffi-bindgen --version` printed
  `uniffi-bindgen 0.5.0+v0.29.5`, confirming the version match against this
  workspace's `uniffi = 0.29.5`.
- **`cargo build -p patala-py`** — built `target/debug/libpatala_py.dylib`.
- **`uniffi-bindgen-go <lib> --out-dir bindings --library --config
  uniffi.toml`** exited `0` and produced `bindings/patala/patala_py.go` +
  `bindings/patala/patala.h` from that real cdylib's embedded metadata.
- **`go vet ./bindings/...`** — clean except one benign, expected
  `possible misuse of unsafe.Pointer` note in generated FFI glue (the same
  class of warning any hand-rolled cgo binding produces; not a real bug).
- **`CGO_ENABLED=1 CGO_LDFLAGS="-lpatala_py -Lbindings/patala" go build
  ./...`** — succeeded (one harmless `ld: warning: ignoring duplicate
  libraries` note).
- **`CGO_ENABLED=0 go build ./...`** — genuinely **fails**, confirming the
  "cgo is mandatory" claim above with real output: `build constraints
  exclude all Go files in .../bindings/patala`.
- **The example actually ran** — `go run ./examples/roundtrip` (and
  separately `make run-example`, exercising the whole
  install→build→generate→run pipeline as one command) printed real output
  from a genuine call through cgo into the compiled Rust cdylib:

  ```
  capabilities OK: class=2 currencies=[USDC USD] holds_funds=false
  quote OK: total_minor=1250 (uint64, not float)
  charge OK: receipt rail_id="mock" amount_minor=1250
  verify OK: genuine receipt verified true
  verify OK: tampered receipt verified false (fail-closed)
  error mapping OK: unsupported currency raised PatalaError: InvalidRequest: Message=rail mock does not support currency EUR

  ALL GO ROUNDTRIP ASSERTIONS PASSED
  ```

  (`class=2` is `RailClassNonCustodialFinal`'s underlying `uint` — UniFFI Go
  enums are plain integer-backed types, printed numerically by `%v` unless a
  caller adds a `String()` method; not a bug, just Go's default enum
  printing.)
- **`otool -L` on the built example binary** confirmed the non-static-binary
  claim above with a real linked-library list, including an absolute
  build-path `LC_LOAD_DYLIB` reference to `libpatala_py.dylib`.
- **`cargo build` at the workspace root still succeeds** after all of the
  above — this package only reads `target/debug/libpatala_py.dylib`, never
  writes into the workspace, and adds no workspace `Cargo.toml` member line
  (see "Which cdylib, which package name?" — no new Rust crate was needed).

One transient wrinkle, for the record: a concurrently-running agent was
mid-way through adding a new `patala-fiat` workspace member while this work
was in progress, which briefly made `cargo build -p patala-py` fail at the
*workspace-manifest-parsing* stage (an unrelated crate's manifest not yet
valid) — not a problem in `patala-py` or this package. To keep making
progress without touching `patala-fiat` or waiting indefinitely on someone
else's in-flight work, bindings were first generated and the example first
run against a scratch copy of `patala-core`+`patala-py` (plus the rail
crates they optionally path-depend on) built in isolation. Once
`patala-fiat` gained its `src/lib.rs`, the real in-tree workspace built
cleanly again, and everything above — generation, build, and the example run
— was re-executed and re-verified against the real
`target/debug/libpatala_py.dylib` in this repo (not the scratch copy); the
output shown above is from that final, in-tree run.

`maturin`/Python tooling was not needed anywhere in this pipeline — this is a
pure Rust+Go+cgo flow.

## `patala-fiat` exposure: verified in this environment (2026-07-21)

Every step below was actually executed here, against the same real
toolchain as above:

- **`cargo build -p patala-py --features fiat-all`** — built a cdylib with
  all 20 `patala-fiat` processor adapters compiled in.
- **`uniffi-bindgen-go <lib> --out-dir bindings --library --config
  uniffi.toml`** against that cdylib — exited `0` and generated
  `PatalaRailNewFiat(provider string, config map[string]string)
  (*PatalaRail, error)` and `PatalaFiatProviders() []string` alongside
  everything `make generate` already produced.
- **`go build`/`go vet -tags fiat ./...`** — clean except the same one
  benign `possible misuse of unsafe.Pointer` note as before (unchanged by
  the fiat surface).
- **`make run-example-fiat`** (the whole `FEATURES=fiat-all generate` →
  `go run -tags fiat ./examples/fiatroundtrip` pipeline, one command) —
  printed real output from a genuine call through cgo into the compiled
  Rust cdylib:

  ```
  fiat providers compiled into this build: [adyen btcpay checkoutcom coinbasecommerce flutterwave iyzico lnbits manual mercadopago midtrans mollie opennode payfast paypal paystack payu razorpay square stripe xendit yoco]
  manual capabilities OK: class=1 holds_funds=false
  manual charge/verify OK: honestly pending (amount_minor=0, verify=false) until a human confirms it
  unknown-provider error mapping OK: PatalaError: InvalidRequest: Message=unknown fiat provider "not-a-real-processor"; see patala-fiat's registry (PORTING.md) for the supported list
  stripe construction-only OK: class=1 holds_funds=true (never charged/verified -- no live network)

  ALL GO FIAT ROUNDTRIP ASSERTIONS PASSED
  ```

  That is 21 provider names (`manual` + all 20 processor adapters), a
  genuine offline `manual` charge → verify round trip through cgo (proving
  the by-name-provider → `map[string]string` config → real `PaymentRail`
  plumbing works end to end, not just the type declarations), a real typed
  error for an unrecognised provider name, and a real construction (not a
  charge/verify — see "Example" above for why) of a feature-gated `stripe`
  rail from a Go `map[string]string`, confirming `RailClassCustodialReversible`
  and `HoldsFunds == true` come through the FFI boundary correctly.
- **`make test-fiat`** — `go test -tags fiat ./...` reports `[no test
  files]` for every package (there are no `_test.go` files in this
  directory; `examples/fiatroundtrip` is exercised via `go run`, matching
  `examples/roundtrip`'s own precedent) but this also proves the whole
  package (including `examples/fiatroundtrip`, gated on `-tags fiat`)
  still type-checks and links cleanly.
- **The plain (non-fiat) targets were re-verified unaffected**: after the
  above, `make build`/`make test`/`make run-example` (no `FEATURES`, no
  `-tags fiat`) were re-run against a freshly regenerated MockRail-only
  cdylib and passed exactly as they did before this task — confirming the
  `//go:build fiat` constraint on `examples/fiatroundtrip/main.go` is what
  keeps the default pipeline from needing (or breaking on the absence of)
  `PatalaRailNewFiat`/`PatalaFiatProviders`.

**UNVERIFIED AGAINST LIVE** for all 20 processor adapters, same as
`patala-py`'s own fiat tests and `patala-fiat` itself — the Go example only
ever constructs `stripe` (never charges/verifies it) and only ever
charges/verifies `manual` (which never touches the network at all).

### What a cackle consumer needs to know

- `PatalaRailNewFiat`/`PatalaFiatProviders` only exist in bindings
  generated from a cdylib built with `--features fiat` (plus whichever
  `fiat-<name>` features you actually need) — a plain `cargo build -p
  patala-py` (what `make build`/`make test`/`make run-example` still do by
  default) does **not** include them. Regenerate with
  `make FEATURES=fiat-all generate` (or a narrower feature list) before
  wiring cackle onto this path.
- Every `config` value is a string, even for numeric/boolean fields (Go's
  `map[string]string`, matching UniFFI's `HashMap<String, String>` on the
  Rust side) — e.g. `"settlement_days": "2"`, `"requires_kyc": "true"`,
  not native Go `int`/`bool`.
- `manual`'s `Charge`/`Verify` alone is not useful for a real payment flow
  through this generic surface (see "Example" above) — cackle would need
  either a real processor adapter (`fiat-stripe` et al.) or to talk to
  `ManualRail`'s `mark_paid`/`mark_failed` directly in Rust (outside this
  FFI boundary) if it wants the manual/bank-transfer flow to actually
  settle.
- This binding still carries the full cgo cost described above ("The cgo
  cost — read this first") — nothing about `patala-fiat` changes that
  trade-off. If cackle needs to stay `CGO_ENABLED=0`/pure-static,
  `patala-sidecar` (once it grows the same by-name fiat endpoint) remains
  the alternative path, not this one.
