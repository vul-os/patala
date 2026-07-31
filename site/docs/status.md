# Status

## Foundational — built and unit-tested; one rail has one live testnet result

The core, the rails and the polyglot layer are all in the repo. `make check`
runs two passes and both are gates: **260 offline tests** across the seven
landed crates in the default workspace build, and **572 more** once every
processor feature is compiled in (`cargo test -p patala-fiat --all-features` +
`cargo test -p patala-py --features fiat-all`). Clippy-clean, fmt-clean; the
default build pulls no chain and no processor.

What that does *not* mean: no rail has been run against a live merchant
account from here, and **only one rail** has been run against a live network
at all — `patala-stellar`, twice, both on testnet, both 2026-07-30: a
single-leg USDC-shaped payment built and submitted through the real
`StellarRail::charge` API, independently confirmed by `StellarRail::verify`
reading it back from Horizon, and separately, a 3-instalment
recurring/pre-authorized schedule (`recurring::RecurringPlan`) that settled
its first instalment immediately, had its second instalment genuinely
**rejected by real Horizon** (`tx_bad_minseq_age_or_gap`) when resubmitted too
early and then accepted once the pacing floor elapsed, and had its third,
still-outstanding instalment permanently invalidated by a real on-chain
cancellation (`tx_bad_seq` on resubmission) — transaction hashes and ledger
sequences for both in `patala-stellar/README.md`. Read both narrowly — neither
says anything about mainnet, and neither says anything about atomic
multi-party splits (`StellarRail::charge_split`/`verify_split`, B1 — tested
offline only, never run against a live network). Every other rail — including
Stellar's own mainnet path — says plainly, in its own README, that it has not
been run live, and the crypto rails name the exact step to validate (fund a
testnet account, run the `#[ignore]`d, env-gated live test). Treat the rails
as a tested foundation to validate against testnet/sandbox, not as
production-proven.

The things that genuinely executed end-to-end are the Python binding, the Go
binding and the sidecar — real round-trips over a real interpreter, real cgo,
and a real socket.

## What's built

| Crate | What it is | Class | Tests | Live-verified? |
|---|---|---|---|---|
| `patala-core` | trait + capability model (incl. `atomic_multi_party`) + `FailoverRail` + `MockRail` + the webhook seam + the destination seam | — | 38 + 3 doctests | offline by design |
| `patala-fiat` | 20 direct processor adapters + the ISO-4217 currency table + the offline `manual` rail | custodial, reversible | 552 (all features) | no — no live merchant account |
| `patala-solana` | SPL-USDC on Solana, ported from an earlier in-house implementation | non-custodial, final | 56 (+1 gated) + 2 doctests | no — testnet step in its README |
| `patala-stellar` | native USDC on Stellar (SDF's own `stellar-xdr`/`stellar-strkey`), incl. atomic `charge_split`/`verify_split` (B1) and `recurring::RecurringPlan` | non-custodial, final | 84 (+3 gated) + 5 doctests | **testnet: yes, twice (2026-07-30)** — a single-leg payment and a 3-instalment recurring schedule; mainnet no, splits untested live, see its README |
| `patala-hyperswitch` | adapter to a self-hosted Hyperswitch (its whole processor set as one rail) | custodial, reversible | 23 | no — needs a live instance |
| `patala-py` | one UniFFI surface → Python and Go today, Swift/Kotlin/wasm later | — | 11 Rust (20 with `fiat-all`) + 19 top-level Go binding tests (`patala-go/bindingtest`) + ✓ ran under Python 3.13 and Go 1.25 | executed, and now CI-enforced |
| `patala-sidecar` | loopback HTTP over the core, token-gated, fail-closed | — | 15 (12 HTTP round-trips + 3 unit) | executed |

## Destination validation by rail

`PaymentRail::validate_destination` answers, **offline and purely**, what a rail
can honestly say about a payout address before any money moves. It is the
pre-flight step of the compensating-payment flow in
[`docs/compensating-payments.md`](https://github.com/vul-os/patala/blob/main/docs/compensating-payments.md).
Every row below is a claim you can check against the cited file.

The two crypto rails pay to a real address, so they can reach the top of the
scale. Everything else has a `destination` that is **not a place money goes**,
so its honest ceiling is `Unknown`.

| Rail | What `destination` is | Checks it performs offline | Can return | Never returns |
|---|---|---|---|---|
| **Solana** (`patala-solana/src/destination.rs`) | a wallet address | base58 alphabet (naming the look-alike character that was probably meant), 32-byte decoded length, whether the bytes are an on-curve Ed25519 point (an off-curve account is a PDA — including every canonical associated token account — and cannot sign), and a table of well-known programs and mints | `Malformed`, `WrongNetwork`, `NotAWallet`, `StructurallyValid` | `Unknown` — this rail always has an answer |
| **Stellar** (`patala-stellar/src/destination.rs`) | a wallet address | StrKey decode: version byte, base32 alphabet, length, CRC-16 checksum; then the key *type* — `G…` account vs `M…` muxed, `C…` contract, and the other StrKey kinds | `Malformed`, `WrongNetwork`, `NotAWallet`, `StructurallyValid` | `Unknown` — this rail always has an answer |
| **Mock** (`patala-core/src/mock.rs`) | a synthetic `<network>:<kind>:<label>` grammar — **not** any real chain's format | exists so a consumer can build and test its whole payout UI, every verdict included, with no chain reachable | all five (`Unknown` via `MockRail::without_destination_checks`) | — |
| **Fiat — 11 redirect-URL rails** (`patala-fiat`: adyen, checkoutcom, iyzico, mercadopago, mollie, payfast, paypal, square, stripe, xendit, yoco) | the URL the **buyer's browser** returns to after hosted checkout | absolute-URL format (every one of these processors documents the field that way), plus a pasted private key or wallet address refused **by name** | `Malformed`, `WrongNetwork`, `Unknown` | **`StructurallyValid`**, `NotAWallet` |
| **Fiat — 4 buyer-email rails** (`patala-fiat`: flutterwave, midtrans, paystack, payu) | the **buyer's email address** | email format, plus the same private-key / wallet-address refusals | `Malformed`, `WrongNetwork`, `Unknown` | **`StructurallyValid`**, `NotAWallet` |
| **Fiat — 6 ignore-it rails** (`patala-fiat`: btcpay, coinbasecommerce, lnbits, opennode, razorpay, manual) | **nothing** — the rail never reads it; `PayRequest::validate()` merely requires it be non-empty | blank only | `Malformed` (blank), `Unknown` | **`StructurallyValid`**, `WrongNetwork`, `NotAWallet` |
| **Hyperswitch** (`patala-hyperswitch/src/rail.rs`) | Hyperswitch's `payment_token` — a reference to a payment method tokenized out of band | blank only | `Malformed` (blank), `Unknown` | **`StructurallyValid`**, `WrongNetwork`, `NotAWallet` |

The fiat and Hyperswitch rows are not a gap, and their `Unknown` ceiling is the
point rather than a limitation. `StructurallyValid` means "this is a well-formed
address for the network this rail pays on". A fiat rail pays on no network and
its `destination` is a redirect URL, an email, or a string it ignores — so
claiming that verdict would tell a caller a `success_url` had been vetted as
somewhere to send a customer's money. It has not, and it is not. Those rails
still check what *is* decidable about their own field, so a wallet address or a
Stellar secret seed pasted into a Stripe `success_url` is refused by name at the
moment someone types it rather than becoming a charge the processor rejects.

Giving a customer their money back differs by class, and the table above is why:
on the **fiat** rails (all `CustodialReversible`) it is `refund` — the money goes
back the way it came and no destination is involved. On the **crypto** rails
(`NonCustodialFinal`) `refund` is `Unsupported` and it is the compensating
payment described above.

`patala-core`'s trait default — inherited by any rail that has not written a
parser — is `Unknown` for anything non-empty and `Malformed` for a blank string.
It is deliberately not `StructurallyValid`: a permissive default would silently
bless every parser-less rail. Guards fail closed, so a blank destination is a
refusal on every rail in the table.

**No rail — including Solana and Stellar — reports that an address is safe.**
The best available status is `StructurallyValid`, which means *no decidable
defect was found*, not *this is safe to send to*. patala does not detect whether
an address belongs to an exchange and will not: that needs commercial
address-attribution data (Chainalysis, TRM) this workspace refuses to depend on,
and a heuristic would be worse than nothing. Every verdict from every rail
therefore carries `human_must_confirm: true` and the same
`exchange_deposit_caveat` string, and there is no API to skip that step.

Reachability, which is the part that has been silently missing before in this
repo: the check is a **trait method**, so it is reachable through
`PatalaRail.validate_destination` on the UniFFI surface (Python, Go, Swift,
Kotlin) and through `POST /v1/rails/:rail_id/validate-destination` on the
sidecar, with all five verdict variants and their reason strings intact. All
five are asserted to survive each boundary distinctly — `patala-py`'s Rust
tests, `patala-go/bindingtest/destination_test.go`,
`patala-sidecar/tests/validate_destination.rs`, and the Python smoke test.

One caveat that table would otherwise hide: **the sidecar's rail registry is
still mock-only.** The server, its auth, its error mapping and all six
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
- What CI enforces: the two Rust passes, the Python binding's real end-to-end
  smoke run, and — new — the Go binding's own test suite
  (`patala-go/bindingtest`, run by `make smoke-go`). CI installs
  `uniffi-bindgen-go` at the pinned tag and uses the runner's C toolchain for
  cgo. The Go binding used to be executed by hand and enforced by nothing;
  that gap is closed, and its `make test`/`make test-fiat` targets now **fail**
  when zero tests ran instead of reporting success.

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
