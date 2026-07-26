# Self-host & vendor

patala is a library, not a service — there is nothing to sign up for. You
vendor the crate(s) you need into your own product and run them on your own
infrastructure. patala itself is stateless and holds no secrets: a rail is
constructed from config the *consumer* supplies each time (a fiat rail's API
keys, a crypto rail's signer).

## Three ways to consume it

### 1. As a Rust crate, direct

Add `patala-core` (and whichever rail crates/features you need) as a
dependency and program against the `PaymentRail` trait. The default build
pulls no chain and no processor — you opt into a rail with its feature flag.

```bash
cargo test -p patala-core
```

### 2. As a Python binding

`patala-py` wraps the same Rust core via UniFFI and exposes **synchronous**
methods, so a one-shot script doesn't need to run an `asyncio` event loop
just to call `charge()`. Each call blocks the calling thread on a
lazily-created multi-thread `tokio::runtime::Runtime` under the hood.

### 3. As a local sidecar (any language, no FFI)

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

- [The rails & interface](#rails) — the trait every rail implements.
- [Status](#status) — what's tested offline vs. verified against a live
  network.
