# Status

## Foundational — built and unit-tested, rails unverified against live networks

The core, four rails, and the polyglot layer are all in the repo and pass a
combined **150 offline tests** (clippy-clean, fmt-clean; the default build
pulls no chain or processor). What that does *not* mean: the crypto and fiat
rails have **not** been run against a live network from here — each says so
plainly in its own README and names the exact step to validate (fund a
testnet account, run the `#[ignore]`d, env-gated live test). Treat the rails
as a tested foundation to validate against testnet, not as production-proven.

The two things that genuinely executed end-to-end are the Python binding and
the sidecar — real round-trips over a real interpreter and a real socket.

## What's built

| Crate | What it is | Class | Tests | Live-verified? |
|---|---|---|---|---|
| `patala-core` | trait + capability model + `FailoverRail` + `MockRail` | — | 13 | offline by design |
| `patala-solana` | SPL-USDC on Solana, ported from an earlier in-house implementation | non-custodial, final | 41 (+1 gated) | no — testnet step in its README |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`) | non-custodial, final | 29 (+1 gated) | no — testnet step in its README |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 18 | no — needs a live instance |
| `patala-py` | one UniFFI surface → Python now, Swift/Kotlin/wasm later | — | ✓ ran under Python 3.13 | executed |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | ✓ HTTP round-trip | executed |

## Honesty conventions

- Every rail beyond mock: unit-tested offline; the live path sits behind an
  `#[ignore]`d test gated on an environment variable. If a rail was never run
  against a live network from this repo, its docs and commits say so
  plainly — **UNVERIFIED AGAINST LIVE** — rather than implying otherwise.
- Nothing here ever fabricates a receipt, a balance, or a "success" a rail
  didn't actually return.
- The default build stays offline: no new mandatory dependencies, no
  network, and CI needs no chain or processor.

## Deferred — designed for, not built

Any-stablecoin mint generalisation, an Algorand rail, a gateway-discovery
phonebook, and a direct `patala-payfast` rail (PayFast is confirmed absent
from Hyperswitch's connector list today). See `PATALA.md` §4 for the full
reasoning.

## First consumer

The Solana rail was ported from an in-house implementation (~1,760 lines, 95
tests, a live-RPC-gated ignored test) and adapted to the shared
`PaymentRail` trait — the pattern any first adopter of a new rail is expected
to follow.

## License

MIT OR Apache-2.0 — © VulOS. No token. No protocol tax.

## Related documents

- [Overview](#overview) — what patala is and deliberately isn't.
- [The rails & interface](#rails) — the trait, the capability model, what
  each rail actually does.
- [Self-host & vendor](#self-host) — embedding patala in your own product.
