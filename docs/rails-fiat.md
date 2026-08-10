# Fiat rails — Hyperswitch and the twenty direct adapters

Fiat is the half patala deliberately does **not** rebuild. Mature fiat
orchestration already exists, so the strategy is adopt-first with a direct
escape hatch:

- **`patala-hyperswitch`** — one `PaymentRail` that talks to a **self-hosted**
  [Hyperswitch](https://github.com/juspay/hyperswitch) instance (Apache-2.0,
  Rust), presenting its whole processor set as a single rail. No processor
  SDKs are vendored.
- **`patala-fiat`** — twenty direct processor adapters against the same trait,
  for the processors Hyperswitch does not cover, plus the ISO-4217 currency
  table and an always-available offline `manual` rail.

Both are `CustodialReversible`. The processor custodies the money and
chargebacks are possible; `holds_funds: true` on a rail's capabilities
describes *that processor*, never patala.

**Fiat coverage is Hyperswitch's coverage, plus twenty.** Paystack is in
Hyperswitch's connector list, so it is free through the adapter. PayFast is
confirmed absent from it — and is one of the twenty direct adapters. Nothing
is ever locked out.

## Hyperswitch (`patala-hyperswitch`)

A thin HTTP client, nothing more. You run Hyperswitch yourself and point the
adapter at it; which processor actually moves the money — Stripe, Paystack, or
Hyperswitch's own merchant-account routing — becomes a **config value**, never
a code branch in patala.

Configuration is never hardcoded. `HyperswitchConfig::from_env()` reads
`HYPERSWITCH_BASE_URL`, `HYPERSWITCH_API_KEY`, `HYPERSWITCH_CONNECTOR`,
`HYPERSWITCH_WEBHOOK_SECRET`, `HYPERSWITCH_REQUIRES_KYC`,
`HYPERSWITCH_CURRENCIES`, `HYPERSWITCH_SETTLEMENT_DAYS` and
`HYPERSWITCH_TIMEOUT_SECS`; `base_url` and `api_key` are required, mirroring
the config type's own invariant. Leaving `connector` unset lets Hyperswitch's
routing decide.

This crate is outside `default-members`, so a plain `cargo build` at the root
never pulls in its HTTP client.

`destination` here is Hyperswitch's `payment_token` — a reference to a payment
method tokenised out of band. It is not an address, so the honest ceiling for
`validate_destination` is `Unknown`, with `Malformed` for a blank string.

**UNVERIFIED AGAINST LIVE** — no live Hyperswitch instance was reachable from
this repo. 23 offline tests.

## The twenty direct adapters (`patala-fiat`)

Ported from cackle's Go payment package. Each processor is its own Cargo
feature of the same name, each implements `patala_core::PaymentRail`, and the
`default` feature set is **empty**:

`adyen`, `btcpay`, `checkoutcom`, `coinbasecommerce`, `flutterwave`,
`iyzico`, `lnbits`, `mercadopago`, `midtrans`, `mollie`, `opennode`,
`payfast`, `paypal`, `paystack`, `payu`, `razorpay`, `square`, `stripe`,
`xendit`, `yoco` — plus the always-on `manual`.

```toml
# Currency table, registry and the offline manual rail. No network stack.
patala-fiat = { path = "../patala-fiat" }

# One processor, and only that processor's dependencies.
patala-fiat = { path = "../patala-fiat", features = ["stripe"] }
```

That is why this crate can stay *inside* `default-members` while the crypto
rails sit outside it: `cargo build` at the root compiles it and still links no
HTTP client. Measured: 17 unique crates by default, **103** with every adapter
compiled in. See [The offline default build](offline-by-default.md).

They are reachable three ways, and every way is the same trait: construct
`StripeRail::new(config)?` directly in Rust or resolve one by name through
`patala_fiat::Registry`; call `new_fiat(name, config)` on the
[Python](python.md) or [Go](go.md) binding; or, once the sidecar's registry
grows past mock, over [HTTP](sidecar.md).

## The currency table

`patala_fiat::currency` is the ISO-4217 minor-unit table — 147 currencies,
merged from two divergent in-house copies into the one canonical table their
own comment said should exist. Five repos in this suite carry a version of
this table; this is the one to reach for.

```rust
use patala_fiat::currency;

// Never assume 2 decimal places.
assert_eq!(currency::exponent("JPY").unwrap(), 0);
assert_eq!(currency::exponent("USD").unwrap(), 2);
assert_eq!(currency::exponent("KWD").unwrap(), 3);

// Integer minor units <-> the major-unit strings processors want on the wire.
assert_eq!(currency::minor_to_major_string(10_050, "ZAR").unwrap(), "100.50");
assert_eq!(currency::major_string_to_minor("1.5", "KWD").unwrap(), 1_500);

// Fails closed, always.
assert!(currency::exponent("XXX").is_err());                       // unknown code
assert!(currency::major_string_to_minor("1.005", "USD").is_err()); // over-precise
assert!(currency::major_string_to_minor("-1.00", "USD").is_err()); // negative
```

| Function | What it does |
|---|---|
| `lookup(code)` | code, exponent, name |
| `exponent(code)` | minor-unit exponent — 0, 2 or 3 |
| `validate` / `normalize` / `name` | accept lowercase, trim, fail closed |
| `supported_currencies()` | every code, in table order |
| `minor_to_major_string(minor, code)` | `10050, "ZAR"` → `"100.50"` |
| `major_string_to_minor(s, code)` | `"100.50", "ZAR"` → `10050` |

Properties worth knowing before depending on it:

- **Everything is integer minor units.** No float touches money anywhere in
  the crate, and `major_string_to_minor` refuses more fractional digits than
  the currency allows rather than silently truncating.
- **It fails closed on an unknown code**, unlike the Go original, which
  defaulted an unrecognised code to exponent 2. The deviation is deliberate
  and documented at the top of the module.
- **It is pinned.** A test asserts a checksum over every
  `(code, exponent, name)` triple, the row count, and the full zero-decimal
  and three-decimal code lists. Drift is detected and has to be justified. It
  runs in the default feature set, so nothing can turn it off.
- **No ledger arithmetic.** `Add`/`Sub`/`Mul` on a money type is not ported —
  no rail here needs it. If you need it, port it deliberately rather than
  reaching for floats.

## `destination` is not a payout address on any rail here

The thing callers most often assume wrongly. `PayRequest::destination` is "a
wallet address for a crypto rail, **or an opaque processor-side destination
token** for a fiat rail", and every rail in this crate is the second kind.
Concretely it is one of exactly three things:

| Shape | Rails | What the string actually is |
|---|---|---|
| Redirect URL | adyen, checkoutcom, iyzico, mercadopago, mollie, payfast, paypal, square, stripe, xendit, yoco | The URL the **buyer's browser** returns to after hosted checkout — Stripe's `success_url`, Adyen's `returnUrl`, Mollie's `redirectUrl`, Square's `checkout_options.redirect_url`, Xendit's `success_redirect_url`, … |
| Buyer email | flutterwave, midtrans, paystack, payu | The **buyer's** email address, which these processors require to open a transaction. |
| Unread | btcpay, coinbasecommerce, lnbits, manual, opennode, razorpay | Nothing. The rail never reads it; `PayRequest::validate()` merely requires it be non-empty. |

None of the three is a place money goes, so **no rail here ever reports
`StructurallyValid`.** That status means "a well-formed address for the
network this rail pays on", and claiming it would tell a caller a
`success_url` had been vetted as somewhere to send a customer's money. The
honest ceiling is `Unknown`: a human must decide.

What `validate_destination` still decides, offline:

- A redirect-URL rail refuses anything that is not an absolute `http(s)` URL
  with a host — and *flags* plain `http://` without refusing it, because
  processors accept it in test mode and refusing it would refuse a payment
  that would have worked.
- A buyer-email rail refuses anything plainly not an email address.
- Either refuses a **blockchain address by name**: "this looks like a Solana
  address, and this rail's destination is the URL the buyer returns to".
  "Invalid" would send someone back to re-type the same wrong thing.
- Either refuses a pasted **Stellar secret seed** as a private-key disclosure,
  without repeating the value. A leaked key is leaked whatever field it went
  into.
- An unread-destination rail invents no format check at all, including no
  refusal of a wallet address: it is genuinely harmless in a field nothing
  reads, and a guard firing at a non-defect is its own kind of dishonesty.
- Every rail refuses a blank destination.

`tests/webhook_coverage.rs` enforces this across every compiled-in adapter: an
adapter that inherits the trait default fails, one that ever reports
`StructurallyValid` fails, and `scripts/check-features.sh` fails the build if
an adapter directory exists that the shape table does not classify.
`check-features.sh` also builds and lints each of the twenty processor features
**alone**, so `--all-features` is no longer the only configuration that
compiles — see [offline by default](offline-by-default.md#what-enforces-this).

## Giving a customer their money back

Not a compensating payment — that is the crypto rails' pattern. Every rail
here is `CustodialReversible`, so use `refund`: the money goes back the way it
came and **no destination is involved**. The rails whose processor scheme has
no refund API return `Unsupported` and say so; there, the refund happens in
the processor's own dashboard.

## Webhooks

Forward the processor's request **verbatim** — same bytes, same headers, same
query string:

```rust
use patala_core::{PaymentRail, WebhookDelivery, WebhookStatus};

let delivery = WebhookDelivery::new(raw_body, now_unix)
    .with_header("Stripe-Signature", sig_header);

let event = rail.verify_webhook(&delivery).await?;   // Err = not authentic
match event.status {
    // Reconcile amount_minor/currency against your own stored order first.
    WebhookStatus::Settled => { /* ... */ }
    WebhookStatus::NotSettled => { /* the rail says it has not settled */ }
    // Authentic but says nothing about money: look up your stored Receipt
    // for `event.object_id` and call `verify` on it.
    WebhookStatus::Unconfirmed => { /* ... */ }
}
```

Two things this deliberately does not do:

- **It never re-encodes your body.** Every scheme signs the exact bytes the
  processor sent, so a body that has been through a JSON round trip will not
  verify.
- **It never claims settlement it did not establish.** BTCPay, Coinbase
  Commerce, OpenNode and LNbits authenticate a notification that names an object
  and nothing else; those four report `Unconfirmed`, not `NotSettled`. Mollie is
  **not** one of them — like iyzico, mercadopago, payfast and paypal it
  re-fetches from the processor and returns a real settlement verdict.

**An unauthenticated delivery is an `Err`, never a `WebhookEvent`.** That was
always the contract; 0.1.1 made iyzico obey it. Its `retrieveCheckoutForm`
round trip *is* its signature check — the callback carries no signature at all —
and the error was being discarded with `.ok()`, so `POST token=anything` from
an anonymous caller produced `Ok(NotSettled)`. No money could be fabricated, but
that verdict can drive a consumer's cancel-order or release-inventory path. An
unrecognised token is now `Err`.

**Replay suppression stays yours**, keyed on `(rail_id, event_id)`. Since 0.1.1
that key is guaranteed rather than merely expected: **eight rails now refuse a
delivery that carries no processor-side id** — payu, payfast, midtrans,
razorpay, adyen and paypal on an empty `event_id`, paystack and flutterwave on
an id of `"0"`. The check used to sit inside the *settled* arm only, so a
correctly signed non-settling redelivery arrived with nothing to suppress it by.

**Three rails no longer read an absent settlement-status field as settled.** Not
reachable against today's payloads, but a processor changing its shape would
have read as paid.

## Honesty conventions on this crate

- **`charge()` returning `Ok` is never settlement.** A charge awaiting the
  buyer returns `Receipt { amount_minor: 0, .. }`. Gate on `verify` returning
  `Ok(true)`.
- **`verify()` always re-fetches** from the processor and returns `Ok(false)`
  on any doubt — wrong rail, malformed proof, unsettled status, amount or
  currency mismatch. `Err` means the check could not be performed, never
  "probably not settled".
- **`holds_funds: true` describes the processor**, never patala. No function
  in this crate receives, stores or forwards funds; it moves JSON describing a
  payment the processor itself carries out.
- **Nothing is fabricated** — no receipt, balance or success a processor did
  not return.

## `manual`

No config, no network, no feature flag. The "bank transfer, a human confirms
it later" rail: `charge()` returns instructions and `amount_minor: 0`, and an
operator marks it paid through `ManualRail`'s own `mark_paid`/`mark_failed`.

Those two are **inherent methods, not trait methods** — reachable from Rust,
and from nowhere else. A binding that only holds `dyn PaymentRail` cannot see
them, which means `manual` alone is not a complete payment flow through the
Python or Go surface. If you need it to settle, that part has to be Rust. See
[Choosing a mode](choosing-a-mode.md).

## Config keys, by provider

Every key is the exact field name of that provider's own `<Provider>Config`
struct — nothing is renamed at the binding boundary. A missing key for a
required field is passed through as an empty string and rejected by that
adapter's own constructor with an `InvalidRequest` naming the field. Boolean
fields are `"true"` or anything-else, case-insensitive; `currencies` is a
comma-separated list, uppercased; numeric fields are parsed and, if present
but malformed, **rejected** rather than silently defaulted.

| Provider | Required keys | Notable optional keys / defaults |
|---|---|---|
| `manual` | *(none)* | Always available. Never dials the network. |
| `stripe` | `secret_key`, `webhook_secret` | `currencies` empty = unrestricted. |
| `paystack` | `secret_key` | `currencies` defaults to NGN/GHS/ZAR/KES/USD. |
| `adyen` | `api_key`, `merchant_account`, `hmac_key_hex`, `api_base_url` | `hmac_key_hex` must be valid hex. |
| `btcpay` | `base_url`, `api_key`, `store_id`, `webhook_secret` | `settlement_seconds` optional; unset means `Instant`. |
| `checkoutcom` | `secret_key`, `webhook_secret`, `api_base_url` | |
| `coinbasecommerce` | `api_key`, `webhook_secret` | `base_url` defaults to Coinbase Commerce's API. |
| `flutterwave` | `secret_key`, `webhook_hash` | `currencies` defaults to Flutterwave's own list. |
| `iyzico` | `api_key`, `secret_key` | `base_url` defaults to production; `currencies` to TRY/USD/EUR/GBP. |
| `lnbits` | `base_url`, `api_key`, `webhook_secret` | `quote_ttl_secs` defaults to 900; must be positive if given. |
| `mercadopago` | `access_token`, `webhook_secret` | `currencies` defaults to its own LatAm list. |
| `midtrans` | `server_key` | No `currencies` key — IDR only. |
| `mollie` | `api_key`, `webhook_url` | |
| `opennode` | `api_key` | `base_url` defaults to OpenNode's API. |
| `payfast` | `merchant_id`, `merchant_key` | `passphrase` optional. No `currencies` key — ZAR only. |
| `paypal` | `client_id`, `client_secret`, `webhook_id`, `env` (`"live"`/`"sandbox"`, exactly) | `env` has **no default** — a typo is `InvalidRequest`, never a silent point at the wrong environment. |
| `payu` | `merchant_key`, `salt` | No `currencies` key — INR. |
| `razorpay` | `key_id`, `key_secret`, `webhook_secret` | No `currencies` key — INR. |
| `square` | `access_token`, `webhook_signature_key`, `location_id`, `notification_url`, `api_base_url` | |
| `xendit` | `secret_key`, `webhook_token` | `currencies` defaults to Xendit's own list. |
| `yoco` | `secret_key`, `webhook_secret` | No `currencies` key — ZAR only. |

Every provider also accepts `requires_kyc` (default `true`, except
`btcpay`/`lnbits`/`coinbasecommerce`/`opennode`, which default `false` — the
self-hosted and crypto-adjacent ones) and `settlement_days` (default `2`,
card-network T+2) or `timeout_secs` (default `15`, or `20` for the
crypto-adjacent adapters).

## Status — UNVERIFIED AGAINST LIVE

No live merchant account for any of these processors was reachable from the
environment this crate was written in. Every request and response shape was
checked against the Go original, which cites the processor's published docs,
and every test mocks HTTP with `wiremock`.

A green `cargo test -p patala-fiat --all-features` — **570 tests** — proves
this crate builds the requests those docs describe and parses the responses
they describe. It is not proof any adapter works against a live sandbox.
Validate that yourself before taking money with it.

```bash
cargo test -p patala-fiat                # default: currency table, registry, manual
cargo test -p patala-fiat --all-features # every adapter
```

Both are gates in `make check`.

## Related documents

- [The rail interface](rails-interface.md) · [Crypto rails](rails-crypto.md)
- [The offline default build](offline-by-default.md) — the feature layout above.
- [Python binding](python.md) · [Go binding](go.md) — the by-name constructor.
- [Status](status.md) — the whole verification picture.
