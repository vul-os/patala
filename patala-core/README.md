# patala-core

The core seam of `patala` — one trait, one capability descriptor, and the two
things every self-hoster gets before a single real rail exists: an offline
default and class-respecting failover.

This crate is early and foundational. It is `patala-core` alone — no rail
beyond the mock ships here yet. Read `../PATALA.md` for the full plan; this
README describes what is actually built.

## What's here

- **`PaymentRail`** — the trait every rail implements: `id`, `capabilities`,
  `quote`, `charge`, `verify`, `refund` (default `Unsupported`),
  `verify_webhook` (default `Unsupported`).
- **`WebhookDelivery` / `WebhookEvent` / `WebhookStatus`** — the push side of
  the seam. A rail's *pull* path is `verify` (you hold a `Receipt` and ask the
  rail to re-derive it); its *push* path is `verify_webhook` (the processor
  calls you, and the rail says whether that delivery is genuine and what it
  claims). Both are on the trait deliberately: anything beside the trait is
  invisible to every consumer that dispatches through `dyn PaymentRail` — the
  UniFFI binding, the sidecar — which can then only ever poll.
  `WebhookStatus` is three states, not a bool, because several real schemes
  authenticate a notification without asserting anything about money;
  `Unconfirmed` says so rather than claiming the payment did not settle.
- **`RailClass`** — `CustodialReversible` | `NonCustodialFinal`. The
  settlement class lives in the type, not a bool, because it changes what you
  owe the payer: a refundable "pending" state with a card form, or a wallet
  address with a signed final receipt. Never flatten it.
- **`RailCapabilities`** — `class`, `reversible`, `requires_kyc`,
  `holds_funds`, `currencies`, `settlement`, `atomic_multi_party`. Everything
  a consumer is allowed to know about a rail, without naming its provider.
  `atomic_multi_party` is always `false` for every fiat processor rail
  (structurally: N payouts are N independent API calls) and, today, `false`
  for every crypto rail too — a chain can support it in principle, but no
  rail here exposes it as an operation yet. A consumer that needs one calls
  `RailCapabilities::require_atomic_multi_party` and is refused rather than
  silently handed N separate payments (`docs/shared-economics.md` §5).
- **`FailoverRail`** — wraps `Vec<Box<dyn PaymentRail>>`, tries them in order,
  falls through on error. It will not silently cross from a
  `NonCustodialFinal` request to a `CustodialReversible` rail (or the
  reverse) — that changes the guarantee you're handing the payer. Opt in
  explicitly with `.allow_cross_class(true)` if you mean it.
- **`MockRail`** — the offline default. Deterministic, no network, no
  external crypto dependency. This is what keeps the default build — and
  your CI — running with no chain and no processor reachable.

Money is always an integer `amount_minor: u64` plus a currency string. Never
a float, anywhere in this crate.

## Using it

```rust
use patala_core::{FailoverRail, MockRail, PayRequest, PaymentRail, RailClass};

# #[tokio::main]
# async fn main() {
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
# }
```

Swap `MockRail` for a real one (once it exists — see `../PATALA.md` §4) and
nothing else in the code above changes. That's the point of the seam.

## What you get today vs. what's coming

| Today | Coming (other waves, see `../PATALA.md`) |
|---|---|
| `patala-core`: trait, capabilities, `FailoverRail`, `MockRail` | `patala-solana` (moved from magnetite), `patala-stellar` (new) |
| Fully offline default build | `patala-hyperswitch` (adopts the self-hosted fiat processor set) |
| | Python binding, HTTP sidecar |

Nothing beyond `patala-core` is a dependency of `patala-core` — real rails
depend on this crate, never the reverse. That keeps this crate's default
build offline forever, not just today.

## Non-custodial invariant

No type or method here can make `patala` itself hold funds. A rail's
`RailCapabilities::holds_funds` describes *that rail's own processor*
(Stripe/Paystack/… behind Hyperswitch, for instance) — never the substrate.
patala-core has no balance, no ledger, no payout queue. See `../PATALA.md` §1
and §8.

## Testing

```
cargo test -p patala-core
```

All tests are offline — no network, no chain, no external process. That
includes the `FailoverRail` cross-class boundary test, the `MockRail`
charge→verify round trip, a deliberately tampered receipt failing `verify`,
and `refund` returning `Unsupported` by default.

## Honesty note

`MockRail`'s "signature" is a small keyed digest built with `std` only — it
is not a cryptographic primitive and isn't trying to be one. It exists so
`verify` round-trips a genuine receipt and rejects a mutated one, offline.
Real rails prove receipts with their chain's or processor's actual signature
scheme; nothing in this crate asks you to trust the mock's digest as
anything more than a deterministic stand-in for testing.

MIT. Part of the Vulos family (`vula` = "open").
