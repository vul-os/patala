# The rails & interface

Every consumer of patala programs against one trait — `PaymentRail` — and one
capability descriptor — `RailCapabilities`. Nothing names a provider-specific
type. The settlement class (`CustodialReversible` vs `NonCustodialFinal`)
lives in the type, not a flag, because it changes what you owe the payer.

## The seam

```rust
pub enum RailClass {
    /// Custodial, reversible (chargebacks possible), usually KYC'd, delayed settlement.
    /// e.g. any fiat processor behind Hyperswitch.
    CustodialReversible,
    /// Non-custodial, final (no reversal), wallet-to-wallet, near-instant.
    /// e.g. Solana/Stellar USDC.
    NonCustodialFinal,
}

pub struct RailCapabilities {
    pub class: RailClass,
    pub reversible: bool,           // chargebacks / refunds possible at the rail level
    pub requires_kyc: bool,
    pub holds_funds: bool,          // does the RAIL custody? (the substrate never does)
    pub currencies: Vec<String>,    // e.g. ["USDC", "USD", "NGN"]
    pub settlement: Settlement,     // Instant | Seconds(u32) | Days(u8)
    pub atomic_multi_party: bool,   // N payouts as ONE atomic settlement, never N API calls.
                                    // Always false for fiat; false for every rail here today.
}

#[async_trait]
pub trait PaymentRail {
    fn id(&self) -> &str;                       // stable rail id, e.g. "solana", "hyperswitch"
    fn capabilities(&self) -> &RailCapabilities;
    async fn quote(&self, req: &PayRequest) -> Result<Quote>;
    async fn charge(&self, req: &PayRequest) -> Result<Receipt>;
    async fn verify(&self, receipt: &Receipt) -> Result<bool>;     // fail-closed
    // Pre-flight: what this rail can honestly say about a destination address
    // BEFORE any money moves. Pure and offline — no network, no clock, no
    // filesystem. Never a Result: "I cannot check" is a verdict, not an error.
    fn validate_destination(&self, dest: &str) -> DestinationVerdict { /* Unknown */ }
    // Optional — a rail that can't do it returns Unsupported, never fakes it:
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> { Err(Error::Unsupported) }
    // The push path: the processor calls YOU. Fail-closed — an unauthentic
    // delivery is an Err, never an Ok with a negative status.
    async fn verify_webhook(&self, d: &WebhookDelivery) -> Result<WebhookEvent> { Err(Error::Unsupported) }
}
```

`verify` and `verify_webhook` are the pull and push halves of the same
question. Both are on the trait on purpose: webhook signature verification is
provider-specific code, and if it sits *beside* a rail rather than on the
trait, every consumer that dispatches through `dyn PaymentRail` — the UniFFI
binding, the sidecar, anything not written in Rust — cannot reach it and is
left able only to poll. `WebhookEvent::status` is three-valued, not a bool:
several real schemes authenticate a notification without asserting anything
about money, and `Unconfirmed` says exactly that instead of claiming the
payment did not settle.

Money is always an integer `amount_minor: u64` plus a currency string. Never
a float, anywhere in this crate.

## Paying a customer back on a final rail

`refund()` returns `Unsupported` on every `NonCustodialFinal` rail, and that is
the honest answer rather than a missing feature — finality is the whole point of
that class. **It does not mean the customer cannot be paid back.** It means this
rail cannot *undo* that transaction. Giving the money back is a **compensating
payment**: a second, independent `charge()` in the opposite direction, with its
own transaction, its own fee, its own confirmation and its own fresh
`reference`. The original receipt is unchanged.

The address that payment goes to must come **from the customer**, and never from
the original transaction. A sending address is very often an exchange
*withdrawal* address, and an exchange does not credit funds arriving there to
whoever withdrew from it — the money is unrecoverable by the customer, by the
merchant and by patala. BitPay, Coinbase Commerce and OpenNode all ask the
customer for a destination for exactly this reason.

`validate_destination` is the offline pre-flight check on that address, so a
consumer can tell someone "that is not a valid Solana address" at the moment
they type it. `DestinationStatus` has five variants, not a bool, because a UI
renders each differently:

| Status | Meaning |
|---|---|
| `Malformed` | Wrong alphabet, length, checksum, or empty. A **refusal**. |
| `WrongNetwork` | Well-formed, wrong chain — a Stellar `G…` in a Solana payout. A **refusal**. |
| `NotAWallet` | A program/contract account, a Solana PDA, a token mint. Nobody holds a key for it. A **refusal**. |
| `StructurallyValid` | Every offline check passed. **Not "valid", not "safe"** — the absence of a decidable defect. |
| `Unknown` | This rail cannot check at all, and says so rather than guessing. The honest answer for a fiat rail. **Never treat as valid.** |

**patala does not detect exchange addresses.** That needs commercial
address-attribution data (Chainalysis, TRM) — hosted services this workspace
will not depend on — and a heuristic would be worse than nothing: a host who
trusts "looks safe" and loses a customer's money is worse off than one told
plainly that this cannot be known. So every verdict, including the most positive
one, carries `human_must_confirm: true` and an `exchange_deposit_caveat` string
a UI shows verbatim. There is no verdict that waives the human step, and no
`is_valid()`/`is_safe()` method that could be mistaken for one.

Which rails check what, and which return `Unknown`, is in
[Status](#status). The full flow — including the wording to put in front of a
customer — is in `docs/compensating-payments.md`.

- **`FailoverRail`** wraps `Vec<Box<dyn PaymentRail>>`, tries them in order,
  falls through on error. It will not silently cross from a
  `NonCustodialFinal` request to a `CustodialReversible` rail (or the
  reverse) — that changes the guarantee you're handing the payer. Opt in
  explicitly with `.allow_cross_class(true)` if you mean it.
- **`MockRail`** is the offline default: deterministic, no network, no
  external crypto dependency. This is what keeps the default build — and
  your CI — running with no chain and no processor reachable.
- **Errors fail closed.** A receipt that cannot be verified is invalid, never
  assumed-valid.

## Using it

```rust
use patala_core::{FailoverRail, MockRail, PayRequest, PaymentRail, RailClass};

let primary = MockRail::new("solana-mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
let backup = MockRail::new("stellar-mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
let rail = FailoverRail::new(vec![Box::new(primary), Box::new(backup)]);

let req = PayRequest {
    amount_minor: 1_500,
    currency: "USDC".into(),
    destination: "wallet-address-or-processor-token".into(),
    reference: "order-42".into(),
};

let receipt = rail.charge(&req).await.unwrap();
assert!(rail.verify(&receipt).await.unwrap());
```

Swap `MockRail` for a real one and nothing else in the code above changes —
that's the point of the seam.

## The rails

| Rail | Class | What it is |
|---|---|---|
| **Mock** (`patala-core`) | — | The offline default: deterministic, no network. |
| **Solana** (`patala-solana`) | `NonCustodialFinal` | SPL-USDC on Solana. Ed25519 — the app's identity key doubles as the wallet key, no mapping table. |
| **Stellar** (`patala-stellar`) | `NonCustodialFinal` | Native USDC (Stellar Asset), Ed25519 (StrKey). Cheapest measured fees of the two crypto rails. |
| **Hyperswitch** (`patala-hyperswitch`) | `CustodialReversible` | A thin HTTP client to a **self-hosted Hyperswitch** instance, presenting its whole fiat processor set (Stripe/Paystack/Xendit/… — 100+ connectors) as one rail. Adopts Hyperswitch; does not vendor a single processor SDK. |
| **Direct fiat adapters** (`patala-fiat`) | `CustodialReversible` | Twenty processors talked to directly, one Cargo feature each: Adyen, BTCPay, Checkout.com, Coinbase Commerce, Flutterwave, iyzico, LNbits, Mercado Pago, Midtrans, Mollie, OpenNode, PayFast, PayPal, Paystack, PayU, Razorpay, Square, Stripe, Xendit, Yoco. Ships the ISO-4217 minor-unit currency table and an always-on offline `manual` rail. |

**Fiat coverage is Hyperswitch's coverage, plus twenty direct adapters.** Any
processor Hyperswitch supports is a config value — Paystack is confirmed
supported, so it's free through the adapter. A processor Hyperswitch lacks
gets a direct adapter in `patala-fiat` against the same `PaymentRail` trait;
PayFast, confirmed absent from Hyperswitch, is one of the twenty. Nothing is
ever locked out.

Every rail beyond the mock is feature-gated and optional; the default build
of the repo stays fully offline no matter how many rails exist in the tree.

## The polyglot layer

One Rust core, four ways to consume it, written once:

- **Rust crate** — direct, for a Rust consumer.
- **`patala-py`** — a Python binding over UniFFI (not PyO3, so Swift/Kotlin
  and wasm/napi bindings can follow from the same IDL rather than a new
  binding crate per language).
- **`patala-go`** — the same UniFFI surface, generated for Go (cgo; see that
  package's README for the honest trade-offs of leaving pure-static Go).
- **`patala-sidecar`** — a thin local HTTP server over the core (`quote` /
  `charge` / `verify` / `validate-destination` / `webhook` as JSON over a
  loopback socket), for any
  language with an HTTP client and zero FFI. Binds to `127.0.0.1` only, unconditionally,
  and refuses to start without `PATALA_SIDECAR_TOKEN` set — there is no
  auto-generated fallback and no unauthenticated payment route besides
  `/healthz`.

## Related documents

- [Overview](#overview) — why this exists and what it deliberately isn't.
- [Self-host & vendor](#self-host) — embedding the core, the Python binding,
  or the sidecar in your own product.
- [Status](#status) — what's tested and what's still unverified against a
  live network.
