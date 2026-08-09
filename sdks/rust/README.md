# patala for Rust

**Direct mode is a dependency line.** patala's core *is* Rust, so a Rust host
does not bind to patala — it contains it. There is no FFI, no shared library,
no `dlopen`, no ABI version probe, no `unsafe`, no build script, no platform
matrix, and nothing to install. Add one line to `Cargo.toml` and call the
trait.

```toml
[dependencies]
# patala is not on crates.io — nothing in this repo is published yet
# (`SECURITY.md`). Vendor it by path, as this directory's own Cargo.toml does,
# or by git URL. Swap in a version when it publishes; nothing else changes.
patala-core = { path = "../../patala-core" }
```

```rust
use patala_core::{MockRail, PayRequest, PaymentRail, RailClass};

let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
let receipt = rail.charge(&req).await?;
assert!(rail.verify(&receipt).await?);          // the entitlement check
```

That is the whole binding. Everything below is either the sidecar (a real
choice, for a real reason) or the measurements behind the claim that direct
costs you nothing.

| | what it is | what it costs |
| --- | --- | --- |
| **Direct** | `use patala_core` — the substrate, in your process, as ordinary types | **98,928 bytes** of release binary; no second artifact, no runtime, no `unsafe` |
| **Sidecar** | `patala-sidecar` as a child process on `127.0.0.1`, driven over HTTP | a process, a port, JSON in place of types, ~160 µs per call |

**Default: direct.** Rust is the one language in `sdks/` where that
recommendation carries no asterisk. Take the sidecar when you want a rail's
signing key to live in exactly one process — see [Which mode](#which-mode).

## Run the examples

Offline, `MockRail` only, no credentials, no network:

```
./sdks/rust/run.sh            # both
./sdks/rust/run.sh direct
./sdks/rust/run.sh sidecar
```

Real output, this machine — macOS 15.7.3 (24G419), Apple silicon, rustc
1.97.1, cargo 1.97.1, patala 0.1.0:

```
==> direct (in-process, no FFI)
patala direct — in-process, no FFI, no shared library
rail:      mock
caps:      NonCustodialFinal / wallet address, signed final receipt
           settlement=Instant holds_funds=false reversible=false currencies=["USDC", "USD"]
quote:     12.50 USDC + 0.00 USDC fee = 12.50 USDC (expires 300s)
charge:    12.50 USDC ref=order-1 rail=mock proof=32B  [12.708µs]
verify:    Ok(true)  <- gate entitlement on exactly this
tampered:  Ok(false) — a refusal is DATA, not an error
dest:      mock:wallet:alice      StructurallyValid
dest:      stellar:wallet:alice   WrongNetwork  (DO NOT SEND)
dest:      not-an-address         Malformed  (DO NOT SEND)
dest:      cus_opaque_token       Unknown  is_refusal=false — and not an approval either
refused:   Error::InvalidRequest("rail mock does not support currency EUR")
refund:    Error::Unsupported("refund") — see docs/compensating-payments.md
webhook:   Error::Unsupported("verify_webhook")
failover:  primary failed -> settled on "backup"
guard:     refused NonCustodialFinal -> CustodialReversible — the payer was promised one of these

OK — offline, MockRail only, no value moved.

==> sidecar (child process over HTTP)
patala sidecar — child process on 127.0.0.1:53705
binary:    /Users/pc/code/vulos/patala/target/release/patala-sidecar
health:    ok
no token:  HTTP 401
caps:      NonCustodialFinal settlement=Instant holds_funds=false currencies=["USDC", "USD"]
quote:     total_minor=1250 (an integer on the wire, never a float)
charge:    1250 USDC ref=order-1 rail=mock  [159.375µs incl. loopback]
verify:    HTTP 200 {"valid":true}
tampered:  HTTP 200 {"valid":false}  <- 200, and false
dest:      HTTP 200 status="WrongNetwork" is_refusal=true human_must_confirm=true
typo:      HTTP 400 — a bad request is not a verdict
no rail:   HTTP 404 — the registry is mock-only
webhook:   HTTP 501 — the mock has no processor, so it invents no event

OK — offline, MockRail only, no value moved. Child reaped on exit.
```

Those two timings are single cold calls from an example, not a benchmark. Do
not quote them as a measurement of patala; quote them as the reason latency is
not the argument for either mode.

## Why this crate is a standalone workspace

[`Cargo.toml`](Cargo.toml) opens with an empty `[workspace]`, so these examples
are **not** members of patala's root workspace. The root's `default-members` is
what keeps a bare `cargo build` at the repo root offline and reqwest-free;
examples that spawn a child process and speak HTTP should not widen it. The
side effect is the useful part: this manifest is exactly what a consumer's own
crate looks like, `path` dependency aside.

## Direct

[`examples/direct.rs`](examples/direct.rs) is the tour. What it shows, and why
each one is here:

- **The settlement class is a type.** `match caps.class { CustodialReversible
  => …, NonCustodialFinal => … }` is exhaustive, so a class added later is a
  compile error in your UX code instead of a wrong payment form in production.
  Across a JSON boundary it is a string, and a string has no exhaustiveness.
- **`charge` → `verify`.** `charge` returning `Ok` is not the entitlement; the
  `Receipt` is, and `verify` is what re-derives that it still holds. Store the
  receipt, gate on `verify`.
- **A tampered receipt is `Ok(false)`, not `Err`.** This is the fail-closed
  contract and Rust makes it un-confusable: `Err` means "I could not check"
  (retryable), `Ok(false)` means "I checked and it does not hold" (never
  retry, never grant). Bindings that flatten both into an exception lose this;
  here it is two variants of a `Result`.
- **`validate_destination` returns no `Result` at all.** "I cannot check this"
  is `DestinationStatus::Unknown`, a verdict, because a caller must handle it
  as carefully as a refusal and an error is too easy to swallow. Every verdict
  — including `StructurallyValid` — carries `human_must_confirm: true`, because
  patala does not detect exchange-owned addresses and will not guess.
- **`refund` on a `NonCustodialFinal` rail is `Error::Unsupported`.** Finality
  is the point of that class. Paying a customer back is a *second charge* to an
  address the customer supplies — never the address the money came from, which
  is very often an exchange deposit address. See
  [`docs/compensating-payments.md`](../../docs/compensating-payments.md).
- **`FailoverRail`, and its cross-class guard.** This is the piece **only Rust
  gets**. The C ABI and the sidecar each hand out one rail at a time, so a
  caller in C, Swift or over HTTP has to rebuild failover *and its guard* by
  hand. In-process it is `FailoverRail::new(vec![…])`, and it refuses to
  complete a `NonCustodialFinal` request on a `CustodialReversible` rail unless
  you write `.allow_cross_class(true)` and mean it.

### Money

Integer minor units plus a currency string. Never a float — not in the type,
not in your formatting, not in your database. `PayRequest.amount_minor` is a
`u64` and the compiler will not let you put a `f64` in it, which is one more
class of bug that direct mode deletes rather than documents.

### async

`PaymentRail`'s methods are `async fn`, and `patala-core` picks no executor —
its entire dependency list is `async-trait`, `serde`, `thiserror`. Bring your
own; the examples use tokio because the rest of the workspace does. A
current-thread runtime is enough.

## What direct mode actually costs

Measured on this machine, release profile, 2026-08-09:

| | bytes |
| --- | --- |
| the example binary with `patala-core` linked | 800,528 |
| the identical binary with the patala calls removed | 701,600 |
| **patala's contribution** | **98,928** |

That delta includes the 250-line example itself, so patala-core's own share is
smaller. Reproduce it: build `examples/direct.rs`, then build a file
containing nothing but `#[tokio::main] async fn main() { println!("baseline"); }`,
and subtract.

For contrast, the same round trip from C, C++ or Swift ships and locates
`libpatala_ffi.dylib` — **844,656 bytes** as a separate file, plus a version
probe, plus `patala_free` discipline on every returned string. Rust pays none
of that. It is not a tie that Rust wins on style; the artifact is not there.

And the list of things a direct binding usually has to warn you about is empty
here, because patala is Rust all the way down: **no second language runtime, no
GC, no scheduler installed behind your back, no signal handlers replaced, no
fork hazard, no `dlclose` that hangs, no `unsafe` in this crate.** If you have
read the SDK READMEs for llmux or openrate, those carry a real list of
Go-runtime caveats. They are true there and false here, and they have
deliberately not been copied.

That is measured, not argued. The languages that reach patala through
`libpatala_ffi` had llmux's own signal probe run against both libraries on this
machine, in the same JVM — the harshest host for this, since HotSpot installs
handlers for `SIGSEGV`, `SIGBUS` and `SIGFPE` and depends on them:

| | HotSpot signal handlers replaced | handler flags altered | shared library |
|---|---|---|---|
| **patala** | **0** | **0** | 844,656 bytes |
| llmux | 5 | 3 | 12,787,504 bytes |

Rust does not even pay the 844,656 bytes: in direct mode there is no shared
library in the picture at all.

## Sidecar

[`examples/sidecar.rs`](examples/sidecar.rs) spawns `patala-sidecar`, waits for
`/healthz`, and drives the same round trip over HTTP.

The reason to choose it from Rust is **not** reach — you already have the
crate. It is **key isolation**: a non-custodial rail's signing key lives inside
whichever process calls `PaymentRail::charge`. Link `patala-core` into five
services and the key is in five address spaces, so a dependency-confusion bug
in any one of them is a path to it. Route them all through one sidecar and it
lives in exactly one narrow process that does nothing else. `patala-sidecar`'s
own README states the limits of that honestly: it defends against an unrelated
local process, not against a co-resident attacker running as the same user.

Things the example demonstrates that you will otherwise learn the hard way:

- **The token gate is fail-closed and covers every `/v1` route**, including the
  read-only capabilities lookup. No token is `401`, and the server refuses to
  start at all without `PATALA_SIDECAR_TOKEN`.
- **A tampered receipt is HTTP `200` with `{"valid":false}`.** Read the body,
  not the status code. Mapping a rail's honest refusal onto an HTTP error is
  exactly how an unpaid order becomes an entitlement the day someone adds a
  retry on 4xx. Same for `validate-destination`: all five verdicts are `200`,
  and a `400` means the *request* was malformed and carries no verdict at all.
- **`is_refusal` is on the wire** because it is a *method* on
  `DestinationVerdict`, and a method does not survive JSON. Re-deriving it from
  `status` fails open for any status added later.
- **The registry is mock-only.** `/v1/rails/solana` is a `404` — the sidecar has
  never heard of it. That is `patala-sidecar/src/registry.rs`, pinned by its own
  test, not a bug in the example.
- **The child is reaped by `Drop`**, on the happy path and on a panic unwind.

The example's HTTP client is ~80 lines of `std::net::TcpStream` so the file has
no dependency you have to trust in order to read it. **It is not an HTTP
client**: one request to `127.0.0.1`, `Connection: close`, no TLS, no
keep-alive, no redirects, no retries. Real programs use `reqwest` or `ureq` and
write the same six lines. The types are not hand-rolled, though — the sidecar
serializes `patala_core`'s own `Receipt`, `Quote` and `RailCapabilities`, so
they deserialize straight back into the same structs the direct example uses.
There is no second DTO set to drift.

### No streaming, in either mode

patala has no streaming operation, so neither this crate nor the C ABI nor the
sidecar has one. If you came here from llmux's SDKs looking for the iterator
that wraps `llmux_stream`, its absence is not an omission — nothing patala does
produces a sequence of chunks.

## Which mode

Direct, unless one of these is true:

- **You want one process to hold the signing key** for several services. That is
  the sidecar's whole purpose.
- **You want the key out of a process that runs third-party code** — a plugin
  host, an extension runtime, anything where "inside your process" stops being a
  trust boundary you control.
- **You are not really a Rust host.** If the Rust service is one of six
  languages in the stack, one sidecar beats six bindings.

Notably absent from that list: performance, safety, platform support and
packaging. None of them is a reason to reach for the sidecar from Rust.

## Also in this repo

- [`docs/rust.md`](../../docs/rust.md) — the prose guide.
- [`patala-core`](../../patala-core/) — the crate itself; its rustdoc is the
  reference for every type named above.
- [`patala-sidecar`](../../patala-sidecar/) — the server, its threat model and
  its endpoint table.
- [`sdks/c`](../c/), [`sdks/cpp`](../cpp/), [`sdks/swift`](../swift/) — the same
  two modes for the languages that need the C ABI to get here.
