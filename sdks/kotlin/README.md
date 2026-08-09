# patala (Kotlin)

Idiomatic Kotlin over the [Java SDK](../java): `use {}`, named arguments,
default parameters, and typed helpers instead of hand-built JSON.

| | type | what it is | recommended? |
|---|---|---|---|
| **Direct** | `Patala.mock()` → `PatalaRail` | loads `libpatala_ffi` into this JVM | **yes** |
| **Sidecar** | `PatalaSidecar` | spawns `patala-sidecar` on `127.0.0.1`, talks HTTP | when you want **key isolation**, or you ship to Windows |

**If you have used llmux's or openrate's Kotlin SDK, the recommendation is
reversed here.** Theirs prefer the sidecar because loading a Go `c-shared`
library replaces five of the JVM's signal handlers. patala is Rust; it replaces
none. That was measured, not assumed —
[`sdks/java/README.md`](../java/README.md#the-jvm-and-patalas-shared-library)
has the probe output for both libraries, taken on the same machine and JDK in
the same session.

```sh
sdks/kotlin/run-examples.sh            # both
sdks/kotlin/run-examples.sh direct     # offline
sdks/kotlin/run-examples.sh sidecar    # loopback only; still offline
sdks/kotlin/uniffi-kotlin-probe.sh     # why this SDK is not generated UniFFI
```

Both examples drive `MockRail`. patala is a payments library, and an example
that moves real value is not an example.

---

## The UniFFI route: evaluated, and measured

This is the first question a reader should have. patala has a UniFFI surface —
`patala-uniffi`, namespace `patala` — and **UniFFI has a first-class Kotlin
backend**, which `patala-py` and `patala-go` are both generated from. Generated
Kotlin would give real `PayRequest`, `Quote`, `Receipt`, `RailCapabilities`,
`DestinationVerdict` and `WebhookEvent` types instead of JSON strings. That is
a genuinely better SDK, and it is what this one should have been.

**It does not compile.**

`sdks/kotlin/uniffi-kotlin-probe.sh` is that sentence, executable. It builds
`patala-uniffi`'s cdylib, generates the bindings with the bindgen this
workspace already pins (`cargo run -p patala-uniffi --bin uniffi-bindgen` —
uniffi **0.29.5**, resolved from the workspace `Cargo.lock`, so nothing here
can drift from the scaffolding the cdylib carries), fetches JNA, and hands the
result to `kotlinc`. Its real output:

```
uniffi-probe: cdylib 881696 bytes, uniffi v0.29.5
uniffi-probe: generated 2758 lines at uniffi/patala/patala.kt
uniffi-probe: jna jna-5.14.0.jar
uniffi-probe: compiling with info: kotlinc-jvm 2.4.10 (JRE 26.0.2)…

uniffi-probe: the generated Kotlin did NOT compile — 12 error(s).

patala.kt:2357:13: error: conflicting declarations:
patala.kt:2357:13: error: 'message' hides member of supertype 'Throwable' and needs an 'override' modifier.
patala.kt:2359:22: error: conflicting declarations:
patala.kt:2360:33: error: overload resolution ambiguity between candidates:
patala.kt:2365:13: error: conflicting declarations:
patala.kt:2365:13: error: 'message' hides member of supertype 'Throwable' and needs an 'override' modifier.
patala.kt:2367:22: error: conflicting declarations:
patala.kt:2368:33: error: overload resolution ambiguity between candidates:
patala.kt:2431:59: error: overload resolution ambiguity between candidates:
patala.kt:2436:59: error: overload resolution ambiguity between candidates:
patala.kt:2460:48: error: overload resolution ambiguity between candidates:
patala.kt:2465:48: error: overload resolution ambiguity between candidates:
```

### The cause, exactly

`patala_core::Error` has two variants carrying a field called `message`, and
`patala-uniffi` mirrors them:

```rust
#[error("rail error: {message}")]
Rail { message: String },
#[error("invalid request: {message}")]
InvalidRequest { message: String },
```

UniFFI's Kotlin backend renders an error enum as a sealed class extending
`kotlin.Exception`, and each variant as a subclass whose fields are constructor
`val`s — plus an `override val message` it synthesises for the display text.
So for these two variants it emits a class declaring `message` twice:

```kotlin
class Rail(
    val `message`: kotlin.String
) : PatalaException() {
    override val message
        get() = "message=${ `message` }"
}
```

Two declarations of the same name in one class body. That is a duplicate
declaration in any Kotlin version, not a strictness change — verified against
`-language-version 2.0` and `2.1`, the only ones kotlinc 2.4.10 still accepts,
which fail identically. It is a UniFFI codegen bug that fires whenever an error
variant has a field named `message`, and patala has two.

### Why it was not worked around here

The two available fixes both live outside this directory:

- rename the field in `patala-uniffi` (which every other binding's public API
  would follow), or
- fix the generator upstream.

The third option — post-processing the generated file with `sed` in a build
step — would work today and would silently rename part of the public API of a
payments SDK, in a script, downstream of the tool that is supposed to own that
API. That is a worse artifact than a JSON-string binding with an honest README.

**The probe's exit code is inverted so this page cannot rot.** It exits `0`
when the compile fails as documented, and `1` when it succeeds — because a
README that quotes a compiler error is only honest while the error is still
there.

### And even if it compiled, JNA is a real cost

UniFFI Kotlin is a `com.sun.jna.Library`. JNA ships its own native stub per
platform, so adopting it in order to load a native library means shipping *two*
native artifacts to solve the problem of shipping one — and `libpatala_ffi` is
844,656 bytes, which JNA's own per-platform payload is not far off. That is not
by itself a reason to reject UniFFI Kotlin, whose typed records are worth
paying something for. It is a reason the choice was close before the compiler
settled it.

### So: what this SDK actually is

A thin Kotlin layer over `sdks/java`'s FFM binding. The FFM calls, the memory
rules and the handle lifecycle stay in the Java classes, because two bindings
to one C ABI is two places for a use-after-free. What Kotlin adds is
`use {}`, default arguments, `Patala.mock(...)` instead of a hand-written
config document, `payRequest(...)` instead of hand-written JSON, and two
`Boolean` accessors — `isValid` and `isRefusal` — whose fail-closed behaviour
is the point of them.

---

## Direct — the recommended default

```kotlin
Patala.mock(feeMinor = 25).use { rail ->
    rail.abiCheck()

    val verdict = rail.validateDestination("mock:wallet:alice")
    if (rail.isRefusal(verdict)) return

    val receipt = rail.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711"))
    check(rail.isValid(receipt))          // the entitlement check
}
```

- **Opening a rail talks to nothing** — no socket, no thread, no environment
  variable. Only a call reaches a network, and only for a rail that has one.
  `Patala.mock()` cannot reach one at all.
- Unknown configuration fields are **refused**: a misspelled `"currencys"` is
  an error, not a rail quietly built with a currency list you did not choose.
- `abiCheck()` goes through `patala_abi_check`, so the version comparison lives
  in the library rather than being reimplemented — and forgotten — per binding.
- Handles are **retired, not recycled**, so use-after-close is a clean error:

  ```
  after close: this rail is closed
  ```

Requires **Java 22+** (the underlying binding is `java.lang.foreign`) and
`--enable-native-access=ALL-UNNAMED`. Worked example:
[`examples/DirectCharge.kt`](examples/DirectCharge.kt).

### `isValid` and `isRefusal` fail closed, and that is the whole design

```kotlin
public fun isValid(receiptJson: String): Boolean = verify(receiptJson).trim() == "{\"valid\":true}"
```

An exact match on the library's own answer, so an unrecognised third answer is
`false`. `verify` returning `false` is patala's honest verdict that a receipt
does not hold — **not** a transient failure to retry, and never an exception,
because an exception is too easy to swallow.

```kotlin
public fun isRefusal(verdictJson: String): Boolean = Json.field(verdictJson, "is_refusal") == "true"
```

Read from the document's own `is_refusal` rather than re-derived from `status`.
A re-derivation falls through to its default for any status added later, and
that default is "not a refusal" — failing open, on the one question in this API
where failing open loses money. `is_refusal` exists in the JSON for exactly
this reason: it is a *method* on the Rust type, and a method does not survive
JSON.

And there is a third state that is neither: `Unknown`. From the example's real
output, using a rail configured without destination checks — the offline
stand-in for a fiat rail, whose destination is an opaque processor-side token:

```
  "mock:wallet:alice" -> StructurallyValid, isRefusal=false, human_must_confirm=true
  "eth:wallet:alice" -> WrongNetwork, isRefusal=true, human_must_confirm=true
  "" (empty) -> Malformed, isRefusal=true, human_must_confirm=true
  the same address on a rail that cannot check: Unknown, isRefusal=false
  Unknown is NOT a refusal and is NOT an approval. It needs a human.
```

`human_must_confirm` is `true` on **every** verdict, `StructurallyValid`
included, because patala does not detect exchange-owned addresses and will not
guess.

### `payRequest` takes a `Long`, and that is not an accident

```kotlin
public fun payRequest(amountMinor: Long, currency: String, destination: String, reference: String): String
```

`amountMinor` is an integer number of minor units. patala never puts a float on
either side of the boundary, and a Kotlin helper that took a `Double` would be
where the rounding bug got in. It returns a `String` rather than taking a data
class through a JSON library for the same reason: your JSON library's default
number handling is not this SDK's business, and a `Receipt` decoded with
`amount_minor` as a `Double` is a payments bug that type-checks.

### No streaming, and therefore no coroutines dependency

There is no `patala_stream` — a quote, a charge, a verification and a
destination check are each one question with one answer — and there is no
`Flow` here.

llmux's Kotlin SDK does depend on `kotlinx-coroutines-core`, because chat
streaming genuinely needs `Flow`. **This SDK depends on nothing but
`kotlin-stdlib`.** If you are calling from a coroutine, wrap the call in
`Dispatchers.IO` at the call site — one line, no dependency.

That matters slightly more here than in openrate: a patala handle owns a
*current-thread* Tokio runtime, so the work genuinely happens on your calling
thread. A `suspend` in front of it would hide where the blocking went rather
than remove it.

---

## Sidecar — for key isolation

```kotlin
PatalaSidecar().use { patala ->
    val receipt = patala.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711"))
    check(patala.isValid(receipt))
}
```

`use {}` stops the child process on every path out. Runs on **Java 11+** with
no native library, no `--enable-native-access` and no platform matrix —
including on Windows, where the direct path does not exist.

**The reason to choose it is key isolation.** A non-custodial rail's signing
key lives inside whichever process calls `charge`; route five services through
one sidecar and it lives in one narrow process instead of five.
`patala-sidecar/README.md` carries the threat model, including what it does not
defend against: a co-resident, same-privilege attacker can read the token out
of the environment.

The token is mandatory — the server refuses to start without
`PATALA_SIDECAR_TOKEN`, with no unauthenticated fallback — so the constructor
mints 32 bytes from `SecureRandom`. Pass `token` and `port`, or use
`PatalaSidecar.attach(baseUrl, token)`, to talk to a sidecar somebody else
runs, which is the shape key isolation actually takes in production.

From the example, which exercises the gate rather than describing it:

```
  a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized
```

Everything that is not a `200` raises. The two answers that look like failures
and are **not** exceptions are `verify` → `{"valid":false}` and every
`validate-destination` verdict, both HTTP `200`, both data.

Worked example: [`examples/SidecarCharge.kt`](examples/SidecarCharge.kt).

---

## Platforms

Same table as [the Java SDK](../java/README.md#platforms), because it is the
same shared library. Short version: **darwin/arm64 is the only target built and
executed** (844,656 bytes, `--release`), and **there is no Windows DLL**. The
sidecar has no matrix — it is an ordinary Rust binary.

## Build

There is **no `build.gradle.kts`**, on purpose. Nothing in this repo runs
Gradle, so a build file would be an unexecuted claim about how the module
builds, and a check nobody runs is worse than no check. `run-examples.sh`
drives `kotlinc` directly and is run for real.

## Toolchain this was built and run on

- OpenJDK **26.0.2** (Homebrew), darwin/arm64. Not on `PATH` on this machine,
  so the scripts fall back to `$JAVA_HOME/bin`.
- Kotlin **2.4.10** (`kotlinc-jvm`), `-jvm-target 22`
- Rust **1.97.1**, patala **0.1.0**
- uniffi **0.29.5** and JNA **5.14.0**, for the probe only — neither is a
  dependency of this SDK

`-jvm-target 22` is a floor, not a preference: `org.vulos.patala.PatalaDirect`
is a Java 22 class file and kotlinc must be able to read it.

## Layout

```
sdks/kotlin/
  src/main/kotlin/org/vulos/patala/kotlin/Direct.kt    Patala.mock(), PatalaRail, payRequest()
  src/main/kotlin/org/vulos/patala/kotlin/Sidecar.kt   PatalaSidecar
  examples/DirectCharge.kt     runnable — offline, MockRail
  examples/SidecarCharge.kt    runnable — loopback only, MockRail
  run-examples.sh
  uniffi-kotlin-probe.sh       the evidence for the build decision above
```

## Real output

`sdks/kotlin/run-examples.sh direct`, verbatim:

```
run-examples: JDK 26, info: kotlinc-jvm 2.4.10 (JRE 26.0.2)
run-examples: library 844656 bytes
run-examples: compiled

================ DirectCharge (in-process, C ABI) ================
library: /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
         844656 bytes
abi version: 0.1.0 (checked by the library, not by us)
id:           {"rail_id":"mock"}
capabilities: {"class":"NonCustodialFinal","reversible":false,"requires_kyc":false,"holds_funds":false,"currencies":["USDC"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight --
  "mock:wallet:alice" -> StructurallyValid, isRefusal=false, human_must_confirm=true
  "eth:wallet:alice" -> WrongNetwork, isRefusal=true, human_must_confirm=true
  "" (empty) -> Malformed, isRefusal=true, human_must_confirm=true
  the same address on a rail that cannot check: Unknown, isRefusal=false

-- quote -> charge -> verify --
  quote:   {"rail_id":"mock","amount_minor":1250,"currency":"USDC","fee_minor":25,"total_minor":1275,"settlement":"Instant","expires_at_unix":1786313639}
  receipt: {"rail_id":"mock","amount_minor":1250,"currency":"USDC","reference":"order-4711","proof":[35,172,...],"settled_at_unix":1786313339}
  isValid(receipt):  true
  isValid(tampered): false   <- an ordinary result, not an exception

-- what this rail refuses to pretend --
  webhook:  patala_call(webhook): patala: verify_webhook is not supported by this rail
  unknown:  patala_call(settle-later): patala: unknown method "settle-later" (want one of: id, capabilities, quote, charge, verify, validate-destination, webhook, caveat, providers)
  a failing rail: patala_call(charge): patala: rail error: mock rail mock is configured to fail

after close: this rail is closed

DirectCharge: OK — offline, no socket opened, no thread started.
```

`sdks/kotlin/run-examples.sh sidecar`, verbatim:

```
================ SidecarCharge (child process, HTTP) =============
INFO patala_sidecar: patala-sidecar listening on 127.0.0.1:59132 (loopback only)
sidecar:  http://127.0.0.1:59132 (loopback, hardcoded in the server)
healthz:  ok   <- the one unauthenticated route
caps:     {"class":"NonCustodialFinal",...,"currencies":["USDC","USD"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight (POST /v1/rails/mock/validate-destination) --
  "mock:wallet:alice" -> StructurallyValid, isRefusal=false
  "eth:wallet:alice" -> WrongNetwork, isRefusal=true
  "" (empty) -> Malformed, isRefusal=true

-- quote -> charge -> verify --
  isValid(receipt):  true   <- 200; this, not the charge, is entitlement
  isValid(tampered): false   <- also 200. false is data, not an HTTP error.

-- what this server refuses to pretend --
  webhook (mock has no push delivery): POST /v1/rails/mock/webhook: HTTP 501 {"error":"verify_webhook is not supported by this rail","kind":"unsupported"}
  an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
  a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized

SidecarCharge: OK — child process stopped.

run-examples: OK
```
