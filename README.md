# patala

A sovereign, centerless payment-rail substrate. One interface to move value —
fiat or crypto — that any product can vendor and self-host. The platform
holds no funds, takes no cut, and no one owns the network.

**patala** is Sesotho/Setswana for *"to pay."* Part of the Vulos family
(`vula` = "open"). `PATALA.md` is the anchor spec and single source of truth;
this README describes what is actually built, honestly, as it lands.

## Status: early and foundational

Right now this repo is `patala-core` and nothing else — one trait, one
capability model, class-respecting failover, and an offline mock rail. No
real payment rail (Solana, Stellar, Hyperswitch) ships yet. Read
`PATALA.md` §4 and §9 for what's planned and in what order; don't assume
anything beyond `patala-core/` exists until it's actually in this repo.

## The idea

Payment adapters split into two kinds, and patala treats them completely
differently:

- **Fiat processors** (Stripe, Paystack, Xendit, …) are custodial and
  reversible — chargebacks, KYC, T+2 settlement. These already exist,
  well-built, in Hyperswitch (Apache-2.0, Rust, self-hostable). patala adopts
  that instead of rebuilding it.
- **Crypto rails** (Solana, Stellar, …) are non-custodial and final —
  wallet-to-wallet, near-instant, no reversal. Nobody ships this
  non-custodially as a library today. This is what patala builds.

The whole value add is (a) the non-custodial crypto rails, and (b) a thin
capability layer that presents both classes behind one honest interface, with
failover, and never blurs which class you're getting. See `PATALA.md` §2 for
the full reasoning.

## The seam

```
patala-core/   trait + capability model + FailoverRail + MockRail + errors + receipt
```

Every consumer of patala programs against one trait — `PaymentRail` — and one
capability descriptor — `RailCapabilities`. Nothing names a provider-specific
type. The settlement class (`CustodialReversible` vs `NonCustodialFinal`)
lives in the type, not a flag, because it changes what you owe the payer.

See `patala-core/README.md` for the trait itself, worked examples, and test
instructions.

## Non-custodial, always

No code path anywhere in this repo may make patala hold funds. A rail can set
`holds_funds: true` on its own capabilities — that's the *rail's* processor
custodying money, e.g. Stripe behind Hyperswitch — but the substrate itself
never does. There is no balance table, no payout queue, no ledger.

## What's coming (not built yet — see `PATALA.md` for the plan)

- `patala-solana` — the non-custodial Solana rail, moved from
  `magnetite/magnetite-seams/src/solana/` and adapted to this trait.
- `patala-stellar` — a new non-custodial Stellar rail (cheapest measured
  fees of the in-scope chains).
- `patala-hyperswitch` — a `PaymentRail` adapter to a self-hosted
  Hyperswitch instance, presenting its whole processor set as one
  `CustodialReversible` rail.
- `patala-py` (bindings) and `patala-sidecar` (local HTTP/gRPC server), so
  the same Rust-built rails are usable from any language without
  reimplementing them.

Every rail beyond the mock is feature-gated and optional; the default build
of this repo stays fully offline no matter how many rails eventually exist
here.

## License

MIT. No token. No protocol tax.
