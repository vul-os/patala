# patala

A sovereign, centerless payment-rail substrate. One interface to move value —
fiat or crypto — that any product can vendor and self-host. The platform
holds no funds, takes no cut, and no one owns the network.

**patala** is Sesotho/Setswana for *"to pay."* Part of the Vulos family
(`vula` = "open"). `PATALA.md` is the anchor spec and single source of truth;
this README describes what is actually built, honestly, as it lands.

## Status: foundational — built and unit-tested, rails unverified against live networks

The core, four rails, and the polyglot layer are all in this repo and pass a
combined **150 offline tests** (clippy-clean, fmt-clean; the default build
pulls no chain or processor). What that does *not* mean: the crypto and fiat
rails have **not** been run against a live network from here — each says so
plainly in its own README and names the exact step to validate (fund a
testnet account, run the `#[ignore]`d, env-gated live test). Treat the rails
as a tested foundation to validate against testnet, not as production-proven.
The two things that genuinely executed end-to-end are the Python binding and
the sidecar (real round-trips over a real interpreter and a real socket).

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

## What's built

| Crate | What it is | Class | Tests | Live-verified? |
|---|---|---|---|---|
| `patala-core` | trait + capability model + `FailoverRail` + `MockRail` | — | 13 | offline by design |
| `patala-solana` | SPL-USDC on Solana, ported from `magnetite-seams/src/solana/` | non-custodial, final | 41 (+1 gated) | **no — testnet step in its README** |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`) | non-custodial, final | 29 (+1 gated) | **no — testnet step in its README** |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 18 | **no — needs a live instance** |
| `patala-py` | one UniFFI surface → Python now, Swift/Kotlin/wasm later | — | ✓ ran under Python 3.13 | executed |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | ✓ HTTP round-trip | executed |

**Fiat coverage is Hyperswitch's coverage, plus a direct-adapter escape
hatch.** Any processor Hyperswitch supports is a config value — **Paystack is
supported** (confirmed in Hyperswitch's connector list), so it's free through
the adapter. A processor Hyperswitch lacks — **PayFast**, for example
(confirmed absent) — gets its own thin `patala-<processor>` rail against the
same `PaymentRail` trait. Nothing is ever locked out.

Every rail beyond the mock is feature-gated and optional; the default build of
this repo stays fully offline no matter how many rails exist here.

## Deferred (designed for, not built)

Any-stablecoin mint generalization, an Algorand rail, gateway-discovery
phonebook, and a direct `patala-payfast` rail. See `PATALA.md` §4.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE) — © VulOS. No token. No protocol tax.

---

<p align="center">
  <a href="https://vulos.org"><img src="docs/assets/vulos-logo.png" alt="vulos" height="20"></a><br>
  <sub><a href="https://vulos.org"><b>vulos</b></a> — open by design</sub>
</p>