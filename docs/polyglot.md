# One core, every language — M×1, never M×N

This is the architectural idea the repo is built around, stated in
`PATALA.md` §5 as a rule rather than an aspiration:

> Write each adapter ONCE in the Rust core, consume it three ways.
> **Do NOT reimplement adapters per language.**

Everything about the shape of this repository follows from it.

## The problem it avoids

A payment adapter is not a thin wrapper. A single processor adapter is
request construction, a response parser, a webhook signature scheme with its
own header names and replay window, a status vocabulary that has to be mapped
onto something honest, an idempotency story, and a set of failure modes that
each have to be decided rather than guessed at.

Now multiply. **M** rails × **N** languages, written per language, is M×N
adapters — and the cost is not the writing, it is that each of the M×N is a
place the *semantics* can diverge. Stripe's `checkout.session.completed`
mapped to "settled" in the Python adapter and to "pending" in the Go one is
not a formatting inconsistency; it is one language marking orders paid that
the other does not.

That failure mode is not hypothetical in this suite. The dominant defect
class found in a sibling product was exactly this: a mock and a core
implementation of the same behaviour, each internally consistent, each
separately tested, and disagreeing with each other. Thirteen of them.

## The rule

**M×1.** Every adapter is written once, in Rust, in `patala-core` or a rail
crate. Every other language is a *generated consumer* of that one definition:

```text
                        ┌──────────────────────────────┐
                        │  patala-core                 │
                        │  trait PaymentRail           │
                        │  RailCapabilities, Receipt   │
                        └──────────────┬───────────────┘
                                       │  implemented once, in Rust
        ┌──────────────┬───────────────┼───────────────┬──────────────┐
        │              │               │               │              │
   patala-solana  patala-stellar  patala-hyperswitch  patala-fiat   MockRail
        │              │               │               │              │
        └──────────────┴───────────────┴───────────────┴──────────────┘
                                       │  one #[uniffi::export] surface
                    ┌──────────────────┼──────────────────┐
                    │                  │                  │
              generated            generated          patala-sidecar
              Python               Go                 (HTTP + JSON)
                    │                  │                  │
                    └──────────────────┴──────────────────┴──► Swift, Kotlin,
                                                               Ruby, Elixir, …
```

The Go binding is the clearest proof that this is real rather than a slogan.
`patala-go` contains **no Rust crate of its own** and reimplements nothing —
not `PatalaRail`, not `RailClass`, not `RailCapabilities`, not `Quote`, not
`Receipt`. It points `uniffi-bindgen-go` at the *same compiled cdylib* the
Python binding loads, reads the UniFFI metadata embedded in it, and emits Go.
Adding Swift or Kotlin is the same command with a different `--language`, not
a new binding crate and not a new adapter.

## Why UniFFI and not PyO3

PyO3 would give slightly nicer Python: real Python classes, direct C-API
calls, no `ctypes` indirection. It is also Python-only. A second language
would mean a second binding crate to write and keep in step — exactly the
M×N this design exists to avoid.

UniFFI generates bindings for every target language from one
`#[uniffi::export]` surface. Given that the suite wants more than Python —
wasm/napi for JS is called out explicitly, and Swift/Kotlin come nearly free
once a UniFFI surface exists — UniFFI is the pick. If patala ever turns out to
need only Python, revisiting PyO3 for the ergonomics is a legitimate future
call; that is not the situation today.

## What this forces onto the trait

The rule has a sharp consequence, and it has already changed the design
twice: **anything that is not on the `PaymentRail` trait is invisible to
every non-Rust consumer.** UniFFI can only see the exported surface, and the
sidecar dispatches through `dyn PaymentRail`. A free function sitting beside a
rail is, from Python, Go, Swift, Kotlin and HTTP, as good as absent.

So three things live on the trait that could plausibly have been free
functions, and each is there for this exact reason:

- **`verify_webhook`** — webhook signature verification is provider-specific
  Rust. Beside the rail, every non-Rust consumer could confirm a payment only
  by *polling* `verify`. On the trait, the push path exists everywhere.
- **`validate_destination`** — the offline pre-flight check on a payout
  address. Beside the rail, a Python payout form could not tell someone "that
  is not a valid Solana address" as they type it.
- **`refund`** — with a default of `Unsupported`, so a rail that cannot do it
  says so rather than faking one.

The same rule is what keeps the reverse from happening: `ManualRail`'s
`mark_paid`/`mark_failed` are deliberately *inherent* methods, not trait
methods, because "an operator confirms a bank transfer" is not something every
rail has. The honest consequence is that they are reachable from Rust and from
nowhere else, and that is documented rather than papered over.

## What this forces onto the type system

A generated binding is only as honest as the types it is generated from, so
nothing may be flattened on the way out:

- **`RailClass` is an enum, not a bool.** `CustodialReversible` and
  `NonCustodialFinal` change what you owe the payer — a refundable pending
  state with a card form, or a wallet address and a final receipt.
- **`WebhookStatus` has three values, not two.** Several real schemes
  authenticate a notification without asserting anything about money.
  `Unconfirmed` says exactly that; `NotSettled` would be a lie.
- **`DestinationStatus` has five, not a bool.** A UI renders each one
  differently, and "I could not check" is a different answer from "I checked
  and it is clean".
- **`is_refusal` crosses the boundary as a field**, even though it is a method
  in Rust, because a method does not survive JSON — and a consumer
  re-deriving it from `status` with a `switch` that has not heard of a status
  added later would fall through to "not a refusal". That default fails open.

## The failure mode this design still has, and how it is gated

Generation removes divergence between *implementations*. It does not remove
divergence between an implementation and what a binding's constants *mean*.

UniFFI lowers enum variants to their ordinal position. `WebhookStatus` crosses
the FFI as a bare integer (`Settled` = 1, `NotSettled` = 2, `Unconfirmed` = 3),
and nothing at runtime names the Rust variant a value came from. Reorder
`patala_core::WebhookStatus`, regenerate, and every call site in Go still
compiles — and means something different. `Unconfirmed` arriving with
`Settled`'s number would mark unpaid orders paid in a consumer that exists.

The pinning is three-layered, because no single layer catches everything:

| Layer | Catches | Blind to |
|---|---|---|
| Constant assertions in `patala-go/bindingtest` | renumbering | an added or removed variant |
| A scan of the generated source for the variant set | added/removed variants | wrong values at runtime |
| Live round-trips against genuinely signed deliveries | a rail mapping its own outcome to the wrong variant | nothing the other two see |

There is a fourth guard one level up: `scripts/check-features.sh` keeps
`patala-fiat`'s processor set in lock-step with the Cargo features that expose
it, because a new adapter left out of `patala-uniffi`'s `fiat-all` feature would
silently vanish from the Go binding's cdylib — present in Rust, absent
everywhere else, and nothing would have failed.

## What the polyglot layer costs

Honesty requires the other column too. M×1 is not free:

- **The generated surface is the lowest common denominator.** Rust-only
  ergonomics — inherent methods, generics, borrowed returns — do not cross.
- **Every binding needs a compiled artifact for its platform.** One cdylib per
  (OS × arch), which for Python means a wheel matrix.
- **cgo, for Go specifically.** See [Choosing a mode](choosing-a-mode.md).
- **Regeneration is a step somebody has to run.** A new constructor in Rust is
  not reachable from Go until the bindings are regenerated from a cdylib built
  with the right features.

The alternative — hand-writing each adapter per language — costs all of that
*plus* the divergence. The trade is not close.

## Related documents

- [Choosing a mode](choosing-a-mode.md) — which consumer to use.
- [The rail interface](rails-interface.md) — the one definition everything
  above is generated from.
- [The offline default build](offline-by-default.md) — the other structural
  property this workspace protects deliberately.
