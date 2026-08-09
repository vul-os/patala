# patala (Ruby)

Two ways to reach patala from Ruby, both supported, **no gem dependencies**
either way.

| mode | what it is | file |
| --- | --- | --- |
| **Direct** | `libpatala_ffi` loaded into your process with `fiddle` — no child process, no port | [`lib/patala/ffi.rb`](lib/patala/ffi.rb) |
| **Sidecar** | `patala-sidecar` as a separate process, JSON over loopback | [`lib/patala.rb`](lib/patala.rb) |

Both drive `MockRail`: deterministic, offline, no credentials. patala is a
payments library, so an example that moves real value is not an example.

```sh
cargo build -p patala-ffi -p patala-sidecar
ruby sdks/ruby/examples/direct_charge.rb
ruby sdks/ruby/examples/sidecar_charge.rb
ruby sdks/ruby/examples/fork_probe.rb
```

## Which one to pick

**Either.** If you have read llmux's or openrate's Ruby page, this is the point
where they tell you the answer depends on whether your process forks, and send
Unicorn, clustered Puma, Passenger, Resque and Spring to the sidecar. That
advice is correct for them and **wrong here**, and the difference is not an
opinion:

llmux and openrate are Go, shipped as `go build -buildmode=c-shared`, so the Go
runtime — GC, scheduler, signal handlers — lands in your Ruby process and does
not survive `fork()`. patala is Rust. Measured on this machine:

```
threads in a bare ruby process                          2
threads after Fiddle.dlopen(libpatala_ffi.dylib)        2
threads after patala_new                                2
threads after a full charge -> verify round trip        2
```

Nothing was started, so there is nothing to be left half-alive by a fork. The
same probe against `libllmux.dylib` from a comparable host goes 1 → 7 threads on
`dlopen` and the forked child **hangs** on the first real call.

So pick on the merits instead:

- **Direct** when you want one fewer process: no port, no supervision, no
  loopback surface, and an 844,656-byte library (mock-only, release build) that
  installs no signal handlers and starts no threads.
- **Sidecar** for **key isolation** — the argument that actually matters. A
  non-custodial rail's signing key lives in whichever process calls `charge`.
  Link the library into every Unicorn worker and the key is in all of them;
  route them through one sidecar and it is in one narrowly-scoped process that
  does nothing else. See
  [`../../patala-sidecar/README.md`](../../patala-sidecar/README.md#threat-model),
  including what it does not defend against.
- **Sidecar** also when you would rather ship one binary than a
  platform-matched shared library (see [Platforms](#platforms)).

## Direct

```ruby
require "patala/ffi"

Patala::Ffi.open do |rail|                    # closes the handle however the block exits
  verdict = rail.validate_destination("mock:wallet:alice")
  raise "refused" if verdict["is_refusal"]    # a field — never re-derived from status

  receipt = rail.charge(amount_minor: 1250, currency: "USDC",
                        destination: "mock:wallet:alice", reference: "order-1")
  rail.verify(receipt).fetch("valid")         # => true
end
```

Requests and responses are the **same JSON the sidecar serves**, so moving a
call site between the two modes is a transport change, not a rewrite.

Library resolution is `PATALA_LIBRARY`, then `sdks/ruby/lib/`, then
`target/{debug,release}/` in a checkout, then the bare soname. Build one with
`cargo build -p patala-ffi`.

Real output, 2026-08-09, ruby 4.0.5 (arm64-darwin24), fiddle 1.1.8:

```
ruby 4.0.5 (arm64-darwin24), fiddle 1.1.8
threads before dlopen: 2
library: /Users/pc/code/vulos/patala/target/debug/libpatala_ffi.dylib
patala:  0.1.0
threads after dlopen + patala_new: 2

the version probe, because a stale library earlier on the load path is silent
  ok  abi_check! against the loaded version passes
  ok  abi_check!('9.9.9') raises and names both versions

capabilities
  ok  id == "mock"
  ok  class is "NonCustodialFinal" — a wallet address and a final receipt, not a card form
  ok  holds_funds is false — patala never holds funds
  ok  reversible is false — there is no refund on this rail

pre-flight: validate-destination, before any money moves
  ok  a well-formed address gives status "StructurallyValid"
  ok  is_refusal is false — a field, never re-derived from status
  ok  human_must_confirm is true even here — patala does not detect exchange addresses
  ok  an empty destination is a Malformed refusal, returned as a verdict and never raised
  ok  caveat returns the sentence to show a human on the address form: patala cannot tell whether this address belongs …

quote -> charge -> verify
  ok  total_minor == 1250 and is an Integer — minor units, never a Float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies true
  ok  a tampered receipt verifies false — fail-closed, and false is DATA, not an exception

errors come back as errors, never as a crash in your process
  ok  an unsupported currency: patala_call(charge): patala: invalid request: rail mock does not support currency EUR
  ok  an unknown method is caught before the FFI call

webhooks: a rail with no push delivery says so
  ok  the mock refuses rather than inventing an event: verify_webhook is not supported by this rail

a closed or invented handle is a clean error, never a segfault
  ok  use-after-close says so: this Patala::Ffi handle is closed
  ok  closing twice is a no-op, so cleanup paths can be idempotent

threads after the whole round trip: 2   <- unchanged

ALL 20 RUBY DIRECT ASSERTIONS PASSED
```

### `fiddle`, not the `ffi` gem

`fiddle` ships with Ruby, so direct mode adds nothing to your dependency graph.
Adding `ffi` would put a native-extension gem in the way and buy nothing:
`dlopen` and a struct-free calling convention is the whole requirement.

There is also **no streaming callback to arrange**, unlike llmux — patala has
no streaming operation, so `Fiddle::Closure` never appears here and neither
does the GVL question that comes with it. Six functions is the entire ABI.

## Sidecar

```ruby
require "patala"

Patala::Sidecar.start do |sc|                 # spawns it, waits for /healthz
  receipt = sc.charge("mock", amount_minor: 1250, currency: "USDC",
                              destination: "mock:wallet:alice", reference: "order-1")
  sc.verify("mock", receipt).fetch("valid")   # => true
end                                           # terminated on the way out

# or point at one somebody else runs:
sc = Patala::Sidecar.new(base_url: "http://127.0.0.1:8420",
                         token: ENV.fetch("PATALA_SIDECAR_TOKEN"))
```

`#try` returns `[status, body]` where `#request` would raise — use it for the
cases where the status *is* what you want to inspect. A non-2xx is an **answer
with a body**, not a transport failure, so `Patala::HTTPError` keeps both:

```
ok  Patala::HTTPError keeps the status and the parsed body:
    patala-sidecar returned 404 — unknown_rail: no rail is registered under id "nope"
```

Binary resolution is `PATALA_SIDECAR_BIN`, then `target/{debug,release}/` in a
checkout, then `patala-sidecar` on `PATH`. `PATALA_SIDECAR_TOKEN` is generated
per spawn (32 random bytes); the server refuses to start without one, and the
bind address is hardcoded to `127.0.0.1`.

**The sidecar's rail registry is mock-only today** — any other `rail_id` is a
`404`. That is a gap in the sidecar, not in this gem.

## fork(), measured

`examples/fork_probe.rb`. Sections 1 and 2 are the good news; section 3 is the
one rule that is real.

```
threads in a bare ruby process: 2
threads after dlopen + patala_new: 2
threads after a charge round trip: 2   <- unchanged: no runtime, no thread pool

1. after fork(), with the library loaded AND USED before the fork
   (this is what Unicorn's `preload_app true` and Puma's `preload_app!` do)
    charge on a FRESH handle                 returned 1250  (0.01s)
    charge on the INHERITED handle           returned 1250  (0.00s)
    charge -> verify, inherited handle       returned {"valid" => true}  (0.01s)
    validate-destination (pure, offline)     returned StructurallyValid  (0.00s)
```

Compare llmux's Ruby page, where the same three lines are `child HUNG
(SIGKILLed after 10s)` — and where the trap is that a *cheap* method still
answers in a broken child, so a boot check reports a clean bill of health for a
worker that will hang on the first real request. There is no such trap here,
because nothing is broken to hide.

### The one rule

`patala.h`: *"Handles are not inherited usefully across a fork; open them in the
child."* Forking from a quiet parent makes that look like superstition. It is
not — a handle's runtime sits behind a mutex, and `fork()` copies a *locked*
mutex as locked. With four parent threads charging on the same handle:

```
    inherited handle                         4/200 hung
    fresh handle in the child                0/200 hung

  (121949 charges completed on the hammering threads meanwhile.)
```

A race against a window a few microseconds wide, so most forks look fine and a
test that forks once is a false green. The fix is placement, and it costs
nothing:

| host | what to do |
| --- | --- |
| **Unicorn** | build the `Patala::Ffi` in `after_fork`, not at boot |
| **Puma clustered** (`workers 2+`) | `on_worker_boot` |
| **Passenger** | per worker, or `passenger_spawn_method direct` |
| **Resque** | in the job's child |
| **Spring** | same, or the sidecar in development |
| Puma **single**, Falcon, Sidekiq, rake, CLI | nothing — these do not fork |

Loading the **library** before the fork is fine; it is the handle that matters.
Every host in that table is a hard "use the sidecar" in llmux's Ruby README.
Here it is a one-line placement note.

## Platforms

Built and exercised here: **darwin/arm64**, `libpatala_ffi.dylib`, from
`cargo build -p patala-ffi` on this machine. Nothing else was produced — no
Linux `.so` was built here (CI's `c-abi` job covers `ubuntu-latest`), no
Windows DLL exists, and `patala_free`'s "not your `free()`" rule matters most
on Windows. Most Ruby is deployed on linux/amd64, which is a row nobody has
built and smoke-tested locally.

The sidecar path needs only the `patala-sidecar` binary for your platform and
has no such matrix.

## Costs that are real

1. **A lazily created tokio runtime per handle** — a *current-thread* runtime,
   so it starts no threads and drives work on whichever thread called in. The
   UniFFI bindings (`patala-py`, `patala-go`) use a process-wide multi-thread
   runtime instead; this one does not.
2. **Calls on ONE handle serialise** — that handle owns one runtime, behind a
   mutex. Calls on *different* handles run concurrently. Open one handle per
   rail, and more than one if you want parallelism on the same rail.
3. **`dlclose` is never called.** The library is loaded once per process and
   left mapped, like every binding in this set.
4. **No auth on the direct boundary, by design.** The sidecar's token gate is a
   property of the HTTP shell; an in-process host is already inside the trust
   boundary.
5. **Handles are inherited across `fork()` but must not be used** — see above.

## Files

| file | mode | what it shows |
| --- | --- | --- |
| `lib/patala/ffi.rb` | direct | the six-function C ABI bound with fiddle, plus `id`/`capabilities`/`quote`/`charge`/`verify`/`validate_destination`/`webhook`/`caveat`/`providers` |
| `lib/patala.rb` | sidecar | spawn + healthz + terminate, and the HTTP API with both a raising and a `[status, body]` form |
| `examples/direct_charge.rb` | direct | ABI version probe, capabilities, pre-flight, quote → charge → verify, tamper detection, errors, use-after-close |
| `examples/sidecar_charge.rb` | sidecar | the same round trip over HTTP, plus all four error codes (400 / 404 / 501 / 401) |
| `examples/fork_probe.rb` | direct | thread counts, `fork()` with the library preloaded, and the inherited-handle race |
