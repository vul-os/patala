# patala-hyperswitch

Rail #4 of `patala` (`PATALA.md` §4): a thin HTTP client to a **self-hosted
[Hyperswitch](https://github.com/juspay/hyperswitch)** instance, presenting
Hyperswitch's whole fiat processor set (Stripe/Paystack/Xendit/... — 100+
connectors, Apache-2.0, Rust, self-hostable) as **one**
`patala_core::PaymentRail` of class `CustodialReversible`.

**This crate ADOPTS Hyperswitch. It does not vendor a single processor SDK.**
There is exactly one `PaymentRail` impl here (`HyperswitchRail`); which
processor actually moves the money is a property of the Hyperswitch instance
behind `base_url` (and, optionally, this crate's `connector` config field —
see below), never a code branch in this crate.

This crate is **not** in the workspace's `default-members` (see the root
`Cargo.toml`): it carries a real HTTP client (`reqwest`) and HMAC/hash deps
(`hmac`, `sha2`, `hex`) on purpose, so plain `cargo build`/`cargo test` at the
workspace root never pulls it in. Build/test it explicitly:

```sh
cargo build -p patala-hyperswitch
cargo test -p patala-hyperswitch   # offline — HTTP is mocked with wiremock
cargo clippy -p patala-hyperswitch --all-targets -- -D warnings
cargo fmt -p patala-hyperswitch -- --check
```

## Configuration — never hardcoded

`HyperswitchConfig` (`src/config.rs`) holds `base_url`, `api_key`, and every
other knob; `HyperswitchConfig::from_env()` reads them from environment
variables (`HYPERSWITCH_BASE_URL`, `HYPERSWITCH_API_KEY`,
`HYPERSWITCH_CONNECTOR`, `HYPERSWITCH_WEBHOOK_SECRET`,
`HYPERSWITCH_REQUIRES_KYC`, `HYPERSWITCH_CURRENCIES`,
`HYPERSWITCH_SETTLEMENT_DAYS`, `HYPERSWITCH_TIMEOUT_SECS` — see the doc
comments there for the full table). Nothing in this crate has a default or
fallback base URL, key, or secret; a missing `base_url`/`api_key` is a hard
`Err`, never a silent default.

## Choosing a processor is a config value, not a code path

`HyperswitchConfig::connector: Option<String>` maps straight onto
Hyperswitch's own `PaymentsCreateRequest.connector: Connector[]` field ("This
allows to manually select a connector with which the payment can go
through" — Hyperswitch's own OpenAPI spec). Setting it to `"paystack"` routes
a charge through Paystack **via Hyperswitch**; setting it to `"stripe"`
routes through Stripe; leaving it `None` lets Hyperswitch apply its own
configured merchant-account routing rules instead. `HyperswitchRail` never
branches on which value this is — the processor selection lives entirely in
configuration.

## African-market coverage: Paystack and PayFast

Checked directly against Hyperswitch's `main`-branch source
(`github.com/juspay/hyperswitch`), not guessed:

- **Paystack (Nigeria): CONFIRMED-SUPPORTED.**
  `crates/hyperswitch_connectors/src/connectors/paystack.rs` exists as a full
  connector module, and `Connector::Paystack` is a variant of the connector
  enum at `crates/common_enums/src/connector_enums.rs:158`. Route a charge
  through it today by setting `HyperswitchConfig::connector =
  Some("paystack".into())` (see above) — no code change to this crate needed.
- **PayFast (South Africa): CONFIRMED-ABSENT.** Enumerating
  `crates/hyperswitch_connectors/src/connectors/` directly (140+ connector
  modules, fetched from the live GitHub tree, as of this crate's authoring)
  contains no `payfast` module, and `crates/common_enums/src/connector_enums.rs`
  has no `PayFast` variant either. (Hyperswitch does have other South
  African-relevant connectors — `peachpayments`, `payjustnow`/
  `payjustnowinstore` — but not PayFast specifically.)

**Design consequence — this adapter is not the only door.** Two paths exist
for getting a processor onto `patala`:

1. **Any processor Hyperswitch already supports** (Paystack included) comes
   free through this one `patala-hyperswitch` adapter — just point
   `HyperswitchConfig::connector` (or the merchant account's own Hyperswitch
   routing config) at it. No new patala code, no new crate.
2. **A processor Hyperswitch lacks** (PayFast today, confirmed above) is
   *not* locked out of `patala` — it gets its own thin **direct** rail crate
   (e.g. a future `patala-payfast`) implementing `patala_core::PaymentRail`
   directly against that processor's own API, exactly the way this crate
   implements it against Hyperswitch's API. `PayFast` is the likely first
   candidate for such a direct adapter, precisely because it is
   CONFIRMED-ABSENT from Hyperswitch and is a common South African
   requirement. **This crate does not build that adapter** — this is a
   forward design note only, so a future direct adapter slots into the same
   seam (`patala_core::PaymentRail`) without patala consumers ever needing to
   know which path (Hyperswitch-fronted vs. direct) a given processor came
   through; both present identically as `RailClass::CustodialReversible`.

## Sources (what was verified, and how)

No live Hyperswitch instance is reachable from the environment this crate was
written in (see "UNVERIFIED AGAINST LIVE" below). Every request/response
shape and header was instead checked against Hyperswitch's own published,
machine-readable spec and its own source code — not guessed:

| Fact used in this crate | Source |
|---|---|
| `POST /payments` request shape (`amount` int64 minor units, `currency`, `confirm`, `payment_token`, `connector: Connector[]`), required fields (`amount`, `currency` only) | `github.com/juspay/hyperswitch`, `api-reference/v1/openapi_spec_v1.json`, schema `PaymentsCreateRequest`, path `/payments` → `post` |
| `POST /payments` / `GET /payments/{payment_id}` response shape (`payment_id`, `status`, `amount`, `amount_received`, `amount_capturable`, `currency`, `connector`, `next_action.redirect_to_url`, `refunds[]`) | same spec, schemas `PaymentsCreateResponseOpenApi` and `PaymentsResponse`, path `/payments/{payment_id}` → `get` (incl. its `force_sync` query param) |
| `IntentStatus` enum — all 17 variants (`succeeded` is the only settled-success state; the rest are final-failure or still-pending) | same spec, schema `IntentStatus` |
| `POST /refunds` request shape (`payment_id` required; `amount`/`reason`/`refund_id` optional) and response shape (`refund_id`, `payment_id`, `amount`, `currency`, `status`, `connector`, ...) | same spec, schema `RefundRequest` / `RefundResponse`, path `/refunds` → `post`, and `/refunds/{refund_id}` → `get` |
| `RefundStatus` enum — all 4 variants (`succeeded`, `failed`, `pending`, `review`) | same spec, schema `RefundStatus` |
| Auth header: `api-key: <value>` | same spec, `components.securitySchemes.api_key` |
| Generic error body shape (`error_type`, `message`, `code`) | same spec, schema `GenericErrorResponseOpenApi` |
| Outgoing-webhook signature: header **`X-Webhook-Signature-512`**, **HMAC-SHA512** over the raw JSON body, **hex-encoded**, keyed by the merchant's own `payment_response_hash_key` | `github.com/juspay/hyperswitch`, `crates/router/src/core/webhooks/types.rs` (`OutgoingWebhookType::get_outgoing_webhooks_signature`, which calls `common_utils::crypto::HmacSha512::sign_message` and `hex::encode`s the result) + `crates/router/src/lib.rs` (`pub const X_WEBHOOK_SIGNATURE: &str = "X-Webhook-Signature-512";`) |
| Paystack / PayFast connector coverage | `crates/hyperswitch_connectors/src/connectors/` directory listing + `crates/common_enums/src/connector_enums.rs` — see "African-market coverage" above |

## NEEDS-CONFIRMATION

These are explicitly flagged, not silently assumed:

- **`PayRequest::destination` → Hyperswitch's `payment_token` mapping.** This
  is a *design choice of this crate*, not a fact read off Hyperswitch's docs:
  `patala_core::PayRequest::destination` is documented as "an opaque
  processor-side destination token" for a fiat rail, and this crate maps it
  onto Hyperswitch's `payment_token` field (a reference to a payment method
  already tokenized out-of-band, e.g. via Hyperswitch's own client-side SDK —
  so raw card data never passes through this crate). The field's presence in
  Hyperswitch's request schema is confirmed; whether `payment_token` alone
  (with no `client_secret`/`card_cvc`) is sufficient to confirm a payment in
  every Hyperswitch deployment/connector combination is **not** confirmed
  against a live instance.
- **No pre-charge fee-quote endpoint.** Hyperswitch's OpenAPI spec has no
  `/payments`-adjacent endpoint that returns a fee estimate before charging.
  `HyperswitchRail::quote()` therefore always reports `fee_minor: 0` — this is
  an honest "we don't have this number", never a guessed nonzero fee. If a
  future Hyperswitch version (or a deployment's own reporting API) exposes
  real pre-charge fees, `quote()` should be updated to use them.
  `patala-solana`'s README documents the same "fee_minor always 0" honesty
  pattern for its own real cost (SOL gas) that doesn't fit `Quote`'s
  same-currency fee field — this crate's reason is different (no visibility)
  but the response (report `0`, state why, never fabricate) is the same.
- **v1 vs. v2 API.** This crate targets Hyperswitch's stable `v1` HTTP API
  (no `/v2` path prefix; matches the OpenAPI spec's own `servers` entry,
  `https://sandbox.hyperswitch.io`, which is v1). A self-hosted instance
  deployed in Hyperswitch's newer "v2" mode may have different endpoint
  shapes; this has not been checked.
- **Webhook scheme staying current.** The HMAC-SHA512 / `payment_response_hash_key`
  scheme above reflects Hyperswitch's `main`-branch source as of this crate's
  authoring. If the target deployment runs an older/patched build, confirm
  against that instance's actual outgoing-webhook behavior before relying on
  `verify_webhook_signature` in production — it still fails closed regardless
  of scheme drift (a mismatch is always rejection, never acceptance), it just
  might reject a genuine webhook from a differently-configured instance.

## The pending/redirect lifecycle, modelled honestly

A card payment through Hyperswitch is not necessarily instant. `charge()`
maps this truthfully rather than reporting "settled" the moment Hyperswitch
accepts the request:

- If Hyperswitch's create response comes back `status: "succeeded"`, the
  returned `Receipt::amount_minor` is the amount actually received.
- If it comes back anything else — `requires_customer_action` (3DS
  redirect), `requires_payment_method`, `processing`, `review`, etc. —
  `Receipt::amount_minor` is **`0`**: no money has moved yet. The receipt's
  opaque `proof` still embeds the real Hyperswitch `payment_id` and the
  status snapshot (plus a `redirect_to_url` if Hyperswitch returned one), so
  a caller can still find where to send the payer, but the `Receipt` itself
  never claims settlement it hasn't happened.
- `patala_core::Receipt`'s own doc comment is explicit that a caller must
  gate on `verify()`, never on `charge()` merely returning `Ok` — this crate
  leans on exactly that contract. `HyperswitchRail::verify()` always
  re-fetches fresh from Hyperswitch (`GET /payments/{id}?force_sync=true`,
  bypassing any cache) and returns `Ok(true)` **only** when `status ==
  "succeeded"` **and** the amount/currency match **and** no `succeeded`
  refund already covers the amount — anything else, including every pending
  state, is `Ok(false)`. This is the fail-closed contract
  `patala_core::PaymentRail::verify` requires (`patala-core/src/rail.rs`).
- `refund()` has the same honesty: Hyperswitch's own `RefundStatus` includes
  `pending`/`review`, not just `succeeded`/`failed` — a refund the processor
  accepted but hasn't completed yet also returns `amount_minor: 0` on the new
  `Receipt` it produces.

## `validate_destination`: a refusal to guess

`PayRequest::destination` on this rail is passed straight through as
Hyperswitch's `payment_token` — a reference to a payment method the caller
tokenized out of band via Hyperswitch's own client-side SDK (that mapping is
itself listed under NEEDS-CONFIRMATION above). Two consequences:

* **It is not a payout address.** Nothing is sent *to* a `payment_token`; it
  names the payment method money is taken *from*. `StructurallyValid` — "a
  well-formed address for the network this rail pays on" — is therefore not a
  status this rail could ever truthfully report, and it never does.
* **Its format is Hyperswitch's to define, not patala's.** Hyperswitch's
  OpenAPI spec types `payment_token` as a plain string with no documented
  grammar, and a self-hosted instance may be any version against any connector.
  So this rail makes **no format check at all** rather than guess one and
  refuse a token a live instance would have accepted.

This is a deliberate difference from `patala-fiat`, whose rails *do* refuse a
pasted wallet address — they can, because their `destination` has a documented
format (a URL, an email) that an address demonstrably is not. This rail has no
such ground to stand on, and a test pins that it does not pretend otherwise.

The verdict is `Unknown` for any non-empty string, with a reason that says why
nothing was checked, and `Malformed` for a blank one (`PayRequest::validate()`
refuses one too, so accepting it here would put the two in disagreement). Every
verdict carries `EXCHANGE_DEPOSIT_CAVEAT` and `human_must_confirm: true`.

Giving a customer their money back on this `CustodialReversible` rail is
`PaymentRail::refund`, which goes back the way it came and needs no destination
at all.

## Non-custodial invariant

`patala` itself never holds funds (`PATALA.md` §1, §8).
`HyperswitchRail::capabilities().holds_funds` is `true` — this describes
**Hyperswitch's underlying connector's** custody of money in flight
(Stripe/Paystack/etc. genuinely do hold funds momentarily), never this
crate's or patala's own. No function in this crate receives, stores, or
transmits real money; every function here only sends/receives JSON
describing a request that Hyperswitch and its connector carry out. See
`src/rail.rs` and `patala-core/src/capabilities.rs`'s own doc on
`holds_funds` for the same point made in code.

## Money

Every amount is a `u64` in the currency's minor units (`PayRequest::amount_minor`,
`Quote`, `Receipt::amount_minor`) — never a float, per `PATALA.md` §8.
Hyperswitch's own API already uses integer minor units for `amount`
(`"amount": 6540` for $65.40, confirmed in its own request/response
examples), so there is no unit conversion anywhere in this crate — a nice,
verifiable alignment rather than a source of silent rounding bugs.

## Testing

All 23 unit tests (`src/config.rs`, `src/rail.rs`, `src/webhook.rs`) run
fully offline. HTTP is mocked with [`wiremock`](https://docs.rs/wiremock)
(a local loopback mock server, not a real network call) and assert:

- the exact request shape sent to `/payments` and `/refunds` (method,
  headers, path, query params);
- correct parsing of a `succeeded` response vs. a pending/`requires_customer_action`
  response, including that the latter yields `amount_minor: 0`, not the
  requested amount;
- `verify()` failing closed on a tampered amount, a wrong currency, a
  foreign `rail_id`, a garbage proof, and a fully-refunded payment;
- a non-2xx HTTP response becoming an `Error::Rail`, never a fabricated
  success;
- the webhook HMAC verifier accepting a genuine signature and rejecting a
  tampered body, a wrong secret, malformed hex, and an empty secret/signature
  — all without panicking.

## UNVERIFIED AGAINST LIVE

**No live Hyperswitch instance — self-hosted or Hyperswitch's own hosted
sandbox — was reachable from the environment this crate was written and
tested in.** Every claim in this README about request/response shapes was
checked against Hyperswitch's own published OpenAPI spec and source (see
"Sources" above), and every test mocks HTTP. That proves this crate builds
the requests Hyperswitch's own docs describe and parses the responses those
docs describe — **it does not prove a real charge, refund, or webhook works
against an actual running Hyperswitch instance.** Do not treat a green
`cargo test -p patala-hyperswitch` as evidence of that. Before relying on
this in production:

1. Stand up a self-hosted Hyperswitch instance (or use its own sandbox) with
   at least one real connector configured (Paystack, Stripe, ...).
2. Run `charge()` against it with a real tokenized payment method and
   confirm the response actually matches what this crate expects — in
   particular the `payment_token` mapping flagged NEEDS-CONFIRMATION above.
3. Confirm `verify()`'s `force_sync=true` retrieval genuinely reflects
   connector-side settlement, not a stale Hyperswitch-side cache.
4. Configure a real `payment_response_hash_key` on that instance and confirm
   `verify_webhook_signature` accepts its actual outgoing webhook delivery.

Following `PATALA.md` §8's convention (as `patala-solana`'s `#[ignore]`d
live-RPC test does for a chain rail), this crate deliberately has **no**
live-network test at all, ignored or otherwise — there being no live
Hyperswitch instance to point one at from here. A future contributor with
access to one should add such a test, gated on an env var, rather than
assume the mocked tests above are sufficient proof.
