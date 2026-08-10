# patala for Deno

A sovereign payment-rail substrate: one `PaymentRail` trait, several rails
behind it, and one rule — patala itself never holds funds.

Two modes in one dependency-free module, [`mod.ts`](mod.ts). The JSON is
identical in both; only the transport differs.

| | **Direct** | **Sidecar** |
|---|---|---|
| what runs | `libpatala_ffi` loaded into this isolate's process | the `patala-sidecar` binary as a child process on `127.0.0.1` |
| exports | `Rail` | `Sidecar` |
| flags | `--allow-ffi` | `--allow-run --allow-net=127.0.0.1 --allow-env` |
| dependencies | none — `Deno.dlopen` | none — `Deno.Command` and `fetch` |
| where a signing key lives | in this process, alongside your app | in the sidecar's process, and nowhere else |
| blocks the isolate | no — `callAsync` is `nonblocking` | no |
| survives a `fork()` | the library yes; **open the handle in the child** — see below | yes |
| extra bytes on disk | **849,584 bytes** | the binary you already have |
| rails reachable today | every rail the library was built with | **`mock` only** — the registry is unwritten |
| platforms | **darwin/arm64 built and executed here** — see below | wherever the binary builds |

Tested on **Deno 2.7.11 (aarch64-apple-darwin)**.

## Which mode

**The sidecar's argument is key isolation, and for a payments substrate it is
the strong one.** A non-custodial rail's signing key lives inside whichever
process calls `charge`. Link the library into five services and that key is
smeared across five processes' memory. Run one sidecar and it lives in one
narrow, purpose-built process that does nothing else.

**Direct mode's argument is that it costs almost nothing and reaches every
rail.** Today it is also the only mode that reaches a rail other than `mock`:
the sidecar's registry has exactly one entry and per-rail registration is
unwritten (see [`patala-sidecar/README.md`](../../patala-sidecar/README.md)).

There is deliberately **no streaming** in either mode, and no `patala_stream` in
the ABI: patala has no streaming operation. Nothing it does produces a sequence
of chunks, so there is nothing to iterate. (llmux, which shares this ABI shape,
does have `llmux_stream`. Do not go looking for patala's.)

---

## `--allow-ffi` and what it does and does not prove

`examples/direct.ts` runs under **`--allow-ffi` and nothing else** — no
`--allow-net`, no `--allow-read`, no `--allow-env` — and drives a full charge →
verify round trip.

Be precise about what that means, because it is easy to oversell. **`--allow-ffi`
is effectively "allow anything", and Deno's own docs say so**: a shared library
opens sockets below the layer where Deno's permission checks live, so the flag
list is evidence about *intent*, not a guarantee about behaviour. openrate's
Deno SDK makes the same point and it is worth repeating.

What makes the claim hold here is the rail, not the flag. `MockRail` is
deterministic and has no code path to a socket at all, and `patala_new` opens
nothing regardless of which rail you ask for — only a call can reach a network,
and only for a rail that has one. A rail that *can* is one you configured
explicitly, with credentials you supplied.

`resolveLibrary()` is written for this permission set: the `PATALA_LIBRARY`
lookup is skipped unless env permission is already granted (checked with
`Deno.permissions.querySync`), so it neither prompts nor throws, and when
`Deno.statSync` fails with anything other than `NotFound` — which under
`--allow-ffi` alone means "cannot look" — the checkout path is returned anyway
and `dlopen` delivers the verdict, rather than silently skipping a library
sitting right there.

---

## Direct

```
cargo build -p patala-ffi --release      # from the repo root
deno task example:direct
# or, spelled out — and note what is NOT in this list:
deno run --allow-ffi examples/direct.ts
```

```
deno        2.7.11 on darwin/aarch64
library     /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
abi         0.1.1

version     patala: ABI version mismatch — this library is 0.1.1, the caller expected "0.0.0-not-this-one". A stale libpatala_ffi is earlier on the load path than the one you installed.

rail        handle 1, id mock
caps        NonCustodialFinal, reversible false, holds_funds false
            currencies USDC, settlement "Instant"

quote       1250 + 25 fee = 1275 USDC
            expires_at_unix 1786313756
charge      1250 USDC for order-1
            proof 32 bytes, settled_at_unix 1786313456
verify      {"valid":true}
tampered    {"valid":false} — a result, not a thrown error
            typeof boolean, so `if (v.valid)` is the only correct gate

destination mock:wallet:alice     StructurallyValid  refusal=false confirm=true
destination mock:program:vault    NotAWallet         refusal=true confirm=true
destination stellar:wallet:alice  WrongNetwork       refusal=true confirm=true
destination nonsense              Malformed          refusal=true confirm=true
caveat      patala cannot tell whether this address belongs to an exchange. A structurally perfect address m…

webhook     patala: verify_webhook is not supported by this rail
providers   patala: this libpatala_ffi was built without --features fiat, so the "fiat" rail is not available in it. Rebuild with `cargo build -p patala-ffi --features fiat` (see patala-ffi/README.md).
unknown     patala: unknown method "refund" (want one of: id, capabilities, quote, charge, verify, validate-destination, webhook, caveat, providers)

failing     patala: rail error: mock rail mock is configured to fail
opaque      Unknown, refusal=false confirm=true — a fiat rail's honest answer
use-after   patala rail is closed

async       charged 500 USDC off-thread in 0.56 ms
            the isolate ticked 0x meanwhile — 0 because there was no time to tick in
async       verify {"valid":true}

all of the above ran under --allow-ffi alone, and sent no packet.
```

Every line of that ran on `MockRail`. **This is a payments library and an
example that moves real value is not an example** — the mock rail is
deterministic, needs no credentials, opens no socket, and is in every build, so
a full charge → verify round trip is reachable before a single secret exists.

### The three things in that output to actually read

**`verify` returning `{valid: false}` is a result, not an error.** It is the
rail's fail-closed answer that a receipt does not hold. Gate entitlement on
`{valid: true}` and nothing else, and never retry a `false` as though it were
transient — that is how an unpaid order becomes an entitlement.

**`validate-destination` never fails.** "I cannot check this address" comes back
as `status: "Unknown"`, not as an exception, because a caller must handle it as
carefully as a refusal and an exception is too easy to swallow. Read
`is_refusal` (do not send) and `human_must_confirm` — which is `true` on *every*
verdict, including `StructurallyValid`. patala does not detect exchange-owned
addresses and will not guess.

**The receipt is the entitlement, not the fact that `charge` returned.** Store
it and hand it back to `verify` later.

### Sync where it is free, async where it might not be

Every method is **synchronous**, because on the mock rail they answer in
microseconds and a promise would cost more than the call. `callAsync` is the
escape hatch: its symbol is declared `nonblocking: true`, so Deno runs it on a
blocking-task thread and the isolate keeps going.

The example measures it honestly rather than implying a benefit that is not
there: the off-thread charge took **0.56 ms** and a 1 ms timer racing it ticked
**0** times, because there was no time to tick in. The point is structural — a
real rail's `charge` is a network round trip to Solana, Stellar or a processor,
and that is the one you do not want on the isolate's thread.

### Handles and memory

`Rail` implements `Symbol.dispose`, so `using rail = Rail.open()` closes the
handle on every exit path out of a block, throw included. `close()` is
idempotent, as `patala_close` is.

Handles are `uint64` registry keys, never pointers, and are **never reused** — a
closed or invented handle is a clean error rather than a segfault, and a
use-after-close says so instead of landing on whatever rail took the slot.

Every `char*` the library returns — results **and** error messages — goes
through `patala_free` before the value reaches you, error path included.
`patala_abi_version` is the one exception, and the code says so where it is
read: it returns a static string that must never be freed.

The version probe is `patala_abi_check`, not a comparison written in this
module. The ABI exports it so twelve bindings do not each reimplement — and each
forget — the check; `Rail.open({ expectVersion })` routes through it.

Strings crossing into the library are copied into a plain `ArrayBuffer` rather
than `TextEncoder.encode`'s `ArrayBufferLike`: Deno's FFI refuses a buffer that
might be shared, and it is right to — on a `nonblocking` call the library reads
it from another thread.

---

## Sidecar

```
cargo build -p patala-sidecar            # from the repo root
PATALA_SIDECAR_BINARY=../../target/debug/patala-sidecar deno task example:sidecar
```

```
deno        2.7.11 on darwin/aarch64
sidecar     http://127.0.0.1:61480  (loopback only, hardcoded — not a knob)
token       a4e083f7… (32 random bytes, in the environment, not argv)
healthz     ok — the one unauthenticated route

caps        NonCustodialFinal, currencies USDC, USD, settlement "Instant"
quote       1250 + 0 fee = 1250 USDC
charge      1250 USDC for order-1, proof 32 bytes
verify      {"valid":true}
tampered    HTTP 200 {"valid":false} — an answer, not a failure

destination mock:wallet:alice   StructurallyValid  refusal=false confirm=true
destination mock:program:vault  NotAWallet         refusal=true confirm=true
destination nonsense            Malformed          refusal=true confirm=true

webhook     HTTP 501 kind=unsupported: patala-sidecar: HTTP 501: verify_webhook is not supported by this rail
registry    HTTP 404 kind=unknown_rail: patala-sidecar: HTTP 404: no rail is registered under id "solana"
no token    HTTP 401 on a read-only route — the gate is in front of everything
wrong token HTTP 401, and indistinguishable from the line above
```

`Sidecar.start()` mints 32 random bytes, picks a free `127.0.0.1` port, launches
the binary and polls `/healthz` until it answers. Binary resolution is
`options.binary` → `PATALA_SIDECAR_BINARY` → `patala-sidecar` on `PATH`.
`Sidecar` implements `Symbol.asyncDispose`, so `await using` kills the child and
waits for it — awaiting is not politeness, Deno fails a run that leaks one.

Note that `--allow-net` is scoped to `127.0.0.1` in the task, and that this is a
scope the sidecar mode can actually honour. Direct mode's `--allow-ffi` could
not make that promise even if it wanted to.

### The token, and where it is not

The sidecar is **fail-closed on auth**: `PATALA_SIDECAR_TOKEN` must be in its
environment or the process refuses to start. There is no generated fallback and
no "runs unauthenticated if you forget" path.

This SDK passes the token in the child's **environment, never in argv**. argv is
world-readable through `ps`, and this token authorises `charge`. If you supply
your own token, supply it the same way.

The gate sits in front of **all** `/v1` routes, including the read-only
capabilities lookup — the last two lines above are the proof. A missing header,
a malformed one and a wrong token are the same detail-free `401`. `/healthz` is
the one unauthenticated route and reveals nothing about which rails exist.

There is **no separate readiness wait**, unlike openrate's SDK that this one is
shaped after: openrate's server fetches rates at startup, patala's sidecar
fetches nothing, so the listener being up is the whole story.

### Read the body, not just the status code

- `verify` on an unverifiable receipt is `200` with `{"valid": false}`.
- All five destination verdicts, refusals included, are `200`. Branch on
  `status` and `is_refusal`.
- `501` (`kind: "unsupported"`) is a rail honestly declining — the mock rail has
  no push delivery and will not invent a webhook event. Different from `502`
  (`kind: "rail_error"`), an operational failure.
- `404` means that `rail_id` is not registered. Today only `"mock"` is.

### `valid` is the JSON boolean `true`, or it is not valid

Since 0.1.1 this SDK **narrows the two documents you make a decision on** at the
boundary, rather than `as`-casting them into their interfaces.

A cast is a compile-time assertion about a value that arrived at runtime over a
socket or across a C ABI, and `JSON.parse` hands back whatever shape it was
given. An absent `valid` is `undefined`; a `valid` that some proxy stringified
is `"false"` — which is **truthy**. `if (result.valid)` then grants entitlement
against a receipt no rail confirmed.

Both directions now fail closed:

| field | value you get |
|---|---|
| `verify().valid` | `true` **only** for the JSON boolean `true` |
| `is_refusal` | `false` **only** for the JSON boolean `false` |
| `human_must_confirm` | `false` **only** for the JSON boolean `false` |

"I could not read the verdict" and "do not send" are the same answer.

### If you are behind a proxy

Deno's `fetch` honours `HTTP_PROXY`/`HTTPS_PROXY` **for loopback URLs too**, so
with one set this process cannot reach its own sidecar and `start()` times out
against a server that is running fine. Export `NO_PROXY=127.0.0.1`.

---

## The costs of direct mode

Short, because the list is short. These are patala's actual costs — the
Go-runtime caveats in llmux's and openrate's SDK READMEs are true there and
**false here**, and have deliberately not been copied. `patala.h` and
[`patala-ffi/README.md`](../../patala-ffi/README.md) say the same.

1. **A lazily-initialised Tokio runtime per handle.** `patala-core`'s trait is
   `async`, so each handle owns a **current-thread** runtime, built in
   `patala_new` and dropped with the handle. A current-thread runtime spawns no
   threads: it drives futures on whichever thread called in. The consequence
   worth knowing is that calls on **one** handle serialise; calls on different
   handles run concurrently. Open one handle per rail, and more than one if you
   want parallelism on the same rail.

2. **The library is 849,584 bytes** in the default mock-only build — measured,
   release, darwin/arm64. A build with all twenty fiat adapters, UniFFI, reqwest
   and TLS is 6,350,144 bytes.

3. **`--allow-ffi` is the widest permission Deno has.** See the section above.
   Granting it to run patala grants it to everything else in the process too.

4. **Platforms.**

   | target | status |
   |---|---|
   | darwin/arm64 | **built and executed here.** Every output in this README came off it. |
   | linux/amd64, linux/arm64 | not built by this session. CI's `c-abi` job builds and runs the `.so` on `ubuntu-latest`. |
   | windows/amd64 | **not built. No DLL exists.** |

   Deno runs on Windows. Nothing here has been run there. The sidecar is the
   answer, and `patala_free`'s "not your `free()`" rule is the one that would
   bite hardest if it were not.

5. **Latency is not the reason to embed.** In-process is microseconds against
   tens of microseconds over loopback — real, and irrelevant next to a Solana
   RPC round trip. The reason **not** to embed is the signing key.

### What is genuinely not a cost here

Stated because the other two products in this suite have to warn about all of
it, and a reader who learns one is entitled to know which warnings travel:

- **No language runtime.** No GC, no preemptive scheduler, no green threads.
- **No signal handlers installed** — not one. Measured with llmux's own probe
  on a JVM, the harshest host there is: all thirteen probed signals come back
  `unchanged`, where a Go `c-shared` library replaces five (`SIGSEGV`,
  `SIGBUS`, `SIGFPE`, `SIGPIPE`, `SIGURG`) and alters the flags on three more.
  Your crash reporter and sampling profiler have nothing to collide with.
- **No threads started**, at load or ever. Measured from Node in the same
  checkout: 7 threads before `dlopen`, 7 after a full round trip; the Go-based
  `libopenrate` takes the same process from 7 to 13.
- **The library is fork-safe; a handle that is *in use* at the moment of the
  fork is not.** Nothing of patala's is running at `fork()` time, so loading
  the library before a fork is fine — but a handle's runtime sits behind a
  mutex and `fork()` copies a locked mutex as locked. Measured: with four
  parent threads charging on the same handle, over 200 forks, an inherited
  handle hung **4–8 times in 200** against **0 in 200** for one opened in the
  child. The window is microseconds wide, so a test that forks once is a false
  green. **Open the handle in the child.** Full measurement:
  [`docs/choosing-a-mode.md`](../../docs/choosing-a-mode.md#the-advantage-that-is-easy-to-miss-no-runtime-in-your-process)
  and [`sdks/README.md`](../README.md#costs-that-are-real-and-are-not-the-siblings-costs).
- **Nothing happens at load.** No socket, no file, no background task.

`patala-ffi/ctest/smoke.c` is where that stops being prose: it counts the
process's threads before `dlopen`, after `dlopen`, and after a full charge →
verify round trip, and **fails** — never skips — if it cannot count them.

---

## Layout and checks

```
sdks/deno/
  deno.json               tasks, fmt and lint config
  mod.ts                  the patala-core JSON shapes, Rail (direct), Sidecar
  checks.ts               counted assertions over the boundary narrowing
  examples/direct.ts      MockRail in-process, end to end, under --allow-ffi alone
  examples/sidecar.ts     MockRail over loopback, end to end
```

```
deno task check      # deno check mod.ts and both examples
deno task lint
deno task fmt:check
```

```
deno task checks     # 18 counted assertions over the narrowing, no I/O
```

`checks.ts` is the only per-SDK regression gate on the narrowing above. It
touches no network, no child process and no shared library, and it asserts the
number of assertions that **ran**, so a suite that quietly stops running half of
itself fails instead of passing.

