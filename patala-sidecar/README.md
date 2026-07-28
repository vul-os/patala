# patala-sidecar

A thin local HTTP server over `patala-core` (`PATALA.md` §5): `quote`,
`charge`, `verify` as JSON, over a loopback socket. This is the universal
polyglot path — any language with an HTTP client can drive the substrate
without FFI, a generated binding, or even a Rust toolchain on the calling
side.

## Why this exists in addition to `patala-py`

Two different problems:

- **`patala-py`** is a same-process binding: lowest latency, richest types,
  but one dependency per consuming language.
- **This sidecar** is a separate *process*: worse latency, JSON instead of
  native types, but it works from literally any language with an HTTP
  client, and it buys a real security property neither the Python binding
  nor a bare Rust dependency gets for free.

**Key isolation.** A non-custodial rail's signing key (or the credentials to
derive one) lives inside whichever process calls `PaymentRail::charge`. If
every product in a polyglot stack links `patala-core` or `patala-py`
directly, that key is smeared across every one of those processes' memory —
a bug or a dependency-confusion attack in *any* of them is a path to the
key. Route them all through one sidecar instead, and the key lives in
exactly one narrowly-scoped, purpose-built process that does nothing else.
This is the same reasoning production payment systems already use for
signing/HSM-adjacent services; `patala-sidecar` is that pattern applied to
patala's non-custodial rails.

## What's exposed

Every payload is a `patala_core` type, serialized directly — no hand-rolled
second DTO set that could drift from the trait (`src/api.rs`). Amounts stay
the `u64` minor-units integers `patala-core` defines; `serde_json` encodes a
`u64` as a JSON number, never as a float or a string (`PATALA.md` §3, §8).

| Method | Path | Body | Response |
|---|---|---|---|
| `GET` | `/healthz` | — | `"ok"` (unauthenticated) |
| `GET` | `/v1/rails/:rail_id` | — | `RailCapabilities` |
| `POST` | `/v1/rails/:rail_id/quote` | `PayRequest` | `Quote` |
| `POST` | `/v1/rails/:rail_id/charge` | `PayRequest` | `Receipt` |
| `POST` | `/v1/rails/:rail_id/verify` | `Receipt` | `{"valid": bool}` |
| `POST` | `/v1/rails/:rail_id/webhook` | the processor's **raw request** | `WebhookEvent` |

`/webhook` is the push counterpart to `/verify`: forward the processor's
webhook request to it **verbatim** — same body bytes, same headers, same
query string — and the rail says whether the delivery is genuine. It is the
one endpoint whose body is not parsed JSON, deliberately: every webhook
scheme signs the exact bytes the processor sent, so re-encoding the body
here would invalidate the signature of every genuine delivery. A `200` means
the rail authenticated it; read `status` (`"Settled"` / `"NotSettled"` /
`"Unconfirmed"`) for what it claims, and gate entitlement on `"Settled"`
only, after reconciling `amount_minor`/`currency` against your own stored
order. A rail with no push delivery — the offline `"mock"` — answers `501`.

A `Receipt`'s `verify` result is `{"valid": false}` with HTTP `200` for an
unverifiable receipt — the fail-closed answer is data, never an HTTP error
— so a caller cannot mistake "verified false" for "the sidecar broke". A
missing/unknown `rail_id` is `404`. Rail-level failures map to `400`
(malformed request), `501` (`Unsupported` — e.g. refund on a
`NonCustodialFinal` rail), `502` (a rail operation failed, or all rails in a
future `FailoverRail`-backed registry failed), or `409` (a `FailoverRail`
cross-class guard tripped) — see `src/api.rs`'s `ApiError` for the exact
mapping.

## Threat model

**What this defends against:** an unrelated local process on the same
machine — a compromised dependency in some other app, a stray script, a
malicious local user process without the right privilege — issuing payment
operations against a rail this sidecar has access to. That is the
attacker this design assumes.

**How:**
- **Bind to loopback only, unconditionally.** `main.rs` hardcodes
  `127.0.0.1` (`Ipv4Addr::LOCALHOST`) — this is not an env-configurable
  knob. A sidecar that could be told to listen on `0.0.0.0` would defeat the
  entire point: the moment it's reachable off-box, "local key isolation"
  becomes "network-exposed payment API with a bearer token," which is a much
  worse posture. Only the port is configurable (`PATALA_SIDECAR_PORT`,
  default `8420`).
- **Fail-closed startup.** The server requires `PATALA_SIDECAR_TOKEN` in its
  environment (`src/auth.rs`, `SidecarToken::from_env`). If it is unset or
  empty, the process refuses to start at all — there is no auto-generated
  fallback token and no "runs unauthenticated if you forget to set it"
  path. `main.rs` prints the exact `export PATALA_SIDECAR_TOKEN=$(openssl
  rand -hex 32)` a caller needs and exits `1`.
- **Every `/v1/...` route requires `Authorization: Bearer <token>`**,
  checked with a constant-time comparison (`src/auth.rs`, `SidecarToken::matches`)
  so a timing side channel can't be used to recover the token
  byte-by-byte. This gate sits in front of *all* payment routes, including
  the read-only capabilities lookup — not just `charge`. A missing header, a
  malformed header, and a wrong token are all the same `401` with no
  distinguishing detail.
- **`/healthz` is the one unauthenticated route**, and it reveals nothing
  about configured rails — just liveness. Everything that could leak
  anything interesting is behind the token.

**What this does *not* defend against** — stated honestly, not hidden
(`PATALA.md` §8):
- **Same-user, same-privilege processes.** Any process running as the same
  OS user (or root) can, in principle, read the token out of this process's
  environment or a config file, or connect to the same loopback port itself.
  Loopback-plus-token raises the bar above "any process on the LAN can hit
  this," not above "a fully co-resident, same-privilege attacker can't."
  True key isolation against a co-resident attacker needs OS-level
  sandboxing (a dedicated user/container, capability restriction) or
  hardware isolation (an HSM/enclave) — out of scope for this wave, and
  explicitly not claimed.
- **No TLS.** There is no network hop to protect against on loopback — this
  is a reasonable simplification for `127.0.0.1`-only traffic, not an
  oversight, but it means this design must never be pointed at a non-local
  interface without adding TLS and re-deriving the whole threat model.
- **No rate limiting / no request auditing.** A caller with the correct
  token can call `charge` as fast as it likes. Rate limiting and audit
  logging are reasonable additions for a production deployment; this wave
  built the fail-closed auth gate, not a full operational hardening pass.

## The rail registry is MOCK-ONLY (read this before wiring anything to it)

`src/registry.rs`'s `default_registry()` returns a
`HashMap<String, Arc<dyn PaymentRail>>` with **exactly one** entry: `"mock"`.
There is no Solana, Stellar, Hyperswitch or fiat rail reachable through this
sidecar. A request naming any other `rail_id` gets a `404`, because this
process has never heard of it. Per-rail registration is **unwritten** —
described in `registry.rs`'s doc comment, not implemented.

Everything *around* the registry is real and tested: the loopback bind, the
fail-closed token gate, the error mapping, all five endpoints, and their
round-trips over a real socket. So "the sidecar works" is true of the HTTP
surface and false of the rail set behind it. `registry.rs`'s
`registry_is_mock_only` test pins that claim so this section cannot rot.

**Correcting an earlier note:** this used to say the rail crates "don't exist
in this tree", which stopped being true when `patala-solana`,
`patala-stellar`, `patala-hyperswitch` and `patala-fiat` landed — all four are
workspace members now. The reason the registration is still unwritten is
ordinary work nobody has done, not a blocker: optional dependencies behind
per-rail features, a decision about where each rail's credentials come from
and what happens when they are missing (the sidecar exists precisely so those
credentials live in one process, so that decision matters), and extending the
lint/test targets to cover the feature-on build. `registry.rs`'s doc comment
spells all three out.

**No redesign is needed when it happens.** Every handler in `src/api.rs` looks
a rail up by `rail_id` and calls trait methods; none names a concrete rail
type. Adding a rail changes **no HTTP route, handler, or wire format** — only
the set of `rail_id`s a request can successfully target.

## Run it

```bash
export PATALA_SIDECAR_TOKEN=$(openssl rand -hex 32)
cargo run -p patala-sidecar
# patala-sidecar listening on 127.0.0.1:8420 (loopback only)

curl -s http://127.0.0.1:8420/healthz
# ok

curl -s http://127.0.0.1:8420/v1/rails/mock \
  -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN"
# {"class":"NonCustodialFinal","reversible":false,"requires_kyc":false,
#  "holds_funds":false,"currencies":["USDC","USD"],"settlement":"Instant"}

curl -s http://127.0.0.1:8420/v1/rails/mock/charge \
  -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_minor":1250,"currency":"USDC","destination":"dest","reference":"order-1"}'
```

## Tests

```bash
cargo test -p patala-sidecar
```

`tests/roundtrip.rs` boots the exact `axum::Router` `main.rs` serves (via the
shared `patala_sidecar::app()` builder) on an OS-assigned loopback port, and
drives real HTTP requests against it with `reqwest`: capabilities lookup,
`charge` → `verify` (asserting `valid: true`), a tampered receipt
(asserting `valid: false`), missing/wrong-token rejection on both a
money-moving and a read-only route (`401` on each), and an unknown
`rail_id` (`404`). All of it runs against `MockRail` — no network beyond
localhost, no real rail required.

## Verified in this environment (2026-07-21)

`cargo test -p patala-sidecar` was actually run here: 3 unit tests
(`auth::tests`, plus `registry::tests::registry_is_mock_only`) and all 6
integration tests in `tests/roundtrip.rs` + `tests/webhook.rs` pass,
exercising the full charge → verify round trip over real HTTP against a
really-bound loopback `TcpListener`. `cargo clippy -p patala-sidecar
--all-targets -- -D warnings` and `cargo fmt -p patala-sidecar -- --check`
are both clean.
