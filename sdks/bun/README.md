# patala for Bun

A sovereign payment-rail substrate: one `PaymentRail` trait, several rails
behind it, and one rule — patala itself never holds funds.

Two modes in one dependency-free module, [`index.ts`](index.ts). The JSON is
identical in both; only the transport differs.

| | **Direct** | **Sidecar** |
|---|---|---|
| what runs | `libpatala_ffi` loaded into this Bun process | the `patala-sidecar` binary as a child process on `127.0.0.1` |
| exports | `Rail` | `Sidecar` |
| dependencies | none — `bun:ffi` | none — `Bun.spawn` and `fetch` |
| where a signing key lives | in this process, alongside your app | in the sidecar's process, and nowhere else |
| blocks the calling thread | **yes** — `bun:ffi` has no async mode | no |
| survives a `fork()` | the library yes; **open the handle in the child** — see below | yes |
| extra bytes on disk | **849,584 bytes** | the binary you already have |
| rails reachable today | every rail the library was built with | **`mock` only** — the registry is unwritten |
| platforms | **darwin/arm64 built and executed here** — see below | wherever the binary builds |

Tested on **Bun 1.3.14, darwin/arm64**.

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

## Direct

```
cargo build -p patala-ffi --release      # from the repo root
bun install
bun run examples/direct.ts
```

```
bun         1.3.14 on darwin/arm64
library     /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
bytes       849584
abi         0.1.1
threads     8 before dlopen -> 8 after

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

threads     8 at startup -> 8 after all of the above
            ±1 run to run, and that ±1 is BUN's: the same measurement with no patala
            loaded was seen going 6 -> 8 -> 9. patala starts no thread — no GC, no
            scheduler, no signal handler — and `make smoke-ffi` is where that is proved
            in C rather than smelled here.
```

Every line of that ran on `MockRail`. **This is a payments library and an
example that moves real value is not an example** — the mock rail is
deterministic, needs no credentials, opens no socket, and is in every build, so
a full charge → verify round trip is reachable before a single secret exists.

### About that thread count, honestly

`8 → 8` is the usual result, but it is **not a proof on Bun**, and the example
says so where it prints it. Across eight runs of the example the closing figure
came out `8` six times and `9` twice — and a control script with **no patala in
it at all**, doing the same shape of work, was measured going `6 → 8 → 9`. Bun
grows its own thread pool lazily; the number drifts under the measurement.

Where the claim does hold cleanly:

- **Node**, in the sibling SDK: `7 → 7` on every run, with a no-patala control
  stable at `7`, and the Go-based `libopenrate` taking the same process `7 → 13`.
  See [`../node/README.md`](../node/README.md).
- **C**, in `patala-ffi/ctest/smoke.c`, which counts threads before `dlopen`,
  after `dlopen`, and after a full charge → verify round trip with nothing else
  in the process — and **fails**, never skips, if it cannot count them.

A measurement that moves for the host's reasons is worth printing and worth
labelling; it is not worth quoting as evidence.

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

### Handles and memory

`Rail` implements `Symbol.dispose`, so `using rail = Rail.open()` closes the
handle on every exit path out of a block, throw included. `close()` is
idempotent, as `patala_close` is.

Handles are `uint64` registry keys, never pointers, and are **never reused** — a
closed or invented handle is a clean error rather than a segfault, and a
use-after-close says so instead of landing on whatever rail took the slot.

Every `char*` the library returns — results **and** error messages — goes
through `patala_free` before the value reaches you, error path included.
`patala_call` is declared `FFIType.ptr`, not `FFIType.cstring`, precisely so bun
cannot decode the result into a string and drop the pointer we still have to
free. `patala_abi_version` *is* declared `cstring`, which is correct there and
only there: it returns a static string that must never be freed.

The version probe is `patala_abi_check`, not a comparison written in this
module. The ABI exports it so twelve bindings do not each reimplement — and each
forget — the check; `Rail.open({ expectVersion })` routes through it.

---

## Off the main thread

`bun:ffi` has **no asynchronous call mode** — no `nonblocking` option the way
`Deno.dlopen` has one, no threadpool variant the way koffi has one. Every direct
call runs on the thread that made it.

On the mock rail that is academic: the calls are microseconds. It is not
academic for a real rail, where `charge` is a network round trip to Solana,
Stellar or a processor, and a synchronous FFI call holds the thread for its
whole duration.

The answer on Bun is a `Worker`: open the rail inside it, post requests in, post
results back. **Measured here, that works** — a Bun worker that has loaded
`libpatala_ffi` and called into it fires `close` and lets the process exit in
about 10 ms:

```ts
import { dlopen, FFIType } from "bun:ffi";
if (Bun.isMainThread) {
  const w = new Worker(new URL(import.meta.url).href);
  console.log("worker said:", await new Promise((r) => { w.onmessage = (e) => r(e.data); }));
  console.log("worker:", await new Promise((r) => w.addEventListener("close", () => r("closed"))));
} else {
  const lib = dlopen(process.env.LIB!, { patala_abi_version: { args: [], returns: FFIType.cstring } });
  postMessage(String(lib.symbols.patala_abi_version()));
}
```

**Be precise about what that does and does not prove.** The same probe against
the Go-based `libopenrate` also terminates cleanly on Bun, so this is not a
patala advantage on this runtime — it is Bun not joining its workers the way
Node does. **On Node the difference is real and large**: there, a
`worker_threads` worker that has entered a Go `c-shared` library never exits,
which is why llmux's and openrate's Node SDKs are synchronous-only, while the
same worker over `libpatala_ffi` exits `0`. The measurements and the controls
are in [`../node/README.md`](../node/README.md).

---

## Sidecar

```
cargo build -p patala-sidecar            # from the repo root
PATALA_SIDECAR_BINARY=../../target/debug/patala-sidecar bun run examples/sidecar.ts
```

```
bun         1.3.14 on darwin/arm64
sidecar     http://127.0.0.1:61483  (loopback only, hardcoded — not a knob)
token       c3a52537… (32 random bytes, in the environment, not argv)
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
awaits its exit.

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

Unlike [node](../node) and [deno](../deno), **this package has no `checks.ts`**:
`narrowVerify` and `narrowVerdict` here are covered only by the two examples.
Those two carry 18 counted assertions each over exactly this narrowing.

---

## The costs of direct mode

Short, because the list is short. These are patala's actual costs — the
Go-runtime caveats in llmux's and openrate's SDK READMEs are true there and
**false here**, and have deliberately not been copied. `patala.h` and
[`patala-ffi/README.md`](../../patala-ffi/README.md) say the same.

1. **Every call is synchronous.** `bun:ffi` gives no choice. See "Off the main
   thread" above.

2. **A lazily-initialised Tokio runtime per handle.** `patala-core`'s trait is
   `async`, so each handle owns a **current-thread** runtime, built in
   `patala_new` and dropped with the handle. A current-thread runtime spawns no
   threads: it drives futures on whichever thread called in. The consequence
   worth knowing is that calls on **one** handle serialise; calls on different
   handles run concurrently. Open one handle per rail, and more than one if you
   want parallelism on the same rail.

3. **The library is 849,584 bytes** in the default mock-only build — measured,
   release, darwin/arm64. A build with all twenty fiat adapters, UniFFI, reqwest
   and TLS is 6,350,144 bytes.

4. **Platforms.**

   | target | status |
   |---|---|
   | darwin/arm64 | **built and executed here.** Every output in this README came off it. |
   | linux/amd64, linux/arm64 | not built by this session. CI's `c-abi` job builds and runs the `.so` on `ubuntu-latest`. |
   | windows/amd64 | **not built. No DLL exists.** |

   Nothing here has been run on Windows. The sidecar is the answer there, and
   `patala_free`'s "not your `free()`" rule is the one that would bite hardest
   if it were not.

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
- **No threads started**, at load or ever — proved in C by
  `patala-ffi/ctest/smoke.c` and measured stably from Node at 7 → 7, against
  7 → 13 for the Go-based `libopenrate`. Bun's own count drifts, so read the
  section above before quoting the example's figure.
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
sdks/bun/
  index.ts                the patala-core JSON shapes, Rail (direct), Sidecar
  examples/direct.ts      MockRail in-process, end to end
  examples/sidecar.ts     MockRail over loopback, end to end
```

```
bun install
bun run check            # tsc --noEmit, reusing the TypeScript pinned by sdks/node
bun run example:direct
bun run example:sidecar
```
