# patala from C

C is the ground truth for every binding in `sdks/` that is not Rust. Swift's
`dlopen`, C++'s RAII wrapper, Node's `bun:ffi`, PHP's `FFI::cdef` — all of them
are doing what [`direct.c`](direct.c) does, wrapped in that language's
ceremony. If you want to know what a call really costs or who really owns a
pointer, read the C.

There is nothing to install. The header is
[`patala-ffi/include/patala.h`](../../patala-ffi/include/patala.h) and it is
the whole surface — six functions:

```c
const char* patala_abi_version(void);
int         patala_abi_check(const char* expected, char** err);
uint64_t    patala_new(const char* config_json, char** err);   /* 0 = failure */
void        patala_close(uint64_t h);
char*       patala_call(uint64_t h, const char* method, const char* request_json, char** err);
void        patala_free(char* p);
```

JSON in, JSON out — the *same* JSON `patala-sidecar` serves, built from the
same Rust types. A body that works against `POST /v1/rails/mock/charge` works
against `patala_call(h, "charge", …)` unchanged. Methods: `id`,
`capabilities`, `quote`, `charge`, `verify`, `validate-destination`,
`webhook`, `caveat`, `providers`.

**There is no streaming entry point, in either mode.** That is not a gap:
patala has no streaming operation. llmux's C ABI has `llmux_stream` because
chat streaming is its main event; nothing patala does produces a sequence of
chunks. `mini_http.c` here therefore has no SSE reader either.

## The two examples

| file | mode | what it shows |
|---|---|---|
| [`direct.c`](direct.c) | direct | links `libpatala_ffi`: version probe, capabilities → UX, caveat, destination pre-flight, quote → charge → verify, a tampered receipt, the error path, one cleanup label |
| [`sidecar.c`](sidecar.c) | sidecar | forks `patala-sidecar` on a free loopback port, polls `/healthz`, proves the token gate, drives the same round trip over HTTP, reaps the child on every path |

```bash
./run-demo.sh            # build and run both
./run-demo.sh direct
./run-demo.sh sidecar
make                     # build only
```

Both run on `MockRail`: deterministic, offline, no credentials, no network.
This is a payments library, and an example that moves real value is not an
example.

**These are examples, not tests.** The test is
[`patala-ffi/ctest/smoke.c`](../../patala-ffi/ctest/smoke.c): it `dlopen()`s
the artifact, resolves all six symbols **by name**, runs 58 checks and then
asserts that 58 checks *ran*. That is what catches a missing `#[no_mangle]`, a
renamed export or a header that has drifted from the library — a different job
from showing someone how to call this. These examples link the library
instead, because that is how a program with an installed library is actually
written.

## Real output

macOS 15.7.3 (24G419), Apple silicon, Apple clang 17.0.0, patala 0.1.1,
`libpatala_ffi.dylib` 849,584 bytes:

```
==> direct (in-process, C ABI)
patala direct (C, in-process) — libpatala_ffi 0.1.1
abi:       matches 0.1.1
rail:      handle 1
caps:      NonCustodialFinal / wallet address, signed final receipt
           holds_funds=false — patala itself never holds funds
caveat:    patala cannot tell whether this address belongs to an exchange. A struct...
dest:      mock:wallet:alice -> StructurallyValid (is_refusal=false, human_must_confirm=true)
quote:     1250 + 0 fee = 1250 minor units of USDC
charge:    1250 minor units, ref=order-1, issued by rail mock
verify:    {"valid":true}  <- the entitlement check
tampered:  {"valid":false}  <- a result, not NULL; a refusal is DATA
refused:   patala: invalid request: rail mock does not support currency EUR

OK — offline, MockRail only, no value moved.

==> sidecar (child process over HTTP)
patala sidecar (C, child process) — 127.0.0.1:55603
binary:    /Users/pc/code/vulos/patala/target/release/patala-sidecar
health:    ok
no token:  HTTP 401
caps:      HTTP 200 NonCustodialFinal holds_funds=false
dest:      HTTP 200 WrongNetwork is_refusal=true human_must_confirm=true
quote:     HTTP 200 total_minor=1250 (parsed as an integer: yes)
charge:    HTTP 200 1250 minor units ref=order-1
verify:    HTTP 200 {"valid":true}  <- the entitlement check
tampered:  HTTP 200 {"valid":false}  <- 200, and false
no rail:   HTTP 404 — the sidecar's registry is mock-only
webhook:   HTTP 501 — the mock has no processor, so it invents no event

OK — offline, MockRail only, no value moved. Child reaped on exit.
```

`leaks --atExit -- ./direct` on the same machine: **0 leaks for 0 total leaked
bytes**, 190 nodes malloced, 2,849 KB physical footprint for the whole process
with the library mapped.

## What loading this library costs you: almost nothing

This is the part that is different from the rest of the suite, so it is stated
plainly rather than buried.

llmux and openrate ship C ABIs with the same six-function shape, but those two
are **Go**, built with `go build -buildmode=c-shared`, which puts the Go
runtime in your process. Their C READMEs correctly warn you about a garbage
collector, a preemptive scheduler, Go's own signal handlers (`SIGSEGV`,
`SIGBUS`, `SIGFPE`, `SIGPIPE`, `SIGURG` — not `SIGPROF`, which Go's
`c-shared` runtime leaves alone), and a
library that is **not fork-safe** — which is why llmux's `sidecar_chat.c` says
in a comment that it is safe to `fork()` only because it never loads
`libllmux`.

patala is Rust. **None of that is true here, and none of it has been copied.**

- **No language runtime.** No GC, no scheduler, no green threads.
- **No signal handlers installed.** Your crash reporter, sampling profiler and
  sanitizer build have nothing to conflict with.
- **No threads started**, at load time or ever. Each handle owns a
  *current-thread* async runtime that drives work on whichever thread called
  in and is dropped with the handle.
- **The library is fork-safe; a handle that is *in use* at the moment of the
  fork is not.** Nothing of ours is running at `fork()` time, so
  [`sidecar.c`](sidecar.c) forks — and unlike llmux's, it would be safe doing
  so even if it had loaded the library. The handle rule is the narrow one that
  is real: a handle's runtime sits behind a mutex and `fork()` copies a locked
  mutex as locked, so with four parent threads charging on one handle an
  inherited handle hung **4–8 times in 200** forks against **0 in 200** for one
  opened in the child. **Open the handle in the child.** Full measurement:
  [`docs/c-abi.md`](../../docs/c-abi.md#what-it-costs).
- **Nothing happens at load.** No socket, no file, no background task.
- **`dlclose` does not hang.** There is no runtime with threads still executing
  the mapping.

None of that is prose you have to take on trust: `ctest/smoke.c` counts the
process's threads before `dlopen`, after `dlopen`, and after a full
charge → verify round trip, and fails if the number ever goes up. On a platform
it does not know how to count threads on it **fails** rather than skipping.

The signal claim has been measured against the sibling product too, rather than
merely reasoned about. Running llmux's own signal probe against both libraries,
same machine, same JVM (done as part of patala's Java/Kotlin SDK work):

| | HotSpot signal handlers replaced | handler flags altered |
|---|---|---|
| **patala** | **0** | **0** |
| llmux | 5 | 3 |

The JVM is the harshest host there is for this — it installs handlers for
`SIGSEGV`, `SIGBUS` and `SIGFPE` and relies on them — so zero there is the
strongest form of the claim. Your crash reporter, profiler and sanitizer build
are in the same position.

### Size

| build | `libpatala_ffi.dylib` |
|---|---|
| default — mock rail only, fully offline | **849,584 bytes** |
| `--features fiat-all` — 20 processor adapters, UniFFI, reqwest, TLS | 6,350,144 bytes |

llmux's `libllmux.dylib` on this same machine is **12,823,104 bytes** (measured,
not quoted — llmux's own README carries a slightly older figure), and its
linux/arm64 build is larger still. That is **15.1×** patala's, and it is a
consequence of the language rather than of doing less: the mock rail here is
the same `MockRail` every other patala surface exercises.

## Building against libpatala_ffi

```
cc -std=c11 -I<repo>/patala-ffi/include -o direct direct.c jsonpeek.c \
   -L<libdir> -lpatala_ffi -Wl,-rpath,<libdir>
```

Build the library first: `cargo build -p patala-ffi --release` writes
`target/release/`. Point the Makefile elsewhere with
`make PATALA_LIB_DIR=/path/to/dir`. No `-lpthread` is needed — the library
starts no threads.

**macOS wart, measured here.** `rustc` stamps a `cdylib`'s `LC_ID_DYLIB` with
the *absolute* path of the copy under `target/<profile>/deps/`:

```
$ otool -D target/release/libpatala_ffi.dylib
/Users/pc/code/vulos/patala/target/release/deps/libpatala_ffi.dylib
```

So a linked executable runs fine on the machine that built it and breaks the
moment the library is installed anywhere else. The Makefile rewrites the
reference in the executable —

```
install_name_tool -change "$(otool -D $LIB | tail -1)" @rpath/libpatala_ffi.dylib direct
```

— after which `otool -L direct` reports `@rpath/libpatala_ffi.dylib` and the
`-rpath` is finally consulted. `install_name_tool` prints a warning about
invalidating the code signature and re-signs ad-hoc; that is expected. If you
are packaging the library rather than consuming it from a checkout, fix it at
the source instead: `install_name_tool -id @rpath/libpatala_ffi.dylib
libpatala_ffi.dylib`. (llmux's C README describes the same fix for a different
cause — Go emits a *bare* install name rather than an absolute one.)

## The rules, in C terms

**Ownership.** Every non-`const char*` the library returns — results *and*
error messages — is freed with `patala_free` and **nothing else**. Not
`free()`: it was not allocated by your allocator, it was allocated by Rust's.
`patala_free(NULL)` is safe, which is why the cleanup block in `direct.c` has
no null checks. `patala_abi_version` is the one exception: its string is
static, so do not free it.

**Errors.** Fallible functions take a trailing `char** err`. The message is
plain UTF-8 **text, not JSON** — print it, do not parse it — and it is yours to
free. Pass `NULL` if you do not want it.

**No RAII, so: one cleanup label.** `direct.c` has exactly one `goto done`
target and no early `return` after the handle exists. That is the C shape of
the guarantee `sdks/cpp`'s destructor and `sdks/rust`'s `Drop` make for free.

**Handles are integers in a registry inside the library, never pointers**, and
are never reused. Calling with a closed or invented handle is a clean error
string, not a segfault in your address space. `patala_close` is idempotent, and
closing `0` is a no-op — which is what lets the cleanup label run
unconditionally.

**Threading.** A handle is safe to use from several threads at once. Calls on
*one* handle serialise (it owns one current-thread runtime); calls on different
handles run concurrently. Open one handle per rail, and more than one if you
want parallelism on the same rail.

**Panics do not cross.** A bug inside patala becomes an error string, not a
crash in your process: every entry point catches unwinding at the boundary.

## Money, and the two things a C caller gets wrong

**Amounts are integer minor units plus a currency string.** Never a float.
[`jsonpeek.h`](jsonpeek.h) deliberately has **no** `json_number` returning a
`double` — a double silently loses every integer above 2^53, and `json_u64`
here *refuses* `1250.0` rather than truncating it. Keep that rule when you swap
in a real parser: cJSON's `valuedouble` is the wrong field for money.

**`verify` returning `{"valid":false}` is not an error.** `patala_call` returns
a *result* there, not `NULL`, precisely so the two cannot be confused:

| | means | what to do |
|---|---|---|
| `NULL` + `*err` | I could not perform the check | retry, alert |
| `{"valid":false}` | I checked, and it does not hold | never retry, never grant |

Over HTTP the same distinction is HTTP `200` with `{"valid":false}` — **not** a
4xx. `sidecar.c` prints both so the shape is visible. The day someone adds
"retry on 4xx" to a shared HTTP helper, a status-code-only integration turns an
unpaid order into an entitlement.

**`validate-destination` never fails.** "I cannot check this address" comes back
as the verdict `{"status":"Unknown"}`, because a caller must handle it as
carefully as a refusal and an error is too easy to swallow. Read `is_refusal`
(do not send) and `human_must_confirm` — which is `true` on *every* verdict,
including `StructurallyValid`. patala does not detect exchange-owned addresses
and will not guess; `"caveat"` is the sentence to show the human who must.

## Which mode to use from C

**Direct**, in almost every case. C is the language with the least FFI friction
of any here — no marshalling layer, no runtime to attach, no GIL — and the
usual reasons to back away do not exist for patala. Take the sidecar when:

- **You want key isolation.** A non-custodial rail's signing key lives in
  whichever process calls `charge`. Five services linking the library means the
  key is in five address spaces; one sidecar puts it in one narrow process that
  does nothing else. This is the only *strong* reason, and it is a good one.
- **Your process is not one you want a payment rail inside** — a plugin host, a
  setuid helper, anything that loads third-party code.
- **There is no library for your platform.** See below.

Not on that list, and deliberately: fork-safety, signal handling, GC pauses,
`dlclose`, and latency.

## Platform reality

Built and executed **on this machine** while writing these examples:

| target | status |
|---|---|
| darwin/arm64 | **built and run.** `libpatala_ffi.dylib`, 849,584 bytes; both examples above |
| linux/x86_64 | **not built here.** CI's `c abi` job builds the `.so` on `ubuntu-latest` and runs `make smoke-ffi` against it |
| linux/arm64 | not built |
| darwin/x86_64 | not built |
| **windows** | **not built. No DLL exists**, and nobody has tried. `patala_free`'s "not your `free()`" rule matters most there, where the CRT mismatch is a real crash rather than a rule |

The sidecar path needs only the `patala-sidecar` binary for your platform, and
choosing it because there is no library for yours is a supported outcome, not a
fallback.

## The two helper files

Neither is a component to reuse; they exist so these examples have no
dependencies:

- **[`jsonpeek.c`](jsonpeek.c)** is not a JSON parser. It scans for `"key":`
  and reads what follows, understanding `\"` and `\\` and nothing else. Real
  programs link cJSON, jansson or yyjson — patala speaks ordinary JSON, so any
  of them works unchanged. Keep `json_u64`'s integer discipline when you do.
- **[`mini_http.c`](mini_http.c)** is not an HTTP client. One request to
  `127.0.0.1` with `Connection: close`. No TLS, no chunked encoding, no
  keep-alive, no redirects, no retries, no SSE (there is nothing to stream).
  Real programs link libcurl.

## See also

- [`sdks/cpp`](../cpp/) — the same ABI with a destructor doing the freeing.
- [`sdks/rust`](../rust/) — no ABI at all; `use patala_core`.
- [`patala-ffi/README.md`](../../patala-ffi/README.md) — the library itself,
  its features, and its test story.
- [`patala-sidecar/README.md`](../../patala-sidecar/README.md) — the server,
  its threat model, and what its token does and does not defend against.
