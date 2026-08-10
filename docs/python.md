# Python binding

`patala-py` is the Python packaging of the same UniFFI surface everything else
is generated from. It reimplements no rail — it does not even *define* the
binding: every exported type lives in `patala-uniffi`, which is also where Go,
Swift and Kotlin come from. See [One core, every language](polyglot.md) for
why that is the rule.

Mechanically it is a compiled cdylib plus a generated `ctypes` wrapper. There
are no native headers to find and no build backend to install; `python3` and
`cargo` are the whole toolchain.

> **The module is `patala`, the library is `libpatala_py`.**
> `from patala import PatalaRail`. UniFFI names the generated module after the
> *namespace*, which `patala-uniffi` declares as `patala`; the native library
> it loads is still this crate's own `libpatala_py.{dylib,so}`. It was
> `patala_py` on both counts until the surface moved out of `patala-py` — a
> Python-flavoured name that every other generated language was inheriting
> too.

## Synchronous on purpose

`PaymentRail`'s methods are `async fn` in Rust. UniFFI *can* export async
functions to Python, driven off `asyncio` — but that would force every caller,
including a one-shot script, to run an event loop just to call `charge()`.

This binding exposes **synchronous** methods instead. Each blocks the calling
Python thread on a single, lazily-created multi-thread `tokio::runtime::Runtime`
owned process-wide by `patala-uniffi`. The Python caller never sees
`async`/`await`:
`rail.charge(req)` returns a `Receipt` or raises `PatalaError`.

That is the opposite trade `patala-sidecar` makes — that crate stays async
because its entire existence *is* an async HTTP server. An async Python
surface could be added alongside the synchronous one later, wrapping the same
`Arc<dyn PaymentRail>`, without redesigning anything.

## Build and run

`make smoke-python` from the workspace root runs exactly these steps and is a
CI job, so they cannot rot:

```bash
# 1. Build the cdylib Python will load. Add --features for the rails you want.
cargo build -p patala-py --features fiat-stripe

# 2. Generate the wrapper from that cdylib's own UniFFI metadata. The bindgen
#    is this workspace's own binary target — no separately installed CLI. It
#    writes `patala.py`, named after the UniFFI namespace.
cargo run -p patala-py --bin uniffi-bindgen -- generate \
    --library target/debug/libpatala_py.dylib \
    --language python \
    --out-dir patala-py/bindings/python
#   (Linux: target/debug/libpatala_py.so)

# 3. The generated module loads its native library by name from its own
#    directory, so put the freshly built one next to it.
cp target/debug/libpatala_py.dylib patala-py/bindings/python/

# 4. Run it.
PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py
```

`patala-py/bindings/` is gitignored — it is build output, reproduced by those
four commands, not checked in.

## A round trip

```python
from patala import PatalaRail, PayRequest, RailClass

rail = PatalaRail.new_mock(
    id="mock",
    _class=RailClass.NON_CUSTODIAL_FINAL,
    currencies=["USDC"],
    fee_minor=0,
    failing=False,
)

req = PayRequest(
    amount_minor=1_250,       # int, never a float
    currency="USDC",
    destination="dest-anything",
    reference="order-1",
)

receipt = rail.charge(req)
assert rail.verify(receipt) is True   # fail-closed: a tampered receipt verifies False
```

`_class` has a leading underscore because `class` is a Python keyword; the
Rust field is `class` and nothing else was renamed.

## What is exposed

- **`PatalaRail`** — the one object type Python ever touches. It wraps
  `Arc<dyn patala_core::PaymentRail>` and exports `id()`, `capabilities()`,
  `quote()`, `charge()`, `verify()`, `verify_webhook()` and
  `validate_destination()`.
- **`RailClass`** (`CUSTODIAL_REVERSIBLE` / `NON_CUSTODIAL_FINAL`) and
  **`Settlement`** (`Instant` / `Seconds` / `Days`) — mirrored 1:1, never
  flattened.
- **`RailCapabilities`, `PayRequest`, `Quote`, `Receipt`** — mirrored records.
  Amounts stay `u64` minor-unit integers across the boundary. Never a float.
- **`PatalaError`** — one variant per `patala_core::Error`: `Unsupported`,
  `Rail`, `InvalidRequest`, `CrossClassFailover`, `AllRailsFailed`. `verify`
  failing closed is still `False`, never an exception.
- **`WebhookDelivery` / `WebhookEvent` / `WebhookStatus`** — the push side.
- **`DestinationStatus` / `DestinationVerdict`** — the pre-flight side.
- **`exchange_deposit_caveat()`** — a module-level function returning the same
  caveat text every verdict carries, for the form where a customer is first
  asked for a payout address, before there is a verdict to render.

## Constructors, and the features behind them

`PatalaRail.new_mock(...)` is always available — no feature flag, and it is
what a bare build gets. Everything else is gated so the default build stays
exactly as offline as `patala-core`'s:

| Cargo feature | Constructor | Class |
|---|---|---|
| *(none)* | `PatalaRail.new_mock(id, _class, currencies, fee_minor, failing)` | either, as asked |
| *(none)* | `PatalaRail.new_mock_without_destination_checks(...)` | a mock that answers `UNKNOWN` for every destination |
| `solana` | `PatalaRail.new_solana(rpc_url, cluster, keypair_seed)` | `NON_CUSTODIAL_FINAL` (SPL-USDC) |
| `stellar` | `PatalaRail.new_stellar(horizon_url, network, usdc_issuer, keypair_seed)` | `NON_CUSTODIAL_FINAL` (native USDC) |
| `hyperswitch` | `PatalaRail.new_hyperswitch(base_url, api_key, connector, webhook_secret, requires_kyc, currencies, settlement_days, timeout_secs)` | `CUSTODIAL_REVERSIBLE` |
| `fiat`, `fiat-<name>`, `fiat-all` | `PatalaRail.new_fiat(provider, config)` | `CUSTODIAL_REVERSIBLE` |

Details that bite:

- **`new_solana`** — `cluster` is `"devnet"` or `"mainnet"`/`"mainnet-beta"`.
  Anything else raises `InvalidRequest`; there is no silent default.
  `keypair_seed` is `None` for a verify-only rail, or exactly 32 raw Ed25519
  seed bytes for one that can also `charge()`. Constructing the rail touches
  no network. Since 0.1.1 the bytes you pass are **zeroised** — both the
  `Vec<u8>` UniFFI allocates for the argument and the fixed-size copy taken from
  it, including when the length is wrong. Python's own `bytes` object is
  immutable and is not yours or patala's to wipe, so a seed you hold in Python
  still lives until it is collected; read it from the environment or a file and
  drop the reference.
- **`new_stellar`** — `network` is `"testnet"`, which **requires**
  `usdc_issuer` because Stellar's testnet USDC issuer rotates and has no fixed
  default, or `"public"`/`"mainnet"`, which ignores `usdc_issuer` and uses the
  Circle mainnet issuer baked into `patala-stellar`.
- **`new_hyperswitch`** — `base_url` and `api_key` are required arguments,
  never a hardcoded endpoint. `connector` optionally pins one
  Hyperswitch-configured processor; `None` lets Hyperswitch's own routing
  decide.

Every constructor raises a typed `PatalaError` on bad input. None of them
panics, and none returns a half-built rail.

Reading the capability model works the same whichever rail is behind the
object: call `rail.capabilities()` and branch on `._class`, without ever
naming the concrete provider.

## The twenty fiat processors, by name

Rather than twenty typed constructors, `patala-fiat`'s adapters are reachable
through one by-name registry constructor:

```python
from patala import PatalaRail

rail = PatalaRail.new_fiat("manual", {})
print(rail.id(), rail.capabilities())

stripe = PatalaRail.new_fiat("stripe", {
    "secret_key": "sk_live_...",
    "webhook_secret": "whsec_...",
})
```

`provider` is matched case-insensitively. An unknown name, or a provider whose
Cargo feature was not compiled into this build's cdylib, both raise
`PatalaError.InvalidRequest` — never a panic, never a silent fallback to a
different rail.

Every `config` value is a **string**, even for numeric and boolean fields:
`"settlement_days": "2"`, `"requires_kyc": "true"`. Keys are the exact field
names of that provider's own `<Provider>Config` struct in `patala-fiat` —
nothing is renamed at this boundary. A missing key for a required field is
rejected by that adapter's own constructor with an `InvalidRequest` naming the
field; a malformed numeric field is rejected rather than silently defaulted.

The full per-provider key table lives in
[Fiat rails](rails-fiat.md#config-keys-by-provider).

`patala_fiat_providers()` lists every provider name *this specific build* can
construct, so a caller can discover what is available instead of hardcoding a
list that may not match the build.

## Webhooks

`verify_webhook(delivery)` is the push counterpart to `verify(receipt)`.
Without it, a consumer on this side of the boundary could only ever poll —
webhook signature verification is provider-specific Rust, and anything not on
the trait is invisible to UniFFI.

```python
import time
from patala import PatalaRail, WebhookDelivery, WebhookStatus, PatalaError

rail = PatalaRail.new_fiat("stripe", {"secret_key": ..., "webhook_secret": ...})

# Forward the processor's request VERBATIM: same bytes, same headers, same
# query string. Every scheme signs exactly what was sent, so a body that has
# been through a JSON round-trip on your side will not verify.
delivery = WebhookDelivery(
    raw_body=request.get_data(),          # bytes, not str, not a parsed dict
    headers=dict(request.headers),        # matched case-insensitively
    query=None,                           # only LNbits reads this (?secret=)
    now_unix=int(time.time()),            # replay windows check against this
)

try:
    event = rail.verify_webhook(delivery)  # raises if not authentic
except PatalaError.InvalidRequest:
    return "", 400
except PatalaError.Unsupported:
    return "", 501                         # this rail has no push delivery

if event.status == WebhookStatus.SETTLED:
    # Reconcile event.amount_minor / event.currency against your own stored
    # order before trusting them, and dedupe on (event.rail_id, event.event_id).
    ...
```

`WebhookStatus` has three values, not two. `UNCONFIRMED` means the delivery is
genuine and carries **no settlement claim** — BTCPay, Coinbase Commerce,
OpenNode, LNbits and Mollie all authenticate a notification that names an
object and nothing else. Look up your stored `Receipt` for `event.object_id`
and call `verify()` on it. Never treat `UNCONFIRMED` as payment.

## Destination pre-flight

`validate_destination(addr)` is **pure and offline** — no network, no clock, no
filesystem — so it is safe to call on every keystroke of an address field. It
returns a verdict and never raises: "I cannot check" is
`DestinationStatus.UNKNOWN`, a verdict, because a caller must handle it as
carefully as a refusal and an exception there could be swallowed by a bare
`except`.

A verdict carries `reason` (never empty, written to be shown to a person),
`human_must_confirm` — **`True` on every verdict, including
`STRUCTURALLY_VALID`** — `exchange_deposit_caveat`, and `is_refusal` as a
field rather than something to re-derive from `status`.

The flow this belongs to, including the wording to put in front of a customer,
is [Paying a customer back](compensating-payments.md).

## Packaging: a real wheel

The shipping story is a maturin-built wheel, not the manual bindgen flow
above. Both use the exact same UniFFI binding — maturin does not replace
UniFFI, it is the wheel-packaging frontend around it.
`pyproject.toml`'s `[tool.maturin] bindings = "uniffi"` tells maturin to build
the cdylib, run this crate's own bindgen against it, and bundle the result
plus the native library into a wheel with proper metadata.

```bash
cd patala-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin

maturin build --release                                    # MockRail only
maturin build --release --features solana,stellar,hyperswitch

pip install target/wheels/patala_py-*.whl
```

`maturin develop --features ...` rebuilds in place for local iteration.

Publishing pre-built wheels means one wheel per (OS × arch × Python ABI),
built in a CI matrix with `maturin-action` or `cibuildwheel`, then
`twine upload dist/*.whl` or `maturin publish`. None of that has been run
against the real PyPI from this repo — there are no credentials here and the
crate is `publish = false`. The commands are exact, not speculative, but
actually publishing is a founder action.

## What has actually been executed

- `cargo test -p patala-uniffi` — Rust unit tests that drive `PatalaRail` directly,
  no Python involved: 11 by default, 20 with `fiat-all`.
- The full build-and-run sequence above, end to end, under a real
  `python3` (3.13) loading the real generated wrapper and the compiled
  library, performing a genuine `MockRail` charge → verify round trip and
  asserting both the valid and the tampered result. It is a CI job.
- **UNVERIFIED AGAINST LIVE** for all 20 fiat adapters. Every fiat test here
  either stays fully offline (`manual`) or only *constructs* a rail without
  calling `charge`/`verify`, which would dial a real processor.

## Related documents

- [Quickstart](quickstart.md) · [Choosing a mode](choosing-a-mode.md)
- [Fiat rails](rails-fiat.md) — the provider list and the config keys.
- [Troubleshooting](troubleshooting.md) — import errors, missing symbols, and
  "that constructor does not exist".
