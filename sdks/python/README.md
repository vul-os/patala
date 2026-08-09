# patala (Python)

**There is no binding in this directory.** `../../patala-py` is the Python
binding — UniFFI-generated from the one `#[uniffi::export]` surface in
`patala-uniffi` — and this directory is runnable examples of it, plus the
sidecar alternative.

That is deliberate. A second, hand-written binding over `patala-ffi`'s C ABI
would be a strictly worse copy: `dict`s instead of records, strings instead of
`RailClass`/`DestinationStatus` enums, one exception type instead of the five
`PatalaError` variants, and a second thing to keep in step with the core every
time a method is added. Read [`../../patala-py/README.md`](../../patala-py/README.md)
for the binding itself — features, the 20 fiat providers, wheels, and the
config-key table.

| mode | what it is | example |
| --- | --- | --- |
| **Direct** | `patala-py` loaded into your interpreter | [`examples/direct_charge.py`](examples/direct_charge.py) |
| **Sidecar** | `patala-sidecar` as a separate process, HTTP over loopback | [`examples/sidecar_charge.py`](examples/sidecar_charge.py) |

Both examples drive `MockRail`: deterministic, offline, no credentials. patala
is a payments library, so an example that moves real value is not an example.

## Which one to pick

**Either. Direct is fine.** That sentence is the whole point of this page,
because the same page in llmux and openrate says the opposite, and the reason
it says the opposite does not exist here.

llmux and openrate are Go, shipped as `go build -buildmode=c-shared`. That puts
the **Go runtime** — GC, scheduler, signal handlers — inside your interpreter,
and it does not survive `fork()`. Their READMEs are right to steer Python users
to the sidecar. patala is Rust. There is no runtime to break, and the
difference is measurable rather than rhetorical:

```
threads in a bare python3 interpreter                          1
threads after dlopen(libllmux.dylib)                           7   (8 after one chat)
threads after dlopen(libpatala_ffi.dylib)                      1
threads after a full patala charge -> verify round trip        1
```

and after `os.fork()`, running a real operation in the child:

| library | in the forked child |
| --- | --- |
| `libllmux` (Go, c-shared) | `models` returns; **`chat` HUNG** — never answered, SIGKILLed by the watchdog |
| `libpatala_ffi` (Rust) | charge, verify, and a fresh handle all returned in 0.00 s |

Same machine, same Python 3.13.9, same probe harness, 2026-08-09. "HUNG" means
the child produced nothing before the watchdog fired and was then `SIGKILL`ed;
the watchdog is **5 s by default** and is set by `PATALA_FORK_TIMEOUT`
([`examples/fork_probe.py`](examples/fork_probe.py) line 44). The distinction
that matters is answered-vs-never-answered, not the wall-clock figure — patala
returned in 0.00 s and llmux never returned at all. Reproduce the patala half
with that probe; the llmux half needs llmux's own `ffi/fakeupstream` and its
prebuilt `dist/ffi/darwin_arm64/libllmux.dylib`.

So the reasons to choose the sidecar here are **not** hazard-avoidance. They
are:

- **Key isolation.** This is the real one. A non-custodial rail's signing key
  lives in whichever process calls `charge`. Link the binding into every Gunicorn
  worker and that key is in all of them; route them through one sidecar and it
  is in one narrowly-scoped process that does nothing else. See
  [`../../patala-sidecar/README.md`](../../patala-sidecar/README.md#threat-model)
  — including what it does *not* defend against.
- **No Rust toolchain on the calling side.** The sidecar needs a binary, not a
  wheel matched to your platform, arch and Python ABI.
- **One process to upgrade** instead of a wheel per service.

## Direct

```python
from patala import PatalaRail, PayRequest, RailClass

rail = PatalaRail.new_mock("mock", RailClass.NON_CUSTODIAL_FINAL, ["USDC"], 0, False)
receipt = rail.charge(PayRequest(amount_minor=1250, currency="USDC",
                                 destination="mock:wallet:alice", reference="order-1"))
assert rail.verify(receipt) is True
```

Build the binding first, from the workspace root:

```sh
make smoke-python                      # builds the cdylib, generates patala.py, runs the smoke test
python3 sdks/python/examples/direct_charge.py
```

or `pip install` a maturin-built wheel — the example tries the normal import
path first, so an installed package always wins over the in-tree build.

Real output, 2026-08-09, python 3.13.9 on darwin/arm64:

```
binding:  /Users/pc/code/vulos/patala/patala-py/bindings/python
python:   3.13.9 on darwin
threads:  1 in this process before the first call

capabilities
  ok  id() == 'mock'
  ok  class is RailClass.NON_CUSTODIAL_FINAL — a wallet address and a final receipt, not a card form
  ok  holds_funds is False — patala never holds funds
  ok  reversible is False — there is no refund on this rail
  ok  currencies == ['USDC', 'USD']

pre-flight: validate_destination, before any money moves
  ok  status is STRUCTURALLY_VALID for a well-formed address
  ok  is_refusal is False — a field, never re-derived from status
  ok  human_must_confirm is True even here — patala does not detect exchange addresses
  ok  an empty destination is a Malformed refusal, returned as a verdict and not raised

quote -> charge -> verify
  ok  total_minor == 1250 and is an int — minor units, never a float
  ok  receipt.amount_minor == 1250
  ok  the genuine receipt verifies True
  ok  a tampered receipt verifies False — fail-closed, and False is data, not an exception

errors are typed, never a crash
  ok  an unsupported currency raised InvalidRequest: message='rail mock does not support currency EUR'

threads:  3 after the whole round trip

ALL 14 PYTHON DIRECT ASSERTIONS PASSED
```

Note the last two numbers: **1 thread before the first call, 3 after.** The
binding blocks on one process-wide multi-thread tokio runtime with two workers,
created lazily (`patala-uniffi/src/lib.rs`, `runtime()`). Construction is inert;
the first `charge` is what starts it. The C ABI never does this at all — each
handle owns a *current-thread* runtime — which is why `fork_probe.py` measures
both.

## Sidecar

```sh
cargo build -p patala-sidecar
python3 sdks/python/examples/sidecar_charge.py
```

The example generates a token, picks a free port, spawns the server, waits for
`/healthz`, runs the round trip with nothing but `urllib`, and terminates it.
Nothing is left running. Real output, same date:

```
binary:   /Users/pc/code/vulos/patala/target/debug/patala-sidecar
listening on http://127.0.0.1:55081 (loopback only — the bind address is not configurable)
python:   3.13.9 on darwin

capabilities
  ok  GET /v1/rails/mock -> 200
  ok  class is 'NonCustodialFinal' — decide the whole UX off this, not off a provider name
  ok  holds_funds is false

pre-flight: validate-destination, before any money moves
  ok  a well-formed address -> 200 'StructurallyValid'
  ok  is_refusal is false — read the body, not just the status code
  ok  human_must_confirm is true even on StructurallyValid
  ok  an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal

quote -> charge -> verify
  ok  total_minor == 1250
  ok  the JSON number decodes to an int — minor units, never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies {'valid': true}
  ok  a tampered receipt is 200 {'valid': false} — fail-closed, and NOT an HTTP error

the error surface, so you can tell these four apart
  ok  an unsupported currency -> 400 'invalid_request'
  ok  an unknown rail_id -> 404 'unknown_rail'
  ok  the mock has no push delivery -> 501 'unsupported', never an invented event
  ok  no Authorization header -> 401 on a READ-ONLY route too

sidecar terminated; nothing left running

ALL 16 PYTHON SIDECAR ASSERTIONS PASSED
```

`PATALA_SIDECAR_TOKEN` is mandatory — the server refuses to start without one —
and the bind address is hardcoded to `127.0.0.1`. Both are deliberate; see the
sidecar's threat model.

**The sidecar's rail registry is mock-only today.** Any `rail_id` other than
`"mock"` is a `404`. That is a gap in the sidecar, not in the examples.

## The one real fork rule

`patala.h` says: *"Handles are not inherited usefully across a fork; open them
in the child."* Forking from a single-threaded parent makes that look like
superstition — the inherited handle works fine. It is not superstition, and
[`examples/fork_probe.py`](examples/fork_probe.py) section 3 shows why: a
handle's runtime sits behind a mutex, and `fork()` copies a *locked* mutex as
locked, with nobody in the child to unlock it. Measured here, with four parent
threads charging on the same handle:

| what the child does | hung |
| --- | --- |
| charge on the **inherited** handle | **4 / 200** |
| open a **fresh** handle, then charge | **0 / 200** |

It is a race against a window a few microseconds wide, so most forks look fine
and a test that forks once is a false green. The fix is one line and costs
nothing: open the handle in the child.

For `multiprocessing`, both `fork` and `spawn` returned `1250` from a `MockRail`
charge. One honest gap: `Runtime::block_on` drives the future on the calling
thread and `MockRail` spawns no tasks, so the two worker threads that are
missing in a forked child are never *needed*. A rail doing real network I/O may
reach for them. **UNMEASURED** — no live rail was reachable from this
environment. The C ABI has no such question to answer, because it starts no
worker threads in the first place.

## Costs that are real

- **A lazily-started tokio runtime** (2 worker threads) inside the UniFFI
  binding, on the first call. Not at import, not at construction.
- **A wheel per platform × arch × Python ABI** if you want `pip install` with
  no Rust toolchain. `../../patala-py/README.md` documents the maturin matrix;
  **no wheel has been published to PyPI** from this environment.
- **`patala-py` was built and exercised on darwin/arm64 only** here. Linux is
  covered by CI; Windows is untried.

## Files

| file | mode | what it shows |
| --- | --- | --- |
| `examples/direct_charge.py` | direct | capabilities, destination pre-flight, quote → charge → verify, tamper detection, typed errors, thread counts |
| `examples/sidecar_charge.py` | sidecar | spawn + healthz + shutdown, the same round trip over HTTP, and all four error codes (400 / 404 / 501 / 401) |
| `examples/fork_probe.py` | both | thread counts, `fork()` across both libraries, `multiprocessing` fork vs spawn, and the inherited-handle race |
