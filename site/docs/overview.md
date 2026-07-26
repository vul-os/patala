# Overview

**patala** is a sovereign, centreless payment-rail substrate. One interface
to move value — fiat or crypto — that any product can vendor and self-host.
The platform holds no funds, takes no cut, and no one owns the network.

**patala** is Sesotho/Setswana for *"to pay."* Part of the Vulos family
(`vula` = "open"). `PATALA.md` in the repo is the anchor spec and single
source of truth; the docs here describe what is actually built, honestly, as
it lands.

## The idea

Payment adapters split into two kinds, and patala treats them completely
differently:

- **Fiat processors** (Stripe, Paystack, Xendit, …) are custodial and
  reversible — chargebacks, KYC, T+2 settlement. These already exist,
  well-built, in [Hyperswitch](https://github.com/juspay/hyperswitch)
  (Apache-2.0, Rust, self-hostable). patala adopts that instead of
  rebuilding it.
- **Crypto rails** (Solana, Stellar, …) are non-custodial and final —
  wallet-to-wallet, near-instant, no reversal. Nobody ships this
  non-custodially as a library today. This is what patala builds.

The whole value add is (a) the non-custodial crypto rails, and (b) a thin
capability layer that presents both classes behind one honest interface, with
failover, and never blurs which class you're getting.

## Non-custodial, always

No code path anywhere in this repo may make patala hold funds. A rail can set
`holds_funds: true` on its own capabilities — that's the *rail's* processor
custodying money, e.g. Stripe behind Hyperswitch — but the substrate itself
never does. There is no balance table, no payout queue, no ledger.

## What it is, and is not

**IS:**

- A thin **library** (Rust core + bindings + optional sidecar) that presents
  many payment rails behind one honest interface.
- **Non-custodial by default.** The substrate holds no funds. Crypto rails
  settle wallet-to-wallet; fiat rails are operated by their processors, and
  the substrate never becomes a custodian.
- **Centreless.** Self-hostable, no registry you must join, no central
  processor. A product can direct-connect rails itself.
- **MIT.** No token. No protocol tax.

**IS NOT:**

- Not a business, not a SaaS, not a custodial hub, not a routing service that
  takes a cut.
- Not a new payment protocol or wire format — the wire protocols already
  exist (SPL, Stellar, EIP-681, Solana Pay, x402, Lightning); patala adopts,
  it doesn't mint another.
- Not a reimplementation of a hundred fiat processors — those already exist
  (see [The rails](#rails), Hyperswitch).

## Related documents

- [The rails & interface](#rails) — `PaymentRail`, the capability model, and
  what's actually built.
- [Self-host & vendor](#self-host) — embedding patala in your own product.
- [Status](#status) — what's tested, what's live-verified, what's deferred.
