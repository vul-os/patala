# patala (Java)

Two ways to run patala from Java, both supported:

| | class | what it is | recommended? |
|---|---|---|---|
| **Direct** | `org.vulos.patala.PatalaDirect` | loads `libpatala_ffi` and runs patala *inside this JVM* | **yes — and that is the opposite of what llmux and openrate recommend** |
| **Sidecar** | `org.vulos.patala.Patala` | spawns `patala-sidecar` on `127.0.0.1`, talks HTTP | when you want **key isolation**, or you ship to Windows |

**Read the row above twice if you have used llmux's or openrate's Java SDK.**
Both of those tell you to prefer the sidecar, and the reason they give is that
loading a Go `c-shared` library replaces five of HotSpot's signal handlers.
That reason is real there and **false here**, and it was checked rather than
assumed — [the measurement is below](#the-jvm-and-patalas-shared-library),
produced by the same probe llmux ships, run on the same machine and JDK
against both libraries on the same day.

```sh
sdks/java/run-examples.sh            # both
sdks/java/run-examples.sh direct     # offline
sdks/java/run-examples.sh sidecar    # loopback only; still offline
```

Neither example touches a network, and neither moves value. patala's default
rail is `MockRail` — deterministic, offline, no credentials — because this is a
payments library and an example that moves real value is not an example.

---

## The JVM and patala's shared library

llmux's and openrate's Java READMEs carry a long list of hazards that comes
from the Go runtime living inside your process. **patala's core is Rust and
carries no runtime**, so the list should be empty. "Should be" is not a
measurement, so here is one.

`sdks/java/signal-probe.sh` is llmux's probe with two additions — it counts the
process's OS threads, and it re-reads the handlers after a full
charge → verify round trip rather than only after `dlopen`, because a handle's
Tokio runtime is created lazily and "nothing at load time" would be the weaker
claim.

```sh
sdks/java/signal-probe.sh              # what changed
sdks/java/signal-probe.sh --checkjni   # HotSpot's own audit of it
sdks/java/signal-probe.sh --jsig       # again, with libjsig preloaded
```

On **OpenJDK 26.0.2 (Homebrew), darwin/arm64, patala 0.1.0**, against
`target/release/libpatala_ffi.dylib`:

```
signal    before                after load            after round trip      verdict
---------------------------------------------------------------------------------------------------
SIGILL    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    unchanged
SIGTRAP   SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGABRT   SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGFPE    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    unchanged
SIGBUS    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    unchanged
SIGSEGV   0x1040cbcc0 f=0x43    0x1040cbcc0 f=0x43    0x1040cbcc0 f=0x43    unchanged
SIGPIPE   0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    unchanged
SIGURG    SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGXCPU   SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGXFSZ   0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    0x1040cbcc0 f=0x42    unchanged
SIGPROF   SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGUSR1   SIG_DFL f=0x0         SIG_DFL f=0x0         SIG_DFL f=0x0         unchanged
SIGUSR2   0x1040cb940 f=0x42    0x1040cb940 f=0x42    0x1040cb940 f=0x42    unchanged

0 handler(s) replaced, 0 left in place with altered flags

threads in this process (the same measurement patala-ffi/ctest/smoke.c makes):
  before dlopen:        23
  after dlopen:         23
  after a round trip:   23
```

And the same probe, same JVM, same session, pointed at
`llmux/dist/ffi/darwin_arm64/libllmux.dylib`:

```
SIGILL    0x102333cc0 f=0x42    0x102333cc0 f=0x43    flags changed (Go added SA_ONSTACK)
SIGFPE    0x102333cc0 f=0x42    0x1320e13d0 f=0x43    HANDLER REPLACED by the Go runtime
SIGBUS    0x102333cc0 f=0x42    0x1320e13d0 f=0x43    HANDLER REPLACED by the Go runtime
SIGSEGV   0x102333cc0 f=0x43    0x1320e13d0 f=0x43    HANDLER REPLACED by the Go runtime
SIGPIPE   0x102333cc0 f=0x42    0x1320e13d0 f=0x43    HANDLER REPLACED by the Go runtime
SIGURG    SIG_DFL f=0x0         0x1320e13d0 f=0x43    HANDLER REPLACED by the Go runtime
SIGXFSZ   0x102333cc0 f=0x42    0x102333cc0 f=0x43    flags changed (Go added SA_ONSTACK)
SIGUSR2   0x102333940 f=0x42    0x102333940 f=0x43    flags changed (Go added SA_ONSTACK)

5 handler(s) replaced, 3 left in place with altered flags
```

Five things worth stating plainly:

1. **`SIGSEGV` is untouched.** HotSpot elides null checks in compiled code and
   recovers them from `SIGSEGV`, and grows stacks through guard-page faults.
   It is the JVM's most load-bearing signal, a Go shared library takes it, and
   patala's does not.
2. **`SIGUSR2` is untouched, flags included.** That is HotSpot's `SR_handler`,
   the thread suspend/resume mechanism safepoints depend on. Go does not
   replace it either, but it does add `SA_ONSTACK` to it. patala changes
   nothing about it at all.
3. **Zero threads.** 23 before `dlopen`, 23 after, 23 after a full round trip.
   patala's handles own a *current-thread* Tokio runtime, so patala's work runs
   on the thread that called in and there is no scheduler to schedule it.
4. **HotSpot agrees.** Under `-Xcheck:jni` the VM audits its own handlers on
   exit. Against llmux it prints `Warning: SIGSEGV handler modified!` and four
   more, ending with `Consider using jsig library.` Against patala it prints
   **nothing**.
5. **So `libjsig` is not needed**, and that is the part that changes the
   recommendation rather than merely the prose. llmux's and openrate's advice
   to prefer the sidecar rests on `libjsig` being a flag on the *java launch
   command* — something a library cannot add to a process that has already
   started, making a direct-mode dependency an operations change rather than a
   drop-in. **patala needs no such flag**, so that argument does not exist
   here, and the direct path is the recommended default.

`--enable-native-access=ALL-UNNAMED` is still required. That is a JDK
restricted-methods rule about FFM in general, not about patala, and it applies
identically to any FFM binding.

### Untested, and stated as such

- **Linux.** Everything above is darwin/arm64. `signal-probe.sh` knows the
  Linux signal numbers and reads `/proc/self/status` for the thread count, and
  will run — nobody has run it.
- **JVMTI agents and async-profiler.** Nothing here should perturb them
  precisely because nothing here installs a handler, but that is a prediction.
- **JDKs other than 26.**

---

## Direct — `org.vulos.patala.PatalaDirect`

patala inside your JVM, through the C ABI in
[`patala-ffi/include/patala.h`](../../patala-ffi/include/patala.h). Six
symbols, JSON in and JSON out — the same JSON the sidecar serves, built from
the same `patala-core` types.

```java
try (PatalaDirect rail = PatalaDirect.open("{\"rail\":\"mock\"}")) {
    rail.abiCheck();                                   // fail loudly on a stale library

    String preflight = rail.validateDestination("mock:wallet:alice");
    String req = "{\"amount_minor\":1250,\"currency\":\"USDC\","
               + "\"destination\":\"mock:wallet:alice\",\"reference\":\"order-4711\"}";

    String receipt = rail.charge(req);
    String verdict = rail.verify(receipt);             // {"valid":true}
}
```

**Creating a rail talks to nothing** — no socket, no thread, no environment
variable read. Only `call` reaches a network, and only for a rail that has one.
The default configuration (`null`, or `{"rail":"mock"}`) is a deterministic
offline `MockRail`, so a full charge → verify round trip runs before a single
secret exists.

Worked example: [`examples/DirectCharge.java`](examples/DirectCharge.java) —
its real output is at the bottom of this file.

### Why FFM and not UniFFI

patala already has a UniFFI surface (`patala-uniffi`, namespace `patala`), and
UniFFI has a **Kotlin** backend. It has no Java backend. Third-party
Java generators exist; none is version-locked to `uniffi = 0.29`, which is what
this workspace pins, and a binding generated by an unpinned tool is a binding
that silently drifts from the scaffolding it talks to.

So Java takes the plain C ABI. The Kotlin SDK evaluated the UniFFI route
properly, ran it, and reports a concrete blocker —
[`sdks/kotlin/README.md`](../kotlin/README.md#the-uniffi-route-evaluated-and-measured)
has the compiler output.

### Why FFM and not JNA

FFM is in the JDK. JNA is a dependency that ships its own native stub per
platform, so adopting it in order to load a native library means shipping *two*
native artifacts to solve the problem of shipping one.

**JNA is the documented fallback for Java 11–21**, where FFM is absent or
preview. It is **not implemented here and not tested**. The shape is
`Pointer patala_call(long, String, String, PointerByReference)` via
`Native.load("patala_ffi", …)`, and the trap is mapping the return as `String`:
JNA copies it and you can no longer hand the original to `patala_free`. Given
that Java 21 is a widely deployed LTS, **the honest recommendation for 11–21 is
the sidecar**, which is fully supported there and needs no native code.

### There is no streaming

There is no `patala_stream` and no streaming method on this class. patala has
no incremental operation: a quote, a charge, a verification and a destination
check are each one question with one answer. llmux, which shares this ABI's
shape, does define `llmux_stream`. **The omission is deliberate and is stated
rather than left to be noticed.**

### Memory and handles

- results are copied into a `java.lang.String` and then freed with
  `patala_free` in a `finally` — never with `free(3)`; this is Rust's
  allocator;
- error strings are read, freed, and turned into `PatalaException`;
- the `char** err` out-parameter is drained **on the success path too**;
- `patala_abi_version` returns a static string and is the one thing never
  freed;
- handles are closed by `close()`, which is idempotent — use
  try-with-resources, as the example does.

Handle numbers are **retired, not recycled**, so use after close is a clean
error rather than a live rail belonging to somebody else. From the example's
real output:

```
after close: this rail is closed
```

`patala.h` exposes no open-handle counter, so — unlike openrate's SDK — there
is no `openHandles()` here to assert against. Saying so is better than adding a
counter of our own that counts something different from the library's.

### `abiCheck()` uses the library's comparison, not ours

`patala_abi_check` exists so the version comparison is not reimplemented — and
forgotten — in each binding. A shared library is resolved off a load path you
may not control; without the probe a stale `libpatala_ffi` earlier on that path
is called silently and misbehaves in ways that look like patala bugs.

### Requirements

- **Java 22+** — `java.lang.foreign` became permanent in Java 22. **Tested on
  OpenJDK 26.0.2 (Homebrew), darwin/arm64.**
- **`--enable-native-access=ALL-UNNAMED`** on the java command line.
- A `libpatala_ffi` for your platform. See [Platforms](#platforms).

---

## Sidecar — `org.vulos.patala.Patala`

```java
try (Patala patala = Patala.start(new Patala.Options())) {
    String receipt = patala.charge("mock", payRequestJson);
    String verdict = patala.verify("mock", receipt);   // {"valid":true}
}
```

**Requires Java 11.** No native library, no FFM, no platform matrix. Runs on
Windows, where the direct path does not exist at all.

Worked example: [`examples/SidecarCharge.java`](examples/SidecarCharge.java).

### The reason to choose it here is key isolation, not the JVM

A non-custodial rail's signing key lives inside whichever process calls
`charge`. Link the direct path into five services and that key is smeared
across five processes' memory, so a bug or a dependency-confusion attack in any
one of them is a path to it. Route them all through one sidecar and the key
lives in exactly one narrow, purpose-built process that does nothing else.

`patala-sidecar/README.md` carries the full threat model, including what it
does **not** defend against: a co-resident, same-privilege attacker can read
the token out of the environment. Loopback-plus-token raises the bar above "any
process on the LAN", not above "a fully co-resident attacker".

### The token is mandatory, and this class mints one

`patala-sidecar` refuses to start without `PATALA_SIDECAR_TOKEN` — no
auto-generated fallback, no "runs unauthenticated if you forget" path. So
`Patala.start()` mints 32 bytes from `SecureRandom`, passes them to the child,
and sends `Authorization: Bearer` on every `/v1` request. `/healthz` is the one
unauthenticated route and reveals nothing but liveness.

Set `Options.token` and `Options.port`, or use `Patala.attach(baseUrl, token)`,
to talk to a sidecar somebody else runs — which is the shape key isolation
actually takes in production, where one long-lived sidecar serves several
services and none of them spawns it.

The example exercises the gate rather than describing it:

```
a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized
```

### `start()` waits for `/healthz` and that is the whole wait

openrate's sidecar needs a second readiness probe because it answers
`/healthz` while its first rate fetch is in flight. patala's has nothing to
warm up: `default_registry()` builds an offline `MockRail` and the server can
answer the moment it binds. There is no `/readyz` here and none is needed. The
wait is a 50 ms poll with a 20 s deadline.

### A non-200 raises

openrate's SDK returns the body whatever the status, because a rates lookup
that failed is merely unhelpful. This one throws, because a `404` or a `502`
body parsed as a Receipt is a Receipt whose fields happened to be absent.

The two answers that look like failures and are **not** exceptions:

- `verify` → `{"valid":false}` arrives as an ordinary `200`. It is the rail's
  fail-closed verdict. Gate entitlement on `true` and nothing else, and never
  retry a `false` as though it were transient.
- `validate-destination` → all five verdicts are `200`. **Read the body, not
  the status code.** Branch on `status` and `is_refusal`, and respect
  `human_must_confirm`, which is `true` on *every* verdict including
  `StructurallyValid` — patala does not detect exchange-owned addresses and
  will not guess.

### The default registry is mock-only

`patala-sidecar`'s `default_registry()` registers exactly one rail, `"mock"`.
Any other id is a `404` — not a failure of that rail, but a process that has
never heard of it. Per-rail registration is unwritten; everything around it —
the loopback bind, the fail-closed token gate, the error mapping, all six
endpoints — is real and tested. From the example:

```
an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
```

### Binary resolution

1. `PATALA_SIDECAR_BINARY`
2. a sibling `bin/patala-sidecar` next to the classes, or under `$PATALA_HOME`
3. `$PATALA_HOME/target/{release,debug}/patala-sidecar`
4. `patala-sidecar` on `PATH`

```sh
cargo build -p patala-sidecar --release
```

---

## The costs of the direct path

The honest list, and it is much shorter than llmux's or openrate's. What is
**not** on it, because it was measured and is not true here: replaced signal
handlers, fork-unsafety, a background thread, and a double-digit-megabyte
artifact.

1. **A C ABI, so JSON strings rather than types.** The direct path hands you
   documents; the Kotlin SDK's UniFFI evaluation was an attempt to get typed
   records instead, and [it did not survive contact with the
   compiler](../kotlin/README.md#the-uniffi-route-evaluated-and-measured).
2. **A lazily-initialised Tokio runtime per handle.** `patala-core`'s trait is
   `async`; a C caller has no event loop, so each handle owns a
   *current-thread* runtime and blocks on it. That is a real cost — the work
   happens on your calling thread and one handle's calls serialise — and it is
   the reason to open more than one handle if you want parallelism on the same
   rail.
3. **One executed platform.** See below.
4. **Latency is not the reason to embed.** No second process and no port is.
   For a payments call the network hop to loopback is not what you are
   optimising.

## Platforms

The **shared library**, direct path only:

| target | status |
|---|---|
| darwin/arm64 | **built and executed.** 844,656 bytes, `--release`. Everything on this page ran on it. |
| darwin/amd64 | **not built here.** |
| linux/amd64 | **not built here.** |
| linux/arm64 | **not built here.** |
| **windows/amd64** | **built nowhere. No DLL exists.** |

Only darwin/arm64 was produced in this work, so only that row claims anything.
`cargo build -p patala-ffi --release` is the whole build — there is no
cross-compile script in this repo to point you at, and pretending otherwise
would be worse than a short table. `PatalaDirect.findLibrary()` says so in its
error message rather than throwing a bare loader error.

For scale: 844,656 bytes against llmux's `libllmux.dylib` at 12,787,504 bytes,
measured on this machine on the same day.

The **sidecar** has no such matrix — it is an ordinary Rust binary and
`cargo build -p patala-sidecar` produces one wherever Rust runs, Windows
included.

## Toolchain this was built and run on

- OpenJDK **26.0.2** (Homebrew), darwin/arm64. Not on `PATH` on this machine,
  so both scripts fall back to `$JAVA_HOME/bin` — a JDK you can only reach
  through `JAVA_HOME` is not "no JDK".
- Maven **3.9.16** (for `pom.xml`; `run-examples.sh` uses plain `javac`)
- Rust **1.97.1**, cargo **1.97.1**, patala **0.1.0**

`mvn -o clean compile` was run: it produces class-file major 55 (Java 11) for
`Patala` and major 66 (Java 22) for `PatalaDirect`, from the one source tree.

## Layout

```
sdks/java/
  src/main/java/org/vulos/patala/Patala.java          sidecar (Java 11+)
  src/main/java/org/vulos/patala/PatalaDirect.java    direct, FFM (Java 22+)
  src/main/java/org/vulos/patala/PatalaException.java
  src/main/java/org/vulos/patala/Json.java            the only JSON this SDK writes
  examples/DirectCharge.java     runnable — offline, MockRail
  examples/SidecarCharge.java    runnable — loopback only, MockRail
  tools/SignalHandlerProbe.java  the evidence for this README
  run-examples.sh
  signal-probe.sh
  pom.xml
```

## Real output

`sdks/java/run-examples.sh`, verbatim:

```
run-examples: JDK 26 (openjdk version "26.0.2" 2026-07-21)
run-examples: library 844656 bytes at /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
run-examples: compiled

================ DirectCharge (in-process, C ABI) ================
library: /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
         844656 bytes
abi version: 0.1.0
abi check against 0.1.0: ok
id:           {"rail_id":"mock"}
capabilities: {"class":"NonCustodialFinal","reversible":false,"requires_kyc":false,"holds_funds":false,"currencies":["USDC"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight --
  "mock:wallet:alice" -> StructurallyValid, is_refusal=false, human_must_confirm=true
  "eth:wallet:alice" -> WrongNetwork, is_refusal=true, human_must_confirm=true
  "" (empty) -> Malformed, is_refusal=true, human_must_confirm=true

-- quote -> charge -> verify --
  quote:   {"rail_id":"mock","amount_minor":1250,"currency":"USDC","fee_minor":25,"total_minor":1275,"settlement":"Instant","expires_at_unix":1786313207}
  receipt: {"rail_id":"mock","amount_minor":1250,"currency":"USDC","reference":"order-4711","proof":[121,219,...],"settled_at_unix":1786312907}
  verify:  {"valid":true}
  verify(tampered amount): {"valid":false}

-- what this rail refuses to pretend --
  webhook:  patala_call(webhook): patala: verify_webhook is not supported by this rail
  providers: patala_call(providers): patala: this libpatala_ffi was built without --features fiat, so the "fiat" rail is not available in it.
  unknown method: patala_call(settle-later): patala: unknown method "settle-later" (want one of: id, capabilities, quote, charge, verify, validate-destination, webhook, caveat, providers)

after close: this rail is closed

DirectCharge: OK — offline, no socket opened, no thread started.

================ SidecarCharge (child process, HTTP) =============
INFO patala_sidecar: patala-sidecar listening on 127.0.0.1:55651 (loopback only)
sidecar:  http://127.0.0.1:55651 (loopback, hardcoded in the server)
healthz:  ok   <- the one unauthenticated route
caps:     {"class":"NonCustodialFinal","reversible":false,...,"currencies":["USDC","USD"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight (POST /v1/rails/mock/validate-destination) --
  "mock:wallet:alice" -> StructurallyValid, is_refusal=false
  "eth:wallet:alice" -> WrongNetwork, is_refusal=true
  "" (empty) -> Malformed, is_refusal=true

-- quote -> charge -> verify --
  verify:  {"valid":true}   <- 200; this, not the charge, is entitlement
  verify(tampered amount): {"valid":false}   <- also 200. false is data, not an HTTP error.

-- what this server refuses to pretend --
  webhook (mock has no push delivery): POST /v1/rails/mock/webhook: HTTP 501 {"error":"verify_webhook is not supported by this rail","kind":"unsupported"}
  an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
  a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized

SidecarCharge: OK — child process stopped.

run-examples: OK
```
