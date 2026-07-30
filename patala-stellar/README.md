# patala-stellar

Rail #3 of `patala` (`PATALA.md` §4, §6): native Circle USDC on Stellar,
`NonCustodialFinal`. Built new for `patala` — there is no magnetite precursor
to port, unlike `patala-solana`. Real Ed25519 signing, hand-assembled XDR
transaction construction (on top of the Stellar Development Foundation's own
codec crates), and Horizon REST submission/verification.

This crate is **not** in the workspace's `default-members` (see the root
`Cargo.toml`): it carries real network + crypto dependencies (`reqwest`,
`ed25519-dalek`, `stellar-xdr`, `stellar-strkey`, `sha2`) on purpose, so plain
`cargo build`/`cargo test` at the workspace root never pulls it in.
Build/test it explicitly:

```sh
cargo build -p patala-stellar
cargo test -p patala-stellar
cargo clippy -p patala-stellar --all-targets
```

## Dependency choice: official codec crates, hand-built transaction

`PATALA.md`'s task brief for this rail allowed either "construct the
transaction envelope yourself (XDR) with `ed25519-dalek` + a light XDR
encoder" or a mature, permissively-licensed Stellar Rust SDK if one exists.

This crate uses **`stellar-xdr`** and **`stellar-strkey`** — both published
and maintained by the Stellar Development Foundation itself, both
Apache-2.0, both generated directly from the same `.x` XDR interface
definitions used by `stellar-core`. Reasoning:

- Hand-rolling the struct/union/variable-length encoding for a `Transaction`,
  its `PAYMENT` operation, `Memo`, `Preconditions`, and the
  `TransactionSignaturePayload` signing base — the way `patala-solana`
  hand-rolls Solana's much simpler legacy-message format — is exactly the
  class of bug (field order, union discriminant, length-prefix mistakes) an
  official, spec-generated codec exists to remove.
- These are **codec** crates, not a high-level "Stellar SDK": they give
  typed Rust structs (`Transaction`, `Operation`, `Asset`, `Memo`, ...) plus
  `ReadXdr`/`WriteXdr`. This crate still does all of the actual work itself —
  building the payment operation, the signing payload, the SHA-256 hash, the
  Ed25519 signature, and the signed envelope — in `src/tx.rs`. Nothing about
  *when* to submit, *how* to poll, or *what* counts as settled is delegated
  to a third party.
- `stellar-strkey` likewise only does the StrKey checksum-and-version-byte
  encode/decode (`G...`/`S...`) — a wire format `PATALA.md` §1 explicitly
  says not to reinvent — and nothing else.

If a reviewer wants zero third-party XDR code at all, `src/tx.rs` is the only
place that would need to change; `src/keys.rs`, `src/rpc.rs`, and `src/lib.rs`
do not touch `stellar-xdr` directly.

## Ed25519 keys, StrKey (`PATALA.md` §6)

Stellar is Ed25519-native, exactly like Solana: `StellarRail`'s configured
`keys::Keypair` is simultaneously the signing identity and the wallet the
funds move from. A Stellar account address (`G...`) and secret seed (`S...`)
are just StrKey encodings of the same raw 32-byte Ed25519 public key / seed —
so there is no identity → wallet mapping table, preserving the property
`PATALA.md` §6 calls out. Load a signer from `STELLAR_SECRET_KEY` (a StrKey
seed) via `keys::Keypair::from_env`; it is never logged, never serialized,
never written anywhere by this crate.

## Money

USDC on Stellar has **7 decimals** (`tx::USDC_DECIMALS`) — classic Stellar
XDR amounts are always this fixed-point `int64` scale (there is no
per-asset decimals field the way an SPL mint carries one). Every amount in
this crate is that integer count of ten-millionths (`u64`/`i64`) — never a
float, per `patala_core`'s `PayRequest`/`Quote`/`Receipt` and `PATALA.md` §8.
`quote()`'s `fee_minor` is always `0` for the same reason `patala-solana`'s
is: the real network fee is paid in XLM (stroops) by the signer, not
deducted from the USDC amount transferred, and `patala_core::Quote` has no
field for a fee in a different currency than the request's own. That is a
real, separate cost of using this rail; it is stated here, not hidden.

## No "commitment" knob

Unlike Solana's probabilistic `confirmed`/`finalized` levels, `verify`
treats a transaction Horizon reports as `successful` inside a closed ledger
as final. Stellar's federated Byzantine agreement (SCP) does not have an
intermediate "confirmed but could still be skipped" state the way a
longest-chain protocol does — a ledger closing *is* the finality event
(`PATALA.md` §6, ~3-5s). There is deliberately no extra confirmation-depth
parameter to configure.

## What `verify` checks, in order

1. Receipt names this rail (`"stellar"`) and currency (`"USDC"`).
2. `Receipt::proof` parses as this crate's binding at all.
3. The claimed asset (code + issuer) matches this rail's **configured**
   issuer — never the receipt's own say-so.
4. `Receipt::amount_minor` and the proof's own claimed amount agree
   (catches either being tampered independently).
5. A domain-separated memo-hash binding re-derives from `(rail id, source,
   destination, reference)` — a receipt cannot be re-pointed at a different
   reference or destination by editing one field.
6. **Offline, no network:** the whole `Transaction` is rebuilt from the
   binding's own scalar fields (source, destination, asset, amount, sequence
   number, fee, memo), hashed with the claimed network's passphrase, and the
   claimed Ed25519 signature is checked against the claimed source over that
   hash. This is a real cryptographic guarantee checkable with no RPC at
   all — only the actual secret key could produce it.
7. **Online, the real trust anchor:** Horizon is asked for this transaction
   hash. Not found ⇒ deny. Found ⇒ `successful` must be `true`, and the
   envelope XDR Horizon *actually returns* is decoded and compared
   operation-for-operation (source, destination, asset, amount, memo,
   sequence number, fee) against the binding — never trusting Horizon's
   summary fields alone for the money-moving details.
8. Any Horizon failure at step 7 propagates as `Err` — an operational
   failure to even check, per `patala_core::PaymentRail::verify`'s own
   contract — never as an implied "verified".

## Paying a customer back: `validate_destination`

Settlement here is final, so `refund()` is `Error::Unsupported("refund")` and
stays that way. Giving the money back is a **compensating payment** instead: a
second, independent `charge` to an address the *customer* supplies — never the
address the payment came from, which is very often an exchange **withdrawal**
address where the funds cannot be credited back to them.

`destination::validate` is the offline, pure check to run on that address.
**Stellar can decide more offline than most chains**, because StrKey is
`[version byte][payload][CRC16-XMODEM]` in base32 rather than raw bytes in
base58:

* **A bad checksum is unambiguous.** A single mistyped character in a Solana
  address usually produces *another perfectly well-formed address*; here it is
  caught, and reported as "the checksum does not match, re-copy the whole
  address" rather than as "invalid".
* **The version byte encodes the type**, so `G…` (account), `M…` (muxed),
  `C…` (Soroban contract), `T…`/`X…`/`P…` (signer types) and `S…` (**secret
  seed**) are told apart from the string alone.

| Verdict | When |
|---|---|
| `Malformed` | Bad CRC16, wrong length for the type, a character outside base32 (the `0`/`O`, `1`/`I`, `8`/`B`, `9`/`G` look-alikes are named), whitespace-wrapped, empty, or not a StrKey at all. A pasted **secret seed** lands here with its own loud refusal — see below. |
| `WrongNetwork` | A well-formed address for another chain, **named**: a Solana base58 address, an Ethereum/EVM `0x…`, a Sui/Aptos `0x…`, a Bitcoin address in either era. |
| `NotAWallet` | A valid StrKey that is not an account this rail can pay: a **muxed account** (`M…` — this crate builds only plain Ed25519 payment destinations, so it would drop the 64-bit subaccount id and the money would land uncredited in a custodian's omnibus account), a Soroban **contract** (`C…`), a **signer** type (`T…`/`X…`/`P…`), or a checksum-valid `G…` whose payload is not on the ed25519 curve. |
| `StructurallyValid` | Version byte says account, CRC verifies, payload is a real ed25519 public key. **Not** "valid" and **not** "safe" — see below. |

### A seed is a disclosure, not a typo

`S…` gets its own refusal naming what happened and what to do — create a new
account, move everything the old key controls, never use it again — and that
check runs **before** the checksum is consulted, so a *mistyped* seed is still
reported as a leaked key rather than sent back to be re-copied. The verdict
never repeats the value.

### Who decides

`stellar-strkey` — the Stellar Development Foundation's own crate, the same one
`keys` already uses — is the sole authority on whether a string is a valid
StrKey. This crate never accepts an address on its own; when that decoder says
no, it works out *why* so the refusal can be explained. A second checksum
implementation here could disagree with the first, and the one place two
validators must never disagree is the one deciding where money goes. (One
consequence, pinned by a test: that decoder is **case-insensitive**, so a
lowercased address really does resolve to the same account and really is paid —
refusing it would refuse money that would have arrived.)

### What this cannot decide

Whether the account **exists** and is funded above the base reserve, and
whether it holds a **USDC trustline** — without one the payment is rejected
with `op_no_trust`, the most common way a Stellar payment fails. Both need
Horizon, so both are a different method than this one. And **whether the
address belongs to an exchange**: patala does not and will not guess at that.
Every verdict — including `StructurallyValid` — carries
`EXCHANGE_DEPOSIT_CAVEAT` and `human_must_confirm: true`. There is no verdict
this rail can produce that means "safe to send to".

## Honesty (`PATALA.md` §8) — READ THIS — UNVERIFIED AGAINST LIVE

**This crate has not been run against a live Stellar network — testnet or
mainnet — from this environment**, because this environment cannot reach
Horizon. Nothing in this crate's behavior against a real network has been
confirmed.

What **is** checked, offline, in `src/tests.rs` (23 tests, all passing, no
network):

- A scripted fake Horizon (`FakeRpc`) that decodes and re-hashes whatever
  `charge` submits using this crate's own pure functions, so the full
  `charge → submit → fetch → verify` loop is exercised end-to-end, including
  every rejection path: wrong rail/currency, tampered amount/reference,
  wrong configured issuer, tampered signature, self-inconsistent hash,
  malformed proof, wrong network, Horizon never having heard of a hash,
  Horizon reporting a landed-but-unsuccessful transaction, and Horizon
  serving back an envelope that disagrees with the binding.
- `envelope_round_trips_through_the_official_xdr_decoder` — a strong,
  network-independent correctness check: encode a transaction with this
  crate's own builder, then decode it with `stellar-xdr`'s own
  spec-generated decoder, and confirm every field (source, destination,
  amount, sequence, fee, memo, asset, signature) survives byte-for-byte. A
  bug in field order, union discriminants, or length-prefix encoding would
  fail this test even though it never touches a network.
- `kat_fixed_inputs_produce_a_deterministic_hash_and_signature` — a
  known-answer test: a fixed seed, fixed destination/issuer bytes, and fixed
  amount/sequence/fee/memo produce a pinned transaction hash and Ed25519
  signature, asserted byte-for-byte. **This is a self-generated regression
  fixture, not an independently-sourced Stellar test vector** — it was
  produced by running this crate's own code once and hardcoding the output.
  Its value is catching a *future* regression in the encoding, signing-key
  handling, or hashing; it is not, by itself, proof that the encoding matches
  real Stellar consensus rules. The XDR round-trip test above is the
  stronger of the two claims — it flows through the Foundation's own
  decoder, not just through this crate's own logic twice.

An opt-in live smoke test exists and is `#[ignore]`d by default:

```sh
PATALA_STELLAR_LIVE=https://horizon-testnet.stellar.org \
  cargo test -p patala-stellar live_horizon -- --ignored --nocapture
```

It only confirms Horizon connectivity and that a hash that cannot exist is
denied cleanly — it does not submit a real payment (no funded testnet
account is assumed to exist). **Running an actual `charge()` against
testnet Horizon and confirming `verify()` accepts the resulting receipt has
not been done from this environment and is the single most important
remaining step before trusting this rail with real value.** A human with
network access should: fund a testnet account via Friendbot, set
`STELLAR_SECRET_KEY`, build a `StellarRail` over `rpc::HorizonRpc`, call
`charge`, and confirm `verify` returns `Ok(true)` — then repeat once against
mainnet with a small real amount before treating this as production-ready.

**Confidence assessment:** the StrKey encode/decode is delegated entirely to
the Foundation's own crate (high confidence). The XDR *codec* — struct
layout, union discriminants, variable-length fields — is likewise the
Foundation's own generated code (high confidence). What this crate adds on
top — which fields go into the `Transaction`/`Operation`/signing payload,
in what order, with what values, and the SHA-256-then-Ed25519 signing
procedure — matches this author's understanding of the Stellar protocol
documentation, and passes an XDR round-trip through the official decoder,
but **has not been cross-checked against stellar-core, js-stellar-sdk,
py-stellar-base, or a live network.** Treat that layer as
**plausible-but-unverified** until the live test above is run.
