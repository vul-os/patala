# patala for Swift

Two modes, both runnable, both offline.

| | what it is | the type |
| --- | --- | --- |
| **Direct** | `dlopen`s `libpatala_ffi` and runs patala **inside your process** | `Patala.Rail` |
| **Sidecar** | spawns `patala-sidecar` and talks HTTP over loopback | `URLSession`, no patala library at all |

**Direct is the better default on macOS, and Swift is one of the languages
where that comes with the fewest asterisks in the whole suite.** C interop is a
first-class Swift feature: `@convention(c)` function types plus `dlopen` reach
patala with no module map, no bridging header and no `unsafeFlags` — see
[No module map](#no-module-map-no-unsafeflags). And because patala is Rust, the
list of things loading it does to your process is empty.

## Tested on

Everything below was executed on this machine, not inferred:

| | |
| --- | --- |
| Swift | Apple Swift **6.1.2** (swiftlang-6.1.2.1.2, clang-1700.0.13.5) |
| swift-driver | 1.120.5 |
| Target | `arm64-apple-macosx15.0` |
| macOS | **15.7.3** (build 24G419), Apple silicon |
| Xcode | **not installed** — Command Line Tools only. See [Checks, not tests](#checks-not-tests) |
| Package | SwiftPM, tools-version 5.9, platform floor macOS 13 |
| patala | 0.1.0; `libpatala_ffi.dylib` **844,656 bytes** |

Not tested: Linux, iOS, any Intel Mac, any other Swift version. See
[Platform reality](#platform-reality) — the honest answer for iOS is not "it
probably works".

## Run the examples

```
./sdks/swift/run.sh            # both examples
./sdks/swift/run.sh direct
./sdks/swift/run.sh sidecar
./sdks/swift/run.sh checks
```

Everything runs on `MockRail`: deterministic, offline, no credentials, no
network. This is a payments library, and an example that moves real value is
not an example.

Real output:

```
==> direct (in-process, C ABI via dlopen)
patala direct (Swift, in-process) — libpatala_ffi 0.1.0
library:   /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
abi:       matches 0.1.0
caps:      NonCustodialFinal / wallet address, signed final receipt
           holds_funds=false reversible=false currencies=["USDC", "USD"]
caveat:    patala cannot tell whether this address belongs to an exchange. A struct...
dest:      mock:wallet:alice -> StructurallyValid (is_refusal=false, human_must_confirm=true)
quote:     1250 + 0 fee = 1250 minor units of USDC
charge:    1250 minor units, ref=order-1, issued by rail mock, proof=32B
verify:    {"valid":true}  <- the entitlement check
tampered:  {"valid":false} -> verifyHolds()=false  <- returned, not thrown
refused:   PatalaError: patala: invalid request: rail mock does not support currency EUR
typo:      refused — patala: invalid configuration document: unknown field `currencys...

OK — offline, MockRail only, no value moved.

==> sidecar (child process over HTTP)
patala sidecar (Swift, child process) — 127.0.0.1:62227
binary:    /Users/pc/code/vulos/patala/target/release/patala-sidecar
health:    ok
no token:  HTTP 401
caps:      HTTP 200 NonCustodialFinal holds_funds=false
dest:      HTTP 200 WrongNetwork is_refusal=true human_must_confirm=true
typo:      HTTP 400 — a bad request is not a verdict
quote:     HTTP 200 total_minor=1250 (an integer on the wire, never a float)
charge:    HTTP 200 1250 minor units ref=order-1
verify:    HTTP 200 {"valid":true}  <- the entitlement check
tampered:  HTTP 200 {"valid":false}  <- 200, and false
no rail:   HTTP 404 — the sidecar's registry is mock-only
webhook:   HTTP 501 — the mock has no processor, so it invents no event

OK — offline, MockRail only, no value moved. Child reaped on exit.
```

## Direct

```swift
import Patala

let library = try Patala.Library.shared()
try library.requireABI("0.1.0")                 // throws on a stale library

let rail = try Rail(configJSON: #"{"rail":"mock","currencies":["USDC"]}"#)
let receipt = try rail.charge(payRequest)        // JSON in, JSON out
if try rail.verifyHolds(receipt) { /* the entitlement */ }
```

**Closing is `deinit`.** `Rail` owns the `UInt64` handle and releases it when
the last reference goes away — on the happy path, on a `throw`, on an early
`return`. ARC is the RAII here; there is no `close()` to forget and no `defer`
to write at the call site.

**Every returned string is freed.** Results and error messages alike go back
through `patala_free` and nothing else — not `free()`, this is Rust's
allocator. `Library.takeString` copies into a Swift `String` and frees the
original *before* the error is constructed, which is the step a hand-written
binding usually misses: it is easy to forget that error strings are malloc'd
exactly like results. `patala_abi_version` is the one exception — a static
string that must **not** be freed, and is not.

**Errors are `throws`,** with the library's own message in
`PatalaError.patala`. That message is plain UTF-8 text and deliberately not
JSON — print it, do not parse it.

### No module map, no `unsafeFlags`

The C ABI is reached with `dlopen`/`dlsym` and `@convention(c)` function types.
Three things follow, and all three matter:

- `swift build` works with nothing on the machine but a Swift toolchain — no
  header, no `-I`, no `-L`.
- The library is located at **run** time, so one build works whether
  `libpatala_ffi` sits in `target/release/`, on `DYLD_LIBRARY_PATH`, or
  wherever `$PATALA_LIBRARY` points.
- **This package can be a dependency of another package.** A target carrying
  `unsafeFlags` cannot be, which rules out the link-time approach for anything
  published — the usual reason a Swift C-interop package that "works locally"
  cannot be consumed.

Resolution order: `$PATALA_LIBRARY`, then `target/{release,debug}/` walking up
from the working directory, then the bare file name handed to the loader.
`Library.shared(path:)` caches one open library per path for the life of the
process and never `dlclose`s it — not because that would hang (it would not;
there is no runtime with threads still in the mapping, which is the hazard the
Go-based products in this suite have) but because handles are registry keys
*inside* the library, and unmapping it under a live `Rail` would turn a clean
"unknown handle" error into a crash.

Probe the version at startup and refuse a mismatch. A shared library resolves
off a load path you may not control, and a stale `libpatala_ffi` earlier on
that path is called silently and then misbehaves in ways that look like patala
bugs — `library.requireABI(_:)` is one line and rules it out.

### Three outcomes, not two

`Rail.verifyHolds` is the API in this package most worth arguing with, so it is
spelled out:

| | means | what to do |
| --- | --- | --- |
| `throws PatalaError` | the check could not be performed | retry, alert |
| returns `false` | the rail checked, and the receipt does not hold | never retry, never grant |
| returns `true` | the entitlement | proceed |

A binding that threw on `{"valid":false}` — the natural Swift instinct, since
"verification failed" sounds like an error — would merge the first two, and
every `catch` that logs-and-retries would then be granting entitlements on
unpaid orders. patala's C ABI keeps them apart by returning a *result* rather
than `NULL`; this package keeps them apart too, and `Rail.verify(_:)` returns
the raw document for callers who want to see it. Over HTTP the same distinction
is a `200` carrying `{"valid":false}`, never a 4xx.

`Rail.validateDestination` never fails at all: "I cannot check this address" is
the verdict `{"status":"Unknown"}`, because a caller must handle it as
carefully as a refusal and an error is too easy to swallow. Read `is_refusal`
(do not send) and `human_must_confirm` — `true` on *every* verdict, including
`StructurallyValid`, because patala does not detect exchange-owned addresses
and will not guess. `Rail.caveat()` is the sentence to show the human who must.

### Money, and two Foundation traps

Amounts are integer minor units plus a currency string. Never a `Double`.

- **`Codable` is the right tool.** The direct example declares
  `amountMinor: UInt64` and lets `JSONDecoder` do the work — which also means
  a fractional amount *fails to decode* rather than silently rounding. That is
  the correct outcome: a rail sending `1250.0` is a defect to notice.
- **`JSONSerialization` hands you `NSNumber` for everything**, and it will give
  you a `.doubleValue` for `amount_minor` without complaint. A `Double` loses
  every integer above 2^53. The same `NSNumber` prints booleans as `0`/`1`
  when interpolated, which is why the sidecar example asks CoreFoundation
  whether a value is a `CFBoolean` before printing it.

### No streaming

patala has no streaming operation, so this package has no `AsyncSequence` and
the C ABI has no `patala_stream`. If you came from llmux's Swift SDK looking
for `gw.chunks(...)`, its absence is not an omission — nothing patala does
produces a sequence of chunks.

## What loading this library costs you: almost nothing

llmux and openrate ship C ABIs of the same shape and their Swift packages carry
a list of Go-runtime caveats — a garbage collector, a scheduler, replaced
signal handlers, no fork-safety. **patala is Rust; none of that is true here
and none of it has been copied.**

- No language runtime, no GC, no scheduler.
- **No signal handlers installed** — which matters more on Apple platforms than
  most, because a crash reporter and a sampling profiler both want them.
- No threads started, at load or ever. Each `Rail` owns a *current-thread*
  async runtime that drives work on whichever thread called in and is dropped
  with the rail.
- Nothing happens at load: no socket, no file, no background task.
- `dlclose` does not hang.

That is a test, not a paragraph: `patala-ffi/ctest/smoke.c` counts the
process's threads before `dlopen`, after `dlopen`, and after a full
charge → verify round trip, and fails if the number ever goes up.

## Checks, not tests

There is **no `swift test` here**, and that is deliberate rather than an
oversight: `XCTest` ships with Xcode, this machine has Command Line Tools only,
and `import XCTest` does not compile at all. A `.testTarget` would be a file
nobody had ever seen pass, sitting next to a README claiming it does.

So the assertions are an ordinary executable,
[`Sources/patala-checks`](Sources/patala-checks/main.swift), which runs
anywhere Swift runs and ends by asserting the **number** of checks that ran —
the same discipline as `ctest/smoke.c`, and for the same reason.

```
$ ./sdks/swift/run.sh checks
patala Swift checks
  library: /Users/pc/code/vulos/patala/target/release/libpatala_ffi.dylib
  ok   patala_abi_version returns a version
  ...
22 checks ran, 0 failed (expected 22)
PASS
```

**Mutation-tested.** A green suite means nothing until you have seen it go red.
Changing `verifyHolds` to `return true` — the exact bug that would grant an
entitlement on a tampered receipt — produces:

```
22 checks ran, 1 failed (expected 22)
FAIL: 1 check(s) failed
```

## Sidecar

[`Sources/patala-sidecar-example`](Sources/patala-sidecar-example/main.swift)
spawns `patala-sidecar`, polls `/healthz`, and drives the same round trip with
`URLSession`. It loads no patala library.

The reasons to choose it from Swift:

- **Key isolation.** A non-custodial rail's signing key lives in whichever
  process calls `charge`. Five services loading the library means the key is in
  five address spaces; one sidecar puts it in one narrow process that does
  nothing else. `patala-sidecar`'s README is honest about the limit: this
  defends against an unrelated local process, not against a co-resident
  attacker running as the same user.
- **There is no shared library for your platform.** For Swift this is the live
  one, and it points at iOS.
- **Your process is sandboxed** in a way that forbids loading it.

Things the example demonstrates that you would otherwise learn the hard way:
the token gate is fail-closed and covers the read-only capabilities route too
(`401`); a tampered receipt is `200` with `{"valid":false}`; a malformed
`validate-destination` *request* is a `400` with no verdict fields at all,
while all five *verdicts* are `200`; the registry is mock-only, so
`/v1/rails/solana` is a `404`; and the child is terminated by a `defer` on
every path.

## Platform reality

| target | status |
|---|---|
| macOS 15 / arm64 | **built and run** — both examples and 22/22 checks, against an 844,656-byte `libpatala_ffi.dylib` |
| macOS / x86_64 | not built |
| Linux | **not built.** The package has `#if canImport(Darwin)` fallbacks to `Glibc` and no other platform code, but it has not been compiled there. CI builds the `.so` for the C smoke test, not for this |
| **iOS / iPadOS** | **not built, and do not assume it works.** Direct mode `dlopen`s a `.dylib` you built yourself, which is not how third-party code ships on iOS; and the sidecar spawns a child process, which iOS does not permit at all. The realistic shapes are an `.xcframework` with a *static* patala and UniFFI-generated Swift, or an app that talks to a `patala-sidecar` on a server it trusts. Neither exists in this tree |
| watchOS / tvOS | not built |

## UniFFI, and why this package does not use it

UniFFI *does* have a Swift backend, and patala's `#[uniffi::export]` surface
lives in [`patala-uniffi`](../../patala-uniffi/). Generated Swift bindings
would give you real structs and enums instead of JSON — `RailCapabilities` with
a `RailClass` you can `switch` over exhaustively, which is a genuine
improvement over `String` comparison.

They are **not generated in this tree**. `patala-go` and `patala-py` are the
two that are, each with a build step (`uniffi-bindgen generate`) and a `make`
target that executes it. Adding a third is ordinary work nobody has done, not a
blocker. Until then this package uses the plain C ABI, which needs no codegen
step and no toolchain beyond Swift — and which you would still be shipping a
cdylib for either way.

## See also

- [`sdks/c`](../c/) — the same six symbols, without the wrapper. The ground
  truth for what a call costs and who owns a pointer.
- [`sdks/rust`](../rust/) — no ABI at all; `use patala_core`.
- [`patala-ffi/README.md`](../../patala-ffi/README.md) — the library, its
  features and its test story.
- [`patala-sidecar/README.md`](../../patala-sidecar/README.md) — the server,
  its endpoint table and its threat model.
