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
  (see [Fiat rails](rails-fiat.md), Hyperswitch).

## No runtime in your process

Worth stating plainly, because it is unusual and because the two sibling
products in this suite cannot say it: **patala's core is Rust, so embedding
it adds no language runtime to your process.** There is no garbage collector
to pause you, no scheduler competing with yours, no signal handlers installed
behind your back, no Go-style fork-unsafety, and nothing that has to be
initialised before `main`. A Go-cored library reached over FFI carries the Go
runtime into whatever host loads it; patala does not have one to carry.

That is measured, not inferred from the language. Loading patala's shared
library into a JVM replaces **0** of HotSpot's signal handlers and alters **0**
flags, where the same probe against a Go library reports 5 and 3; a Node
`worker_threads` worker that has entered patala exits in ~33 ms where the Go
control never exits at all; and the offline library is **844,656 bytes** against
llmux's 12,787,504. Each was taken against a Go library in the same environment
as a control. The consequences are practical: Java and Kotlin default to
in-process here where the siblings default to the sidecar. See
[The C ABI](c-abi.md) and
[Fifteen language packages](language-packages.md).

What embedding patala *does* cost is a lazily-created Tokio runtime **only in
the bindings** that need one — `patala-uniffi` creates it on first call so a
Python caller never has to run an event loop
([Python binding](python.md)) — and, if you choose the Go binding, cgo. The
cgo cost is real and is documented rather than buried:
[Go binding](go.md), and [Choosing a mode](choosing-a-mode.md) is the page
that helps you decide whether to pay it.

## Where to go next

New here? Read these three, in order:

1. [Quickstart](quickstart.md) — a `charge` → `verify` round trip in your
   language, in a few minutes, against a rail that needs no network.
2. [Choosing a mode](choosing-a-mode.md) — crate, binding, or sidecar. This is
   the decision that shapes everything after it, so make it deliberately.
3. [The rail interface](rails-interface.md) — `PaymentRail`, the capability
   model, and why the settlement class lives in the type.

Then, as you need them:

- [Fifteen language packages](language-packages.md) — a working package per
  language in `sdks/`, two modes each, with a run command for every one.
- [The C ABI](c-abi.md) — the six-function `extern "C"` surface eleven of those
  packages are built on.
- [One core, every language](polyglot.md) — the "M×1, never M×N" principle
  this repo is built on.
- [The offline default build](offline-by-default.md) — why `cargo build` here
  links no HTTP client, and how that is enforced.
- [Crypto rails](rails-crypto.md) and [Fiat rails](rails-fiat.md) — what each
  rail actually does.
- [Paying a customer back](compensating-payments.md) — the compensating-payment
  flow, including the wording to show a customer.
- [Status](status.md) — what is tested, what is live-verified, what is not.
