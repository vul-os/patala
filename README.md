<p align="center">
  <img src="brand/logo.svg" alt="The patala mark: a cowrie shell" width="96" height="96">
</p>

<h1 align="center">patala</h1>

<p align="center">
  A sovereign, centerless payment-rail substrate. One interface to move value —
  fiat or crypto — that any product can vendor and self-host.
</p>

<!-- Plain-text badge on purpose: rendering this README triggers no external
     image fetches — the same no-default-network-calls ethos as the rails. -->
<p align="center">
<sub><a href="LICENSE-MIT">MIT</a> OR <a href="LICENSE-APACHE">Apache-2.0</a> · Rust · non-custodial · no token</sub>
</p>

The platform holds no funds, takes no cut, and no one owns the network.
**patala** is Sesotho/Setswana for *"to pay."* Part of the Vulos family
(`vula` = "open"). `PATALA.md` is the anchor spec and single source of truth;
this README describes what is actually built, honestly, as it lands.

patala is a **library and a sidecar — there is no GUI**. Everything below is
either a crate, a trait, or a process you run next to your own app.

## Status: foundational — built and unit-tested; one rail has one live testnet result

The core, the rails and the polyglot layer are all in this repo. `make check`
runs two passes and both are gates: **293 offline tests** across the seven
landed crates in the default workspace build, and **572 more** once every
processor feature is compiled in
(`cargo test -p patala-fiat --all-features` + `cargo test -p patala-uniffi
--features fiat-all`). Clippy-clean, fmt-clean; the default build pulls no
chain and no processor.

What that does *not* mean: **no rail here has been run against a live
merchant account from this repo**, and only **one** rail has been run
against a live network at all — `patala-stellar`, twice, both on
**testnet**, both 2026-07-30: a single-leg USDC-shaped payment built and
submitted through the real `StellarRail::charge` API, independently
confirmed by `StellarRail::verify` reading it back from Horizon (B7), and
separately, a 3-instalment recurring/pre-authorized schedule (B4,
`recurring::RecurringPlan`) that settled its first instalment immediately,
had its second instalment genuinely **rejected by real Horizon**
(`tx_bad_minseq_age_or_gap`) when resubmitted too early and then accepted
once the pacing floor elapsed, and had its third, still-outstanding
instalment permanently invalidated by a real on-chain cancellation
(`tx_bad_seq` on resubmission) — transaction hashes and ledger sequences for
both in `patala-stellar/README.md`. Read both narrowly — neither says
anything about mainnet, and neither says anything about atomic multi-party
splits (`patala-stellar` now has one — `StellarRail::charge_split`/
`verify_split`, B1 — tested offline only, never run against a live network),
or about any other rail, which each still say plainly, in their own
READMEs, that they have not been run live and name the exact step to
validate (fund a testnet account, run the `#[ignore]`d, env-gated live
test). Treat the rails as a tested foundation to validate against
testnet/sandbox, not as production-proven.

The things that genuinely executed end-to-end are the **Python binding, the
Go binding and the sidecar** — real round-trips over a real interpreter,
real cgo, and a real socket. All three are CI jobs now: the two Rust passes,
the Python smoke run, and the Go binding's test suite (CI installs
`uniffi-bindgen-go` at the pinned tag and uses the C toolchain the runner
already has).

## The idea

Payment adapters split into two kinds, and patala treats them completely
differently:

| | Fiat processors (Stripe, Paystack, Xendit, …) | Crypto rails (Solana, Stellar, …) |
|---|---|---|
| Shape | REST calls + webhook verification | tx construction + signing + chain RPC |
| Trust | custodial, **reversible** (chargebacks), KYC, T+2 | non-custodial, **final**, wallet-based, near-instant |
| Build vs. adopt | **adopt** — Hyperswitch already ships 100+, Apache-2.0, Rust, self-hostable | **build** — this is the part nobody provides non-custodially |

The whole value add is (a) the non-custodial crypto rails, and (b) a thin
capability layer that presents both classes behind one honest interface, with
failover, and never blurs which class you're getting. See `PATALA.md` §2 for
the full reasoning.

<p align="center">
  <img src="brand/settlement-flow.svg" alt="Diagram: patala sits beside the money path, never in it. It quotes, charges and verifies; value itself moves directly, either wallet-to-wallet on a crypto rail or payer-to-processor-to-payee on a fiat rail." width="720">
</p>

## The seam

```
patala-core/   trait + capability model + FailoverRail + MockRail + errors + receipt + webhook
```

Every consumer of patala programs against one trait — `PaymentRail` — and one
capability descriptor — `RailCapabilities`. Nothing names a provider-specific
type. The settlement class (`CustodialReversible` vs `NonCustodialFinal`)
lives in the type, not a flag, because it changes what you owe the payer.

This is `patala-core`'s own crate-level doctest, word for word — it runs
under `cargo test --doc` on every `make check`:

```rust
use patala_core::{MockRail, PayRequest, PaymentRail, RailClass};

let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);

let req = PayRequest {
    amount_minor: 500, // 5.00 USDC — integer minor units, never a float
    currency: "USDC".into(),
    destination: "wallet-or-processor-token".into(),
    reference: "order-1".into(),
};

// `charge` returns the Receipt — the entitlement.
let receipt = rail.charge(&req).await.unwrap();
assert_eq!(receipt.reference, "order-1");

// Gate on `verify` returning `Ok(true)`, never on `charge` merely having
// returned `Ok`: a receipt can be stored and re-checked later, and only
// `verify` re-derives whether it still holds.
assert!(rail.verify(&receipt).await.unwrap());
```

Swap `MockRail` for a real one — or wrap several in a `FailoverRail`, which
tries them in order and refuses to silently cross from a `NonCustodialFinal`
request to a `CustodialReversible` rail — and nothing else in the code above
changes; that's the point of the seam. See `patala-core/README.md` for the
`FailoverRail` example, the full method list, and test instructions.

## Non-custodial, always

No code path anywhere in this repo may make patala hold funds. A rail can set
`holds_funds: true` on its own capabilities — that's the *rail's* processor
custodying money, e.g. Stripe behind Hyperswitch — but the substrate itself
never does. There is no balance table, no payout queue, no ledger.

## What's built

| Crate | What it is | Class | Tests | Live-verified? |
|---|---|---|---|---|
| `patala-core` | trait + capability model + `FailoverRail` + `MockRail` + the webhook seam + the destination seam | — | 38 + 3 doctests | offline by design |
| `patala-fiat` | 20 direct processor adapters + the ISO-4217 currency table + the offline `manual` rail | custodial, reversible | 552 (all features) | **no — no live merchant account** |
| `patala-solana` | SPL-USDC on Solana, ported from `magnetite-seams/src/solana/` | non-custodial, final | 56 (+1 gated) + 2 doctests | **no — testnet step in its README** |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`) | non-custodial, final | 84 (+3 gated) + 5 doctests | **no — testnet step in its README** |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 23 | **no — needs a live instance** |
| `patala-uniffi` | the one UniFFI surface, namespace `patala` → Python and Go today, Swift/Kotlin/wasm later | — | 11 Rust (20 with `fiat-all`) + 19 top-level Go binding tests, 34 with the `fiat` build tag (`patala-go/bindingtest`) + ✓ ran under Python 3.13 and Go 1.25 | executed, and now CI-enforced |
| `patala-py` | the Python wheel over `patala-uniffi` (cdylib + generated `patala.py`) | — | 3 (namespace + re-export) + the CI `smoke-python` job | executed, and now CI-enforced |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | 15 (12 HTTP round-trips + 3 unit) | executed |

One honest caveat on that table: **the sidecar's rail registry is still
mock-only.** The server, its auth, its error mapping and all six endpoints
are real and exercised over a real socket, but `default_registry()` registers
exactly one rail — `"mock"`. Reaching a Solana, Stellar, Hyperswitch or fiat
rail *through the sidecar* needs the per-rail registration its
`patala-sidecar/src/registry.rs` documents and does not yet have. Everything
else in the table is built code with tests behind it.

**Fiat coverage is Hyperswitch's coverage, plus twenty direct adapters.** Any
processor Hyperswitch supports is a config value — **Paystack is supported**
(confirmed in Hyperswitch's connector list), so it's free through the adapter.
A processor Hyperswitch lacks — **PayFast**, for example (confirmed absent) —
gets a direct adapter in `patala-fiat` against the same `PaymentRail` trait;
PayFast is one of the twenty that exist today. Nothing is ever locked out.

Every rail beyond the mock is feature-gated and optional; the default build of
this repo stays fully offline no matter how many rails exist here.

## One trait, both directions

Every consumer — Rust, Python, Go, or an HTTP client talking to the sidecar —
gets the same seven methods on `PaymentRail`: `id`, `capabilities`, `quote`,
`charge`, `verify`, `verify_webhook`, and `validate_destination`.
`verify` is for when you hold a receipt and want it re-derived; `verify_webhook`
is the *push* path, for when the processor calls you and you need to know the
delivery is genuine; `validate_destination` is the *pre-flight* path, for
checking a payout address offline before any money moves. All three live on the
trait deliberately — anything beside it is invisible to every consumer that
dispatches through `dyn PaymentRail`, which leaves them able only to poll.

### Paying a customer back

`refund` returns `Unsupported` on every `NonCustodialFinal` rail — finality is
the whole point of that class — and that is not a gap. Giving the money back
there is a **compensating payment**: a second, independent `charge` to an
address the **customer** supplies, never the address the payment came from,
which is very often an exchange withdrawal address where the funds cannot be
credited back to them. `validate_destination` is the offline check on that
address; every verdict it returns requires a human to confirm, because patala
does not detect exchange-owned addresses and will not guess. The whole flow,
including the wording to show a customer, is in
[`docs/compensating-payments.md`](docs/compensating-payments.md).

## The polyglot layer — one adapter, three ways in

Every adapter is written once, in Rust, in `patala-core` or a rail crate.
Nothing is reimplemented per language:

<p align="center">
  <img src="brand/one-interface.svg" alt="Diagram: four consumers — Rust, Python, Go, HTTP — all reach every rail through the one PaymentRail trait, never directly." width="720">
</p>

1. **Rust** — direct, `patala-core` plus whichever rail crates you enable.
2. **`patala-uniffi`** — the one UniFFI surface (namespace `patala`),
   generating both the Python binding packaged by `patala-py/` and (via
   `uniffi-bindgen-go`) the Go binding in `patala-go/`. Real round-trips,
   real cgo, CI-enforced on both languages.
3. **`patala-sidecar`** — a thin local HTTP server over the core, token-gated
   and fail-closed. Any language with an HTTP client can drive the substrate
   with zero FFI; keys live in one hardened process instead of being smeared
   across every app.

## Security

No code path holds funds; every receipt fails closed if it can't be
verified; a rail that can't do a refund or a webhook check returns
`Unsupported` rather than faking one. See `SECURITY.md` for the reporting
process and the full scope — and for the plain statement that **patala
publishes no artifacts today**, so there is nothing to sign or checksum yet.

## Deferred (designed for, not built)

Any-stablecoin mint generalization, an Algorand rail, and a gateway-discovery
phonebook. See `PATALA.md` §4. (A direct PayFast rail was on this list; it now
exists, as `patala-fiat`'s `payfast` adapter.)

## Brand

The mark in [`brand/`](brand/) is the source of truth. Every icon this repo
ships — favicon, PWA and app icons, the mark in the README and on the site — is
rendered from `brand/logo.svg` rather than redrawn, so there is one approved
drawing and no second copy to drift.

Copy it outward, never edit a derived copy, and never edit `brand/` to match
something downstream.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE) — © VulOS. No token. No
protocol tax. All seven crates in this workspace declare
`license = "MIT OR Apache-2.0"` in their `Cargo.toml`, matching the pair
offered here — a tool resolving licences from crate metadata (`cargo deny`,
`cargo about`, an SBOM generator) sees the same grant a human reading this
file does.

---

<p align="center">
  <a href="https://vulos.org"><img src="docs/assets/vulos-logo.png" alt="vulos" height="20"></a><br>
  <sub><a href="https://vulos.org"><b>vulos</b></a> — open by design</sub>
</p>
