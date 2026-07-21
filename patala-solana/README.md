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
