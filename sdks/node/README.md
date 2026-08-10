# patala for Node

A sovereign payment-rail substrate: one `PaymentRail` trait, several rails
behind it, and one rule — patala itself never holds funds.

Two modes. The JSON is identical in both; only the transport differs.

| | **Sidecar** | **Direct** |
|---|---|---|
| what runs | the `patala-sidecar` binary as a child process on `127.0.0.1` | `libpatala_ffi` loaded into this Node process |
| module | `require("patala")` | `require("patala/direct")` |
| dependencies | none | `koffi` (optional peer) |
| where a signing key lives | in the sidecar's process, and nowhere else | in this process, alongside your app |
| blocks the event loop | no | only if you want it to — see below |
| survives a `fork()` | yes | the library yes; **open the handle in the child** — see below |
| extra bytes on disk | the binary you already have | **849,584 bytes** |
| rails reachable today | **`mock` only** — the registry is unwritten | every rail the library was built with |
| platforms | wherever the binary builds | **darwin/arm64 built and executed here** — see below |

Tested on **Node v24.12.0, koffi 3.1.4, darwin/arm64**.

## Which mode

**The sidecar's argument is key isolation, and for a payments substrate it is
the strong one.** A non-custodial rail's signing key lives inside whichever
process calls `charge`. Link the library into five services and that key is
smeared across five processes' memory, where a bug or a dependency-confusion
attack in any one of them reaches it. Run one sidecar and the key lives in one
narrow, purpose-built process that does nothing else.

**Direct mode's argument is that it costs almost nothing and reaches every
rail.** Today it is also the only mode that reaches a rail other than `mock`:
the sidecar's registry has exactly one entry and per-rail registration is
unwritten (see [`patala-sidecar/README.md`](../../patala-sidecar/README.md)).
That is a real limitation and it is stated here rather than discovered.

There is deliberately **no streaming** in either mode, and no `patala_stream` in
the ABI: patala has no streaming operation. Nothing it does produces a sequence
of chunks, so there is nothing to iterate. (llmux, which shares this ABI shape,
does have `llmux_stream`. Do not go looking for patala's.)

---

## Direct

```
cargo build -p patala-ffi --release      # from the repo root
npm install
npm install koffi                        # optional peer, direct mode only
npm run example:direct
```

```
node        v24.12.0 on darwin/arm64
library     /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
bytes       849584
abi         0.1.1
threads     7 before dlopen -> 7 after

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
threads     7 after every synchronous call above

async       charged 500 USDC off-thread in 0.44 ms
            the event loop ticked 1x meanwhile — low because there was no time to tick in
async       verify {"valid":true}
threads     7 at startup -> 11 after the off-thread call
            the difference is libuv's threadpool (UV_THREADPOOL_SIZE, default 4), not patala's:
            no GC, no scheduler, no signal handler, no thread of its own. That is the Rust dividend.
```

Every line of that ran on `MockRail`. **This is a payments library and an
example that moves real value is not an example** — the mock rail is
deterministic, needs no credentials, opens no socket, and is in every build, so
a full charge → verify round trip is reachable before a single secret exists.

### The three things in that output to actually read

**`verify` returning `{valid: false}` is a result, not an error.** It is the
rail's fail-closed answer that a receipt does not hold. Gate entitlement on
`{valid: true}` and nothing else, and never retry a `false` as though it were
transient — that is how an unpaid order becomes an entitlement. The ABI returns
a *result* there rather than `NULL` precisely so the two cannot be confused.

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
handle on every exit path out of the block, throw included. `close()` is
idempotent, as `patala_close` is.

Handles are `uint64` registry keys, never pointers, and are **never reused** — a
closed or invented handle is a clean error rather than a segfault, and a
use-after-close says so instead of landing on whatever rail took the slot.

Every `char*` the library returns — results **and** error messages — goes
through `patala_free` before the value reaches you, error path included.
`patala_call` is declared as returning `void*`, not `char*`, precisely so koffi
cannot decode it into a string and discard the pointer we still have to free.
`patala_abi_version` *is* declared `const char*`, which is correct there and
only there: it returns a static string that must never be freed.

The version probe is `patala_abi_check`, not a comparison written in this
package. The ABI exports it so that twelve bindings do not each reimplement —
and each forget — the check; `Rail.open({ expectVersion })` routes through it.

### Why koffi

Node has no FFI in its standard library. koffi is declared an **optional peer
dependency**, so the sidecar path installs with no native code at all.

- **`node-ffi-napi`** — effectively unmaintained; its last release predates
  several Node majors. Not viable.
- **A hand-written N-API addon** — the honest alternative. Rejected on cost: it
  means node-gyp and a C toolchain at install time, or `prebuildify` artifacts
  for every platform × Node-ABI pair, for a *six-function* ABI. It would buy a
  build pipeline and, for patala, nothing else — there is no callback in this
  ABI to need `napi_threadsafe_function`, because there is no streaming.
- **`koffi`** — MIT, actively released (3.1.4 here), ships prebuilt binaries so
  `npm install` needs no compiler, ~1.9 MB installed, and its declarative C
  prototypes make this binding a transcription of `patala.h` rather than a
  reimplementation of it.

The cost is real and worth naming: koffi is native code in your process, and a
bug in it is a segfault, not an exception. That is the same trade openrate and
llmux made for the same reason.

---

## Off the main thread — and why this section is different here

llmux's and openrate's Node SDKs are synchronous-only, and their READMEs explain
why: a Node thread that has entered a Go `c-shared` library never terminates, so
both `worker_threads` and koffi's `.async` return the right answer and then hang
the process forever.

**That hazard does not exist here, and it was verified rather than assumed.**
patala is Rust: no runtime in the host process, so no thread is stuck inside one
at exit. Both approaches work, and this package ships `callAsync` because of it.

Measured on darwin/arm64, Node v24.12.0, koffi 3.1.4, with the Go control run
side by side in the same environment:

| | `libpatala_ffi` (Rust) | `libopenrate` (Go, control) |
|---|---|---|
| main thread, synchronous | works, exits | works, exits |
| `worker_threads` worker | **works, worker exits 0 in ~33 ms** | answers, then **never exits** — killed at 15 s |
| koffi `.async` (libuv threadpool) | **works, process exits 0** | answers, then **hangs** — killed at 12 s |
| threads across `dlopen` + a round trip | **7 → 7** | **7 → 13** |

The worker reproduction, with no patala logic in it at all:

```js
import { Worker, isMainThread, parentPort } from "node:worker_threads";
if (!isMainThread) {
  const koffi = (await import("koffi")).default;
  const lib = koffi.load(process.env.LIB);
  parentPort.postMessage(lib.func("const char *patala_abi_version()")());
} else {
  const w = new Worker(new URL(import.meta.url));
  console.log("worker said:", await new Promise((r) => w.on("message", r)));
  console.log("worker exited with", await new Promise((r) => w.on("exit", r)));
}
```

Against `libpatala_ffi.dylib` it prints `worker said: 0.1.1` then
`worker exited with 0`. Against `libopenrate-darwin-arm64.dylib`, with only the
symbol name changed, it prints the version and then hangs — in the run recorded
here, hard enough that a 5 s `setTimeout` racing the `exit` event never fired
either. So it is the Go runtime, not koffi and not Node.

`callAsync` is worth using for a rail that talks to a network — `charge` on
Solana, Stellar or a fiat processor is a round trip — and worth nothing for the
mock rail, which answers in 0.44 ms. The example measures it rather than
implying a benefit.

**One honest note on the thread count.** The example's last measurement is
`7 → 11`, and those four threads are **libuv's**, not patala's: the default
threadpool is created lazily on its first request. A process that has never
heard of patala moves the same number identically with one
`await fs.promises.readFile(...)`. Every synchronous line in the example leaves
it at 7.

---

## Sidecar

```
cargo build -p patala-sidecar            # from the repo root
PATALA_SIDECAR_BINARY=../../target/debug/patala-sidecar npm run example:sidecar
```

```
node        v24.12.0 on darwin/arm64
sidecar     http://127.0.0.1:61477  (loopback only, hardcoded — not a knob)
token       4a3305a7… (32 random bytes, in the environment, not argv)
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
`Sidecar` implements `Symbol.asyncDispose`, so `await using` kills the child on
the way out.

### The token, and where it is not

The sidecar is **fail-closed on auth**: `PATALA_SIDECAR_TOKEN` must be in its
environment or the process refuses to start. There is no generated fallback and
no "runs unauthenticated if you forget" path.

This SDK passes the token in the child's **environment, never in argv**. argv is
world-readable through `ps` on every mainstream OS, and this token authorises
`charge`. If you supply your own token, supply it the same way.

The gate sits in front of **all** `/v1` routes, including the read-only
capabilities lookup — the last two lines above are the proof. A missing header,
a malformed one and a wrong token are the same detail-free `401`.

`/healthz` is the one unauthenticated route and reveals nothing about which
rails are configured.

Unlike openrate's SDK, which this one is shaped after, there is **no separate
readiness wait**. openrate's server fetches rates at startup, so its `/healthz`
can answer before a conversion would work; patala's sidecar fetches nothing, so
the listener being up is the whole story.

### Read the body, not just the status code

- `verify` on an unverifiable receipt is `200` with `{"valid": false}`. Never an
  HTTP error, so "verified false" cannot be mistaken for "the sidecar broke".
- All five destination verdicts, refusals included, are `200`. Branch on
  `status` and `is_refusal`.
- `501` (`kind: "unsupported"`) is a rail honestly declining — the mock rail has
  no push delivery and will not invent a webhook event. It is a different thing
  from `502` (`kind: "rail_error"`), which is an operational failure.
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

---

## The costs of direct mode

Short, because the list is short. These are patala's actual costs — the
Go-runtime caveats in llmux's and openrate's SDK READMEs are true there and
**false here**, and have deliberately not been copied. `patala.h` and
[`patala-ffi/README.md`](../../patala-ffi/README.md) say the same.

1. **koffi is native code in your process.** A bug in it is a segfault. This is
   the only "a crash instead of an exception" risk in the direct path — patala
   itself catches unwinding at every entry point and turns a panic into an error
   string.

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

   Node is heavily used on Windows, so say it plainly: **nothing here has been
   run on Windows.** The sidecar is the answer there, and `patala_free`'s "not
   your `free()`" rule is the one that would bite hardest if it were not.

5. **Latency is not the reason to embed.** In-process is microseconds against
   tens of microseconds over loopback — real, and irrelevant next to a Solana
   RPC round trip. The reasons are the byte count, the absence of a second
   process and a loopback surface, and reaching a rail the sidecar's registry
   does not yet expose. The reason **not** to embed is the signing key.

### What is genuinely not a cost here

Stated because the other two products in this suite have to warn about all of
it, and a reader who learns one is entitled to know which warnings travel:

- **No language runtime.** No GC, no preemptive scheduler, no green threads.
- **No signal handlers installed** — not one. Measured with llmux's own probe
  on a JVM, the harshest host there is: all thirteen probed signals come back
  `unchanged`, where a Go `c-shared` library replaces five (`SIGSEGV`,
  `SIGBUS`, `SIGFPE`, `SIGPIPE`, `SIGURG`) and alters the flags on three more.
  Your crash reporter and sampling profiler have nothing to collide with.
- **No threads started**, at load or ever. Measured above: 7 → 7.
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
sdks/node/
  index.ts              the sidecar client + a re-export of direct mode
  types.ts              the patala-core JSON shapes, shared by both transports
  direct.ts             the C ABI binding: Rail
  sidecar.ts            spawn, token, /healthz wait, the five endpoints
  checks.ts             counted assertions over the boundary narrowing
  examples/direct.ts    MockRail in-process, end to end
  examples/sidecar.ts   MockRail over loopback, end to end
```

```
npm run build                # tsc -> index/direct/sidecar/types .js and .d.ts
npm run typecheck            # tsc over the library sources
npm run typecheck:examples   # tsc over examples/ (ESM, its own tsconfig)
npm run example:direct
npm run example:sidecar
```

```
npm run checks               # 18 counted assertions over the narrowing, no I/O
```

`checks.ts` is the only per-SDK regression gate on the narrowing above. It
touches no network, no child process and no shared library, and it asserts the
number of assertions that **ran**, so a suite that quietly stops running half of
itself fails instead of passing.

