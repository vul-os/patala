# patala

<!-- Plain-text badge on purpose: rendering this README triggers no external
     image fetches — the same no-default-network-calls ethos as the rails. -->
<sub><a href="LICENSE-MIT">MIT</a> OR <a href="LICENSE-APACHE">Apache-2.0</a> · Rust · non-custodial · no token</sub>

A sovereign, centerless payment-rail substrate. One interface to move value —
fiat or crypto — that any product can vendor and self-host. The platform
holds no funds, takes no cut, and no one owns the network.

**patala** is Sesotho/Setswana for *"to pay."* Part of the Vulos family
(`vula` = "open"). `PATALA.md` is the anchor spec and single source of truth;
this README describes what is actually built, honestly, as it lands.

## Status: foundational — built and unit-tested, rails unverified against live networks

The core, the rails and the polyglot layer are all in this repo. `make check`
runs two passes and both are gates: **168 offline tests** in the default
workspace build, and **547 more** once every processor feature is compiled in
(`cargo test -p patala-fiat --all-features` + `cargo test -p patala-py
--features fiat-all`). Clippy-clean, fmt-clean; the default build pulls no
chain and no processor.

What that does *not* mean: no rail here has been run against a live network or
a live merchant account from this repo — each says so plainly in its own
README, and the crypto rails name the exact step to validate (fund a testnet
account, run the `#[ignore]`d, env-gated live test). Treat the rails as a
tested foundation to validate against testnet/sandbox, not as
production-proven. The things that genuinely executed end-to-end are the
Python binding, the Go binding and the sidecar — real round-trips over a real
interpreter, real cgo, and a real socket. All three are CI jobs now: the two
Rust passes, the Python smoke run, and the Go binding's test suite (CI
installs `uniffi-bindgen-go` at the pinned tag and uses the C toolchain the
runner already has). The Go binding used to be the one that was executed but
not enforced; it is enforced.

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
patala-core/   trait + capability model + FailoverRail + MockRail + errors + receipt + webhook
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
| `patala-core` | trait + capability model + `FailoverRail` + `MockRail` + the webhook seam | — | 19 + 1 doctest | offline by design |
| `patala-fiat` | 20 direct processor adapters + the ISO-4217 currency table + the offline `manual` rail | custodial, reversible | 533 (all features) | **no — no live merchant account** |
| `patala-solana` | SPL-USDC on Solana, ported from `magnetite-seams/src/solana/` | non-custodial, final | 41 (+1 gated) + 1 doctest | **no — testnet step in its README** |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`) | non-custodial, final | 29 (+1 gated) + 1 doctest | **no — testnet step in its README** |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 20 | **no — needs a live instance** |
| `patala-py` | one UniFFI surface → Python and Go today, Swift/Kotlin/wasm later | — | 14 Rust + 24 Go binding tests (`patala-go/bindingtest`) + ✓ ran under Python 3.13 and Go 1.25 | executed, and now CI-enforced |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | 9 (6 HTTP round-trips + 3 unit) | executed |

One honest caveat on that table: **the sidecar's rail registry is still
mock-only.** The server, its auth, its error mapping and all five endpoints
are real and exercised over a real socket, but `default_registry()` registers
exactly one rail — `"mock"`. Reaching a Solana, Stellar, Hyperswitch or fiat
rail *through the sidecar* needs the per-rail registration its
`src/registry.rs` documents and does not yet have. Everything else in the
table is built code with tests behind it.

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
gets the same six methods on `PaymentRail`: `id`, `capabilities`, `quote`,
`charge`, `verify`, and `verify_webhook`. The last one is the *push* path:
`verify` is for when you hold a receipt and want it re-derived; `verify_webhook`
is for when the processor calls you and you need to know the delivery is
genuine. Both live on the trait deliberately — anything beside it is invisible
to every consumer that dispatches through `dyn PaymentRail`, which leaves them
able only to poll.

## Deferred (designed for, not built)

Any-stablecoin mint generalization, an Algorand rail, and a gateway-discovery
phonebook. See `PATALA.md` §4. (A direct PayFast rail was on this list; it now
exists, as `patala-fiat`'s `payfast` adapter.)

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE) — © VulOS. No token. No protocol tax.

### ⚠️ Open owner decision: crate `license` metadata says `MIT`, the repo offers `MIT OR Apache-2.0`

Recorded here rather than changed, because it is the owner's call and not a
maintenance detail.

All seven crates declare `license = "MIT"` in their `Cargo.toml`
(`patala-core`, `patala-fiat`, `patala-py`, `patala-sidecar`, `patala-solana`,
`patala-stellar`, `patala-hyperswitch`). This repo ships both `LICENSE-MIT`
and `LICENSE-APACHE`, and the line above offers the pair.

**Nothing is over-claimed.** The metadata is *narrower* than what is offered,
so a consumer reading only the crate metadata gets MIT — a licence they
genuinely have. Nobody is misled into relying on a grant they were never
given, which is why this is not filed as a bug and why it was not changed
unilaterally.

**But it is still a mismatch, with two concrete consequences.** Anyone
resolving licences from crate metadata (`cargo deny`, `cargo about`, an SBOM
generator, a corporate policy scanner) will see MIT alone and will not know
the Apache-2.0 option exists — and Apache-2.0 is the one that carries an
explicit patent grant, which is often exactly why a downstream user wants the
dual offer. Second, the two sources disagree, and a reader who notices has to
work out which one governs.

The Rust-ecosystem convention for a dual-licensed crate is
`license = "MIT OR Apache-2.0"` (a valid SPDX expression; this is what the
Rust project itself and most of the ecosystem use). Adopting it would be a
one-line change in each of the seven manifests and would broaden, never
narrow, what is granted.

**Not changed here.** Which licences are offered is the owner's decision, and
a change that broadens a grant should be made deliberately by whoever holds
the copyright, not as a drive-by consistency fix. Filed for that decision.

---

<p align="center">
  <a href="https://vulos.org"><img src="docs/assets/vulos-logo.png" alt="vulos" height="20"></a><br>
  <sub><a href="https://vulos.org"><b>vulos</b></a> — open by design</sub>
</p>