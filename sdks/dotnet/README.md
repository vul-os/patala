# patala (.NET)

Two ways to run patala from C#, both supported:

| | type | what it is | recommended? |
|---|---|---|---|
| **Sidecar** | `Patala.Sidecar` | spawns `patala-sidecar` on `127.0.0.1`, talks HTTP | **yes — for .NET** |
| **Direct** | `Patala.Direct` → `PatalaRail` | loads `libpatala_ffi` into this process | when you are not shipping to Windows |

**For .NET the sidecar is the recommended default, and the deciding reason is
platform coverage.** patala's shared library has been built and run *from .NET*
on **exactly one target** — darwin/arm64 (CI also builds the linux/amd64 `.so`
and smoke-tests it from C, but no .NET has run there) — and there is no Windows
DLL at all. .NET has a
very large Windows install base; a direct-mode dependency would simply not load
for a large fraction of the people who took it.

**Note what is *not* a reason here.** llmux's and openrate's .NET SDKs warn
about the Go runtime in your process — its GC, its scheduler, its signal
handlers, its fork-unsafety. patala is Rust and carries none of that. The
[Java SDK](../java/README.md#the-jvm-and-patalas-shared-library) measured it and
[the Java SDK's recommendation is the reverse of llmux's because of
it](#what-was-measured-and-what-was-not). .NET's answer stays "sidecar" on a
completely different ground: there is no DLL to load on Windows.

```sh
sdks/dotnet/run-examples.sh            # both
sdks/dotnet/run-examples.sh direct     # offline
sdks/dotnet/run-examples.sh sidecar    # loopback only; still offline
```

Both examples drive `MockRail`. patala is a payments library, and an example
that moves real value is not an example.

---

## Sidecar — `Patala.Sidecar`

```csharp
using Patala;

using var patala = Sidecar.Start();

string receipt = await patala.ChargeAsync(
    Json.PayRequest(1250, "USDC", "mock:wallet:alice", "order-4711"));

bool paid = await patala.IsValidAsync(receipt);    // the entitlement check
```

`using` stops the child process on every path out. No native library, no
platform matrix — it works on Windows.

Like openrate's and unlike llmux's .NET SDK, this is **not a process-wide
singleton**: you get an instance you own and dispose. Two sidecars with
different rail sets is a normal thing to want.

Worked example: [`examples/SidecarCharge.cs`](examples/SidecarCharge.cs).

### The reason to reach for it, beyond Windows: key isolation

A non-custodial rail's signing key lives inside whichever process calls
`charge`. Link the direct path into five services and that key is smeared
across five processes' memory, so a bug or a dependency-confusion attack in any
one of them is a path to it. Route them all through one sidecar and it lives in
exactly one narrow, purpose-built process.

`patala-sidecar/README.md` carries the full threat model, including what it
does **not** defend against: a co-resident, same-privilege attacker can read
the token out of the environment. Loopback-plus-token raises the bar above "any
process on the LAN", not above "a fully co-resident attacker".

### The token is mandatory, and `Start()` mints one

`patala-sidecar` refuses to start without `PATALA_SIDECAR_TOKEN` — no
auto-generated fallback inside the server, no "runs unauthenticated if you
forget" path. So `Start()` mints 32 bytes from `RandomNumberGenerator`, passes
them to the child, and sends `Authorization: Bearer` on every `/v1` request.
`/healthz` is the one unauthenticated route and reveals nothing but liveness.

`Options.Token` + `Options.Port`, or `Sidecar.Attach(baseUrl, token)`, talk to a
sidecar somebody else runs — the shape key isolation actually takes in
production. From the example, exercising the gate rather than describing it:

```
a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized
```

### `Start()` waits for `/healthz`, and that is the whole wait

openrate's .NET SDK needs a second readiness probe because its server answers
`/healthz` while its first rate fetch is in flight. patala's has nothing to
warm up: `default_registry()` builds an offline `MockRail` and the process can
answer the moment it binds. There is no `/readyz` here and none is needed.

### Anything that is not 200 raises

openrate's SDK returns the body whatever the status, because a rates lookup
that failed is merely unhelpful. This one throws, because a `404` or a `502`
body deserialised into a `Receipt` is a `Receipt` whose fields happened to be
absent.

The two answers that look like failures and are **not** exceptions:

- `VerifyAsync` → `{"valid":false}`, HTTP `200`. The rail's fail-closed
  verdict. Gate entitlement on `true` and nothing else, and never retry a
  `false` as though it were transient.
- `ValidateDestinationAsync` → all five verdicts, HTTP `200`. **Read the body,
  not the status code.**

### The default registry is mock-only

`default_registry()` registers exactly one rail, `"mock"`. Any other id is a
`404` — not a failure of that rail, but a process that has never heard of it.
Per-rail registration is unwritten; everything around it is real and tested.

```
an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
```

### Binary resolution

1. `PATALA_SIDECAR_BINARY`
2. `bin/patala-sidecar` next to the assembly (`bin\patala-sidecar.exe` on Windows)
3. `$PATALA_HOME/target/{release,debug}/patala-sidecar`
4. `patala-sidecar` on `PATH`

```sh
cargo build -p patala-sidecar --release
```

---

## Direct — `Patala.Direct`

patala inside your process, through the C ABI in
[`patala-ffi/include/patala.h`](../../patala-ffi/include/patala.h). Six
symbols, JSON in and JSON out.

```csharp
using var rail = Direct.Mock(feeMinor: 25);

Direct.AbiCheck();                                          // fail loudly on a stale library

string verdict = rail.ValidateDestination("mock:wallet:alice");
if (rail.IsRefusal(verdict)) return;

string receipt = rail.Charge(Json.PayRequest(1250, "USDC", "mock:wallet:alice", "order-4711"));
bool paid = rail.IsValid(receipt);
```

**Creating a rail talks to nothing** — no socket, no thread, no environment
variable read. Unknown configuration fields are **refused**: a misspelled
`"currencys"` is an error, not a rail quietly built with a currency list you did
not choose.

Worked example: [`examples/DirectCharge.cs`](examples/DirectCharge.cs).

### There is no streaming, and therefore no `IAsyncEnumerable`

No `patala_stream`. A quote, a charge, a verification and a destination check
are each one question with one answer. llmux's .NET SDK, which binds the same
ABI shape, does expose `IAsyncEnumerable<string>` for `llmux_stream`. **The
absence here is deliberate and stated rather than left to be noticed.**

The direct methods are also **synchronous** rather than `Task`-returning, and
that is the honest shape: a handle owns a *current-thread* Tokio runtime, so
the work happens on your calling thread. An `async` wrapper over it would
return a `Task` that was already complete and would say something false about
where the blocking went. Use `Task.Run` at the call site if you need to move it
off a UI thread — deliberately, and visibly.

### `LibraryImport`, `out IntPtr`, and `AllowUnsafeBlocks`

The binding uses **`LibraryImport`** (source-generated, .NET 7+) rather than
`DllImport`: compile-time stubs, NativeAOT-compatible, every string across the
boundary declared rather than guessed.

**Every function returning a string returns `IntPtr`, never `string`.** A
`string` return compiles, runs, and leaks: the marshaller copies the C string
and has no idea the original must go back to `patala_free` — and it must go back
to `patala_free` specifically, not to `free(3)`, because that memory came from
Rust's allocator. Results are copied with `Marshal.PtrToStringUTF8` and freed in
a `finally`; error strings are freed after becoming exceptions; and the
`char** err` out-parameter is drained **on the success path too**.
`patala_abi_version` returns a static string and is the one thing never freed.

Because patala has no callback, `char**` is expressed as `out IntPtr` and
**nothing in `PatalaDirect.cs` uses a pointer, a function pointer or a fixed
buffer**. The project still sets `AllowUnsafeBlocks`, because `LibraryImport`
requires it unconditionally — the generator emits pointer-using stubs
regardless of the declared signature and says so as `SYSLIB1062`. llmux's .NET
binding writes unsafe code of its own, because `llmux_stream` takes a function
pointer.

### `SafeHandle`

A rail is a `PatalaSafeHandle`, so:

- `using` closes it deterministically, including on the exception path;
- if you forget, the base class's finaliser closes it rather than never;
- `DangerousAddRef`/`DangerousRelease` around each call means a concurrent
  `Dispose` cannot close a handle mid-flight;
- double `Dispose` is safe, and calling after `Dispose` is a clean
  `PatalaException`, not a crash.

patala handles are `uint64` registry keys rather than pointers, and `SafeHandle`
is still the right vehicle: it is the type the runtime knows how to keep alive
across a P/Invoke and release exactly once.

From the example's real output:

```
after dispose: this rail is disposed
```

Use after dispose is a clean error because handle numbers are **retired, not
recycled** — a stale handle can never reach somebody else's rail. `patala.h`
exposes no open-handle counter, so there is no `OpenHandles()` here as openrate's
SDK has; saying so beats adding a counter of our own that counts something
different from the library's.

### `IsValid` and `IsRefusal` fail closed

```csharp
public bool IsValid(string receiptJson) => Verify(receiptJson).Trim() == "{\"valid\":true}";
```

An exact match on the library's own answer, so an unrecognised third answer is
`false`.

```csharp
public bool IsRefusal(string verdictJson) => Json.Field(verdictJson, "is_refusal") == "true";
```

Read from the document's own `is_refusal` rather than re-derived from `status`.
A re-derivation falls through to its default for any status added later, and
that default is "not a refusal" — failing open, on the one question in this API
where failing open loses money. `is_refusal` is in the JSON for exactly this
reason: it is a *method* on the Rust type, and a method does not survive JSON.

There is also a third state that is neither. From the example, using a rail
configured without destination checks — the offline stand-in for a fiat rail,
whose destination is an opaque processor-side token:

```
  "mock:wallet:alice" -> StructurallyValid, IsRefusal=False, human_must_confirm=true
  "eth:wallet:alice" -> WrongNetwork, IsRefusal=True, human_must_confirm=true
  "" (empty) -> Malformed, IsRefusal=True, human_must_confirm=true
  the same address on a rail that cannot check: Unknown, IsRefusal=False
  Unknown is NOT a refusal and is NOT an approval. It needs a human.
```

`human_must_confirm` is `true` on **every** verdict, `StructurallyValid`
included, because patala does not detect exchange-owned addresses and will not
guess.

### `Json.PayRequest` takes a `ulong`

`amountMinor` is an integer number of minor units — 1250 is USDC 0.01250, or
ZAR 12.50. patala never puts a float on either side of the boundary, and a
convenience overload taking a `decimal` or a `double` would be where the
rounding bug got in. This is also why the SDK returns documents as strings
rather than deserialising them for you: a `Receipt` decoded with `amount_minor`
as a `double` is a payments bug that type-checks.

---

## What was measured, and what was not

Stated precisely, because the interesting claim here is a negative one.

**Measured, and reproducible from this repo:**

- On **HotSpot** (OpenJDK 26.0.2, darwin/arm64), loading `libpatala_ffi`
  replaces **zero** signal handlers and alters **zero** flags, and the process
  has the same OS-thread count before `dlopen`, after `dlopen`, and after a
  full charge → verify round trip. `-Xcheck:jni` prints nothing.
  → `sdks/java/signal-probe.sh`, output quoted in
  [`sdks/java/README.md`](../java/README.md#the-jvm-and-patalas-shared-library).
- From **plain C**, with no managed runtime involved at all, the same thread
  count across `dlopen` and across a round trip:
  `scripts/ffi-ctest.sh` → `ok   dlopen started no threads (no runtime in the
  host process)` and `ok   a full charge/verify round trip started no threads
  either`, part of a run of **55 checks, 0 failed**.

**Not measured:** any of this under **CoreCLR**. The .NET examples here ran
clean, repeatedly, on darwin/arm64 — that is evidence, not proof.

What makes the inference reasonable rather than a hope: the property is a
property of *the library*. A shared object that calls `sigaction` zero times and
creates zero threads does so regardless of which runtime dlopened it, and two
independent hosts — HotSpot and a bare C program — agree that this one does.
CoreCLR would only differ if it were disturbed by something patala does, and
what was measured is that patala does nothing.

If you adopt the direct path on .NET, test it under load on your platform
anyway.

---

## The costs of the direct path

The honest list, and it is much shorter than llmux's or openrate's. Absent
from it, because it was measured: replaced signal handlers, fork-unsafety, a
background thread, and a double-digit-megabyte artifact.

1. **One executed platform, and it is not Windows.** See below. This is the
   whole .NET recommendation.
2. **A C ABI, so JSON strings rather than types.** No `IAsyncEnumerable`, and
   no generated records: UniFFI has a C# generator, but it is third-party and
   is not version-locked to the `uniffi = 0.29` this workspace pins, so a
   binding generated by it silently drifts from the scaffolding it talks to.
   The Kotlin SDK evaluated the equivalent question with UniFFI's *own*
   first-party generator and [reports what happened](../kotlin/README.md#the-uniffi-route-evaluated-and-measured).
3. **A lazily-initialised Tokio runtime per handle.** `patala-core`'s trait is
   `async` and a C caller has no event loop, so each handle owns a
   *current-thread* runtime and blocks on it. Calls on one handle serialise;
   open more than one handle if you want parallelism on the same rail.
4. **Latency is not the reason to embed.** No second process and no port is.

## Platforms

The **shared library**, direct path only:

| target | status |
|---|---|
| darwin/arm64 | **built and executed.** 844,656 bytes, `--release`. Everything on this page ran on it. |
| linux/amd64 | the `.so` **is** built and the C smoke test **does** run against it, in CI's `c abi` job (`make smoke-ffi` on `ubuntu-latest`, twice — default and `--features fiat-all`). **No .NET has ever been run there.** |
| darwin/amd64 | **not built.** |
| linux/arm64 | **not built.** |
| **windows/amd64** | **built nowhere. No DLL exists.** |

darwin/arm64 is the only row this SDK itself claims anything about; linux/amd64
is a library CI proves loads from C, with no CoreCLR behind it.
`cargo build -p patala-ffi --release` is the whole build. `Direct.FindLibrary()`
says all of this in its error message rather than throwing a bare
`DllNotFoundException`.

**On Windows, use the sidecar** — an ordinary Rust binary,
`cargo build -p patala-sidecar --release`.

## Toolchain this was built and run on

- .NET SDK **10.0.302**, targeting **net8.0**, darwin/arm64
- Rust **1.97.1**, cargo **1.97.1**, patala **0.1.0**

The examples project sets `RollForward=LatestMajor` so a net8.0 build starts on
a machine that only has the .NET 10 runtime, which is the case here.

## Layout

```
sdks/dotnet/
  Patala.cs               sidecar (no unsafe, no native)
  PatalaDirect.cs         direct: LibraryImport + SafeHandle
  Json.cs                 the only JSON this SDK writes, plus PatalaException
  Patala.csproj
  examples/DirectCharge.cs    runnable — offline, MockRail
  examples/SidecarCharge.cs   runnable — loopback only, MockRail
  examples/Program.cs         picks one
  examples/Examples.csproj
  run-examples.sh
```

## Real output

`sdks/dotnet/run-examples.sh`, verbatim:

```
run-examples: dotnet 10.0.302, cargo 1.97.1 (c980f4866 2026-06-30)
run-examples: library 844656 bytes
run-examples: built

================ DirectCharge (in-process, C ABI) ================
library: /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
         844656 bytes
abi version: 0.1.0 (compared by the library, not by us)
id:           {"rail_id":"mock"}
capabilities: {"class":"NonCustodialFinal","reversible":false,"requires_kyc":false,"holds_funds":false,"currencies":["USDC"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight --
  "mock:wallet:alice" -> StructurallyValid, IsRefusal=False, human_must_confirm=true
  "eth:wallet:alice" -> WrongNetwork, IsRefusal=True, human_must_confirm=true
  "" (empty) -> Malformed, IsRefusal=True, human_must_confirm=true
  the same address on a rail that cannot check: Unknown, IsRefusal=False

-- quote -> charge -> verify --
  quote:   {"rail_id":"mock","amount_minor":1250,"currency":"USDC","fee_minor":25,"total_minor":1275,"settlement":"Instant","expires_at_unix":1786314001}
  receipt: {"rail_id":"mock","amount_minor":1250,"currency":"USDC","reference":"order-4711","proof":[82,156,...],"settled_at_unix":1786313701}
  IsValid(receipt):  True
  IsValid(tampered): False   <- an ordinary result, not an exception

-- what this rail refuses to pretend --
  webhook:  patala_call(webhook): patala: verify_webhook is not supported by this rail
  unknown:  patala_call(settle-later): patala: unknown method "settle-later" (want one of: id, capabilities, quote, charge, verify, validate-destination, webhook, caveat, providers)
  a failing rail: patala_call(charge): patala: rail error: mock rail mock is configured to fail

after dispose: this rail is disposed

DirectCharge: OK — offline, no socket opened, no thread started.

================ SidecarCharge (child process, HTTP) =============
INFO patala_sidecar: patala-sidecar listening on 127.0.0.1:61733 (loopback only)
sidecar:  http://127.0.0.1:61733 (loopback, hardcoded in the server)
healthz:  ok   <- the one unauthenticated route
caps:     {"class":"NonCustodialFinal",...,"currencies":["USDC","USD"],"settlement":"Instant","atomic_multi_party":false}

-- destination pre-flight (POST /v1/rails/mock/validate-destination) --
  "mock:wallet:alice" -> StructurallyValid, IsRefusal=False
  "eth:wallet:alice" -> WrongNetwork, IsRefusal=True
  "" (empty) -> Malformed, IsRefusal=True

-- quote -> charge -> verify --
  IsValid(receipt):  True   <- 200; this, not the charge, is entitlement
  IsValid(tampered): False   <- also 200. false is data, not an HTTP error.

-- what this server refuses to pretend --
  webhook (mock has no push delivery): POST /v1/rails/mock/webhook: HTTP 501 {"error":"verify_webhook is not supported by this rail","kind":"unsupported"}
  an unregistered rail: GET /v1/rails/stellar: HTTP 404 {"error":"no rail is registered under id \"stellar\"","kind":"unknown_rail"}
  a wrong bearer token: GET /v1/rails/mock: HTTP 401 unauthorized

SidecarCharge: OK — child process stopped.

run-examples: OK
```
