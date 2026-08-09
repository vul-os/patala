# Quickstart

Five quickstarts, one per audience. Every one of them does the same thing —
a `charge` → `verify` round trip against `MockRail`, the offline default rail
— because that round trip *is* the seam. Swap the mock for a real rail later
and none of the code below changes shape.

Nothing here needs a chain, a processor, a merchant account or an internet
connection. That is the point of starting with the mock: you can wire your
whole payment path, including the failure branches, before you have
credentials for anything.

> **One rule to carry out of this page.** Gate entitlement on `verify`
> returning `true`, never on `charge` returning `Ok`. A `charge` that
> succeeded means *the rail accepted the request*; on several rails it comes
> back with `amount_minor: 0` because the buyer has not paid yet. `verify` is
> the question "did this actually settle", and it re-derives the answer from
> the rail every time.

## Before you start: patala is not published yet

There is no `cargo add patala-core`, no `pip install patala-py`, and no
`go get`. `SECURITY.md` says it plainly: **patala publishes no artifacts
today.** Vendor it by path or by git URL from
<https://github.com/vul-os/patala> until that changes. Every command below
assumes you have the repo checked out.

## 1. Rust

Add the core crate (plus whichever rail crates you want — none of them are
required, and none are pulled in by default):

```toml
[dependencies]
# Not on crates.io yet — vendor by path or git until it publishes.
patala-core = { git = "https://github.com/vul-os/patala" }
```

```rust
use patala_core::{MockRail, PayRequest, PaymentRail, RailClass};

let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);

let req = PayRequest {
    amount_minor: 500, // 5.00 USDC — integer minor units, never a float
    currency: "USDC".into(),
    destination: "wallet-or-processor-token".into(),
    reference: "order-1".into(),
};

// `charge` returns the Receipt — the entitlement.
let receipt = rail.charge(&req).await.unwrap();
assert_eq!(receipt.reference, "order-1");

// Gate on `verify` returning `Ok(true)`, never on `charge` merely having
// returned `Ok`: a receipt can be stored and re-checked later, and only
// `verify` re-derives whether it still holds.
assert!(rail.verify(&receipt).await.unwrap());
```

That is `patala-core`'s own crate-level doctest, word for word — it runs
under `cargo test --doc` on every `make check`, so it cannot rot into
something that no longer compiles.

```bash
cargo test -p patala-core
```

More: [Rust, embedded](rust.md).

## 2. Python

`patala-py` is a UniFFI binding: a compiled cdylib plus a generated `ctypes`
wrapper. There is no build backend to install and no native headers to find —
`python3` and `cargo` are the whole toolchain. The generated module is
`patala.py` (named after the UniFFI namespace), and it loads
`libpatala_py.{dylib,so}` from its own directory.

```bash
# From the workspace root. `make smoke-python` runs exactly these four steps.
cargo build -p patala-py --features fiat-stripe

cargo run -p patala-py --bin uniffi-bindgen -- generate \
    --library target/debug/libpatala_py.dylib \
    --language python \
    --out-dir patala-py/bindings/python
#   (Linux: target/debug/libpatala_py.so)

cp target/debug/libpatala_py.dylib patala-py/bindings/python/

PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py
```

```python
from patala import PatalaRail, PayRequest, RailClass

rail = PatalaRail.new_mock(
    id="mock",
    _class=RailClass.NON_CUSTODIAL_FINAL,
    currencies=["USDC"],
    fee_minor=0,
    failing=False,
)

req = PayRequest(
    amount_minor=1_250,       # int, never a float
    currency="USDC",
    destination="dest-anything",
    reference="order-1",
)

receipt = rail.charge(req)
assert rail.verify(receipt) is True   # fail-closed: a tampered receipt verifies False
```

The methods are **synchronous**. A one-shot script does not have to stand up
an `asyncio` event loop just to call `charge()`; the binding blocks on a
Tokio runtime it owns internally. See [Python binding](python.md) for the
reasoning and for the wheel-packaging story.

## 3. Go

The Go package is generated from the *same* UniFFI surface the Python binding
uses — it is not a second, hand-written adapter. It uses cgo, and that has
real consequences for your build; read [Go binding](go.md) before you commit
to it, and [Choosing a mode](choosing-a-mode.md) if a pure-static binary
matters to you.

```bash
# From patala-go/. `make test` generates the bindings first, then runs the
# suite through a gate that fails when zero tests ran.
make test
```

```go
import patala "github.com/vul-os/patala/patala-go/bindings/patala"

rail := patala.PatalaRailNewMock(
    "mock", patala.RailClassNonCustodialFinal, []string{"USDC"}, 0, false,
)

req := patala.PayRequest{
    AmountMinor: 1_250, // uint64, never a float
    Currency:    "USDC",
    Destination: "dest-anything",
    Reference:   "order-1",
}

receipt := must(rail.Charge(req))
valid := must(rail.Verify(receipt)) // fail-closed: a tampered receipt verifies false
```

Adapted from `patala-go/examples/roundtrip/main.go`, which runs for real over
cgo in CI.

## 4. Any language, over HTTP

`patala-sidecar` puts the same core behind a loopback HTTP API. No FFI, no
generated binding, no Rust toolchain on the calling side — an HTTP client and
a JSON parser are the entire dependency list.

```bash
export PATALA_SIDECAR_TOKEN=$(openssl rand -hex 32)
cargo run -p patala-sidecar
# patala-sidecar listening on 127.0.0.1:8420 (loopback only)
```

The server **refuses to start** without that token. There is no
auto-generated fallback and no unauthenticated-by-default path.

```bash
curl -s http://127.0.0.1:8420/healthz
# ok

curl -s http://127.0.0.1:8420/v1/rails/mock/charge \
  -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_minor":1250,"currency":"USDC","destination":"dest","reference":"order-1"}'
```

Then post that receipt back to `/v1/rails/mock/verify` and read `valid`. Note
that an unverifiable receipt is `200` with `{"valid": false}`, **not** an HTTP
error — a fail-closed answer is data. The full endpoint table, including the
error mapping and the five-state destination verdict, is in
[The sidecar HTTP API](sidecar.md).

One thing to know before you plan around it: the sidecar's rail registry is
**mock-only** today. The server is real and tested; the set of rails reachable
through it is one. See [Status](status.md).

## 5. Your language, already packaged

The four above are the four surfaces. You probably do not have to wire one up
by hand: `sdks/` holds a **working package for fifteen languages**, each with an
in-process path and a managed-sidecar path, and each with two runnable examples
that do the exact round trip on this page. Pick a row, run it, read the README
next to it.

| | Direct | Sidecar |
|---|---|---|
| [rust](../sdks/rust/README.md) | `sdks/rust/run.sh direct` | `sdks/rust/run.sh sidecar` |
| [c](../sdks/c/README.md) | `sdks/c/run-demo.sh direct` | `sdks/c/run-demo.sh sidecar` |
| [cpp](../sdks/cpp/README.md) | `sdks/cpp/run-demo.sh direct` | `sdks/cpp/run-demo.sh sidecar` |
| [swift](../sdks/swift/README.md) | `sdks/swift/run.sh direct` | `sdks/swift/run.sh sidecar` |
| [java](../sdks/java/README.md) | `sdks/java/run-examples.sh direct` | `sdks/java/run-examples.sh sidecar` |
| [kotlin](../sdks/kotlin/README.md) | `sdks/kotlin/run-examples.sh direct` | `sdks/kotlin/run-examples.sh sidecar` |
| [dotnet](../sdks/dotnet/README.md) | `sdks/dotnet/run-examples.sh direct` | `sdks/dotnet/run-examples.sh sidecar` |
| [node](../sdks/node/README.md) | `cd sdks/node && npm run example:direct` | `cd sdks/node && npm run example:sidecar` |
| [deno](../sdks/deno/README.md) | `cd sdks/deno && deno task example:direct` | `cd sdks/deno && deno task example:sidecar` |
| [bun](../sdks/bun/README.md) | `cd sdks/bun && bun run example:direct` | `cd sdks/bun && bun run example:sidecar` |
| [python](../sdks/python/README.md) | `python3 sdks/python/examples/direct_charge.py` | `python3 sdks/python/examples/sidecar_charge.py` |
| [ruby](../sdks/ruby/README.md) | `ruby sdks/ruby/examples/direct_charge.rb` | `ruby sdks/ruby/examples/sidecar_charge.rb` |
| [php](../sdks/php/README.md) | `php sdks/php/examples/direct_charge.php` | `php sdks/php/examples/sidecar_charge.php` |
| [go](../sdks/go/README.md) | `sdks/go/examples/run.sh direct` | `sdks/go/examples/run.sh sidecar` |
| [elixir](../sdks/elixir/README.md) | `cd sdks/elixir && mix run examples/direct_charge.exs` | `cd sdks/elixir && mix run examples/sidecar_charge.exs` |

Direct mode needs `cargo build -p patala-ffi --release` first (Python and Go
build their own binding); the sidecar rows need
`cargo build -p patala-sidecar --release`. Which of the two a given language
*should* default to is a real question with a per-language answer —
[Fifteen language packages](language-packages.md) is that answer, and
[`sdks/README.md`](../sdks/README.md) is the index that ships with the code.

## What to read next

- [Choosing a mode](choosing-a-mode.md) — you have just tried all five; this
  is how to pick one on purpose.
- [The rail interface](rails-interface.md) — the seven methods, and why the
  settlement class is in the type.
- [Troubleshooting](troubleshooting.md) — for when one of the commands above
  did not do what this page said it would.
