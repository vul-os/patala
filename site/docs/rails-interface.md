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
}

#[async_trait]
pub trait PaymentRail {
    fn id(&self) -> &str;                       // stable rail id, e.g. "solana", "hyperswitch"
    fn capabilities(&self) -> &RailCapabilities;
    async fn quote(&self, req: &PayRequest) -> Result<Quote>;
    async fn charge(&self, req: &PayRequest) -> Result<Receipt>;
    async fn verify(&self, receipt: &Receipt) -> Result<bool>;     // fail-closed
    // Optional — a rail that can't do it returns Unsupported, never fakes it:
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> { Err(Error::Unsupported) }
}
```

Money is always an integer `amount_minor: u64` plus a currency string. Never
a float, anywhere in this crate.

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

**Fiat coverage is Hyperswitch's coverage, plus a direct-adapter escape
hatch.** Any processor Hyperswitch supports is a config value — Paystack is
confirmed supported, so it's free through the adapter. A processor
Hyperswitch lacks gets its own thin `patala-<processor>` rail against the
same `PaymentRail` trait. Nothing is ever locked out.

Every rail beyond the mock is feature-gated and optional; the default build
of the repo stays fully offline no matter how many rails exist in the tree.

## The polyglot layer

One Rust core, three ways to consume it, written once:

- **Rust crate** — direct, for a Rust consumer.
- **`patala-py`** — a Python binding over UniFFI (not PyO3, so Swift/Kotlin
  and wasm/napi bindings can follow from the same IDL rather than a new
  binding crate per language).
- **`patala-sidecar`** — a thin local HTTP server over the core (`quote` /
  `charge` / `verify` as JSON over a loopback socket), for any language with
  an HTTP client and zero FFI. Binds to `127.0.0.1` only, unconditionally,
  and refuses to start without `PATALA_SIDECAR_TOKEN` set — there is no
  auto-generated fallback and no unauthenticated payment route besides
  `/healthz`.

## Related documents

- [Overview](#overview) — why this exists and what it deliberately isn't.
- [Self-host & vendor](#self-host) — embedding the core, the Python binding,
  or the sidecar in your own product.
- [Status](#status) — what's tested and what's still unverified against a
  live network.
