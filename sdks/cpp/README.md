# patala from C++

The same six-function C ABI [`sdks/c`](../c/) uses, with a destructor doing the
bookkeeping. [`patala.hpp`](patala.hpp) is header-only, C++17, and has no
dependency beyond the standard library and
[`patala-ffi/include/patala.h`](../../patala-ffi/include/patala.h).

```cpp
#include "patala.hpp"

patala::require_abi("0.1.0");                 // throws on a stale library
const patala::Rail rail{R"({"rail":"mock","currencies":["USDC"]})"};

const std::string receipt = rail.charge(pay_request);   // throws on failure
if (rail.verify_holds(receipt)) { /* the entitlement */ }
```

`Rail` closes its handle in `~Rail`. Every `char*` the library returns —
results *and* error messages — is released with `patala_free` by an owning
object, on the happy path and on the throw path alike. That second one is the
whole reason this file exists; see [What the wrapper is
for](#what-the-wrapper-is-for).

## The two examples

| file | mode | what it shows |
|---|---|---|
| [`direct.cpp`](direct.cpp) | direct | links `libpatala_ffi` through `patala.hpp`: version probe, capabilities → UX, caveat, destination pre-flight, quote → charge → verify, a tampered receipt returned rather than thrown, the throw path, move semantics |
| [`sidecar.cpp`](sidecar.cpp) | sidecar | forks `patala-sidecar` on a free loopback port, polls `/healthz`, proves the token gate, drives the same round trip over HTTP, reaps the child in a destructor |

```bash
./run-demo.sh            # build and run both
./run-demo.sh direct
./run-demo.sh sidecar
make                     # build only
```

Both run on `MockRail`: deterministic, offline, no credentials, no network.
This is a payments library, and an example that moves real value is not an
example. These are **examples, not tests** — the ABI's test is
[`patala-ffi/ctest/smoke.c`](../../patala-ffi/ctest/smoke.c), which dlopens the
artifact and resolves every symbol by name.

## Real output

macOS 15.7.3 (24G419), Apple silicon, Apple clang 17.0.0, `-std=c++17`,
patala 0.1.0, `libpatala_ffi.dylib` 844,656 bytes:

```
==> direct (in-process, C ABI via patala.hpp)
patala direct (C++, in-process) — libpatala_ffi 0.1.0
abi:       matches 0.1.0
rail:      handle 1
caps:      NonCustodialFinal / wallet address, signed final receipt
           holds_funds=false — patala itself never holds funds
caveat:    patala cannot tell whether this address belongs to an exchange. A struct...
dest:      mock:wallet:alice -> StructurallyValid (is_refusal=false, human_must_confirm=true)
quote:     1250 + 0 fee = 1250 minor units of USDC
charge:    1250 minor units, ref=order-1, issued by rail mock
verify:    {"valid":true}  <- the entitlement check
tampered:  {"valid":false} -> verify_holds()=false  <- returned, not thrown
refused:   patala::Error: patala: invalid request: rail mock does not support currency EUR
moved:     handle 0 -> 2, one owner, one close

OK — offline, MockRail only, no value moved.

==> sidecar (child process over HTTP)
patala sidecar (C++, child process) — 127.0.0.1:57219
binary:    /Users/pc/code/vulos/patala/target/release/patala-sidecar
health:    ok
no token:  HTTP 401
caps:      HTTP 200 NonCustodialFinal holds_funds=false
dest:      HTTP 200 WrongNetwork is_refusal=true human_must_confirm=true
typo:      HTTP 400 — a bad request is not a verdict
quote:     HTTP 200 total_minor=1250 (parsed as an integer, never a double)
charge:    HTTP 200 1250 minor units ref=order-1
verify:    HTTP 200 {"valid":true}  <- the entitlement check
tampered:  HTTP 200 {"valid":false}  <- 200, and false
no rail:   HTTP 404 — the sidecar's registry is mock-only
webhook:   HTTP 501 — the mock has no processor, so it invents no event

OK — offline, MockRail only, no value moved. Child reaped on exit.
```

## What the wrapper is for

The happy path is not the interesting one. `patala_call` hands back a malloc'd
`char*` for the result **and**, on failure, a malloc'd `char*` for the message
— through a trailing `char** err`. Written by hand at each call site that is
four lines of bookkeeping per operation, and the one everybody forgets is the
error string on the failure path, because at that moment you are busy throwing.

`patala.hpp` keeps the raw pointer inside `detail::Owned` for the whole of it:

```cpp
detail::Owned out(patala_call(handle_, method, request, &err));
detail::Owned message(err);
if (!out) throw Error(message.copy());   // copy() may throw; `message` still frees
```

`message.copy()` allocates a `std::string` and can itself throw
`std::bad_alloc`. Either way — the `Error` is thrown, or the copy fails first —
`~Owned` runs during unwinding and the library's message is released.

### Measured, and mutation-tested

On this machine:

```
$ leaks --atExit -- ./direct
Process 47439: 192 nodes malloced for 27 KB
Process 47439: 0 leaks for 0 total leaked bytes.
```

A zero from a leak checker only means something if you know it can produce a
non-zero. So the guard was removed and the run repeated — one line changed in
`Rail::call`, from the `Owned message(err)` above to
`throw Error(std::string(err))`:

```
Process 48305: 1 leak for 80 total leaked bytes.
```

80 bytes, once, on the one error `direct.cpp` provokes. That is exactly the
shape of the bug this file exists to prevent, and exactly how invisible it is:
a program that throws on 0.1% of calls leaks a few dozen bytes a day and never
looks broken.

`sdks/c`'s `direct.c` gets the same guarantee from having exactly one
`goto done` label; `sdks/rust` gets it from `Drop` without anyone writing a
line. C++ is the language where it is a destructor you have to remember to
write, which is why it is written once here rather than at every call site.

## Three outcomes, not two

This is the design decision in `patala.hpp` most worth arguing with, so it is
spelled out. `Rail::verify_holds` has **three** results:

| | means | what to do |
|---|---|---|
| throws `patala::Error` | the check could not be performed | retry, alert |
| returns `false` | the rail checked, and the receipt does not hold | never retry, never grant |
| returns `true` | the entitlement | proceed |

A wrapper that turned `{"valid":false}` into an exception — which is the
natural C++ instinct, since "verification failed" sounds exceptional — would
merge the first two, and every `catch` that logs-and-retries would then grant
entitlements on unpaid orders. patala's C ABI keeps them apart by returning a
*result* rather than `NULL`; `patala.hpp` keeps them apart too, and
`Rail::verify` returning the raw document is there for callers who want to see
it. The same distinction over HTTP is a `200` carrying `{"valid":false}`, never
a 4xx.

`validate_destination` has the same shape and never fails at all: "I cannot
check this address" is the verdict `{"status":"Unknown"}`. Read `is_refusal`
(do not send) and `human_must_confirm`, which is `true` on *every* verdict
including `StructurallyValid` — patala does not detect exchange-owned addresses
and will not guess. `Rail::caveat()` is the sentence to show the human who
must.

## Money

Integer minor units plus a currency string, never a float.
[`jsonpeek.hpp`](jsonpeek.hpp) deliberately has **no** `number()` returning a
`double`: a double silently loses every integer above 2^53, and `peek::u64`
*refuses* `1250.0` rather than truncating it. Keep that rule when you swap in a
real JSON library — `nlohmann::json`'s implicit conversions will happily hand
you a `double` for an amount if you let them.

## What loading this library costs you: almost nothing

llmux and openrate ship C ABIs of the same shape, but they are **Go**, built
with `-buildmode=c-shared`, so their READMEs correctly warn about a garbage
collector, a scheduler, replaced signal handlers and a library that is not
fork-safe.

patala is Rust. **None of that applies here and none of it has been copied.**
No language runtime, no GC, no signal handlers installed, no threads started at
load or ever, nothing done at load time, and `dlclose` does not hang. Each
handle owns a *current-thread* async runtime that runs on whichever thread
called in and is dropped with the handle. [`sidecar.cpp`](sidecar.cpp) forks,
and would be safe doing so even if it had linked the library.

That claim is a test, not prose: `ctest/smoke.c` counts the process's threads
before `dlopen`, after `dlopen`, and after a full round trip, and fails if the
number ever goes up.

The signal half has been measured against the sibling product as well. Running
llmux's own signal probe against both libraries, same machine, same JVM (done
as part of patala's Java/Kotlin SDK work):

| | HotSpot signal handlers replaced | handler flags altered |
|---|---|---|
| **patala** | **0** | **0** |
| llmux | 5 | 3 |

Measured here, `cargo build -p patala-ffi --release`:

| build | `libpatala_ffi.dylib` |
|---|---|
| default — mock rail only, fully offline | **844,656 bytes** |
| `--features fiat-all` — 20 processor adapters, UniFFI, reqwest, TLS | 6,330,544 bytes |

llmux's `libllmux.dylib` on this same machine is 12,787,504 bytes — **15.1×**,
measured rather than quoted.

## Building

```
c++ -std=c++17 -I<repo>/patala-ffi/include -o direct direct.cpp \
    -L<libdir> -lpatala_ffi -Wl,-rpath,<libdir>
```

Build the library first with `cargo build -p patala-ffi --release`; override
its location with `make PATALA_LIB_DIR=/path/to/dir`. No `-lpthread`: the
library starts no threads.

**macOS wart, measured here.** `rustc` stamps a `cdylib`'s `LC_ID_DYLIB` with
the absolute path of the copy under `target/<profile>/deps/`, so a linked
executable hard-codes a path out of somebody's build tree. The Makefile
rewrites it in the executable with `install_name_tool -change … @rpath/…`;
`install_name_tool` warns that it invalidates the code signature and re-signs
ad-hoc, which is expected. A packager should fix the library instead:
`install_name_tool -id @rpath/libpatala_ffi.dylib libpatala_ffi.dylib`.

## Platform reality

| target | status |
|---|---|
| darwin/arm64 | **built and run**, both examples, plus `leaks` |
| linux/x86_64 | **not built here.** CI's `c abi` job builds the `.so` on `ubuntu-latest` and runs the C smoke test against it. The C++ wrapper is portable C++17 with no platform code, but it has not been compiled there |
| darwin/x86_64, linux/arm64 | not built |
| **windows** | **not built. No DLL exists.** `patala_free`'s "not your `free()`" rule matters most there, and `sidecar.cpp` uses `fork`/`execl` and would need rewriting on top of that |

## Which mode

**Direct.** C++ has no marshalling layer, no GIL and no runtime to attach, and
none of the usual reasons to back away from an in-process payment library exist
for patala. Take the sidecar when you want **key isolation** — a rail's signing
key in one narrow process instead of in all five services that link the library
— when your process loads third-party code, or when there is no shared library
for your platform. Not for fork-safety, signal handling or latency.

## See also

- [`sdks/c`](../c/) — the same ABI without the destructor; the ground truth.
- [`sdks/rust`](../rust/) — no ABI at all; `use patala_core`.
- [`patala-ffi/README.md`](../../patala-ffi/README.md) — the library, its
  features and its test story.
- [`patala-sidecar/README.md`](../../patala-sidecar/README.md) — the server and
  its threat model.
