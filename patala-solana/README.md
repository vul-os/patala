# patala-solana

Rail #2 of `patala` (`PATALA.md` §4): SPL-USDC on Solana, `NonCustodialFinal`.
Ported from `magnetite/magnetite-seams/src/solana/` (~1760 lines, 95 tests,
real Ed25519 signing + SPL transaction construction + JSON-RPC) and adapted to
`patala_core::PaymentRail`.

This crate is **not** in the workspace's `default-members` (see the root
`Cargo.toml`): it carries real network + crypto dependencies (`reqwest`,
`tokio`, `ed25519-dalek`, `curve25519-dalek`, `sha2`, `bs58`) on purpose, so
plain `cargo build`/`cargo test` at the workspace root never pulls it in.
Build/test it explicitly:

```sh
cargo build -p patala-solana
cargo test -p patala-solana
```

## Mapping from magnetite's seam to patala's

| magnetite (`magnetite_seams::payment::PaymentRail`) | patala (`patala_core::PaymentRail`) |
|---|---|
| `checkout_item(buyer, item, split)` (async) | [`SolanaRail::charge`] — one `(amount_minor, currency, destination, reference)` `PayRequest`, not a multi-way developer/operator/protocol-fee `PaymentSplit`. `patala_core`'s seam has no split concept, so `charge` builds one memo + one SPL `TransferChecked` leg. |
| `verify_receipt` / `verify_receipt_for_item(r, item)` (**sync**) | [`SolanaRail::verify`] (**async**) — `patala_core::PaymentRail::verify` is already `async`, so the `block_on`/thread-spawn scaffolding magnetite needed to do network I/O from a sync method is gone. That was adapter plumbing, not crypto; every actual check inside it (chain/mint match, reference-hash binding, tx success, commitment, signer flag, memo, exact balance deltas) is intact. There is also no separate `item` parameter — `patala_core::Receipt::reference` carries it, and the binding hash covers `(payer, reference)`, so tampering `reference` after the fact breaks the same way tampering magnetite's `item` did. |
| `checkout(buyer, split)` (unbound) | Not ported — `patala_core`'s `charge` always requires a `destination` and `reference`; there is no "unbound" checkout in this seam. |
| `open_channel` / `escrow` | Not ported — magnetite-specific (hosting-fee payment channels / wager escrow), no equivalent in `patala_core::PaymentRail`. Both were `Unsupported` stubs in magnetite too (no on-chain program backed them). |
| (rail always signed every receipt with a fixed `rail` keypair as a "self-consistency marker, NOT the security boundary") | Dropped. `patala_core::Receipt::proof` is already the designated place for a rail's binding data, and the actual security boundary — on-chain state: the buyer's real Ed25519 tx signature, the memo binding, the exact token-balance deltas — is unchanged. The redundant non-cryptographic signature added no security value magnetite's own comment didn't already disclaim. |
| — | `refund` → `Error::Unsupported("refund")`, explicitly. Crypto settlement is final; this rail will not fake a reversal (`PATALA.md` §3, §8). |

## What ported byte-for-byte

* `src/tx.rs` — base58 pubkeys, associated-token-account (PDA) derivation via
  SHA-256 + curve25519 on-curve check, SPL `TransferChecked` + Memo
  instruction encoding, legacy transaction message serialization. Identical
  logic to `magnetite-seams::solana::tx`, only the `PubKey` import path
  changed.
* `src/rpc.rs` — the `SolanaRpc` seam (so verification is unit-testable
  offline against a fake) plus the real `HttpRpc` JSON-RPC client. Unlike
  magnetite, `HttpRpc` is not behind a `solana` cargo feature here — this
  whole crate plays that role for the `patala` workspace.
* `src/keys.rs` — the Ed25519 keypair, trimmed from magnetite's
  `RawKeypairAuth` (which also carried challenge/response login and scoped
  token minting, out of scope for a payment rail) down to sign/verify/pubkey/
  env-loading (`SOLANA_KEYPAIR_PATH` / `SOLANA_KEYPAIR`).

## Ed25519 keys (`PATALA.md` §6)

Solana is Ed25519-native: `SolanaRail`'s configured `keys::Keypair` is
simultaneously the signing identity and the wallet the funds move from —
there is no identity → wallet mapping table. The public key bytes *are* the
base58 wallet address.

## Money

USDC has 6 decimals (`tx::USDC_DECIMALS`). Every amount in this crate is an
integer count of micro-USDC (`u64`) — never a float, per `patala_core`'s
`PayRequest`/`Quote`/`Receipt` and `PATALA.md` §8. `quote()`'s `fee_minor` is
always `0`: the real Solana network fee is paid in SOL (lamports) by whoever
signs the transaction, not deducted from the USDC amount transferred, and
`patala_core::Quote` has no field for a fee in a *different* currency than
the request's own. That is a real cost of using this rail; it is stated here
rather than hidden.

## Paying a customer back: `validate_destination`

Settlement here is final, so `refund()` is `Error::Unsupported("refund")` and
stays that way. That is not the same as "the customer cannot be paid back":
giving the money back on Solana is a **compensating payment** — a second,
independent `charge` to an address the *customer* supplies, with its own
transaction, its own fee and its own confirmation.

Never reuse the address the payment came from. It is very often an exchange
**withdrawal** address, and an exchange does not credit funds arriving there to
the customer who withdrew from it. BitPay, Coinbase Commerce and OpenNode all
ask the customer for a destination instead; that is the correct design, not a
fallback.

`destination::validate` is the offline, pure check to run on that address at
the moment a person types it. It needs no rail, no RPC URL and no keypair — it
runs in a browser, on a gate device with no uplink, and in a test with no
validator.

| Verdict | When |
|---|---|
| `Malformed` | Not base58 (the offending character is named, including the `0`/`O` and `I`/`l` look-alikes base58 omits), or base58 that does not decode to exactly 32 bytes, or surrounded by whitespace, or empty. A pasted Stellar **secret seed** lands here too, with its own loud refusal — see below. |
| `WrongNetwork` | A well-formed address for another chain, **named**: a Stellar `G…`/`C…`/`M…`, an Ethereum/EVM `0x…`, a Sui/Aptos `0x…`, a Bitcoin address in either era. "This looks like a Stellar address" is the message that saves the money; "invalid" is not. |
| `NotAWallet` | A real 32-byte account nobody can be paid at: the System Program, SPL Token / Token-2022, the ATA program, Memo, Compute Budget, Stake, Vote, the Rent/Clock sysvars, the incinerator, or the USDC/wSOL **mints** themselves — plus anything **off the ed25519 curve**, which is a program-derived address. That last one catches an associated token account, and matters: `charge` derives the recipient's token account *from their wallet address*, so passing an ATA builds a transfer against the token account of a token account. |
| `StructurallyValid` | Every offline check passed. **Not** "valid" and **not** "safe" — see below. |

A pasted `S…` seed is reported as a **private key disclosure**, not as an
invalid address: the verdict says a key was leaked and what to do about it, and
deliberately never repeats the value (a verdict is shown to a person and very
likely logged on the way there).

**What this cannot decide, and does not attempt.** Whether the account exists,
is rent-exempt, or already holds a USDC token account — all chain queries, and
therefore a different method than this one. Whether an *on-curve* key is a
plain wallet or a token account created from a keypair — indistinguishable
without reading the account's owner program. And **whether the address belongs
to an exchange**: patala does not and will not guess at that, because it needs
commercial address-attribution data this suite refuses to depend on, and a
heuristic would be worse than nothing. Every verdict — including
`StructurallyValid` — therefore carries `EXCHANGE_DEPOSIT_CAVEAT` and
`human_must_confirm: true`. There is no verdict this rail can produce that
means "safe to send to".

## Honesty (`PATALA.md` §8) — UNVERIFIED AGAINST LIVE

Every offline test in `src/tests.rs` runs with **no network** — the RPC is a
scripted fake (95 assertions ported/adapted from magnetite's own offline
suite). The one test that touches a live cluster,
`live_rpc_reachable_and_unknown_signature_denies`, is `#[ignore]`d and gated
on `PATALA_SOLANA_LIVE_RPC`:

```sh
solana-test-validator -r &
PATALA_SOLANA_LIVE_RPC=http://127.0.0.1:8899 \
  cargo test -p patala-solana live_rpc -- --ignored --nocapture
```

**This crate has not been run against a live Solana RPC from this
environment.** The live path — and this rail's behavior against a real
devnet/mainnet cluster generally — is **UNVERIFIED AGAINST LIVE** until
someone runs that ignored test (or exercises `charge`/`verify` against a real
cluster) and confirms it passes.
