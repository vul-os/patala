# Paying a customer back on a final rail

**How to give a customer their money back when the rail cannot reverse a payment —
and why the address you send it to must come from the customer, never from the
original transaction.**

This document covers the whole flow: what patala validates, what it refuses to
guess at, the exact wording to put in front of a customer, and the call sequence
in Rust, Go, Python and over the sidecar's HTTP surface.

---

## 1. The rule

> **Never send a crypto refund to the address the payment came from.**

BitPay, Coinbase Commerce and OpenNode all ask the customer for a destination
instead. They are not being cautious for its own sake — they are avoiding a
specific, common, unrecoverable failure:

**A sending address is very often an exchange withdrawal address.** When a
customer pays you from Coinbase, Binance or Kraken, the address the funds leave
from belongs to the *exchange*, not to the customer. It is a hot wallet serving
thousands of users. An exchange does not credit funds arriving at a withdrawal
address to whichever customer happened to withdraw from it last — there is no
record associating that inbound transfer with them. The money lands in the
exchange's wallet and stops there.

When that happens:

- **the customer** has no claim on it — the exchange has no reason to think the
  money is theirs;
- **the merchant** has already sent it and cannot pull it back — the rail is
  `NonCustodialFinal`;
- **patala** never held it and can do nothing.

Recovery, where it happens at all, means the customer opening a support ticket
with the exchange, proving the transfer was theirs, and hoping. Usually it is
simply gone.

So asking the customer for a destination is not a fallback for when you cannot
determine the sender. **It is the correct design**, and the flow is
irreducibly two-party.

## 2. A refund on a final rail is not a reversal

`PaymentRail::refund` returns `Error::Unsupported` on every `NonCustodialFinal`
rail, and that is not a gap to be filled later. Finality is the whole point of
that settlement class — it is why `RailClass` exists in the type rather than as
a boolean flag.

Giving the money back on such a rail is a **compensating payment**:

|  | Reversal (`refund`) | Compensating payment (`charge`) |
|---|---|---|
| What it is | The original payment is undone | A *second, independent* payment, the other way |
| Transaction | The original one changes state | Its own transaction |
| Fee | None, or the original's | **Its own fee**, paid again |
| Confirmation | N/A | **Its own confirmation**, on its own timeline |
| Can it fail? | N/A | **Yes**, entirely independently |
| Idempotency key | The original `reference` | **Its own fresh `reference`** |
| Proof it happened | The original receipt | **The payout's receipt**, not the original |
| Available on | `CustodialReversible` rails | Every rail — it is just a `charge` |

Conflating the two would flatten exactly the distinction `RailClass` exists to
preserve. That is why this flow ends in `charge()` and not in `refund()`, and
why `refund()` must keep saying `Unsupported` rather than quietly becoming an
alias for "pay them back somehow".

The original receipt stays valid and verifiable throughout. A compensating
payment does not alter the payment it compensates for; you now have two
receipts, and your books should show two payments.

## 3. What patala can and cannot decide

`PaymentRail::validate_destination(dest) -> DestinationVerdict` is the
pre-flight check. It is **pure and offline** — no network, no clock, no
filesystem, no global state — so it runs in a browser through wasm, on a gate
device with no uplink, and in a unit test with no RPC configured. You can call
it on every keystroke of an address field.

**It can decide** whether a string is a well-formed address for that rail's own
network: alphabet, length, checksum, and — where the network encodes it, as
Solana does through whether the point is on the ed25519 curve — whether it is a
plain wallet or a program/contract account.

**It cannot decide who owns it.** In particular:

> ### patala does not detect exchange addresses, and never will
>
> Determining that an address belongs to an exchange requires commercial
> address-attribution data — Chainalysis, TRM and similar. Those are **hosted
> services**. Depending on one would break the rule that nothing in this
> workspace depends on a third party, and the default build exists precisely to
> avoid that (`ureq` is off by default; every REST shape is tested offline
> against canned bytes).
>
> A **heuristic** would be worse than nothing. A host who trusts "looks safe"
> and loses a customer's money is worse off than a host who was told plainly
> that this cannot be known. There is no partial credit here: a wrong "probably
> fine" is indistinguishable from a right one until the money is gone.
>
> So patala validates what is decidable and surfaces the rest as a warning a
> human must confirm.

This is why the most positive verdict is called `StructurallyValid` and not
`Valid`, and why `DestinationVerdict` deliberately has **no** `is_valid()` or
`is_safe()` method: there is no answer this code can give that means "safe to
send to", so it offers no method that could be mistaken for one.

## 4. The five verdicts, and what to do with each

`DestinationStatus` has five variants. It is not a bool and not a `Result`,
because a UI has to render each one differently.

| Status | Means | `is_refusal` | What your UI does |
|---|---|---|---|
| `Malformed` | Wrong alphabet, wrong length, bad checksum, or empty. The rail positively established a defect. | `true` | Put the cursor back in the field. Show `reason`. **Do not charge.** |
| `WrongNetwork` | Well-formed, but for a different network than this rail pays on — a Stellar `G…` in a Solana payout. | `true` | Say which network it looks like and which you pay on. **Do not charge.** |
| `NotAWallet` | Valid on this network but not a plain wallet — a program/contract account, a Solana PDA (off-curve, unsignable), a token mint. Nobody holds a key for it. | `true` | Explain that funds sent there are unrecoverable. **Do not charge.** |
| `StructurallyValid` | Every offline check passed. **Not "valid". Not "safe".** The *absence of a decidable defect*. | `false` | Proceed to the human confirmation step below. |
| `Unknown` | This rail cannot check this destination at all, and says so instead of guessing. The honest answer for a fiat rail (whose destination is an opaque processor-side token) and for any rail without a parser. | `false` | Proceed to the human confirmation step below — with *no* structural reassurance to offer. **Never treat as valid.** |

Two rules that are easy to get wrong:

- **A refusal is a refusal.** Do not offer a human the option to confirm past
  `Malformed`, `WrongNetwork` or `NotAWallet`. Unlike the residual exchange
  risk, these are things the rail *knows*. Guards fail closed.
- **`is_refusal == false` is not a green light.** It is also `false` for
  `Unknown`, where nothing at all was established. Branch on `status`, not on
  the negation of `is_refusal`.

Do not re-derive `is_refusal` from `status` in your own language. It crosses
every boundary as data — a field on the FFI record, a field in the JSON —
precisely because a `switch` that has not heard of a status added later falls
through to its default, and the default anyone writes is "not a refusal". That
fails **open**, on the one question that decides whether money goes to an
address the rail already knew was wrong.

## 5. The human confirmation step is unconditional

Every `DestinationVerdict` carries `human_must_confirm: true` — **including
`StructurallyValid`**. There is no verdict that waives it, and there is no API
to skip it. That absence is the design.

Every verdict also carries `exchange_deposit_caveat`, the same string every
time, so a UI can show it verbatim without composing its own wording:

> patala cannot tell whether this address belongs to an exchange. A
> structurally perfect address may still be an exchange deposit or withdrawal
> address, and an exchange will not credit funds arriving there to the person
> who gave you the address. Determining that needs commercial
> address-attribution data patala deliberately does not depend on, so only the
> person who owns the wallet can confirm they control this address and can
> receive on it.

That is the text for an operator or developer. Below is the text for the
customer.

## 6. Wording to show a customer

Adapt the tone, keep the content. Every one of these sentences is load-bearing.

### 6.1 Asking for the address

> **Where should we send your refund?**
>
> Please give us a wallet address you control directly.
>
> **Do not give us the address you paid from** if you paid from an exchange
> (Coinbase, Binance, Kraken, or similar). That address belongs to the
> exchange, not to you — funds we send there will not be credited to your
> account, and neither we nor the exchange will be able to recover them.
>
> If you paid from an exchange, use your personal wallet address instead, or
> your exchange's **deposit** address for this asset — the one the exchange
> gives you to receive funds, not the one your payment came from.
>
> This refund is sent on **{network}**. An address for any other network will
> not work and the funds would be lost.

### 6.2 On a refusal (`Malformed` / `WrongNetwork` / `NotAWallet`)

> **We can't use that address.**
>
> {verdict.reason}
>
> Please check it and try again. We have not sent anything.

Show `reason` — it is never empty, and it is written to be shown. Do not offer
a "send anyway" button.

### 6.3 The confirmation, before you pay out

> **Please confirm this is right.**
>
> We will send **{amount} {currency}** on **{network}** to:
>
> `{address}`
>
> This is a new payment, not a reversal of your original one. Once it is sent
> it **cannot be undone or recalled by anyone**, including us.
>
> We have checked that this address is correctly formed for {network}. We
> **cannot** check who it belongs to. If it is an exchange address that is not
> your own deposit address, the exchange will not credit the funds to you and
> they cannot be recovered.
>
> ☐ **I confirm I control this wallet and can receive {currency} on
> {network} at this address.**
>
> [ Send refund ]

The checkbox is not decoration. It is the step that `human_must_confirm`
exists to require, and it is the only thing standing between a
`StructurallyValid` verdict and an unrecoverable transfer.

For an `Unknown` verdict, replace the "We have checked…" sentence with:

> We **cannot** check this address at all — not its format, and not who it
> belongs to. Please check it character by character against your wallet.

## 7. The flow, end to end

```
   merchant                          customer                        patala
      │                                  │                             │
 1.   │  "we owe you a refund"  ────────▶│                             │
      │                                  │                             │
 2.   │◀──── an address they control ────│                             │
      │      (NEVER the sender address)  │                             │
      │                                                                │
 3.   │  validate_destination(addr) ──────────────────────────────────▶│  pure,
      │◀───────────────── DestinationVerdict ──────────────────────────│  offline
      │                                                                │
 4.   │  is_refusal? ── yes ──▶ show reason, back to step 2            │
      │       │                                                        │
      │       no                                                       │
      │       ▼                                                        │
 5.   │  show reason + exchange_deposit_caveat to a HUMAN,             │
      │  who ticks "I control this wallet"          ← not automatable  │
      │       │                                                        │
      │       ▼                                                        │
 6.   │  charge(PayRequest {                                           │
      │      destination: addr,           ← the validated one          │
      │      reference:   <FRESH key>,    ← its own idempotency key    │
      │      amount_minor, currency,                                   │
      │  })  ─────────────────────────────────────────────────────────▶│
      │◀──────────────────── Receipt (the PAYOUT's) ───────────────────│
      │                                                                │
 7.   │  store BOTH receipts. verify(payout_receipt) is the proof      │
      │  the customer was paid. The original receipt is unchanged.     │
```

Step 5 is a person. It is the only step in patala that cannot be automated, and
that is a deliberate property of the design rather than a missing feature.

## 8. Calling it

The same sequence in each consumer surface. All of these are offline —
`validate_destination` never dials anything.

### Rust

```rust
use patala_core::{DestinationStatus, PayRequest, PaymentRail};

let verdict = rail.validate_destination(customer_supplied);

if verdict.is_refusal() {
    return Err(ShowToCustomer(verdict.reason));   // step 4: stop here
}

// step 5 — a person reads both of these and ticks the box
show(&verdict.reason);
show(&verdict.exchange_deposit_caveat);
assert!(verdict.human_must_confirm);              // always true
if !human_ticked_the_box { return Ok(NotSent); }

// step 6 — a compensating payment, with its OWN reference
let payout = rail.charge(&PayRequest {
    amount_minor: original.amount_minor,
    currency:     original.currency.clone(),
    destination:  customer_supplied.to_string(),
    reference:    format!("{}-payout", original.reference),
}).await?;
```

### Go

```go
v := rail.ValidateDestination(customerSupplied)

if v.IsRefusal {
    return showToCustomer(v.Reason)   // step 4: stop here
}

// step 5 — a person reads both of these and ticks the box
show(v.Reason)
show(v.ExchangeDepositCaveat)        // == patala.ExchangeDepositCaveat()
// v.HumanMustConfirm is true on every verdict, including StructurallyValid
if !humanTickedTheBox {
    return nil
}

// step 6
payout, err := rail.Charge(patala.PayRequest{
    AmountMinor: original.AmountMinor,
    Currency:    original.Currency,
    Destination: customerSupplied,
    Reference:   original.Reference + "-payout",  // its own idempotency key
})
```

Branch on `v.Status` (`patala.DestinationStatusUnknown` and friends) when you
need to distinguish "checked and clean" from "could not check".

### Python

```python
verdict = rail.validate_destination(customer_supplied)

if verdict.is_refusal:
    return show_to_customer(verdict.reason)      # step 4: stop here

# step 5
show(verdict.reason)
show(verdict.exchange_deposit_caveat)            # == exchange_deposit_caveat()
assert verdict.human_must_confirm                # always True
if not human_ticked_the_box:
    return

# step 6
payout = rail.charge(PayRequest(
    amount_minor=original.amount_minor,
    currency=original.currency,
    destination=customer_supplied,
    reference=f"{original.reference}-payout",
))
```

`DestinationStatus.STRUCTURALLY_VALID`, `.MALFORMED`, `.WRONG_NETWORK`,
`.NOT_A_WALLET`, `.UNKNOWN`.

### Over the sidecar (any language, no FFI)

```http
POST /v1/rails/solana/validate-destination
Authorization: Bearer $PATALA_SIDECAR_TOKEN
Content-Type: application/json

{"destination": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"}
```

```json
200 OK
{
  "rail_id": "solana",
  "status": "StructurallyValid",
  "reason": "...one sentence, never empty, safe to show a person...",
  "human_must_confirm": true,
  "exchange_deposit_caveat": "patala cannot tell whether this address belongs to an exchange. ...",
  "is_refusal": false
}
```

**Read the body, not just the status code.** A `200` means *the rail answered*,
not that the address is good — all five verdicts come back as `200`, exactly as
`POST /verify` returns `200` with `{"valid": false}`. Mapping some verdicts onto
HTTP error codes would flatten a five-state answer into "worked / did not work".

| Code | Meaning |
|---|---|
| `200` | A verdict. Branch on `status` and `is_refusal`. |
| `400` | The **request** was malformed — not JSON, missing/non-string `destination`, or an unexpected field. No verdict is invented for a request whose meaning is unclear. |
| `401` | Missing or wrong bearer token. |
| `404` | No rail registered under that `rail_id`. |

Note the distinction the `400` does not swallow: `{"destination": ""}` is a
perfectly well-formed *request*, so it returns `200` — carrying the rail's
`Malformed` refusal, because an empty string is undeliverable on every rail
there is.

Then `POST /v1/rails/:rail_id/charge` with the validated destination and a
fresh `reference`.

## 9. Which rails check what — and which do not need this flow at all

The per-rail table is in
[`site/docs/status.md`](../site/docs/status.md#destination-validation-by-rail).
The short version splits on settlement class, and the split is the whole reason
this document exists:

- **Crypto rails** (`patala-solana`, `patala-stellar` — `NonCustodialFinal`)
  parse a real address and can reach `StructurallyValid`. `refund` is
  `Unsupported`, so **this document's flow is the only way to pay a customer
  back**.
- **Fiat rails** (`patala-fiat`'s 20 adapters, `patala-hyperswitch` —
  `CustodialReversible`) have a `destination` that is a redirect URL, a buyer's
  email, an opaque `payment_token`, or a string the rail never reads. None is a
  place money goes, so their honest ceiling is `Unknown` and they never return
  `StructurallyValid`. **They do not use this flow at all**: the processor can
  reverse the original payment, so giving a customer their money back there is
  `refund` (or the processor's own dashboard) — the money goes back the way it
  came and no destination is involved.

Those rails still validate their own field, so a wallet address or a private key
pasted into a Stripe `success_url` is refused by name rather than becoming a
charge the processor rejects.

A rail that has not implemented a parser inherits the trait's default: `Unknown`
for anything non-empty, `Malformed` for a blank string. That default is
deliberately not `StructurallyValid` — a permissive default would silently bless
every parser-less rail.

## 10. What this does not do

Stated plainly so nothing here is read as more than it is:

- **No exchange detection.** See §3. Not now, not later, not heuristically.
- **No account-existence check.** Whether the account exists, is rent-exempt,
  or has a trustline for the asset are all *chain queries*. They are a
  different method, not this one, and this one's purity contract is what makes
  it usable in a browser and on an offline device.
- **No ownership proof.** patala cannot verify the customer controls the
  address. Only the customer can assert that, which is what step 5 is.
- **No protection against a customer who lies or is mistaken.** If they tick
  the box for an address they do not control, the money is gone. The box exists
  so that a person has looked, not so that a machine has approved.

## Related

- `patala-core/src/destination.rs` — the type, the caveat constant, and the
  reasoning in rustdoc form.
- `patala-core/src/rail.rs` — `validate_destination`'s contract and
  `refund`'s "`Unsupported` does not mean the customer cannot be paid back".
- [`PATALA.md`](../PATALA.md) §3 — the seam.
- [`site/docs/status.md`](../site/docs/status.md) — what is tested, and what is
  unverified against a live network.
