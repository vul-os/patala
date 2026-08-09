# Crypto rails — Solana and Stellar

Two `NonCustodialFinal` rails: SPL-USDC on Solana, and native USDC on
Stellar. This is the half of patala that nobody else ships non-custodially as
a library — fiat orchestration already exists and is
[adopted rather than rebuilt](rails-fiat.md).

Both live outside the workspace's `default-members`, so a plain `cargo build`
never pulls in their network and crypto dependencies. Reach them by name:

```bash
cargo build -p patala-solana
cargo test  -p patala-stellar
```

See [The offline default build](offline-by-default.md) for why that is
structural rather than a habit.

## What "non-custodial, final" commits you to

- **The substrate never holds the money.** Value moves wallet to wallet. There
  is no balance table, no payout queue, no ledger in this repo.
- **`refund()` returns `Unsupported`, permanently.** Finality is the entire
  point of the class, and faking a reversal would flatten the distinction
  `RailClass` exists to preserve. Paying a customer back is a *second*
  payment — see [Paying a customer back](compensating-payments.md).
- **The identity key is the wallet key.** Both chains are Ed25519-native, so
  there is no identity → wallet mapping table to keep and no second key to
  lose. The public key bytes *are* the address.
- **The network fee is not in the currency you are moving.** `quote()`'s
  `fee_minor` is `0` on both rails, and that is honest rather than convenient:
  the real fee is paid in SOL or XLM by whoever signs, not deducted from the
  USDC being transferred, and `Quote` has no field for a fee denominated in a
  different currency. It is a real cost of using these rails, stated rather
  than hidden.

## Solana (`patala-solana`)

SPL-USDC, ported from an in-house implementation of roughly 1,760 lines with
95 tests — real Ed25519 signing, SPL transaction construction and JSON-RPC —
and adapted to the shared trait.

**Money.** USDC has 6 decimals on Solana; every amount is an integer count of
micro-USDC.

**What ported unchanged:** base58 pubkeys, associated-token-account derivation
via SHA-256 plus the curve25519 on-curve check, `TransferChecked` and Memo
instruction encoding, legacy transaction message serialization; the RPC seam
that makes verification unit-testable offline against a fake, plus the real
JSON-RPC client; and the Ed25519 keypair, trimmed down to
sign/verify/pubkey/env-loading (`SOLANA_KEYPAIR_PATH`, `SOLANA_KEYPAIR`).

**What did not port**, and why it is worth knowing: the original seam had a
multi-way `PaymentSplit`, an "unbound" checkout, and payment-channel and
escrow stubs. `patala_core`'s seam has one destination and one reference, so
`charge` builds one memo plus one `TransferChecked` leg. The channel and
escrow methods were `Unsupported` stubs there too — no on-chain program backed
them — so nothing real was lost.

One deliberate deletion: the old rail signed every receipt with a fixed rail
keypair as a "self-consistency marker, NOT the security boundary", by its own
comment. That is dropped. `Receipt::proof` is already the designated place for
a rail's binding data, and the actual security boundary — the buyer's real
Ed25519 transaction signature, the memo binding, the exact token-balance
deltas — is unchanged.

### Destination verdicts, Solana

| Verdict | When |
|---|---|
| `Malformed` | Not base58 — the offending character is **named**, including the `0`/`O` and `I`/`l` look-alikes base58 omits — or base58 that does not decode to exactly 32 bytes, or surrounded by whitespace, or empty. |
| `WrongNetwork` | A well-formed address for another chain, **named**: a Stellar `G…`/`C…`/`M…`, an EVM `0x…`, a Sui/Aptos `0x…`, a Bitcoin address in either era. "This looks like a Stellar address" is the message that saves the money; "invalid" is not. |
| `NotAWallet` | A real 32-byte account nobody can be paid at: the System Program, SPL Token / Token-2022, the ATA program, Memo, Compute Budget, Stake, Vote, the Rent/Clock sysvars, the incinerator, the USDC and wSOL **mints** — plus anything **off the ed25519 curve**, which is a program-derived address. |
| `StructurallyValid` | Every offline check passed. Not "valid", not "safe". |

The off-curve rule earns its place: `charge` derives the recipient's token
account *from their wallet address*, so passing an associated token account
would build a transfer against the token account of a token account. Every
canonical ATA is off-curve, so that mistake is caught before any money moves.

A pasted `S…` seed is reported as a **private-key disclosure**, not as an
invalid address: the verdict says a key was leaked and what to do about it,
and deliberately never repeats the value — a verdict is shown to a person and
very likely logged on the way there.

What it cannot decide, and does not attempt: whether the account exists, is
rent-exempt, or already holds a USDC token account (all chain queries);
whether an *on-curve* key is a plain wallet or a token account created from a
keypair (indistinguishable without reading the owner program); and whether the
address belongs to an exchange, which patala does not guess at and will not
learn to.

### Live status — UNVERIFIED AGAINST LIVE

Every offline test runs with no network against a scripted fake RPC. The one
test that touches a live cluster is `#[ignore]`d and env-gated:

```sh
solana-test-validator -r &
PATALA_SOLANA_LIVE_RPC=http://127.0.0.1:8899 \
  cargo test -p patala-solana live_rpc -- --ignored --nocapture
```

**This crate has not been run against a live Solana RPC from this repo.** The
step to change that is the command above.

## Stellar (`patala-stellar`)

Native USDC, built new — there is no precursor to port. Real Ed25519 signing,
hand-assembled XDR transaction construction on top of the Stellar Development
Foundation's own codec crates, and Horizon REST submission and verification.

**Money.** USDC on Stellar has **7 decimals**: classic Stellar XDR amounts are
always that fixed-point `int64` scale, with no per-asset decimals field the
way an SPL mint carries one. Every amount is an integer count of
ten-millionths.

**Dependency choice.** `stellar-xdr` and `stellar-strkey`, both published and
maintained by the SDF, both Apache-2.0, both generated from the same `.x`
interface definitions `stellar-core` uses. Hand-rolling the struct, union and
variable-length encoding for a `Transaction`, its `PAYMENT` operation, `Memo`,
`Preconditions` and the signing payload is exactly the class of bug — field
order, union discriminant, length prefixes — that an official spec-generated
codec exists to remove. These are *codec* crates, not a high-level SDK: this
crate still builds the payment operation, the signing payload, the SHA-256
hash, the signature and the signed envelope itself. Nothing about *when* to
submit, *how* to poll, or *what counts as settled* is delegated.

**No commitment knob.** Unlike Solana's probabilistic
`confirmed`/`finalized` levels, `verify` treats a transaction Horizon reports
as successful inside a closed ledger as final. Stellar's federated Byzantine
agreement has no intermediate "confirmed but could still be skipped" state the
way a longest-chain protocol does — a ledger closing *is* the finality event,
in roughly 3–5 seconds. There is deliberately no confirmation-depth parameter.

### What `verify` checks, in order

1. The receipt names this rail and this currency.
2. `Receipt::proof` parses as this crate's binding at all.
3. The claimed asset — code and issuer — matches this rail's **configured**
   issuer, never the receipt's own say-so.
4. `Receipt::amount_minor` and the proof's own claimed amount agree, catching
   either being tampered independently.
5. A domain-separated memo-hash binding re-derives from
   `(rail id, source, destination, reference)`, so a receipt cannot be
   re-pointed at a different reference or destination by editing one field.
6. **Offline, no network:** the whole transaction is rebuilt from the
   binding's own scalar fields, hashed with the claimed network's passphrase,
   and the claimed Ed25519 signature is checked against the claimed source
   over that hash. Only the actual secret key could have produced it — a real
   cryptographic guarantee, checkable with no RPC at all.
7. **Online, the real trust anchor:** Horizon is asked for this transaction
   hash. Not found, deny. Found, `successful` must be true — and the envelope
   XDR Horizon actually returns is decoded and compared operation for
   operation against the binding, never trusting Horizon's summary fields
   alone for the money-moving details.
8. Any Horizon failure at step 7 propagates as `Err` — an operational failure
   to even check — never as an implied "verified".

### Destination verdicts, Stellar

StrKey decode: version byte, base32 alphabet, length, CRC-16 checksum; then
the key *type* — a `G…` account versus an `M…` muxed address, a `C…` contract,
and the other StrKey kinds. Same five-verdict vocabulary as Solana, same
refusal of a pasted secret seed as a disclosure rather than a typo.

### Beyond the trait: splits and recurring

Two capabilities live on the concrete `StellarRail`, deliberately not on
`PaymentRail`:

**Atomic multi-party splits** — `charge_split` / `verify_split`. One
transaction, 1–100 `PAYMENT` operations, and one `Memo::Hash` binding every
leg's destination, amount **and order**, so a receipt cannot have a leg
re-pointed, re-priced, re-ordered, added or dropped without the binding — and
therefore the signature — ceasing to match. `receipt.amount_minor` is the sum
of the legs, and the split binding is a distinct shape that a plain
`charge`/`verify` receipt can never be mistaken for, in either direction.

They are not on the trait because the trait is single-recipient by design, and
a consumer that needs an atomic split holds a concrete `StellarRail`. This is
also why `capabilities().atomic_multi_party` stays **`false`** even though the
method pair exists: that field is read through the *generic* interface, where
these methods are not reachable at all. Declaring `true` there would be
exactly the "capability claimed but unreachable through the interface that
reports it" bug this codebase has been bitten by before.

**Recurring** — `recurring::RecurringPlan`: N pre-signed, time-bounded
transactions on one dedicated source account. Non-custodial (the payer's own
account, no platform-held funds), no contract, cancellable. It depends on
`PreconditionsV2` carrying `min_seq_num`, `min_seq_age` and
`min_seq_ledger_gap` — checked against the pinned crate's generated source and
pinned as a running test rather than taken on trust from documentation.

### Live status — testnet, twice, both on 2026-07-30

This is the only rail in the repo with any live result at all. Read it exactly
this narrowly.

**A single-leg payment settled.** A throwaway keypair paid another throwaway
keypair a USDC-shaped payment on **testnet**, built and submitted through the
real `StellarRail::charge`, and independently re-confirmed by
`StellarRail::verify` reading it back from Horizon.

- transaction hash `32663937fe1407f9de3e781effa6ac9f4b1d29340ea63e72f6335a6c91effb89`
- ledger sequence `3882739`
- Horizon: `"successful": true`, `"operation_count": 1`

**A 3-instalment recurring schedule ran.** Its first instalment settled
immediately; its second was genuinely **rejected by real Horizon**
(`tx_bad_minseq_age_or_gap`) when resubmitted too early, then accepted once
the pacing floor elapsed; its third, still outstanding, was permanently
invalidated by a real on-chain cancellation (`tx_bad_seq` on resubmission).

Reproduce the first — no secret is committed, every keypair is generated fresh
at runtime:

```sh
PATALA_LIVE_TESTNET=1 cargo test -p patala-stellar live_testnet_round_trip \
  -- --ignored --nocapture
```

A separate, always-on, network-free test asserts that the live test still
exists and is still gated, so deleting or weakening it cannot pass silently.

**What those results do not say:**

- ❌ **Mainnet is untouched and unproven.** A structurally different,
  real-money network.
- ❌ **Not Circle's own USDC.** The settled payment used a throwaway,
  self-issued asset with the right wire shape — a 4-byte code `"USDC"` as a
  `CreditAlphanum4` — because testnet has no durable, free, officially issued
  Circle USDC reachable without a trustline already in place. Real Circle
  USDC, on either network, is unexercised.
- ❌ **Atomic splits are unproven live.** They were added the same day, *after*
  that payment settled. Implemented and unit-tested, not live-verified.
- ❌ It is not a claim that the rail is production-ready.

## Which rail to pick

| | Solana | Stellar |
|---|---|---|
| Asset | SPL USDC, 6 decimals | native USDC (Stellar Asset), 7 decimals |
| Keys | Ed25519, base58 | Ed25519, StrKey (`G…`/`S…`) |
| Fee (independently measured) | ~$0.0035 | **~$0.0001** |
| Settlement | sub-second | 3–5s (ledger close) |
| Finality model | commitment levels | ledger close *is* finality |
| Live-verified from this repo | no | testnet, twice |
| Atomic splits | no | yes, off-trait |
| Recurring | no | yes, off-trait |

Decentralisation caveats worth stating rather than hiding: Stellar had a 2019
academic finding of SDF centralisation — two SDF nodes could halt consensus —
likely improved since, and unmeasured for 2026. Solana's validator set fell
roughly 68% (2,500 to about 800) through a deliberate 2025 Foundation pruning;
its Nakamoto coefficient is around 18–19 with no validator above roughly 3.2%.
Neither disqualifies a payment *use* of the chain.

EVM chains are deprioritised on purpose: they are secp256k1, so they need a
mapping table, and their fees are 100× to 10,000× higher ($0.05–$0.50 against
~$0.0001–$0.0035).

## Related documents

- [The rail interface](rails-interface.md) — the trait both rails implement.
- [Paying a customer back](compensating-payments.md) — what to do when
  `refund` says `Unsupported`.
- [Splits and shared economics](shared-economics.md) — where atomic
  multi-party settlement fits.
- [Status](status.md) — the whole verification picture in one table.
