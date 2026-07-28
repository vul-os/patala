# Status

## Foundational — built and unit-tested, rails unverified against live networks

The core, the rails and the polyglot layer are all in the repo. `make check`
runs two passes and both are gates: **167 offline tests** in the default
workspace build, and **547 more** once every processor feature is compiled in
(`cargo test -p patala-fiat --all-features` + `cargo test -p patala-py
--features fiat-all`). Clippy-clean, fmt-clean; the default build pulls no
chain and no processor.

What that does *not* mean: no rail has been run against a live network or a
live merchant account from here — each says so plainly in its own README, and
the crypto rails name the exact step to validate (fund a testnet account, run
the `#[ignore]`d, env-gated live test). Treat the rails as a tested foundation
to validate against testnet/sandbox, not as production-proven.

The things that genuinely executed end-to-end are the Python binding, the Go
binding and the sidecar — real round-trips over a real interpreter, real cgo,
and a real socket.

## What's built

| Crate | What it is | Class | Tests | Live-verified? |
|---|---|---|---|---|
| `patala-core` | trait + capability model + `FailoverRail` + `MockRail` + the webhook seam | — | 19 + 1 doctest | offline by design |
| `patala-fiat` | 20 direct processor adapters + the ISO-4217 currency table + the offline `manual` rail | custodial, reversible | 533 (all features) | no — no live merchant account |
| `patala-solana` | SPL-USDC on Solana, ported from an earlier in-house implementation | non-custodial, final | 41 (+1 gated) + 1 doctest | no — testnet step in its README |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`) | non-custodial, final | 29 (+1 gated) + 1 doctest | no — testnet step in its README |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 20 | no — needs a live instance |
| `patala-py` | one UniFFI surface → Python and Go today, Swift/Kotlin/wasm later | — | 14 + ✓ ran under Python 3.13 and Go 1.25 | executed |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | 8 (HTTP round-trips) | executed |

One caveat that table would otherwise hide: **the sidecar's rail registry is
still mock-only.** The server, its auth, its error mapping and all five
endpoints are real and exercised over a real socket, but `default_registry()`
registers exactly one rail — `"mock"`. Reaching a Solana, Stellar, Hyperswitch
or fiat rail *through the sidecar* needs the per-rail registration its
`src/registry.rs` documents and does not yet have.

## Honesty conventions

- Every rail beyond mock: unit-tested offline; the live path sits behind an
  `#[ignore]`d test gated on an environment variable. If a rail was never run
  against a live network from this repo, its docs and commits say so
  plainly — **UNVERIFIED AGAINST LIVE** — rather than implying otherwise.
- Nothing here ever fabricates a receipt, a balance, or a "success" a rail
  didn't actually return. That extends to webhooks: a rail whose callback
  scheme authenticates a notification without asserting anything about money
  reports `Unconfirmed`, not "did not settle".
- The default build stays offline: no new mandatory dependencies, no
  network, and CI needs no chain or processor. `cargo tree -e normal` on the
  default workspace build resolves no HTTP client at all.
- What CI enforces vs what was run by hand: the two Rust passes and the
  Python binding's real end-to-end smoke run are CI jobs. The Go binding was
  executed by hand (it needs `uniffi-bindgen-go` at a pinned tag plus a C
  toolchain, which CI does not install) — so "Go: executed" means executed,
  not enforced.

## Deferred — designed for, not built

Any-stablecoin mint generalisation, an Algorand rail, and a gateway-discovery
phonebook. See `PATALA.md` §4 for the full reasoning.

A direct PayFast rail was on this list — PayFast is confirmed absent from
Hyperswitch's connector list — and is no longer deferred: it exists as
`patala-fiat`'s `payfast` adapter, one of twenty.

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
