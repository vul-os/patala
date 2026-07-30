# Shared economics across the Vulos distributed platforms

**Status: decision record, not spec, and mostly not built.** Recorded 2026-07-30 so it
is not lost. Where this document and any repo's code disagree, the code is true.
Nothing described here has ever settled a real payment on any rail.

**Narrow update, same day (B7):** `patala-stellar`'s underlying single-leg payment
primitive — `StellarRail::charge`/`verify`, called directly, not through any product —
has since settled once on Stellar **testnet** (2026-07-30; tx hash and ledger in
`patala-stellar/README.md`). That is evidence the rail primitive itself moves and
verifies real money on a real network. It is **not** evidence for anything else on
this page: no recurring schedule, atomic split, escrow, or citation split described
below has been exercised, by that test or by anything else, and mainnet remains
untouched. Every claim past this paragraph stands exactly as before.

## 1. Who this is for

Six Vulos products are **distributed platforms** — multi-operator, no central
authority, all on the KOTVA substrate:

| Product | Domain | Economic need |
|---|---|---|
| **envoir** | mail / messaging (DMTAP reference node) | pay a gateway operator — **recurring** |
| **soko** | commerce (TRACT reference implementation) | one-off orders, escrow, disputes |
| **vuna** | crawl → index → search / retail radar | peer fan-out settlement, citation splits, **recurring** |
| **molao** | decentralised case-law commons | fund the commons — **recurring** |
| **magnetite** | games | one-off purchase, hosting fees (**recurring**), voluntary legs |
| **evermesh** | media | tips (one-off), creator support (**recurring**), gated access |

Not in scope: the self-hosted single-operator apps (beepbite, flowstock, slipscan,
diwan, kerf, and the rest) — they need a rail, not a shared distributed economics.
`beepbite` already binds patala and is the reference for *how* to bind it.

**Five of six need recurring payment.** That is what settles whether it is worth
sharing.

## 2. Share the primitive, not the product

The governing rule, learned the expensive way. This family already carries **three
canonical-CBOR implementations, three content-hash conventions, and two disagreeing
root-hash implementations**, and every one of those duplications produced a defect or
a near-defect. Payments are the worst possible place to repeat that: highest stakes,
most expensive to get right.

But sharing too much is the opposite failure. A "Patreon system" implies tiers, member
lists, feeds, dunning and a UI — that is **product surface**, and evermesh's creator
support and magnetite's server subscription share no screens. Build that centrally and
five products will each bend it.

| Share | Do not share |
|---|---|
| **Rails** — movement + verification (`patala`) | **Policy** — gate or not, atomic or not, escrow or not |
| **Money type** — `amount_minor` + `currency`, unit always explicit | **Receipt meaning** — claim vs proof |
| **Verification** — per rail, once, not per product | **Product surface** — subscription UI, tiers |
| **The recurring schedule primitive** (§3) | |
| **The declaration format** (§4) | |

Policy genuinely differs, and forcing it into one engine fits none of them:

| | evermesh | magnetite | soko / TRACT | vuna |
|---|---|---|---|---|
| Payment gates access | no | **yes, fail-closed** | yes | AI synthesis only |
| Atomic N-way split | not needed | **required** | per order | required (citations) |
| Funds held | never | never | **escrow** | never |
| Receipt is | a claim | a proof | order state | a usage attestation |

**The unit-explicitness rule is not stylistic.** `magnetite` currently carries a live
bug where the backend produces *cents* and the Solana rail consumes *micro-USDC* — a
10,000× error that has never mispriced anything only because nothing has ever settled.
`patala_core::PayRequest` already carries `amount_minor` **plus** `currency`, which
makes that class of bug structurally impossible. Every consumer should adopt that
vocabulary rather than a bare integer.

## 3. The recurring primitive

Recurring crypto payment is usually described as requiring payment channels, streaming
contracts, or a standing allowance — all of which mean either a smart contract or a
custody-shaped authorisation. **There is a third option that needs neither, and it
works on Stellar today.**

**A subscription is N pre-signed, time-bounded transactions.**

- Stellar time bounds are optional on both ends. Set `minTime` per period, and a
  transaction **cannot be submitted early**.
- The payer signs N periods and hands them over. The recipient — or anyone, since a
  signed transaction is public — submits one per period. The recipient can never take
  more than was signed, nor accelerate the schedule.
- Non-custodial, no contract, no standing mandate.
- **Cancellable**: the payer bumps their sequence number past the pre-signed set, or
  moves the funds.
- Use a **dedicated source account** so ordinary wallet activity does not break the
  sequence chain, and Protocol 19's `minSeqNum` / `minSeqAge` / `minSeqLedgerGap`
  preconditions to relax sequence coupling.

**The same protocol property serves two requirements**: unbounded *upper* time bounds
give offline signing for disconnected deployments; `minTime` *lower* bounds give
subscriptions. One mechanism, one code path.

Honest limits: no balance guarantee (a pre-signed transaction fails if funds are short
— same failure mode as a declined card, needing the same retry/grace/lapse handling);
fixed amounts only (usage-based billing means pre-signing a ceiling and refunding, or
re-signing); finite horizon (sign 12–24 periods, then renew).

**Where it belongs:** the mechanism in **patala**, because it is fundamentally how to
move value repeatedly. The semantics — what a subscription *means*, receipt shape,
lapse behaviour — in **kotva**, where `PAY` is already one of the seven primitives.
Not invented inside a product: SOVEREIGNTY.md's rule is that adopters pin the
substrate, never a path inside a product.

## 4. The declaration format already exists — adopt it

`evermesh/spec/010-economics.md` §1 specifies a rail-neutral registry:

```
PaymentPointer = [ type: uint, value: text ]
```

Ordered by the publisher's preference, with *"new rails are new registry entries"* and
*"clients render the rails they understand and MUST ignore unknown types."*

**Magnetite and vuna should adopt this rather than mint parallel formats.** Two
incompatible ways to say "pay me here" is the same disease as three CBOR codecs, and
this is the moment to avoid it rather than discover it later.

The division of labour: **evermesh's registry declares; patala executes and verifies.**
Complementary, not competing. Products that only declare and display (evermesh today)
need the registry alone and no patala dependency. Products that **gate** on payment
(magnetite, soko, vuna's synthesis tier) need both.

Also from `010` §2, and this is prior art the other products should take rather than
re-derive: *"a receipt proves the payer said they paid; settlement proof lives in the
rail… SHOULD label unverified receipts as claims."* Magnetite independently arrived at
the same two-tier model — **settled** (chain-verified) versus **signed-but-unsettled**
(locally verified, pending) — while designing for disconnected operation, and arrived
later. Evermesh got there first and made it normative.

## 5. The seam boundary, and one decision that matters now

`patala_core`'s seam is **single-recipient**: one `PayRequest` is one recipient. So
atomic N-way splits live *below* it, per chain. That produces two capability tiers:

- **Tier A — every patala rail, including all 20 fiat processors.** Single-recipient
  payment plus verification. "Pay the developer" works on Stripe, Paystack, Yoco,
  PayFast, Solana, Stellar.
- **Tier B — atomic multi-party split.** Per-chain work beneath the seam. Exists only
  where someone writes it — **written for Stellar (B1, 2026-07-30):**
  `StellarRail::charge_split`/`verify_split`, tested offline only, never run live.
  Not written for Solana or any fiat rail.

This is a property of the rail, not a gap in patala: N payouts through a fiat
processor are N independent API calls and **cannot** be made atomic. Therefore add
`atomic_multi_party: bool` to `RailCapabilities` alongside `RailClass` and
`Settlement`, so a consumer declares its requirement and a rail that cannot meet it is
**refused** rather than silently degrading.

**Done (B3, 2026-07-30):** the field, `RailCapabilities::require_atomic_multi_party`,
and `false` declared explicitly on every rail in the workspace — `patala-core`'s own
test suite greps `patala-fiat`/`patala-hyperswitch` so a future rail cannot silently
claim `true` without a real atomic operation behind it. Declared `false` on every
rail, **including `patala-stellar` after B1 landed it an atomic operation the same
day** — deliberately: `charge_split`/`verify_split` are concrete-`StellarRail`
methods, not on `PaymentRail`, so `capabilities()` (read through the generic trait) has
no way to report them truthfully as `true` without misleading a caller who only has a
`Box<dyn PaymentRail>` and no way to reach them. `patala-solana` has neither the field
value nor the operation. The capability field is a declaration a future *generic*
atomic-charge path (if `patala_core` ever grows one) would read; nothing yet calls
`require_atomic_multi_party` at runtime.

> ### DECISION — the Stellar multi-operation work lands in `patala-stellar`
>
> **Correction, 2026-07-30 (checked against code while landing B7/B3):** the
> `tx.rs:169`/`tx.rs:237` citation below is stale. `tx.rs` already has N-leg builder
> (`build_payment_transaction`, `PaymentLeg`) and decoder (`decode_payments`)
> primitives, tested offline for up to the protocol's 100-operation limit. What was
> genuinely still true, at the moment this correction was first written: the trait
> methods every consumer calls, `StellarRail::charge`/`verify`, still built and
> accepted exactly one `Payment` operation each; the N-leg primitives were not
> reachable through the rail.
>
> **Update, same day: closed (B1).** `StellarRail::charge_split`/`verify_split` now
> expose exactly that — atomic, N-leg, built on the same primitives, tested offline.
> Deliberately not added to the `PaymentRail` trait itself (single-recipient by
> design); a consumer needing an atomic split holds a concrete `StellarRail`. See
> `patala-stellar/README.md`'s own section on this.
>
> `patala-stellar` currently builds **one** `Payment` operation and its verification
> **rejects** multiples (`tx.rs:169`, `tx.rs:237`) — see the correction above; this was
> true when this record was written and is no longer the full picture. Magnetite
> needs N operations for its atomic split; vuna will need it for citation splits;
> soko will need it for multi-seller orders.
>
> **Put it in `patala-stellar`, not in a consumer-side adapter.** Same work, same
> effort, and it is the difference between one implementation and three. This is the
> single highest-leverage deduplication available today, and it is why this record
> exists now rather than after the fact.
>
> Note `patala-stellar` already implements `Memo::Hash` item binding and has 29 offline
> tests, a `StellarRpc` trait and real XDR construction — the multi-operation change is
> an extension to working, tested code, not new work.

## 6. Rail choice, and how much it matters

Magnetite selected **Stellar** (record in `magnetite/ALIGNMENT.md`), after a
sixteen-candidate sweep against primary sources (`magnetite/docs/chain-candidates.md`).

**Stellar is not uniquely qualified** — Radix, Sui and Solana also clear every hard
filter. It was chosen on cost-to-ship, unbounded offline validity, lightest nodes, and
being the only candidate where validating requires no bonded stake. Two criteria from
that sweep bear on any future rail added here:

- **Minimum payment granularity.** Cardano cannot create an output below ~0.97 ADA — a
  floor on payment *size*, not rent — which makes sub-cent legs impossible. That kills
  voluntary contributions and vuna's citation splits outright. Stellar (1 stroop), Sui
  (1 MIST) and Radix (1 atto) have no such floor. **Check this for any rail added.**
- **Batch integrity.** On any chain with atomic multi-recipient payments, one
  unprepared or hostile recipient can veto the whole batch — Stellar via a missing
  trustline, Radix via deposit rules. Radix has a free native remedy
  (`AccountLocker::airdrop`); Stellar's `CREATE_CLAIMABLE_BALANCE` locks 0.5 XLM per
  leg; Sui is structurally immune.

Because the money vocabulary is rail-agnostic, **switching rails is a contained
change**. That is the point of §2's rule, and it is why the chain decision should not
be treated as load-bearing for the whole family.

## 7. Sequencing — this is the destination, not the next step

Do **not** build the shared layer first. As of this record, magnetite has seven fixed
but unmerged bugs across five worktrees, two crates absent from `main`, roughly a dozen
corrected-but-unmerged false documentation claims, a normative KOTVA ruling it
violates, and **no payment ever settled**. Vuna cannot crawl-to-query at all.

Generalising a design that has never moved money is designing against an unvalidated
assumption — and every assumption checked during this investigation moved, including
two chain facts that were simply false.

1. Integrate magnetite's outstanding work; collapse its duplicate CBOR and root-hash
   implementations. — **not done by this record's update.**
2. Extend `patala-stellar` to multi-operation, per §5. — **done, 2026-07-30 (B1).**
   `StellarRail::charge_split`/`verify_split` now expose the N-leg primitives
   `tx.rs` already carried, atomically, tested offline. `PaymentRail::charge`/`verify`
   (the trait every other consumer calls) are unchanged and still single-operation —
   see §5's "deliberately not on `PaymentRail`" note for why that is by design, not
   a shortfall.
3. **Settle one real payment on Stellar testnet.** This is the gate on every economic
   claim in every product. — **done, 2026-07-30 (B7).** One single-leg payment,
   through `StellarRail::charge`/`verify`, on testnet only. See `patala-stellar/README.md`.
   The split path from step 2 above was NOT part of this settlement and remains
   untested live.
4. Then lift what proved out: the recurring primitive, the pointer registry,
   `atomic_multi_party`. — **`atomic_multi_party` done (B3, 2026-07-30)**, the
   capability field only — deliberately still `false` on `patala-stellar` even after
   step 2 landed an atomic operation, because that operation sits outside the generic
   trait the field is read through (see §5). The recurring primitive and the pointer
   registry remain not built.
