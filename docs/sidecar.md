# The sidecar HTTP API

`patala-sidecar` is a thin local HTTP server over `patala-core`: `quote`,
`charge`, `verify`, `validate-destination` and `webhook` as JSON, over a
loopback socket. It is the universal polyglot path — any language with an HTTP
client can drive the substrate with no FFI, no generated binding, and no Rust
toolchain on the calling side.

It is also the only mode that buys a **security** property the others cannot.

## Two reasons to run it

**1. Your language has no binding.** Ruby, Elixir, PHP, Java, C#, Node — an
HTTP client and a JSON parser are the entire dependency list. So does Go, when
[cgo is not acceptable](go.md#the-cgo-cost).

**2. Key isolation.** A non-custodial rail's signing key — or the credentials
to derive one — lives inside whichever process calls `PaymentRail::charge`. If
every product in a polyglot stack links the core or a binding directly, that
key is resident in every one of those processes' memory, and a bug or a
dependency-confusion attack in *any* of them is a path to it. Route them all
through one sidecar and the key lives in exactly one narrowly-scoped process
that does nothing else. This is the pattern production payment systems already
use for signing services, applied at the scale of one machine.

The trade against a binding is worse latency and JSON instead of native types.

## Running it

```bash
export PATALA_SIDECAR_TOKEN=$(openssl rand -hex 32)
cargo run -p patala-sidecar
# patala-sidecar listening on 127.0.0.1:8420 (loopback only)
```

The process **refuses to start** without `PATALA_SIDECAR_TOKEN`. There is no
auto-generated fallback and no unauthenticated-by-default path; it prints the
exact `export` line you need and exits `1`. Only the port is configurable
(`PATALA_SIDECAR_PORT`, default `8420`) — the bind address is not.

## Endpoints

Every payload is a `patala_core` type serialized directly. There is no
hand-rolled second DTO set that could drift from the trait. Amounts stay the
`u64` minor-unit integers the core defines; `serde_json` encodes a `u64` as a
JSON number, never a float and never a string.

| Method | Path | Body | Response |
|---|---|---|---|
| `GET` | `/healthz` | — | `"ok"` — **unauthenticated** |
| `GET` | `/v1/rails/:rail_id` | — | `RailCapabilities` |
| `POST` | `/v1/rails/:rail_id/quote` | `PayRequest` | `Quote` |
| `POST` | `/v1/rails/:rail_id/charge` | `PayRequest` | `Receipt` |
| `POST` | `/v1/rails/:rail_id/verify` | `Receipt` | `{"valid": bool}` |
| `POST` | `/v1/rails/:rail_id/validate-destination` | `{"destination": string}` | `DestinationVerdict` + `is_refusal` |
| `POST` | `/v1/rails/:rail_id/webhook` | the processor's **raw request** | `WebhookEvent` |

Every `/v1/...` route requires `Authorization: Bearer <token>` — including the
read-only capabilities lookup, not just the money-moving ones.

```bash
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

## Read the body, not just the status code

This is the single most important thing to get right about this API.

A `200` means **the rail answered**, not that the answer was yes.

- `/verify` returns `200` with `{"valid": false}` for an unverifiable receipt.
  The fail-closed answer is *data*, never an HTTP error, so a caller cannot
  mistake "verified false" for "the sidecar broke".
- `/validate-destination` returns `200` for **all five** verdicts —
  `Malformed`, `WrongNetwork`, `NotAWallet`, `StructurallyValid`, `Unknown`.
  Mapping some of them onto HTTP codes would flatten a five-state answer into
  "worked / did not work". Branch on `status` and `is_refusal`.

`is_refusal` is added to the core type's JSON shape because it is a *method*
on `DestinationVerdict` in Rust, and a method does not survive JSON. A
consumer re-deriving it from `status` would fall through to its default for
any status added later — and that default is "not a refusal", which fails
open. Every verdict also carries `human_must_confirm: true`, including
`StructurallyValid`, and the `exchange_deposit_caveat` text.

A `400` means the **request** was malformed — not JSON, a missing or
non-string `destination`, an unexpected field — and carries no verdict fields
at all, so a rejected request can never be mistaken for a checked address.
Note that `{"destination": ""}` is a well-formed *request*, and answers `200`
with the rail's `Malformed` refusal.

### Status codes

| Code | Meaning |
|---|---|
| `200` | The rail answered. Read the body. |
| `400` | The request was malformed, or the rail rejected it as invalid. |
| `401` | Missing, malformed or wrong bearer token. All three are identical and carry no distinguishing detail. |
| `404` | Unknown `rail_id` — this process has never heard of it. |
| `409` | A `FailoverRail` cross-class guard tripped. |
| `501` | `Unsupported` — e.g. a refund on a `NonCustodialFinal` rail, or a webhook on a rail with no push delivery. |
| `502` | A rail operation failed, or every rail in a failover set failed. |

The exact mapping lives in `patala-sidecar/src/api.rs`'s `ApiError`.

## Webhooks: forward the body verbatim

`/webhook` is the push counterpart to `/verify`. Forward the processor's
request — same body, same headers, same query string — and the rail says
whether the delivery is genuine.

The **body** is byte for byte. It is the one endpoint whose body is not parsed
as JSON, deliberately: every webhook scheme signs the exact bytes the processor
sent, so re-encoding it here would invalidate the signature of every genuine
delivery.

### Three headers are dropped, since 0.1.1

`authorization`, `proxy-authorization` and `cookie` are **not** forwarded into
`WebhookDelivery`, and one of those is not cosmetic. Every `/v1` route sits
behind the token gate, so a request that reaches this handler is *guaranteed* to
carry `Authorization: Bearer <PATALA_SIDECAR_TOKEN>` — this sidecar's own
credential, the thing whose isolation is the entire reason the process exists.
Forwarding headers verbatim copied that token into `WebhookDelivery::headers`,
where it was handed to arbitrary rail code and sat in a `Debug`-printable map.

Nothing legitimate is lost. None of the twenty-two schemes reads any of those
names — they read `Stripe-Signature`, `X-Paystack-Signature`, `verif-hash`,
`webhook-*` and so on — and none could: a processor's own `Authorization` header
cannot survive the proxy hop that has to *replace* it with the sidecar token to
get past the gate at all.

A header whose value is not valid UTF-8 is dropped for the same reason, so a
malformed one produces a clean "invalid signature" rather than a 500.

### A broken clock refuses the delivery

If the system clock reads before the epoch, the request is refused and says why,
rather than substituting `0`. `now` is the only input to every replay-window
check in the workspace, and each computes `|now - signed_timestamp|` — so a `0`
makes every genuine delivery look aeons old. Every rail *with* a window (Stripe's
five minutes, Yoco's) would reject everything while every rail *without* one
carried on: a silent, partial, fleet-wide outage disguised as a signature
failure. There is no honest `now` to substitute.

A `200` means the rail authenticated it. Read `status` — `"Settled"`,
`"NotSettled"` or `"Unconfirmed"` — for what it claims, and gate entitlement
on `"Settled"` **only**, after reconciling `amount_minor` and `currency`
against your own stored order. A rail with no push delivery — the offline
`"mock"` — answers `501`.

Replay suppression stays yours, keyed on `(rail_id, event_id)`.

## Threat model

Stated in full, including what it does not do.

**What it defends against:** an unrelated local process on the same machine —
a compromised dependency in some other app, a stray script, a malicious local
process without the right privilege — issuing payment operations against a
rail this sidecar can reach. That is the attacker this design assumes.

**How:**

- **Loopback only, unconditionally.** `main.rs` hardcodes `127.0.0.1`. This is
  not an env-configurable knob, and that is the point: a sidecar that could be
  told to listen on `0.0.0.0` would turn "local key isolation" into
  "network-exposed payment API with a bearer token", which is a much worse
  posture.
- **Fail-closed startup.** No token, no process. No auto-generated fallback,
  no "runs unauthenticated if you forget".
- **Constant-time token comparison**, so a timing side channel cannot be used
  to recover the token byte by byte. The gate sits in front of *all* payment
  routes.
- **`/healthz` is the one unauthenticated route**, and reveals nothing about
  configured rails — just liveness.

**What it does *not* defend against:**

- **Same-user, same-privilege processes.** Any process running as the same OS
  user, or as root, can in principle read the token out of this process's
  environment or a config file, or connect to the same loopback port itself.
  Loopback-plus-token raises the bar above "anything on the LAN can hit this",
  not above "a fully co-resident, same-privilege attacker cannot". True
  isolation against that attacker needs OS-level sandboxing — a dedicated
  user or container, capability restriction — or hardware isolation. Out of
  scope, and explicitly not claimed.
- **No TLS.** There is no network hop to protect on loopback. This is a
  reasonable simplification for `127.0.0.1`-only traffic, not an oversight —
  but it means this design must never be pointed at a non-local interface
  without adding TLS and re-deriving the whole threat model.
- **No rate limiting and no request auditing.** A caller with the correct
  token can call `charge` as fast as it likes. Both are reasonable additions
  for a production deployment; this is a fail-closed auth gate, not a full
  operational hardening pass.

## The rail registry is mock-only — read this before wiring anything to it

`default_registry()` returns a map with **exactly one** entry: `"mock"`. There
is no Solana, Stellar, Hyperswitch or fiat rail reachable through the sidecar
today. A request naming any other `rail_id` gets a `404`, because this process
has never heard of it.

Everything *around* the registry is real and tested: the loopback bind, the
fail-closed token gate, the error mapping, all six endpoints and their round
trips over a real socket. So "the sidecar works" is true of the HTTP surface
and false of the rail set behind it. A `registry_is_mock_only` test pins that
claim so this paragraph cannot rot into a lie.

Why the registration is still unwritten is ordinary work nobody has done, not
a blocker: optional dependencies behind per-rail features, a decision about
where each rail's credentials come from and what happens when they are missing
— the sidecar exists precisely so those credentials live in one process, so
that decision matters — and extending the lint and test targets to cover the
feature-on build.

**No redesign is needed when it lands.** Every handler looks a rail up by
`rail_id` and calls trait methods; none names a concrete rail type. Adding a
rail changes no route, no handler and no wire format — only the set of
`rail_id`s a request can successfully target.

## Tests

```bash
cargo test -p patala-sidecar
```

`tests/roundtrip.rs` boots the exact `axum::Router` `main.rs` serves — via the
shared `patala_sidecar::app()` builder, so the tested object is the served
object — on an OS-assigned loopback port, and drives real HTTP against it:
capabilities lookup, `charge` → `verify` asserting `valid: true`, a tampered
receipt asserting `valid: false`, missing- and wrong-token rejection on both a
money-moving and a read-only route, and an unknown `rail_id`. 15 tests: 12
HTTP round trips and 3 unit tests. All against `MockRail` — no network beyond
localhost.

## Related documents

- [Choosing a mode](choosing-a-mode.md) — sidecar versus binding versus crate.
- [The rail interface](rails-interface.md) — the trait every endpoint
  dispatches to.
- [Paying a customer back](compensating-payments.md) — the flow
  `/validate-destination` exists for.
- [Status](status.md) — what has been executed, and the mock-only caveat.
