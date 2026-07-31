# patala — a sovereign, centerless payment-rail substrate

> **patala** — Sesotho/Setswana, *"to pay."* Part of the Vulos family (`vula` = "open").
>
> This document is the single source of truth. Every agent and wave builds against the
> seams and decisions here. Do not invent parallel abstractions — implement what is below.

## 0. Vision (one sentence)

**One interface to move value — fiat or crypto — that any product can vendor and self-host,
where the platform holds no funds, takes no cut, and no one owns the network; a "gateway" is a
permissionless role anyone can run, not a company you depend on.**

## 1. What this is, and is NOT

**IS:**
- A thin **library** (Rust core + bindings + optional sidecar) that presents many payment rails
  behind one honest interface.
- **Non-custodial by default.** The substrate holds no funds. Crypto rails settle wallet-to-wallet;
  fiat rails are operated by their processors, and the substrate never becomes a custodian.
- **Centerless.** Self-hostable, no registry you must join, no central processor. A product can
  direct-connect rails itself. Gateway discovery, if used, is a phonebook, not an authority.
- **MIT.** No token. No protocol tax. Consistent with the rest of the Vulos suite.

**IS NOT:**
- Not a business, not a SaaS, not a custodial hub, not a routing service that takes a cut.
- **Not a new payment protocol/wire format.** The wire protocols already exist (SPL, Stellar,
  EIP-681, Solana Pay, x402, Lightning). We do not mint another. Where an intent/request format
  is needed, we ADOPT an existing one.
- Not a reimplementation of 100 fiat processors — those already exist (see §4, Hyperswitch).

## 2. The core insight (why this is thin, not a death-march)

Payment adapters split into two kinds, and we treat them completely differently:

| | Fiat processors (Stripe, Paystack, Xendit…) | Crypto rails (Solana, Stellar…) |
|---|---|---|
| Shape | REST calls + webhook verification | tx construction + signing + chain RPC |
| Trust | custodial, **reversible** (chargebacks), KYC, T+2 | non-custodial, **final**, wallet-based, near-instant |
| Build vs adopt | **ADOPT** — Hyperswitch already ships 100+, Apache-2.0, Rust, self-hostable | **BUILD** — this is the part nobody provides non-custodially |

The whole value we add is **(a) the non-custodial crypto rails, and (b) the thin unifying
capability layer** that presents both classes behind one interface with failover. Everything else
is adopted. Do not rebuild what exists.

## 3. THE SEAM (implement exactly this)

The core crate is `patala-core`. Nothing in a consumer names a provider-specific type — they see
only the trait and the capability descriptor.

```rust
/// The settlement class is IN THE TYPE. A consumer MUST be able to read it,
/// because it changes the UX contract (a refundable "pending" state + a card form,
/// vs a wallet address + a signed final receipt). Never flatten these.
pub enum RailClass {
    /// Custodial, reversible (chargebacks possible), usually KYC'd, delayed settlement.
    /// e.g. any fiat processor behind Hyperswitch.
    CustodialReversible,
    /// Non-custodial, final (no reversal), wallet-to-wallet, near-instant.
    /// e.g. Solana/Stellar USDC.
    NonCustodialFinal,
}

pub struct RailCapabilities {
    pub class: RailClass,
    pub reversible: bool,           // chargebacks / refunds possible at the rail level
    pub requires_kyc: bool,
    pub holds_funds: bool,          // does the RAIL custody? (the substrate never does)
    pub currencies: Vec<String>,    // e.g. ["USDC", "USD", "NGN"]
    pub settlement: Settlement,     // Instant | Seconds(u32) | Days(u8)
    pub atomic_multi_party: bool,   // N payouts as ONE atomic settlement, never N API calls.
                                    // Structurally false for every fiat processor (docs/shared-economics.md §5).
}

#[async_trait]
pub trait PaymentRail {
    fn id(&self) -> &str;                       // stable rail id, e.g. "solana", "hyperswitch"
    fn capabilities(&self) -> &RailCapabilities;
    async fn quote(&self, req: &PayRequest) -> Result<Quote>;      // fees, fx, expiry
    async fn charge(&self, req: &PayRequest) -> Result<Receipt>;   // initiate/settle a payment
    async fn verify(&self, receipt: &Receipt) -> Result<bool>;     // fail-closed
    // Pre-flight, PURE and OFFLINE: what this rail can honestly say about a
    // destination address before any money moves. Never a Result — "I cannot
    // check" is a VERDICT (`Unknown`), because a caller must handle it as
    // carefully as a refusal. Never a bool — five states, each rendered
    // differently by a UI. The default answers `Unknown`, never
    // `StructurallyValid`, so a rail can never accidentally claim a check it
    // does not perform. See §3a.
    fn validate_destination(&self, dest: &str) -> DestinationVerdict { /* Unknown */ }
    // Optional (a rail that can't do it returns Unsupported, does NOT fake it):
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> { Err(Error::Unsupported) }
    // The push path (added after the pull-only shape above shipped): the
    // processor calls the consumer, and only the rail can say whether the
    // delivery is genuine. It belongs on the trait because a free function
    // beside a rail is unreachable through `dyn PaymentRail`, i.e. from every
    // binding and from the sidecar — which leaves those consumers able only to
    // poll `verify`. Same Unsupported-not-faked rule as `refund`.
    async fn verify_webhook(&self, d: &WebhookDelivery) -> Result<WebhookEvent> { Err(Error::Unsupported) }
}
```

- **`FailoverRail`** — wraps `Vec<Box<dyn PaymentRail>>`, tries them in order (or by a policy:
  cheapest, most-reliable, currency-match), falls through on error. **This is how "one rail fails,
  continue on another" works, and it lives in the substrate so every self-hoster gets it — it is
  NOT a gateway's privilege.** It must respect class: do not silently fail a `NonCustodialFinal`
  request over to a `CustodialReversible` rail without the consumer opting in — the guarantees differ.
- **`MockRail`** — the offline default. Deterministic, no network, so CI and the default build need
  no chain and no processor. Every rail beyond mock is feature-gated (`--features solana`, etc.).
- **Errors fail closed.** A receipt that cannot be verified is invalid, never assumed-valid.

## 3a. Paying a customer back on a final rail (the compensating-payment flow)

Full walkthrough, including the wording to show a customer:
**[`docs/compensating-payments.md`](docs/compensating-payments.md)**. The binding
decisions are here.

`refund()` returns `Unsupported` on every `NonCustodialFinal` rail and **that stays true** —
finality is the whole point of the class. Giving the money back there is a **compensating
payment**: a second, independent `charge()` in the opposite direction, with its own transaction,
its own fee, its own confirmation, its own fresh `reference`, and its own ability to fail. The
original receipt is unchanged. Conflating the two would flatten exactly the distinction
`RailClass` exists to preserve — so this flow ends in `charge()`, never in `refund()`.

**NEVER send a refund to the address the payment came from.** BitPay, Coinbase Commerce and
OpenNode all ask the customer for a destination instead, for one concrete reason: a *sending*
address is very often an exchange **withdrawal** address, and an exchange does not credit funds
arriving there to the customer who withdrew from it. The money is unrecoverable — by the customer,
by the merchant, and by patala. Asking the customer is the correct design, not a fallback, and it
makes the flow two-party: the merchant initiates, the **customer** supplies an address they
control, a human confirms, the merchant approves.

**patala does NOT detect exchange addresses, and must not learn to.** That needs commercial
address-attribution data (Chainalysis, TRM) — hosted services, which would break the rule that
nothing here depends on a third party and which the offline default build exists to avoid. A
*heuristic* would be worse than nothing: a host who trusts "looks safe" and loses a customer's
money is worse off than one told plainly that this cannot be known. So `validate_destination`
decides what is decidable and surfaces the rest as a warning a human must confirm:

- Every `DestinationVerdict` carries `human_must_confirm: true` — **including the most positive
  verdict** — and `exchange_deposit_caveat`, verbatim, for a UI to show. There is no verdict that
  waives the human step and no API to skip it.
- The best status is `StructurallyValid`, not `Valid`: it is the *absence of a decidable defect*,
  not a safety claim. There is deliberately no `is_valid()`/`is_safe()` on the type, because there
  is no answer this crate can give that means "safe to send to".
- `Malformed` / `WrongNetwork` / `NotAWallet` are **refusals** — defects the rail knows about.
  Guards fail closed: do not offer a human the option to confirm past one.
- `Unknown` means nothing was established. **Never treat it as valid**, and never as a refusal
  either — "checked and clean" and "could not check" are different answers.

It is a **trait method**, not a free function beside each rail, for the same reason
`verify_webhook` is: a free function is unreachable from every non-Rust consumer, since the UniFFI
surface and the sidecar both dispatch through `dyn PaymentRail`. It is exposed on both
(`PatalaRail.validate_destination`, `POST /v1/rails/:rail_id/validate-destination`), with every
verdict variant and its reason string intact — a verdict that flattened to a bool at a boundary
would defeat the design.

## 4. Rails to ship (in order)

1. **`MockRail`** — offline default (core crate).
2. **Solana rail** (`--features solana`) — **MOVE the existing implementation from
   `magnetite/magnetite-seams/src/solana/`** (it is real, ~1760 lines, 95 tests, has a live-RPC
   ignored test). Adapt it to the `patala-core` trait. `NonCustodialFinal`. Ed25519 — the app's
   identity key doubles as the wallet key, no mapping table.
3. **Stellar rail** (`--features stellar`) — NEW. `NonCustodialFinal`, native USDC (Stellar Asset),
   Ed25519 (StrKey). Cheapest measured fees (~$0.0001), 3–5s finality. Build tx construction +
   signing + Horizon/RPC; gate the live path behind an ignored test like Solana does. If you cannot
   verify against Stellar testnet from here, say so plainly and mark it UNVERIFIED-AGAINST-LIVE.
4. **Hyperswitch fiat adapter** (`--features hyperswitch`) — ADOPT, do not rebuild. A single
   `PaymentRail` impl that talks to a **self-hosted Hyperswitch** instance over its HTTP API,
   presenting the whole fiat processor set (Stripe/Paystack/Xendit/…) as one `CustodialReversible`
   rail. Do not vendor 100 processor SDKs. Hyperswitch is Apache-2.0 Rust, self-hostable — cite it.

**Deferred (design for, don't build yet):** any-stablecoin mint generalization, Algorand rail,
gateway discovery phonebook. Note them; don't implement in wave 1.

## 5. Polyglot (M×1, never M×N)

We use a variety of stacks (Rust, Python, …). Write each adapter ONCE in the Rust core, consume it
three ways:
1. **Rust crate** — direct (magnetite).
2. **Native bindings** — a **Python** binding first (UniFFI preferred for multi-language reach, or
   PyO3 for Python-only ergonomics — pick one, justify). wasm/napi later for JS.
3. **Sidecar** — a thin local gRPC/HTTP server wrapping the core, so any language speaks to it with
   zero FFI. For non-custodial signing this is also a **security win**: keys live in one hardened
   process, not smeared across every app. Design the sidecar; a minimal working HTTP version in
   wave 1 is enough, gRPC can follow.

Do NOT reimplement adapters per language.

## 6. Chain research (grounded, 2026 — use for decisions, state caveats honestly)

- **Ed25519-native (identity key = wallet key, no mapping table): Solana, Stellar, Algorand.**
  EVM chains (Polygon/Arbitrum/Optimism/Base) are secp256k1 — need a mapping table — AND 100×–10,000×
  higher fees ($0.05–$0.50 vs ~$0.0001–$0.0035). Deprioritize EVM.
- **Native Circle USDC** on all in-scope chains (SPL / Stellar Asset / ASA / EVM contracts).
- Fees (independently measured): **Stellar ~$0.0001** (3–5s), **Solana ~$0.0035** (sub-second).
- Decentralization caveats to state, not hide: Stellar had a 2019 academic finding of SDF
  centralization (two SDF nodes could halt consensus) — likely improved since, unmeasured for 2026.
  Solana's validator set fell ~68% (2,500→~800) via a deliberate 2025 Foundation pruning; Nakamoto
  coefficient ~18–19, no validator >~3.2%. Neither disqualifies a payment *use* of the chain.
- **The gap this fills:** mature fiat orchestration exists (Hyperswitch) and crypto standards exist
  (x402/L402/EIP-681) as SEPARATE ecosystems. No decentralized, self-hostable layer unifies
  custodial-reversible fiat AND non-custodial-final crypto behind one honest interface. That union
  is the whole point.

## 7. First consumer

**magnetite** switches from its in-crate `PaymentRail` seam to depending on `patala`. Its Solana
rail moves here; `magnetite-seams` depends on `patala-core` (+`--features solana`) and keeps the
mock default. This must land with all magnetite tests still green, the offline/default build still
needing no chain, and the class boundary preserved. See §7 of magnetite's DECENTRALIZATION.md for
the seam it already has.

## 8. Honesty conventions (binding)

- Every rail beyond mock: unit-tested offline; the live path behind an `#[ignore]`d test gated on an
  env var (as Solana already does). If it was never run against a live network from here, the docs
  and the commit say **UNVERIFIED AGAINST LIVE** — do not imply otherwise.
- Never fabricate a receipt, a balance, or a "success" a rail didn't return.
- The substrate is non-custodial: no code path may make patala hold funds. A fiat rail's custody is
  the *processor's*, surfaced via `holds_funds: true` on that rail's capabilities — never patala's.
- Default build stays offline: no new mandatory deps, no network, CI needs no chain or processor.

## 8b. Consumer guidance — provider credentials

patala itself is stateless and holds no secrets: a rail is constructed from config the *consumer*
supplies each time (a fiat rail's API keys, a crypto rail's signer). But a consumer that *persists*
those provider credentials (a store's Stripe key, a gateway's Paystack secret) is handling live
money-moving material, and should:

- **Encrypt them at rest** (AES-256-GCM or equivalent) under a key that is not itself in the
  database — never store a provider secret in plaintext.
- **Make them write-only**: accept on create/update, never return them in an API response after.
- **Scope access** to admin/management credentials only.

This is not patala's job to enforce (it never sees the store) — it is stated here so a consumer
building on the substrate does not have to learn it the hard way.

## 9. Repo shape

```
patala/
  Cargo.toml                 # workspace
  patala-core/               # trait + capability model + FailoverRail + MockRail + errors + receipt
  patala-solana/  (or core feature)   # moved from magnetite
  patala-stellar/ (or core feature)
  patala-hyperswitch/        # fiat adapter (HTTP client to self-hosted Hyperswitch)
  patala-fiat/                # direct fiat-processor adapters + the ISO-4217 currency table + the offline `manual` rail
  patala-py/                 # Python binding (UniFFI/PyO3)
  patala-go/                  # Go binding, generated via uniffi-bindgen-go from patala-py's UniFFI surface
  patala-sidecar/            # thin local HTTP server over the core
  README.md  LICENSE-MIT  LICENSE-APACHE  PATALA.md
```

(This list reflects §9's original wave-1 plan; `patala-fiat` and `patala-go` landed later and are real, not aspirational — see README.md's "What's built" table for what is actually shipped, tested and live-verified today.)
Feature-gating vs separate crates is an implementation call — keep the DEFAULT build offline and
dep-free either way.
