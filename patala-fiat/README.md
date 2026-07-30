# patala-fiat

<sub><a href="../LICENSE-MIT">MIT</a> · Rust · offline by default · 20 processor adapters + the currency table</sub>

The fiat side of [patala](../README.md): direct `patala_core::PaymentRail`
adapters for twenty payment processors, the ISO-4217 minor-unit currency
table they all share, a provider registry, and an always-available offline
`manual` rail.

Ported from cackle's Go payment package (`internal/payments/` +
`internal/money/`). `PORTING.md` is the repeatable recipe for adding one
more; this file is what a *consumer* needs.

## Status

**Unit-tested offline; UNVERIFIED AGAINST LIVE.** No live merchant account
for any of these processors was reachable from the environment this crate was
written in. Every request/response shape was checked against cackle's own
adapter (which cites the processor's published docs) and every test here
mocks HTTP with `wiremock`. A green `cargo test -p patala-fiat --all-features`
proves this crate builds the requests those docs describe and parses the
responses they describe. It is not proof any adapter works against a live
sandbox — validate that yourself before taking money with it.

## Offline by default

The `default` feature set is **empty**. The currency table, the registry and
the `manual` rail compile and test with zero optional dependencies — no
`reqwest`, no `hmac`/`sha2`/`hex`. Each processor is its own Cargo feature and
pulls in only what it needs:

```toml
# Just the currency table and the offline manual rail — no network stack.
patala-fiat = { version = "0.1", path = "../patala-fiat" }

# One processor.
patala-fiat = { version = "0.1", path = "../patala-fiat", features = ["stripe"] }
```

That is why the workspace root keeps this crate in `default-members` while the
crypto rails sit outside it: `cargo build` at the root compiles this crate and
still links no HTTP client.

## The currency table

`patala_fiat::currency` is the ISO-4217 minor-unit table, 147 currencies,
merged from cackle's two copies into the one canonical table cackle's own
comment says should exist. Five repos in this suite carry their own version of
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
assert!(currency::exponent("XXX").is_err());       // unknown code
assert!(currency::exponent("").is_err());          // not a code at all
assert!(currency::major_string_to_minor("1.005", "USD").is_err()); // over-precise
assert!(currency::major_string_to_minor("-1.00", "USD").is_err()); // negative
```

| Function | What it does |
|---|---|
| `lookup(code) -> CurrencyInfo` | code, exponent, name |
| `exponent(code) -> u8` | minor-unit exponent — 0, 2 or 3 |
| `validate(code)` / `normalize(code)` / `name(code)` | accept lowercase, trim, fail closed |
| `supported_currencies() -> Vec<&str>` | every code, in table order |
| `minor_to_major_string(minor, code)` | `10050, "ZAR"` → `"100.50"` |
| `major_string_to_minor(s, code)` | `"100.50", "ZAR"` → `10050` |

Properties worth knowing before you depend on it:

- **Everything is integer minor units.** No float touches money anywhere in
  this crate, and `major_string_to_minor` refuses more fractional digits than
  the currency allows rather than silently truncating.
- **It fails closed on an unknown code**, unlike cackle's
  `payments/currency.go`, which defaults an unrecognised code to exponent 2.
  That deviation is deliberate and documented at the top of `src/currency.rs`.
- **It is pinned.** `tests/currency_table.rs` asserts a checksum over every
  `(code, exponent, name)` triple, the row count, and the full zero-decimal
  and three-decimal code lists. Drift is detected and has to be justified; the
  file's own docs say how. It runs in the default feature set, so nothing can
  turn it off.
- **No ledger arithmetic.** cackle's `money.Amount` `Add`/`Sub`/`Mul` is not
  ported — no rail here needs it. If you need it, port it deliberately rather
  than reaching for floats.

## The rails

Twenty processors, each behind a feature of the same name, each implementing
`patala_core::PaymentRail`, plus the always-on `manual`:

`adyen`, `btcpay`, `checkoutcom`, `coinbasecommerce`, `flutterwave`,
`iyzico`, `lnbits`, `mercadopago`, `midtrans`, `mollie`, `opennode`,
`payfast`, `paypal`, `paystack`, `payu`, `razorpay`, `square`, `stripe`,
`xendit`, `yoco`.

They are reachable three ways, and every way is the same trait:

- **Rust** — construct `StripeRail::new(config)?` directly, or use
  `patala_fiat::Registry` to resolve one by name.
- **Python / Go / Swift / Kotlin** — `patala-py`'s UniFFI surface exposes all
  twenty through one by-name constructor, `PatalaRail::new_fiat(name, config)`.
- **Any language with an HTTP client** — `patala-sidecar`.

### Honesty conventions (binding — `PATALA.md` §8)

- **`charge()` returning `Ok` is never settlement.** A charge awaiting the
  buyer returns `Receipt { amount_minor: 0, .. }`. Gate on
  `PaymentRail::verify` returning `Ok(true)`.
- **`verify()` always re-fetches** from the processor and returns `Ok(false)`
  on any doubt — wrong rail, malformed proof, unsettled status, amount or
  currency mismatch. `Err` means the check could not be performed, never
  "probably not settled".
- **`holds_funds: true` describes the processor**, never patala. No function
  in this crate receives, stores or forwards funds; it moves JSON describing a
  payment the processor itself carries out.
- **Nothing is fabricated** — no receipt, balance or success a processor did
  not return.

## `destination` is not a payout address on any rail here

The thing callers most often assume wrongly. `patala_core::PayRequest`
documents `destination` as "a wallet address for a crypto rail, or an **opaque
processor-side destination token** for a fiat rail", and every rail in this
crate is the second kind. Concretely it is one of exactly three things:

| Shape | Rails | What the string actually is |
|---|---|---|
| Redirect URL | adyen, checkoutcom, iyzico, mercadopago, mollie, payfast, paypal, square, stripe, xendit, yoco | The URL the **buyer's browser** returns to after the hosted checkout — Stripe's `success_url`, Adyen's `returnUrl`, Mollie's `redirectUrl`, Square's `checkout_options.redirect_url`, Xendit's `success_redirect_url`, … |
| Buyer email | flutterwave, midtrans, paystack, payu | The **buyer's** email address, which these processors require to open a transaction. |
| Unread | btcpay, coinbasecommerce, lnbits, manual, opennode, razorpay | Nothing. The rail never reads it; `PayRequest::validate()` merely requires it be non-empty. |

None of the three is a place money goes, so **no rail in this crate ever
reports `DestinationStatus::StructurallyValid`** — that status means "a
well-formed address for the network this rail pays on", and claiming it would
tell a caller a `success_url` had been vetted as somewhere to send a customer's
money. The honest ceiling is `Unknown`: "a human must decide".

What `validate_destination` *does* still decide, offline, in
`patala_fiat::destination`:

* A redirect-URL rail refuses anything that is not an absolute `http(s)` URL
  with a host, and flags plain `http://` without refusing it (processors accept
  it in test mode; refusing it would refuse a payment that would have worked).
* A buyer-email rail refuses anything plainly not an email address.
* Either refuses a **blockchain address by name** — "this looks like a Solana
  address, and this rail's `destination` is the URL the buyer returns to" —
  because "invalid" sends someone back to re-type the same wrong thing.
* Either refuses a pasted **Stellar secret seed** as a private-key disclosure,
  without repeating the value. A leaked key is leaked whatever field it went
  into.
* An unread-destination rail invents no format check at all, including no
  refusal of a wallet address: it is genuinely harmless in a field nothing
  reads, and a guard firing at a non-defect is its own kind of dishonesty.
* Every rail refuses a blank destination, matching `PayRequest::validate()`.

`tests/webhook_coverage.rs` enforces this across every compiled-in adapter: a
new adapter that inherits the trait default fails, one that ever reports
`StructurallyValid` fails, and `scripts/check-features.sh` fails the build if
an adapter directory exists that its `dest_shape()` table does not classify.

### Giving a customer their money back

Not a compensating payment to a customer-supplied address — that is the crypto
rails' pattern. Every rail here is `CustodialReversible`, so use
`PaymentRail::refund`: the money goes back the way it came and **no destination
is involved**. The rails whose processor scheme has no refund API return
`Error::Unsupported("refund")` and say so; there, the refund happens in the
processor's own dashboard.

## Webhooks

Inbound webhook verification is on the trait, as
`PaymentRail::verify_webhook(&self, delivery: &WebhookDelivery)`. Forward the
processor's request **verbatim** — same bytes, same headers, same query
string:

```rust
use patala_core::{PaymentRail, WebhookDelivery, WebhookStatus};

let delivery = WebhookDelivery::new(raw_body, now_unix)
    .with_header("Stripe-Signature", sig_header);

let event = rail.verify_webhook(&delivery).await?;   // Err = not authentic
match event.status {
    // Reconcile amount_minor/currency against your own stored order first.
    WebhookStatus::Settled => { /* ... */ }
    WebhookStatus::NotSettled => { /* the rail says it has not settled */ }
    // The delivery is authentic but says nothing about money: look up your
    // stored Receipt for `event.object_id` and call `verify` on it.
    WebhookStatus::Unconfirmed => { /* ... */ }
}
```

Two things this deliberately does not do:

- **It never re-encodes your body.** Every scheme signs the exact bytes the
  processor sent, so a body that has been through a JSON round-trip will not
  verify. Pass the raw bytes.
- **It never claims settlement it did not establish.** BTCPay, Coinbase
  Commerce, OpenNode, LNbits and Mollie authenticate a notification that names
  an object and nothing else; those report `Unconfirmed`, not `NotSettled`.

Replay suppression stays yours, keyed on `(rail_id, event_id)` —
`event.event_id` is non-empty and stable across redelivery of the same event.

`tests/webhook_coverage.rs` asserts every compiled-in adapter implements this
and fails closed, and pins each scheme's documented header names.

## `manual`

`manual` needs no config, no network and no feature flag. It is the "bank
transfer, a human confirms it later" rail: `charge()` returns instructions and
`amount_minor: 0`, and an operator marks it paid through `ManualRail`'s own
`mark_paid`/`mark_failed` — inherent methods outside the trait, so they are
reachable from Rust but not through a binding that only holds
`dyn PaymentRail`.

## Tests

```sh
cargo test -p patala-fiat                # default: currency table, registry, manual
cargo test -p patala-fiat --all-features # every adapter (both run in CI)
```

The default run and the all-features run are both gates in `make check`; see
the workspace `Makefile`.

## License

MIT — © VulOS. No token. No protocol tax.
