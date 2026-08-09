# Self-host & vendor

patala is a library, not a service — there is nothing to sign up for. You
vendor the crate(s) you need into your own product and run them on your own
infrastructure. patala itself is stateless and holds no secrets: a rail is
constructed from config the *consumer* supplies each time (a fiat rail's API
keys, a crypto rail's signer).

## Four ways to consume it

Every adapter is written once, in Rust, in `patala-core` or a rail crate —
never reimplemented per language (`PATALA.md` §5, "M×1, never M×N"). The same
`charge` → `verify` round trip below is real code from this repo, shown in
each of the four languages that can reach it.

### 1. As a Rust crate, direct

Add `patala-core` (and whichever rail crates/features you need) as a
dependency and program against the `PaymentRail` trait. The default build
pulls no chain and no processor — you opt into a rail with its feature flag.
**patala isn't on crates.io yet** (`SECURITY.md`: "no crate is published to
crates.io") — vendor it by path or `git` until it is:

```toml
[dependencies]
# Not on crates.io yet (SECURITY.md) — vendor by path or git until it publishes.
patala-core    = { git = "https://github.com/vul-os/patala" }
patala-stellar = { git = "https://github.com/vul-os/patala" }  # opt in per rail
```

```bash
cargo test -p patala-core
```

### 2. As a Python binding

`patala-py` wraps the same Rust core via UniFFI and exposes **synchronous**
methods, so a one-shot script doesn't need to run an `asyncio` event loop
just to call `charge()`. Each call blocks the calling thread on a
lazily-created multi-thread `tokio::runtime::Runtime` under the hood.

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

Adapted from `patala-py/examples/smoke_test.py`, which this same round trip
runs for real under a real interpreter — see [Status](status.md).

### 3. As a Go binding

`patala-go` generates a Go package from the exact same UniFFI surface
`patala-py` exposes, via [`uniffi-bindgen-go`](https://github.com/NordSecurity/uniffi-bindgen-go)
— not a second hand-written adapter. It uses cgo: if your Go project needs a
pure-static, trivially cross-compiled binary, reach for the sidecar below
instead (`patala-go/README.md` says so up front, not as a buried caveat).

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

Adapted from `patala-go/examples/roundtrip/main.go`, one of 19 top-level Go
binding tests run over real cgo and CI-enforced — see [Status](status.md).

### 4. As a local sidecar (any language, no FFI)

`patala-sidecar` runs `patala-core` behind a thin local HTTP API —
`quote` / `charge` / `verify` as JSON over a loopback socket. Any language
with an HTTP client can drive the substrate without a generated binding or a
Rust toolchain on the calling side.

```bash
export PATALA_SIDECAR_TOKEN=$(openssl rand -hex 32)
cargo run -p patala-sidecar
# patala-sidecar listening on 127.0.0.1:8420 (loopback only)

curl -s http://127.0.0.1:8420/healthz
# ok

curl -s http://127.0.0.1:8420/v1/rails/mock/charge \
  -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_minor":1250,"currency":"USDC","destination":"dest","reference":"order-1"}'
```

**Why a sidecar in addition to the Python binding.** If every product in a
polyglot stack links `patala-core` or `patala-py` directly, a non-custodial
rail's signing key is smeared across every one of those processes' memory —
a bug or a dependency-confusion attack in any of them is a path to the key.
Route them all through one sidecar instead, and the key lives in exactly one
narrowly-scoped, purpose-built process that does nothing else.

**Sidecar threat model, stated honestly:**

- Binds to `127.0.0.1` only, unconditionally — not an env-configurable knob.
- Refuses to start without `PATALA_SIDECAR_TOKEN` set — no auto-generated
  fallback, no unauthenticated-by-default path.
- Every `/v1/...` route requires a bearer token, checked in constant time.
  `/healthz` is the one unauthenticated route and reveals nothing about
  configured rails.
- It does **not** defend against a same-user, same-privilege co-resident
  process reading the token out of its environment, and it has **no TLS**
  (a reasonable simplification for loopback-only traffic — never point it at
  a non-local interface without adding TLS and re-deriving the threat model)
  and **no rate limiting**.

## Self-hosting the fiat rail

`patala-hyperswitch` doesn't process fiat itself — it's a thin adapter to
a **self-hosted [Hyperswitch](https://github.com/juspay/hyperswitch)**
instance (Apache-2.0, Rust). You run Hyperswitch yourself, point
`patala-hyperswitch` at it via `HyperswitchConfig::from_env()`
(`HYPERSWITCH_BASE_URL`, `HYPERSWITCH_API_KEY`, `HYPERSWITCH_CONNECTOR`, and
related environment variables — see that crate's README for the full table),
and which processor actually moves the money — Stripe, Paystack, or
Hyperswitch's own merchant-account routing — becomes a config value, never a
code branch in patala.

This crate is deliberately **not** in the workspace's default members, so a
plain `cargo build`/`cargo test` at the repo root never pulls in its HTTP
client dependencies.

## Consumer guidance — provider credentials

patala itself holds no secrets, but a consumer that *persists* provider
credentials (a store's Stripe key, a gateway's Paystack secret) is handling
live money-moving material, and should:

- **Encrypt them at rest** (AES-256-GCM or equivalent) under a key that is
  not itself in the database.
- **Make them write-only** — accept on create/update, never return them in
  an API response after.
- **Scope access** to admin/management credentials only.

This is not patala's job to enforce — it never sees the store — but it's
stated here so a consumer building on the substrate doesn't have to learn it
the hard way.

## Related documents

- [Choosing a mode](choosing-a-mode.md) — the decision page behind the four
  options above, with the trade-offs laid out side by side.
- [The rail interface](rails-interface.md) — the trait every rail implements.
- [The sidecar HTTP API](sidecar.md) — every endpoint, status code and error
  mapping.
- [Status](status.md) — what's tested offline vs. verified against a live
  network.
