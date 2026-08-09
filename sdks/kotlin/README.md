# patala (Kotlin)

**Generated UniFFI bindings**, plus a thin idiomatic layer: real
`PayRequest`, `Quote`, `Receipt`, `RailCapabilities`, `DestinationVerdict` and
`WebhookEvent` types, a `PatalaError` you can `when` over, and no JSON string
anywhere on the direct path.

| | type | what it is | recommended? |
|---|---|---|---|
| **Direct** | `Patala.mock()` → `uniffi.patala.PatalaRail` | generated UniFFI, loads `libpatala_uniffi` into this JVM | **yes** |
| **Sidecar** | `PatalaSidecar` | spawns `patala-sidecar` on `127.0.0.1`, talks HTTP | when you want **key isolation**, or you ship to Windows |

**If you have used llmux's or openrate's Kotlin SDK, the recommendation is
reversed here.** Theirs prefer the sidecar because loading a Go `c-shared`
library replaces five of the JVM's signal handlers. patala is Rust; it replaces
none. That was measured, not assumed —
[`sdks/java/README.md`](../java/README.md#the-jvm-and-patalas-shared-library)
has the probe output for both libraries, taken on the same machine and JDK in
the same session.

```sh
make -C sdks/kotlin check        # generate + compile + run + checks + probe
make -C sdks/kotlin generate     # bindings only
sdks/kotlin/run-examples.sh      # both examples
sdks/kotlin/run-examples.sh direct
sdks/kotlin/run-examples.sh sidecar
sdks/kotlin/run-examples.sh checks
sdks/kotlin/uniffi-kotlin-probe.sh   # why patala's error field is `detail`
```

Both examples drive `MockRail`. patala is a payments library, and an example
that moves real value is not an example.

---

## What changed, and why this page was rewritten

This SDK used to be a thin Kotlin layer over [`sdks/java`](../java)'s C-ABI
binding, passing JSON strings across every call. This page used to explain,
at length and with pasted compiler output, that the alternative — generated
UniFFI Kotlin — **did not compile**:

`patala-uniffi`'s error enum had two variants carrying a field called
`message`. UniFFI's Kotlin backend renders an error enum as a sealed class
extending `kotlin.Exception` and synthesises an `override val message` for the
display text, so for those variants it emitted a class declaring `message`
twice. 12 kotlinc errors, on a duplicate declaration no Kotlin version accepts.

**The field was renamed to `detail`** (`patala-uniffi`, commit `79e5002`) and
the blocker is gone. The generated Kotlin compiles, and this SDK *is* that
generated Kotlin. The wrapper is deleted, not deprecated.

That is the correct trade for a payments SDK. A mistyped field in a JSON
document is a money bug that no compiler sees; the same mistake against a
generated record does not build. Concretely, these three lines used to be
Kotlin functions in this directory that re-parsed a document to recover
something the Rust type already knew, and are now just fields:

```kotlin
verdict.isRefusal          // was: Json.field(verdictJson, "is_refusal") == "true"
rail.verify(receipt)       // was: verify(receiptJson).trim() == "{\"valid\":true}"
caps.railClass             // was: a String compared against "NonCustodialFinal"
```

The last one is the one that matters most. `RailClass` is an enum with two
members, so a `when` over it is exhaustive with no `else` branch — a third rail
class added upstream stops your build. The `String` compare shipped and took
the wrong branch.

---

## Direct — the recommended default

```kotlin
import org.vulos.patala.kotlin.Patala
import org.vulos.patala.kotlin.payRequest

Patala.mock(feeMinor = 25).use { rail ->
    val verdict = rail.validateDestination("mock:wallet:alice")
    if (verdict.isRefusal) return

    val receipt = rail.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711"))
    check(rail.verify(receipt))          // the entitlement check
}
```

- **Opening a rail talks to nothing** — no socket, no thread, no environment
  variable. Only a call reaches a network, and only for a rail that has one.
  `Patala.mock()` cannot reach one at all.
- `use {}` releases it on every path out; the generated class is
  `AutoCloseable` and is also registered with a `java.lang.ref.Cleaner`, so a
  dropped rail is freed eventually either way.
- **Use-after-close is a clean error**, from the generated call counter:
  `PatalaRail object has already been destroyed`.
- **A stale library is caught at load.** There is no `abiCheck()` here any more
  and it is not missing: loading the cdylib checks the UniFFI contract version
  *and* a checksum per exported function against these bindings, and throws if
  either disagrees. That is strictly more than the C ABI's version-string
  comparison — it catches a library built from a *different shape* of the same
  version.

Worked example: [`examples/DirectCharge.kt`](examples/DirectCharge.kt).

### Three outcomes, not two

`verify` is the API worth arguing with, so it is spelled out:

| | means | what to do |
| --- | --- | --- |
| throws `PatalaException` | the check could not be performed | retry, alert |
| returns `false` | the rail checked, and the receipt does not hold | never retry, never grant |
| returns `true` | the entitlement | proceed |

`false` is patala's honest verdict, decided inside Rust, and it is a `Boolean`
rather than a document — there is no third answer for a caller to
mis-recognise, which is what the old `isValid` helper's exact string match
existed to defend against.

`validateDestination` never fails at all: "I cannot check this address" is the
verdict `DestinationStatus.UNKNOWN`, because a caller must handle it as
carefully as a refusal and an error is too easy to swallow. From the example's
real output:

```
  "mock:wallet:alice" -> STRUCTURALLY_VALID, isRefusal=false, human_must_confirm=true
  "eth:wallet:alice" -> WRONG_NETWORK, isRefusal=true, human_must_confirm=true
  "" (empty) -> MALFORMED, isRefusal=true, human_must_confirm=true
  the same address on a rail that cannot check: UNKNOWN, isRefusal=false
  UNKNOWN is NOT a refusal and is NOT an approval. It needs a human.
```

`humanMustConfirm` is `true` on **every** verdict, `STRUCTURALLY_VALID`
included, because patala does not detect exchange-owned addresses and will not
guess. `Patala.caveat()` is the sentence to show the human who must; it is also
on every verdict as `exchangeDepositCaveat`.

### What this SDK adds on top of the generated code, and no more

Everything money-shaped lives in the generated file. This directory is 200
lines of defaults and spelling:

| | |
|---|---|
| `Patala.mock(...)` | the five-argument generated constructor with Kotlin default arguments, and a `destinationChecks` flag picking between the two generated constructors |
| `payRequest(...)` | a `Long` amount, checked non-negative before it becomes the record's `ULong` |
| `Patala.useLibrary` / `findLibrary` | where the cdylib is loaded from — a build concern, not an API one |
| `caps.railClass` | `caps.\`class\`` without the backticks |
| `settlement.describe()` | `instant` instead of `Settlement$Instant@16c0663d` |
| `PayRequest.toJson()` | the sidecar path's wire format, and the only JSON left |

`payRequest` takes a `Long` and refuses a negative one because
`(-1L).toULong()` is 18446744073709551615 — a wrapped amount is a money bug
that type-checks. There is no `Double` overload and there will not be one.

### No streaming, and therefore no coroutines dependency

There is no `patala_stream` — a quote, a charge, a verification and a
destination check are each one question with one answer — and there is no
`Flow` here. llmux's Kotlin SDK does depend on `kotlinx-coroutines-core`,
because chat streaming genuinely needs it.

**This SDK depends on `kotlin-stdlib` and JNA.** If you are calling from a
coroutine, wrap the call in `Dispatchers.IO` at the call site — one line, no
dependency. A `suspend` in front of these calls would hide where the blocking
went rather than remove it.

### The JNA cost, stated plainly

Generated UniFFI Kotlin is a `com.sun.jna.Library`, so this SDK has a
dependency the wrapper did not: **JNA 5.14.0**, which ships its own native stub
per platform. You are shipping two native artifacts to solve the problem of
shipping one.

It is worth it here, and it was not obviously worth it before the compiler
settled the question: what you buy is that every value crossing the boundary is
a checked type rather than a string, in a library that moves money. It is also
why `sdks/java` still exists on the C ABI — `java.lang.foreign` needs no JNA,
and the sidecar client in this directory is still that Java code.

One consequence worth knowing: on **JDK 22+** the JVM warns when a library on
the classpath calls `System.load`, which is what JNA does.

```
WARNING: A restricted method in java.lang.System has been called
WARNING: java.lang.System::load has been called by com.sun.jna.Native in an unnamed module
```

`--enable-native-access=ALL-UNNAMED` silences it; `run-examples.sh` passes it
when the JDK is 22 or newer. It is a warning today and will be an error in a
future release, so pass it.

**The floor dropped, though.** The wrapper needed **Java 22+**, because
`sdks/java`'s direct binding is `java.lang.foreign`. JNA does not need that, so
everything here compiles at `-jvm-target 11` and runs on **Java 11+**.

---

## Sidecar — for key isolation, and unchanged

```kotlin
PatalaSidecar().use { patala ->
    val receipt = patala.charge(payRequest(1250, "USDC", "mock:wallet:alice", "order-4711").toJson())
    check(patala.isValid(receipt))
}
```

This path is deliberately **not** regenerated. An HTTP boundary carries JSON;
that is what it is. What changed is one line: you build the same typed
`PayRequest` the direct path takes and call `.toJson()` on it at the wire
boundary, instead of assembling a document by hand. Responses are still
strings, still read with `Json.field`, still going through
[`sdks/java`](../java)'s HTTP client — one client, not two.

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

## Reproducible generation

A generated binding nobody can regenerate identically is a liability, so
[`Makefile`](Makefile) follows the house pattern from
[`patala-go/Makefile`](../../patala-go/Makefile): pin the bindgen, refuse to
run without the toolchain, and **assert the shape of what came out**.

- **The bindgen is this workspace's own binary** — `cargo run -p patala-uniffi
  --bin uniffi-bindgen` — not a separately installed CLI, so unlike
  `patala-go` (which needs NordSecurity's `uniffi-bindgen-go` at a matching
  tag) nothing here can drift from the scaffolding compiled into the cdylib.
- **`UNIFFI_VERSION := 0.29.5` is asserted anyway**, against what `cargo tree`
  resolves. A `cargo update` that moves uniffi changes every generated binding
  in this repo; it should be a decision, not a surprise.
- **`JNA_VERSION := 5.14.0` lives in exactly one place.** `run-examples.sh`
  reads it back out of the Makefile rather than repeating it.
- **The package clause is asserted**: the output must be
  `bindings/uniffi/patala/patala.kt` declaring `package uniffi.patala`. Both
  are the UniFFI *namespace* — `uniffi::setup_scaffolding!("patala")` — so if
  someone drops it and the namespace reverts to the crate name, every import
  here breaks with an error that points at Kotlin rather than at the Rust crate
  that caused it. `make generate` fails first, naming the cause. This is the
  same assertion `patala-go/Makefile` makes on its `package patala` clause, for
  the same reason and after the same near-miss.
- **The typed surface is asserted**: `data class PayRequest`, `data class
  Receipt`, `data class DestinationVerdict`, `enum class RailClass` and
  `sealed class PatalaException` must all be present. Their absence is the
  whole reason not to ship this binding, so it is checked one line after
  generation rather than in kotlinc's output.
- **Nothing under `bindings/` is checked in.** It is build output, like
  `/target`, `patala-py/bindings/` and `patala-go/bindings/`.

```
$ make -C sdks/kotlin generate
generate: uniffi 0.29.5 (pinned)
generate: OK — package uniffi.patala, 2758 lines, typed surface present
```

### Why the error field is called `detail`

`PatalaError::Rail { detail }` and `PatalaError::InvalidRequest { detail }`
would read better as `message`. They are not, and the only reason is a UniFFI
Kotlin codegen bug: an error variant with a field named `message` produces a
class that declares `message` twice, because the backend also synthesises an
`override val message`.

A constraint that lives in a commit message is a constraint the next person
re-litigates, so it lives in
[`uniffi-kotlin-probe.sh`](uniffi-kotlin-probe.sh) instead, which reproduces
the bug from a six-line UDL that has nothing to do with patala — no cdylib, no
cargo build. **Its exit code is inverted**: `0` while the bug is still there,
`1` when it is fixed and the rename can be reconsidered, `2` if the probe
itself could not run. It compiles a **control** case (the same UDL with a
`detail` field) first, because a probe whose only outcome is "kotlinc said no"
cannot tell a codegen bug from a missing jar.

```
$ sdks/kotlin/uniffi-kotlin-probe.sh
uniffi-probe: uniffi v0.29.5, info: kotlinc-jvm 2.4.10 (JRE 26.0.2)
uniffi-probe: jna jna-5.14.0.jar

uniffi-probe: control — an error variant with a `detail` field…
uniffi-probe: control COMPILED, as it must. The toolchain is sound.

uniffi-probe: subject — an error variant with a `message` field…

uniffi-probe: the `message` case did NOT compile — 6 error(s).
This is the documented state, and it is why patala-uniffi's error
variants carry `detail`. README.md quotes it.

subject.kt:1044:13: error: conflicting declarations:
subject.kt:1044:13: error: 'message' hides member of supertype 'Throwable' and needs an 'override' modifier.
subject.kt:1046:22: error: conflicting declarations:
subject.kt:1047:33: error: overload resolution ambiguity between candidates:
subject.kt:1078:59: error: overload resolution ambiguity between candidates:
subject.kt:1087:48: error: overload resolution ambiguity between candidates:

uniffi-probe: OK (expected failure reproduced; control compiled)
```

The probe's earlier subject — patala's own generated Kotlin — is gone because
it compiles now. The probe is not, because the constraint is not.

---

## Checks, not tests

There is **no `kotlin.test` here and no `build.gradle.kts`**, on purpose.
Nothing in this repo runs Gradle, so a build file would be an unexecuted claim
about how the module builds and a test source set would be a file nobody had
ever seen pass. `run-examples.sh` drives `kotlinc` directly and is run for
real.

So the assertions are an ordinary program, [`checks/Checks.kt`](checks/Checks.kt),
which ends by asserting the **number** of checks that ran — the same discipline
as `patala-ffi/ctest/smoke.c` and the Swift package's `patala-checks`, and for
the same reason: a suite that silently stops executing half of itself must fail
rather than pass.

```
$ make -C sdks/kotlin checks
patala Kotlin checks (generated UniFFI bindings)
  library: /Users/pc/code/vulos/patala/target/release/libpatala_uniffi.dylib
  ok   RailClass has exactly 2 variants
  ok   DestinationStatus has exactly 5 variants
  ok   WebhookStatus has exactly 3 variants
  ok   UNCONFIRMED is a distinct WebhookStatus, never SETTLED
  ...
  ok   verify() is false for a tampered amount
  ok   verify() is false for a tampered currency
  ok   verify() is false for a tampered reference
  ok   verify() is false for a tampered proof
  ok   the mock rail reports webhook verification Unsupported
  ok   an unsupported currency is InvalidRequest, with a detail
  ok   a rail that cannot check answers UNKNOWN
  ok   UNKNOWN is not a refusal — it needs a human
  ok   UNKNOWN still requires a human
  ok   payRequest refuses a negative amount rather than wrapping it
  ok   use-after-close is an error, not a crash

34 checks ran, 0 failed (expected 34)
PASS
```

The four tamper checks are `receipt.copy(...)` on a data class — the mutation a
real bug would make, not a mangled string. That is a check the JSON binding
could not write.

**Mutation-tested.** A green suite means nothing until you have seen it go red.
Deleting `payRequest`'s non-negative guard — the exact bug that would send
`(-1L).toULong()` minor units — produces:

```
  FAIL payRequest refuses a negative amount rather than wrapping it

34 checks ran, 1 failed (expected 34)
FAIL: 1 check(s) failed
```

and the count assertion was seen to fire for real while this suite was being
written, when 34 checks ran against a stale `EXPECTED_CHECKS = 26`:

```
34 checks ran, 0 failed (expected 26)
FAIL: 34 checks ran, expected 26
```

---

## Platforms

Same table as [the Java SDK](../java/README.md#platforms) in shape, but the
library is a different one: this SDK loads **`libpatala_uniffi`** (881,696
bytes, `--release`), not `libpatala_ffi`. Short version: **darwin/arm64 is the
only target built and executed**, and **there is no Windows DLL**. The sidecar
has no matrix — it is an ordinary Rust binary.

## Toolchain this was built and run on

- OpenJDK **26.0.2** (Homebrew), darwin/arm64. Not on `PATH` on this machine —
  and macOS's `/usr/bin/java` is a *stub* that exists and fails, so
  [`lib.sh`](lib.sh) runs each candidate JDK before believing it.
- Kotlin **2.4.10** (`kotlinc-jvm`), `-jvm-target 11`
- Rust **1.97.1**, patala **0.1.0**
- uniffi **0.29.5**, JNA **5.14.0** — both now real dependencies of this SDK,
  not probe-only

## Layout

```
sdks/kotlin/
  Makefile                     generate (pinned + asserted), build, run, checks, probe
  lib.sh                       JDK / kotlin-stdlib / JNA discovery, shared by the scripts
  run-examples.sh              compile and run: direct, sidecar, checks
  uniffi-kotlin-probe.sh       the inverted probe behind the `detail` field name
  bindings/                    GENERATED, gitignored — uniffi/patala/patala.kt
  src/main/kotlin/org/vulos/patala/kotlin/
      Direct.kt                Patala.mock(), payRequest(), railClass, describe()
      Sidecar.kt               PatalaSidecar, PayRequest.toJson()
  examples/DirectCharge.kt     runnable — offline, MockRail, typed end to end
  examples/SidecarCharge.kt    runnable — loopback only, MockRail
  checks/Checks.kt             34 counted assertions
```

## Real output

`sdks/kotlin/run-examples.sh direct`, verbatim (the `make generate` lines it
starts with are elided):

```
run-examples: JDK 26 (/opt/homebrew/opt/openjdk/bin), info: kotlinc-jvm 2.4.10 (JRE 26.0.2)
run-examples: cdylib 881696 bytes
run-examples: jna jna-5.14.0.jar
run-examples: compiled (generated bindings + SDK + examples + checks)

================ DirectCharge (in-process, generated UniFFI) ======
library: /Users/pc/code/vulos/patala/target/release/libpatala_uniffi.dylib
         881696 bytes
loading it also checks the UniFFI contract version and every
function checksum against these bindings — a stale cdylib throws here.
id:           mock
capabilities: NON_CUSTODIAL_FINAL settlement=instant
              reversible=false holds_funds=false
              currencies=[USDC]
              -> wallet address, signed final receipt, no reversal

-- destination pre-flight --
  "mock:wallet:alice" -> STRUCTURALLY_VALID, isRefusal=false, human_must_confirm=true
  "eth:wallet:alice" -> WRONG_NETWORK, isRefusal=true, human_must_confirm=true
  "" (empty) -> MALFORMED, isRefusal=true, human_must_confirm=true
  human_must_confirm is true on EVERY verdict, STRUCTURALLY_VALID included.
  patala does not detect exchange-owned addresses and will not guess.
  the same address on a rail that cannot check: UNKNOWN, isRefusal=false
  UNKNOWN is NOT a refusal and is NOT an approval. It needs a human.

-- quote -> charge -> verify --
  quote:   1250 + 25 fee = 1275 minor units of USDC, instant
  receipt: 1250 USDC ref=order-4711 proof=32B issued by mock
  verify(receipt):  true
  verify(tampered): false   <- returned, not thrown

-- what this rail refuses to pretend --
  wrong currency: InvalidRequest: rail mock does not support currency EUR
  webhook:        Unsupported(verify_webhook) — this rail has no such thing
  a failing rail: Rail: mock rail mock is configured to fail

after close: PatalaRail object has already been destroyed

DirectCharge: OK — offline, no socket opened, MockRail only.
```

`sdks/kotlin/run-examples.sh sidecar`, verbatim:

```
================ SidecarCharge (child process, HTTP) ==============
INFO patala_sidecar: patala-sidecar listening on 127.0.0.1:53843 (loopback only)
sidecar:  http://127.0.0.1:53843 (loopback, hardcoded in the server)
healthz:  ok   <- the one unauthenticated route
caps:     {"class":"NonCustodialFinal","reversible":false,"requires_kyc":false,"holds_funds":false,"currencies":["USDC","USD"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight (POST /v1/rails/mock/validate-destination) --
  "mock:wallet:alice" -> StructurallyValid, isRefusal=false
  "eth:wallet:alice" -> WrongNetwork, isRefusal=true
  "" (empty) -> Malformed, isRefusal=true
  all five verdicts are HTTP 200. A 200 means the rail ANSWERED,
  not that the address is good — read the body, not the status.

-- quote -> charge -> verify --
  quote:   {"rail_id":"mock","amount_minor":1250,"currency":"USDC","fee_minor":0,"total_minor":1250,"settlement":"Instant","expires_at_unix":1786316349}
  receipt: {"rail_id":"mock","amount_minor":1250,"currency":"USDC","reference":"order-4711","proof":[97,98,...],"settled_at_unix":1786316049}
  isValid(receipt):  true   <- 200; this, not the charge, is entitlement
  isValid(tampered): false   <- also 200. false is data, not an HTTP error.

-- what this server refuses to pretend --
  webhook (mock has no push delivery): POST /v1/rails/mock/webhook: HTTP 501 {"error":"verify_webhook is not supported by this rail","kind":"unsupported"}
  an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
  a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized

SidecarCharge: OK — child process stopped.

run-examples: OK
```

## See also

- [`sdks/java`](../java/) — the C-ABI binding this SDK used to wrap, still the
  home of the sidecar HTTP client and still `java.lang.foreign` with no JNA.
- [`sdks/swift/uniffi`](../swift/uniffi/) — the same generated surface in
  Swift, alongside that package's dlopen binding.
- [`patala-uniffi/README.md`](../../patala-uniffi/) — the one
  `#[uniffi::export]` surface every generated binding comes from.
- [`patala-go/Makefile`](../../patala-go/Makefile) — the house pattern this
  directory's Makefile follows.
