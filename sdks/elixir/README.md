# patala (Elixir)

Two ways to reach patala from Elixir, **no dependencies** either way — `JSON`
is Elixir's own (1.18+) and HTTP is `:gen_tcp`, not even `:inets`.

| mode | what it is | module | default |
| --- | --- | --- | --- |
| **Sidecar** | `patala-sidecar` as a separate OS process, JSON over loopback | [`Patala.Sidecar`](lib/patala/sidecar.ex) | **yes** |
| **Direct** | a dirty-IO NIF over the C ABI | [`Patala.Direct`](lib/patala/direct.ex) | works, with eyes open |

Both drive `MockRail`: deterministic, offline, no credentials. patala is a
payments library, so an example that moves real value is not an example.

```sh
cargo build -p patala-ffi -p patala-sidecar
cd sdks/elixir
mix run examples/direct_charge.exs
mix run examples/sidecar_charge.exs
```

## The direct path exists here, unlike in llmux and openrate

llmux's and openrate's Elixir SDKs ship **no** in-process binding at all, and
one of their stated reasons is that a Go runtime inside a BEAM scheduler is a
poor fit. That reason does not apply to patala: patala is Rust and brings no
runtime, no GC, no signal handlers and no threads into your process. So the NIF
was built rather than assumed away, and it works — 22 assertions, offline, and
41,827 charges/second through it from 40 concurrent processes while a plain
BEAM process kept scheduling 4 million iterations alongside.

**And the recommendation is still the sidecar.** The remaining reasons are
about the BEAM, not about patala, and they are enough on their own:

- **A NIF cannot be killed.** `Task.await/2`'s timeout returns to your caller;
  the dirty scheduler thread keeps going. `Process.exit(pid, :kill)` does not
  reach into native code.
- **A NIF cannot be supervised.** There is no process to restart, so the one
  recovery mechanism the whole platform is built around does not apply.
- **A fault in native code takes the VM**, not one process. patala catches Rust
  panics at its own boundary and turns them into error strings, which removes
  *patala's* bugs from that list — but not the binding's, and not a mismatched
  `libpatala_ffi`'s.
- **Dirty schedulers are a fixed pool.** `:erlang.system_info(:dirty_io_schedulers)`
  printed **10** on this machine (`+SDio`, default). Ten concurrent in-flight
  rail calls saturate it and the eleventh queues behind them — invisible to any
  BEAM-level backpressure you have built.

`examples/sidecar_charge.exs` demonstrates the other side of that rather than
asserting it:

```
what the process boundary buys, measured
  ok  a charge inside a Task returns normally
  ok  Task.shutdown/:brutal_kill really stops it — there is a PROCESS to kill, which is the whole difference
  ok  a failure comes back as data to a supervised process, not as a VM-wide event
  ok  the rail runs in OS pid 90756, this VM is 90572 — a segfault there is not a segfault here
```

The sidecar also buys **key isolation**: a non-custodial rail's signing key
lives in whichever process calls `charge`, so one sidecar means one process
holds it. See
[`../../patala-sidecar/README.md`](../../patala-sidecar/README.md#threat-model),
including what it does not defend against.

**Take `Patala.Direct` when** you are writing a CLI, a migration, a test
helper, or a service where you would rather not supervise a second process and
can accept the four points above. It is a real option, not a trap — just not
the default.

## Sidecar

```elixir
# Production shape: point at one somebody else runs.
sc = Patala.Sidecar.connect("http://127.0.0.1:8420", System.fetch_env!("PATALA_SIDECAR_TOKEN"))

{:ok, verdict} = Patala.Sidecar.validate_destination(sc, "mock", address)
if verdict["is_refusal"], do: raise(verdict["reason"])

{:ok, receipt} = Patala.Sidecar.charge(sc, "mock", %{
  amount_minor: 1250, currency: "USDC",
  destination: address, reference: "order-1"
})
{:ok, %{"valid" => true}} = Patala.Sidecar.verify(sc, "mock", receipt)
```

`Patala.Sidecar.spawn/1` starts one for you — fresh 32-byte token, free port,
waits for `/healthz` — and is what the example uses. `try/5` returns
`{:ok, status, body}` where the typed helpers would return `{:error, {status,
kind, message}}`; use it where the status *is* what you want to look at.

Real output, 2026-08-10, Elixir 1.20.3 / OTP 29 on darwin/arm64:

```
elixir 1.20.3 / OTP 29
binary:  /Users/pc/code/vulos/patala/target/debug/patala-sidecar
listening on http://127.0.0.1:64500 (loopback only — the bind address is not configurable)
os pid:  90756   <- a real OS process, which is the entire point

capabilities
  ok  class is "NonCustodialFinal" — decide the whole UX off this, not off a provider name
  ok  holds_funds is false

pre-flight: validate-destination, before any money moves
  ok  a well-formed address -> 200 "StructurallyValid"
  ok  is_refusal is false — read the body, not just the code
  ok  human_must_confirm is true even on StructurallyValid
  ok  an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal

quote -> charge -> verify
  ok  total_minor == 1250 and decodes as an integer — never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies {"valid": true}
  ok  a tampered receipt is 200 {"valid": false} — fail-closed, and NOT an HTTP error

the error surface, so you can tell these four apart
  ok  an unsupported currency -> 400 "invalid_request"
  ok  an unknown rail_id -> 404 "unknown_rail"
  ok  the mock has no push delivery -> 501 "unsupported", never an invented event
  ok  no Authorization header -> 401 on a READ-ONLY route too

sidecar terminated; nothing left running

ALL 18 ELIXIR SIDECAR ASSERTIONS PASSED
```

### `Port.close/1` does not kill the child — found the hard way

Worth knowing before you copy the spawn code anywhere. Closing a port shuts the
pipes; it does not signal the OS process, and `patala-sidecar` keeps serving
happily with its stdin at EOF — correctly, since it is a network server and not
a filter. The symptom is a script that prints its last line and then **never
exits**, because an orphaned child is still holding the inherited stdout open,
and the sidecar is still bound to its port after the VM that started it is
gone. `Patala.Sidecar.stop/1` therefore SIGTERMs the OS pid, waits for the
`{:exit_status, _}` the port delivers, and only then closes the port.

**The sidecar's rail registry is mock-only today** — any other `rail_id` is a
`404`. That is a gap in the sidecar, not in this package.

## Direct (the NIF)

```elixir
Patala.Direct.with_rail(fn rail ->                 # closes it however the fun exits
  {:ok, receipt} = Patala.Direct.charge(rail, %{
    amount_minor: 1250, currency: "USDC",
    destination: "mock:wallet:alice", reference: "order-1"
  })
  {:ok, %{"valid" => true}} = Patala.Direct.verify(rail, receipt)
end)
```

`mix compile` builds `priv/patala_nif.so` through the Makefile — no
`elixir_make` dependency, because a dep to shell out to `make` is a poor trade
for one compiler invocation. **A missing `make` or C compiler is a warning, not
a build failure**: `Patala.Sidecar` needs neither, and failing the whole build
for the optional half would be the wrong call.

The NIF does not link `libpatala_ffi`. It `dlopen`s it at load time from a path
resolved in Elixir (`PATALA_LIBRARY`, then `priv/`, then
`target/{debug,release}/`, then the bare soname), which keeps path resolution
somewhere readable and lets you override it without rebuilding anything.

Three decisions in [`c_src/patala_nif.c`](c_src/patala_nif.c) worth naming:

- **A rail is an `ErlNifResource`, not a bare integer.** Its destructor calls
  `patala_close`, so a rail that goes out of scope is released at GC instead of
  leaking a handle for the life of the VM. Call `close/1` anyway — GC is not a
  schedule.
- **`new` and `call` are `ERL_NIF_DIRTY_JOB_IO_BOUND`.** `MockRail` answers in
  microseconds, but a real rail does network I/O, and a NIF that occupies a
  normal scheduler for hundreds of milliseconds degrades every process sharing
  it. That is measured below.
- **`unload` deliberately does not `dlclose`.** Rails may still be reachable,
  and unmapping a library out from under live resources is how a clean shutdown
  becomes a segfault.

Real output, same date:

```
elixir 1.20.3 / 29 (erts 17.0.5)
library: /Users/pc/code/vulos/patala/target/debug/libpatala_ffi.dylib
patala:  0.1.0
schedulers: 8 normal, 10 dirty-IO

the version probe, because a stale library earlier on the load path is silent
  ok  abi_check! against the loaded version passes
  ok  abi_check("9.9.9") returns {:error, message} naming both versions

capabilities
  ok  id() == "mock"
  ok  class is "NonCustodialFinal" — a wallet address and a final receipt, not a card form
  ok  holds_funds is false — patala never holds funds
  ok  reversible is false — there is no refund on this rail

pre-flight: validate_destination, before any money moves
  ok  a well-formed address gives status "StructurallyValid"
  ok  is_refusal is false — a field, never re-derived
  ok  human_must_confirm is true even here — patala does not detect exchange addresses
  ok  an empty destination is a Malformed refusal — a verdict in {:ok, _}, never {:error, _}
  ok  caveat/1 is the sentence for the address form: patala cannot tell whether this address belo…

quote -> charge -> verify
  ok  total_minor == 1250 and is an integer — minor units, never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies true
  ok  a tampered receipt is {:ok, %{"valid" => false}} — fail-closed, and false is DATA

errors are {:error, message}, never a crash in the VM
  ok  an unsupported currency: "patala: invalid request: rail mock does not support currency EUR"
  ok  an unknown method is caught before the NIF call
  ok  the mock has no push delivery and refuses rather than inventing an event

the boundary holds: bad input is an error, not a segfault
  ok  use-after-close says so — handles are registry keys, never pointers, and never reused
  ok  closing twice is a no-op, so cleanup paths can be idempotent
  ok  a misspelled config field is REFUSED, not defaulted to a currency list you did not choose

concurrency, measured — the dirty-IO pool is a fixed resource
    8000 charges from 40 concurrent processes in 191.3 ms (41827/s)
    a plain BEAM process kept scheduling throughout: 4052000 iterations
  ok  normal schedulers stayed free — that is what ERL_NIF_DIRTY_JOB_IO_BOUND buys

ALL 22 ELIXIR DIRECT ASSERTIONS PASSED
```

Two numbers to read together: 40 concurrent processes against **10** dirty-IO
schedulers still managed 41k charges/second, because a MockRail charge is
microseconds and the queue drains as fast as it fills. Substitute a rail that
waits 200 ms on a processor and the same 40 processes are a 4-deep queue on
every scheduler thread, with nothing at the BEAM level showing it. That is the
shape of the problem, not the throughput.

## What the direct path does NOT cost you

Stated because the equivalent pages for llmux and openrate carry the opposite
list, and none of it was copied:

- **No language runtime** in the VM — no second GC, no second scheduler.
- **No signal handlers installed**, so the BEAM's own handling is untouched.
- **No threads started** by the library, at load or at any other time. Each
  handle owns a *current-thread* tokio runtime that runs on whichever thread
  called in — which, for a dirty NIF, is a dirty scheduler thread.
- **Nothing happens at load**: no socket, no file, no background task.
- The mock-only library is **844,656 bytes** (release), against llmux's ~13 MB.

## Platforms

Built and exercised here: **darwin/arm64** — `libpatala_ffi.dylib` from
`cargo build -p patala-ffi`, and `priv/patala_nif.so` from this directory's
Makefile. Nothing else was produced: no Linux `.so` was built here, and Windows
is untried (the Makefile has no MSVC path — a NIF there needs a different link
line entirely). The sidecar path needs only the `patala-sidecar` binary for
your platform and has no such matrix.

## Files

| file | mode | what it is |
| --- | --- | --- |
| `lib/patala.ex` | — | the choice between the two, argued |
| `lib/patala/sidecar.ex` | sidecar | spawn + healthz + a `stop/1` that actually stops it, and the HTTP API |
| `lib/patala/http.ex` | sidecar | HTTP/1.1 over `:gen_tcp`, so there is no dependency |
| `lib/patala/direct.ex` | direct | the friendly surface over the NIF |
| `lib/patala/native.ex` | direct | the five raw NIF functions and library resolution |
| `c_src/patala_nif.c` | direct | the dirty-IO NIF: resource-backed rails, `dlopen` at load |
| `examples/direct_charge.exs` | direct | the round trip, the error surface, and the concurrency measurement |
| `examples/sidecar_charge.exs` | sidecar | the round trip, all four error codes, and what the process boundary buys |
