# Porting a cackle adapter into `patala-fiat`

This is the recipe a fan-out agent follows to port **one more** provider from
cackle's `internal/payments/` Go package into this crate as a
`patala_core::PaymentRail`. `manual`, `stripe`, and `paystack` are already
done — read them alongside this doc; they are the worked examples, not just
prose. This is **money code**. Port faithfully. Do not "improve" cackle's
logic, do not skip cackle's edge cases, and do not skip porting cackle's
tests. When something in cackle's Go genuinely has no clean equivalent in
`patala_core`'s trait, say so explicitly in your code's doc comments (see
"Gaps" below) — do not silently paper over it.

## 0. Before you start

Read, in this order:

1. `PATALA.md` §3 (the seam) and §8 (honesty conventions) at the workspace
   root.
2. `patala-core/src/rail.rs` and `patala-core/src/capabilities.rs` — the
   `PaymentRail` trait and `RailCapabilities` you implement.
3. `patala-hyperswitch/src/rail.rs` — an existing `CustodialReversible`
   adapter in this same workspace (different shape: it ADOPTS a gateway
   rather than talking to one processor directly, but the honesty
   conventions — pending/redirect lifecycle, fail-closed verify, wiremock
   tests — are identical to what you're about to write).
4. This crate's own `src/stripe/` and `src/paystack/` in full (5-6 files
   each: `mod.rs`, `config.rs`, `models.rs`, `proof.rs`, `rail.rs`,
   `webhook.rs`). These are your template. Copy their SHAPE, not their
   Stripe/Paystack-specific content.
5. The cackle source you are porting: `internal/payments/<provider>.go` and
   `internal/payments/<provider>_test.go`, plus `internal/payments/provider.go`
   (the `Provider` interface, `Capabilities`, `Order`/`Result`/`Charge`) for
   context on what every field in cackle's adapter means.

## 1. File layout

Create `src/<provider>/` with the same five files as `stripe`/`paystack`:

| File | Contents | Visibility |
|---|---|---|
| `mod.rs` | Module doc + `pub mod`/`mod` declarations + re-exports | — |
| `config.rs` | `<Provider>Config` struct + `from_env()` | `pub` |
| `models.rs` | Wire DTOs (request/response shapes) + amount conversion helpers + error classification | private (`mod models;`) |
| `proof.rs` | What goes in `Receipt::proof` | private (`mod proof;`) |
| `rail.rs` | `<Provider>Rail`, the actual `PaymentRail` impl | `pub` |
| `webhook.rs` | Free function(s) to verify+parse a webhook; `rail.rs`'s `verify_webhook` wraps them | `pub` |

Then:

- Add `#[cfg(feature = "<provider>")] pub mod <provider>;` to `src/lib.rs`,
  plus a re-export block mirroring the existing `stripe`/`paystack` ones.
- Add a `<provider>` feature to `Cargo.toml`, gated the same way
  `stripe`/`paystack` are:
  ```toml
  <provider> = ["dep:reqwest", "dep:hmac", "dep:sha2", "dep:hex"]
  ```
  (Only list the deps you actually need — e.g. a provider signing with
  SHA-256 doesn't need `sha2`'s SHA-512 machinery pulled in specially, it's
  the same crate either way; but if a provider uses, say, RSA/ECDSA
  signatures instead of HMAC, add whatever crate that needs and gate it the
  same way.)
- **Never add a new mandatory (non-optional) dependency.** Every network/
  crypto crate your adapter needs must be `optional = true` and only pulled
  in by your feature. Run `cargo build -p patala-fiat` (no features) after
  you're done — it must still succeed with zero new deps compiled in. Verify
  with `cargo tree -p patala-fiat -e normal` (no `--all-features`): your
  provider's crates must NOT appear.

## 2. The `PaymentRail` trait, and how cackle's `Provider` maps onto it

```rust
#[async_trait]
pub trait PaymentRail: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> &RailCapabilities;
    async fn quote(&self, req: &PayRequest) -> Result<Quote>;
    async fn charge(&self, req: &PayRequest) -> Result<Receipt>;
    async fn verify(&self, receipt: &Receipt) -> Result<bool>;
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> { Err(Error::Unsupported("refund")) }
}
```

| cackle `Provider` method | patala-core equivalent | Notes |
|---|---|---|
| `Name() string` | `PaymentRail::id(&self) -> &str` | Same stable lowercase id, e.g. `"adyen"`. |
| `Capabilities() Capabilities` | `PaymentRail::capabilities(&self) -> &RailCapabilities` | **Not a 1:1 field mapping** — see §4 below. Build once in `new()`, store as a struct field, return `&self.capabilities` (the trait needs a reference, not a fresh value). |
| — (no cackle equivalent) | `PaymentRail::quote(&self, req) -> Result<Quote>` | Cackle has NO pre-charge fee-quote concept anywhere in this package. Every existing rail (`manual`, `stripe`, `paystack`, and `patala-hyperswitch`) returns an honest `fee_minor: 0` quote rather than fabricating a number it cannot obtain. Do the same unless the specific provider's docs actually expose a quote/estimate endpoint — if so, use it and say so. |
| `Begin(ctx, Order) (Charge, error)` | `PaymentRail::charge(&self, req: &PayRequest) -> Result<Receipt>` | See §3 (the `Order`/`PayRequest` field gap) and §5 (honest pending lifecycle). |
| `Verify(ctx, reference string) (Result, error)` | `PaymentRail::verify(&self, receipt: &Receipt) -> Result<bool>` | Cackle's `Verify` takes a bare string and returns a rich `Result` (status/amount/currency/paid-at/event-id). `patala_core::verify` takes the WHOLE `Receipt` you issued and returns only `bool`. See §6. |
| `Webhook(ctx, *http.Request) (Result, error)` | `PaymentRail::verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent>`, wrapping a free function in your `webhook.rs`, e.g. `pub fn verify_and_parse(secret: &str, raw_body: &[u8], signature_header: &str, ...) -> Result<YourWebhookEvent, YourWebhookError>`. | **Both, in that order.** The free function stays pure (it takes exactly what the scheme signs, no `&self`) so it is directly testable; the trait method is what makes it reachable — a consumer dispatching through `dyn PaymentRail` (the UniFFI binding, the sidecar) cannot see a free function at all. Pull the signature header off `delivery` by name, map your event onto `WebhookEvent::settlement(..)`, and use `WebhookEvent::unconfirmed(..)` if your scheme authenticates a notification without asserting settlement. See §6b. |
| (no cackle `Refund` method exists — `Provider` interface has none) | `PaymentRail::refund(&self, receipt: &Receipt) -> Result<Receipt>` | See §7 — this is new code for almost every provider, not a port. |

## 3. `Order`/`Charge`/`Result` vs `PayRequest`/`Quote`/`Receipt`

```rust
pub struct PayRequest { pub amount_minor: u64, pub currency: String, pub destination: String, pub reference: String }
pub struct Receipt { pub rail_id: String, pub amount_minor: u64, pub currency: String, pub reference: String, pub proof: Vec<u8>, pub settled_at_unix: u64 }
```

cackle's `Order` has SEVEN fields (`Reference`, `EventID`, `OrgID`,
`BuyerEmail`, `BuyerName`, `AmountMinor`, `Currency`, `CallbackURL`,
`Metadata`) where `PayRequest` has FOUR (`amount_minor`, `currency`,
`destination`, `reference`). This is the single biggest, structural mapping
gap you will hit, and every existing adapter hits it differently:

- **Stripe** needed a `CallbackURL` (Checkout requires `success_url`/
  `cancel_url`) and had no field for it, so `stripe::rail` reinterprets
  `PayRequest::destination` AS the callback/return URL.
- **Paystack** needed a `BuyerEmail` (Initialize requires one) and had no
  field for it, so `paystack::rail` reinterprets `destination` AS the
  buyer's email.

**This is legitimate, not a hack**: `patala_core::PayRequest::destination` is
documented as "opaque... only the rail that receives it knows which" — every
rail is free to interpret it however it needs to. Do the same for your
provider: figure out what single piece of cackle's `Order` your provider's
`Begin` call absolutely cannot proceed without (a callback URL? an email? a
customer name? a phone number for mobile money?), and reinterpret
`destination` as that ONE thing. **Document the reinterpretation loudly**, in
your `rail.rs`'s module doc comment, exactly as `stripe::rail` and
`paystack::rail` do — a caller must be able to find out what to put in
`destination` for your specific rail without reading your provider's own
Go source.

Everything else in `Order` that has no `PayRequest` home (`EventID`, `OrgID`,
`BuyerName`, most of `Metadata`) is simply **not sent** — note this as an
information-loss gap, don't invent a workaround. `metadata[patala_reference]
= req.reference` (or your provider's equivalent metadata field) is the one
metadata key both `stripe` and `paystack` still set, since `reference` is
core to reconciliation.

If cackle's `Begin` has a fallback for an empty/optional `Order` field (e.g.
Paystack's `if currency == "" { currency = "ZAR" }`), check whether
`PayRequest::validate()` (called first, always) already makes that branch
unreachable — `paystack::rail`'s module docs give a worked example. If so,
DON'T port the fallback; note in your docs that it's structurally dead here.

## 4. `Capabilities` vs `RailCapabilities` — the "gap" fields

```rust
pub struct RailCapabilities {
    pub class: RailClass,          // CustodialReversible | NonCustodialFinal
    pub reversible: bool,
    pub requires_kyc: bool,
    pub holds_funds: bool,
    pub currencies: Vec<String>,
    pub settlement: Settlement,    // Instant | Seconds(u32) | Days(u8)
    pub atomic_multi_party: bool,  // ALWAYS false for a fiat rail -- see below
}
```

cackle's `Capabilities` is `{Currencies, Countries, Flow, Refunds, Payouts,
Webhooks, ZeroDecimalOK}`. Mapping:

- `Currencies` -> `currencies`. **Preserve cackle's own semantics exactly.**
  Some cackle adapters are `nil` (unrestricted/broad — Stripe); some are a
  real hardcoded list (Paystack's 5 currencies). Port whichever your
  provider actually has; do not invent a list cackle doesn't have, and don't
  silently narrow a `nil` (broad) provider to a guessed list.
- `Countries` -> **no `RailCapabilities` field exists.** There is nowhere to
  put this. Note it as a dropped field in your rail's module docs (see
  `registry.rs`'s `CapabilityFilter` doc for the identical point at the
  registry layer).
- `Flow` -> **no field exists either.** `RailClass` (`CustodialReversible`/
  `NonCustodialFinal`) is the closest thing `patala_core` has, and every
  fiat provider you'll port is `CustodialReversible` — but that's a coarser
  distinction than cackle's `FlowRedirect`/`FlowInline`/`FlowManual`/
  `FlowInvoice`. If your provider is redirect-based (the common case: a
  hosted checkout page), `charge()` should still return a `Receipt`, with
  the redirect URL carried in `proof` (see §5) — never a struct field, since
  none exists.
- `Refunds`/`Payouts`/`Webhooks`/`ZeroDecimalOK` -> **no fields exist.**
  `Refunds` roughly informs whether you implement `refund()` for real or
  leave the trait default (§7). `Webhooks` informs whether you write a
  `webhook.rs` at all. `Payouts` (recipient/transfer management, e.g.
  cackle's Paystack `ListBanks`/`CreateRecipient`) is **out of scope** —
  `PaymentRail` is about moving money INTO a merchant account, not paying
  organisers out of one; don't port payout-management methods.
  `ZeroDecimalOK` isn't a capability field at all here — it's a PROPERTY of
  whether you actually route every amount conversion through
  `crate::currency` (or your provider's own equivalent, if it disagrees with
  the general ISO-4217 table — see §8). Get it right, don't report it.
- `requires_kyc` and `settlement` (as `Settlement::Days(n)`) -> **cackle has
  NO field for either, for ANY adapter.** This isn't provider-specific; it's
  a structural gap between the whole `Capabilities` struct and
  `RailCapabilities`. Every existing rail in this crate (and
  `patala-hyperswitch`) handles it the same way: add `requires_kyc: bool`
  and `settlement_days: u8` to your `<Provider>Config`, default
  `requires_kyc = true` (the honest assumption for any custodial card/bank
  rail) and `settlement_days = 2` (card-network T+2, `PATALA.md` §3's own
  example) unless your provider's actual public docs state a different
  typical settlement window (e.g. some real-time bank-transfer rails settle
  same-day — if so, use that and cite where you got it). Make both
  configurable via env vars, following `stripe::config`/`paystack::config`'s
  exact naming pattern (`<PROVIDER>_REQUIRES_KYC`, `<PROVIDER>_SETTLEMENT_DAYS`).
- `atomic_multi_party` -> always `false`, for every fiat rail, with no
  exception and no config knob. N payouts through any processor are N
  independent API calls; there is no way to make them land atomically. Set
  `atomic_multi_party: false` literally in your constructor (do not compute
  it from config) — `patala-core`'s own test suite
  (`capabilities::tests::every_fiat_processor_rail_in_this_workspace_declares_no_atomic_multi_party`)
  greps every file in `patala-fiat/src` and `patala-hyperswitch/src` for
  exactly this and fails the build if a new rail omits it or sets it `true`.
- `holds_funds` -> always `true` for a fiat processor (`PATALA.md` §1, §8:
  this describes the PROCESSOR's custody, never patala's). Every fiat rail
  in this crate sets this unconditionally.

## 5. Honest pending/redirect lifecycle (binding, not optional)

A `charge()` that hasn't actually settled money yet MUST return
`Receipt { amount_minor: 0, .. }`. `charge()` returning `Ok(_)` is NEVER
itself proof of settlement — only `verify()` returning `Ok(true)` is. This is
`patala_core::Receipt`'s own documented contract, and it's what makes a
redirect-based checkout (buyer hasn't paid yet when `Begin`/`charge` returns)
honestly representable at all. Concretely:

- If your provider's create-transaction call succeeds but the buyer hasn't
  paid (the overwhelmingly common case for a hosted-checkout redirect flow):
  `amount_minor: 0`, `settled_at_unix: 0`.
- Embed whatever your provider's OWN settlement-state-check needs (a session
  id, a transaction reference, a payment intent id — whatever `verify()`
  will look up by) in `proof`, as a small `serde`-serialized struct in your
  `proof.rs`. See `stripe::proof::ChargeProof` (session id + status
  snapshot) vs `paystack::proof::ChargeProof` (nothing load-bearing — your
  provider might not even need a proof struct if, like Paystack, its own
  reference IS the value you supplied, with no separate provider-assigned
  id — check whether your provider works this way before assuming you need
  a Stripe-shaped proof).
- `patala_core::Receipt` has **no field for a redirect/checkout URL.**
  Every hosted-checkout rail in this crate carries it inside `proof` instead
  (`redirect_url: Option<String>` on `ChargeProof`) purely for caller UI
  convenience — `patala_core` treats `proof` as fully opaque either way, so
  this is safe, just note it in your `proof.rs` doc comment the same way the
  existing two do.

## 6. Fail-closed `verify()`

`verify(&self, receipt: &Receipt) -> Result<bool>` must:

1. Return `Ok(false)` (never `Err`, never assume valid) if:
   - `receipt.rail_id` doesn't match `self.id()`.
   - `receipt.proof` doesn't decode into your provider's own proof shape
     (garbage/tampered proof).
   - Re-fetching from the provider shows the charge is not (yet, or no
     longer) in a genuinely-settled state. **Enumerate every status value
     your provider's API can return and map ALL of them explicitly** — a
     `match` with a fail-closed default arm, exactly like cackle's own
     `switch` statements do (`stripe.go`'s `payment_status` switch,
     `paystack.go`'s `status` switch). An unrecognised/new status string
     must never be treated as settled.
   - The provider's reported amount is LESS than `receipt.amount_minor`
     (never exact equality — see below) — this is the anti-fraud check
     mirroring cackle's `Reconcile`/`ErrAmountMismatch` ("pay R10, claim
     R1000" must be rejected). Use `>=`, not `==`, because a genuine
     just-charged `Receipt` legitimately has `amount_minor: 0` (pending, see
     §5) and must still verify `true` once the provider confirms settlement.
   - The provider's reported currency doesn't match `receipt.currency`.
2. Return `Err(_)` ONLY for a genuine operational failure to even perform
   the check (the HTTP request itself failed, a non-2xx/malformed response
   that isn't a content-level "not settled" answer). Never use `Err` to mean
   "probably not settled" — that's what `Ok(false)` is for.
3. **Always re-fetch from the provider.** Never trust a locally-cached
   status (including whatever snapshot you embedded in `proof` at charge
   time) as the verdict — only as a lookup KEY for the fresh call.

`patala_core::verify` returns only `bool` — cackle's `Verify`/`Webhook`
return a rich `Result` (status/amount/currency/`PaidAt`/`EventID`). The
`EventID` (used by cackle's `SeenStore`/`HandleWebhook` for webhook replay
protection) has **no home in `bool`**. If your provider's webhook path needs
replay protection, put an `event_id: String` field on your webhook module's
own event struct (see `stripe::webhook::StripeWebhookEvent`/
`paystack::webhook::PaystackWebhookEvent`) — replay-dedup is then the
CALLER's job (keyed on `(rail_id, event_id)`), same as cackle's own
`HandleWebhook` orchestration is a layer above `Provider.Webhook` itself.

## 6b. `verify_webhook()` — the push path, on the trait

`verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent>`
is the trait's push counterpart to `verify()`. Implement it on your rail as
a thin wrapper over the pure function in your `webhook.rs`:

```rust
async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
    let event = crate::<provider>::webhook::verify_and_parse(
        &self.config.webhook_secret,
        &delivery.raw_body,
        delivery.header_or_empty("X-Your-Signature"),
    )
    .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    Ok(WebhookEvent::settlement(
        &self.id,
        event.event_id,
        event.reference,
        event.settled,
        event.amount_minor,
        event.currency,
    ))
}
```

Rules:

1. **Fail closed as `Err`, never as a negative status.** A missing,
   malformed, stale or mismatched signature is `Err(Error::InvalidRequest)`.
   Returning `Ok` means "this delivery genuinely came from my processor".
   Reserve `Err(Error::Rail)` for a rail that could not perform the check at
   all (PayPal's verification is a live API call, so it has both).
2. **Never claim settlement you did not establish.** If your scheme
   authenticates a notification that names an object and nothing else
   (`btcpay`, `coinbasecommerce`, `opennode`, `lnbits`), return
   `WebhookEvent::unconfirmed(&self.id, event_id, object_id)` —
   `WebhookStatus::Unconfirmed`, not `settled: false`. `NotSettled` means
   "the rail established this has NOT settled"; `Unconfirmed` means "the
   rail cannot say". Collapsing them is the exact dishonesty
   `WebhookStatus` exists to prevent.
3. **Read headers off `delivery` by name**, case-insensitively
   (`delivery.header_or_empty("Cko-Signature")`). Timestamp tolerances read
   `delivery.now_unix`, never the system clock, so a delivery is
   reproducible in a test. A scheme whose secret is in the URL rather than a
   header (`lnbits`) reads `delivery.query_param("secret")`.
4. **`event_id` must be non-empty and stable** across redelivery of the same
   event — a caller cannot suppress a duplicate it cannot name. Replay-dedup
   itself stays the CALLER's job, keyed on `(rail_id, event_id)`, exactly as
   cackle's `HandleWebhook`/`SeenStore` sits above `Provider.Webhook`.
5. If your processor has no push delivery at all, **leave the trait default**
   (`Err(Error::Unsupported("verify_webhook"))`) — see `manual.rs`. Do not
   write a stub that appears to work.

Add your adapter to `tests/webhook_coverage.rs`'s `adapters()` list, naming
the headers your scheme documents. That file asserts, for every compiled-in
adapter, that `verify_webhook` is implemented (not the trait default), that a
forged delivery is rejected, and that the header names you listed are the ones
your rail actually reads. `./scripts/check-features.sh` fails the build if a
`src/<provider>/` directory exists with no entry there, so a new adapter
cannot silently under-run the harness.

## 7. `refund()` — almost always NEW code, not a port

Check cackle's `Provider` interface (`provider.go`): **it has no `Refund`
method at all.** `Capabilities.Refunds` is descriptive metadata some
adapters set `true` (Stripe) and some set `false` with an explicit
"supports it, not implemented here" comment (Paystack) — neither is Go code
you can port.

`patala_core::PaymentRail` DOES require every rail to answer `refund()`
(default: `Err(Error::Unsupported("refund"))`). Where the processor
genuinely supports refunds (check ITS public API docs, not cackle):

- Write it as **new code**, grounded directly in that provider's own public
  Refund API docs — cite the URL, exactly as `stripe::rail::refund`/
  `paystack::rail::refund` do in their doc comments.
- Same honesty conventions as everywhere else: a pending/async refund
  returns `Receipt { amount_minor: 0, .. }` until the provider confirms it
  actually moved money back; only a provider-confirmed terminal "succeeded"
  status reports the real amount.
- If you cannot find your provider's refund API documented, or the
  processor doesn't support refunds at all (e.g. most crypto/stablecoin
  rails, some real-time bank-transfer rails), leave the trait default
  (`Err(Unsupported)`) and say so in your module docs — do NOT fabricate a
  refund implementation.
- If cackle's own file has a `Refunds: false` with a "not implemented here"
  comment (like Paystack), that is a hint the processor supports it and it's
  a legitimate gap to fill — not a reason to skip it.

## 8. Money and currency rules (binding — read `src/currency.rs` first)

- **Every amount is an integer minor unit (`u64` here, `int64` in cackle) —
  NEVER a float, anywhere, in any request/response model or conversion.**
- **Always route currency exponent lookups through `crate::currency`**
  (`exponent`/`minor_to_major_string`/`major_string_to_minor`/`normalize`/
  `validate`), never a hardcoded `/100` or `*100`. This is the single
  highest-value thing this port protects against — a wrong exponent is a
  100x over/undercharge.
- **Check whether your provider disagrees with the general ISO-4217 table**
  before assuming it uses this crate's `currency` module untouched. Some
  providers do NOT use "integer minor units" the way Stripe/Paystack/
  Razorpay do — cackle's own `internal/payments/currency.go` doc comment
  names several: Flutterwave, Xendit, Midtrans, Mercado Pago, PayU, and
  iyzico all take (or can take) a **decimal MAJOR-unit string** on the wire
  (e.g. `"100.50"` meaning R100.50, not `10050`). If you're porting one of
  these, use `crate::currency::minor_to_major_string`/`major_string_to_minor`
  to convert at the wire boundary — do NOT assume `amount_minor` passes
  through unchanged the way it does for Stripe/Paystack.
- **Check for a provider-specific EXCEPTION to the general table.** Stripe is
  the worked example: `stripe::models` has its own
  `STRIPE_FORCED_TWO_DECIMAL` list (ISK/UGX — ISO-4217 says exponent 0, but
  Stripe's own docs say multiply by 100 anyway) and refuses three-decimal
  currencies outright rather than guess. If your provider's docs call out
  ANY currency-specific quirk like this, port it exactly — cite the source,
  same as `stripe::models`'s doc comments do.
- Port cackle's OWN tests for whatever conversion logic you touch. If your
  provider has no currency quirks of its own, you don't need new tests in
  `currency.rs` — you're just calling the already-tested `crate::currency`
  functions — but you DO need `models.rs`/`rail.rs` tests asserting your
  provider's specific mapping (see `stripe::models::tests` for the ISK/UGX
  case as a template).

## 9. Porting an `httptest` test to `wiremock`

Cackle's Go tests use `net/http/httptest.NewServer` with a handler closure
that inspects the request and writes a canned response. The Rust equivalent
is `wiremock`. Mapping:

```go
// cackle
ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
    if !strings.HasSuffix(r.URL.Path, "/transaction/verify/ord_1") {
        t.Fatalf("unexpected path %s", r.URL.Path)
    }
    w.Write([]byte(`{"status":true,"data":{"status":"success","amount":5000,"currency":"ZAR"}}`))
}))
defer ts.Close()
p := &PaystackProvider{baseURL: ts.URL, ...}
```

```rust
// patala-fiat
let server = MockServer::start().await;
Mock::given(method("GET"))
    .and(path("/transaction/verify/ord_1"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "status": true,
        "data": {"status": "success", "amount": 5000, "currency": "ZAR"}
    })))
    .mount(&server)
    .await;
let rail = rail_for(server.uri()); // test helper that overrides base_url
```

Rules:

- **Give your rail a private, test-only `base_url` field**, defaulted to the
  real API base in `new()`, overridden directly in `#[cfg(test)]` (same-file
  tests have access to private fields — see `stripe::rail::tests::rail_for`/
  `paystack::rail::tests::rail_for`). Never make the base URL a public,
  always-overridable config knob just for testing.
- **A cackle test that asserts the server is NEVER called** (e.g. a
  three-decimal currency refused before any network call) maps to: start a
  `MockServer` with NO `Mock` registered for the relevant path. `wiremock`
  panics if an unmatched request arrives, so this proves the adapter
  short-circuited before touching the network — see
  `stripe::rail::tests::charge_refuses_three_decimal_currency_without_calling_server`.
- **A cackle HMAC-signed-webhook test**: compute the signature yourself in
  the test (same secret, same algorithm) using the `hmac`/`sha2`/`hex`
  crates directly — see `stripe::webhook::tests::sig_header`/
  `paystack::webhook::tests::sign`. Port EVERY signature-failure case
  cackle has: missing signature, tampered body (signed correctly, then body
  mutated after), wrong secret, malformed/non-hex signature, and (if your
  provider has one) a stale timestamp.
- **A cackle test with a slow/hanging handler (timeout test)**: these test
  cackle's own `http.Client{Timeout: ...}` wiring. Port them if you want
  extra confidence, but they're the least essential to port faithfully since
  `reqwest::Client::builder().timeout(...)` (already wired in every
  `<Provider>Rail::new`) is generic HTTP-client behavior, not
  provider-specific logic.
- **Response-size-limit tests** (cackle's `paystackReadLimited`/
  `stripeReadLimited`/oversized-response tests): this crate's
  `httpshared::bounded_len_check` covers the CONTRACT (oversized is always
  rejected) but not the streaming MECHANISM (see that module's own honesty
  note on why) — a wiremock test asserting a huge response is rejected is
  still worth porting if you want the coverage, but isn't required to prove
  fidelity to cackle's specific mechanism, since this crate already
  disclosed the mechanism difference once, crate-wide, in `httpshared.rs`.

## 10. Honesty rules (binding, repeated for emphasis)

- **State plainly that your adapter is UNVERIFIED AGAINST LIVE.** No rail in
  this crate (or `patala-hyperswitch`) has been run against a live processor
  account from this environment. Every unit test mocks HTTP. Say this in
  your `mod.rs` or `rail.rs` module doc, exactly as `stripe`/`paystack` do.
- **Never fabricate a receipt, a balance, or a "success" the processor
  didn't actually return.** If a status is ambiguous or unrecognised, treat
  it as NOT settled — never guess in the optimistic direction.
- **Pending ≠ settled**, always — see §5. This is the single most important
  invariant in this whole crate.
- **`holds_funds: true` describes the PROCESSOR, never patala.** No code
  path in your rail may receive, store, or forward actual funds — every
  function here only ever moves JSON describing a request to move money
  that the processor itself carries out.
- **Cite your sources.** Every wire-shape assumption, every currency quirk,
  every refund-API detail should have a doc comment naming where it came
  from — cackle's own file/function (`stripe.go`'s `stripeAmount`, e.g.), or
  the processor's own public docs URL for anything cackle didn't already
  need (like a refund endpoint).

## 11. Checklist before you're done

- [ ] `cargo build -p patala-fiat` (no features) still succeeds and pulls in
      zero new dependencies (`cargo tree -p patala-fiat -e normal` — your
      provider's crates must not appear).
- [ ] `cargo build -p patala-fiat --features <provider>` succeeds in
      isolation (not just `--all-features`).
- [ ] `cargo test -p patala-fiat --all-features` — all tests pass, including
      every cackle test you ported.
- [ ] `cargo clippy -p patala-fiat --all-features --all-targets` — clean.
- [ ] `cargo fmt -p patala-fiat -- --check` — clean.
- [ ] Every ported cackle test has a Rust equivalent, OR an explicit doc
      comment explaining why it was intentionally not ported (e.g. a
      timeout test, a payout-management test for something out of scope).
- [ ] Every place your rail's behavior diverges from cackle's Go — a
      reinterpreted `PayRequest` field, a `RailCapabilities` field with no
      cackle equivalent, a `refund()` that's new code, a currency-table
      deviation — has a doc comment saying so, citing the cackle
      file/function (or the processor's own docs) it came from or diverges
      from.
- [ ] `src/lib.rs` updated: `#[cfg(feature = "<provider>")] pub mod
      <provider>;` + re-exports, matching the `stripe`/`paystack` pattern.
- [ ] `Cargo.toml`: new feature added, new deps `optional = true` under it.
- [ ] `rail.rs` implements `verify_webhook` (or documents why the trait
      default is correct), and `tests/webhook_coverage.rs` has a
      `#[cfg(feature = "<provider>")]` entry constructing your rail.
      `./scripts/check-features.sh` fails the build if it does not.
