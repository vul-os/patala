# patala-ffi

A plain `extern "C"` shared library over `patala-core`. JSON in, JSON out,
`uint64` handles, six exported symbols.

`patala-uniffi` covers the languages UniFFI has a backend for — Python, Go,
Swift, Kotlin, Ruby. It has **no backend for C, C++, Node/Deno/Bun, PHP or
Elixir**. Those load this instead:

| Host | How it loads this |
|---|---|
| C / C++ | link, or `dlopen` + `dlsym` — see [`include/patala.h`](include/patala.h) |
| Node / Deno / Bun | `node:ffi`/`ffi-napi`, `Deno.dlopen`, `bun:ffi` |
| PHP | the FFI extension (`FFI::cdef`) |
| Elixir / Erlang | a NIF wrapper, or a Port around a small C shim |
| anything else with a C FFI | the same six symbols |

It is not a second implementation. `patala_new`'s fiat and real-rail
configurations are built by `patala-uniffi`'s own constructors; the mock is
built straight off `patala-core`. Everything ends up as the same
`Arc<dyn patala_core::PaymentRail>` every other surface uses.

If in-process is the wrong shape for your host, [`patala-sidecar`](../patala-sidecar/)
— a loopback-only, token-gated HTTP server over the same core — is a supported
answer, not a fallback. It speaks the same JSON this library does.

## The advantage: no runtime in your process

This is the thing worth knowing about patala's C ABI, and it is worth being
precise about rather than vague.

`llmux` and `openrate` ship C ABIs too, and the suite deliberately uses the
same ABI shape across all three so a reader who learns one has learned the
others. But those two are **Go**, so their libraries are built with
`go build -buildmode=c-shared`, which puts the Go runtime inside the host
process. Their headers correctly warn about a garbage collector, a preemptive
scheduler, Go's own signal handlers, and the fact that the result is not
fork-safe — which breaks Python `multiprocessing`'s default `fork` start method
and pre-fork servers like uWSGI and Unicorn. (The signals are `SIGSEGV`,
`SIGBUS`, `SIGFPE`, `SIGPIPE` and `SIGURG`, plus `SA_ONSTACK` added to `SIGILL`,
`SIGXFSZ` and `SIGUSR2`. `SIGPROF` is *not* among them: under
`-buildmode=c-shared` Go's `sigInstallGoHandler` refuses everything but the
synchronous signals plus `SIGPIPE` and `SIGURG`.)

patala is Rust. **None of that applies here, and none of it has been copied
into this README.** Concretely:

- **No language runtime.** No GC, no scheduler, no green threads.
- **No signal handlers installed.** A JVM host or a profiling Python build has
  nothing to conflict with.
- **No threads started** — at load time or at any other time. Each handle owns
  a *current-thread* async runtime (patala-core's trait is `async`), which
  drives work on whichever thread called in and is dropped with the handle.
- **The library is fork-safe; a handle that is *in use* at the moment of the
  fork is not.** Nothing of ours is running at `fork()` time, so
  `multiprocessing` with `fork`, uWSGI and Unicorn need no special handling for
  the *library*. The handle is the narrow rule that is real: its runtime sits
  behind a mutex and `fork()` copies a locked mutex as locked, so with four
  parent threads charging on one handle an inherited handle hung **4–8 times in
  200** forks against **0 in 200** for one opened in the child (reproduced in
  Python and Ruby). The window is microseconds wide, so a test that forks once
  is a false green. **Open the handle in the child** — Unicorn's `after_fork`,
  clustered Puma's `on_worker_boot`, or simply per request. Full measurement:
  [`docs/c-abi.md`](../docs/c-abi.md#what-it-costs).
- **Nothing happens at load.** No socket, no file, no background task. The
  library is inert until called.

The thread claim is not a promise in prose: `ctest/smoke.c` counts the
process's threads before `dlopen`, after `dlopen`, and after a full
charge → verify round trip, and fails if the number ever goes up. On a platform
it does not know how to count threads on, it **fails** rather than skipping.

### Measured artifact size

`cargo build -p patala-ffi --release`, macOS arm64, this machine, 2026-08-09:

| Build | `libpatala_ffi.dylib` |
|---|---|
| default — mock rail only, fully offline | **844,656 bytes (0.81 MiB)** |
| `--features fiat-all` — 20 processor adapters, UniFFI, reqwest, TLS | 6,330,544 bytes (6.04 MiB) |

The default build is under a megabyte because it links `patala-core`, `serde`,
`serde_json` and tokio's `rt` feature and nothing else — not even
`patala-uniffi`, which is an *optional* dependency pulled in only by a rail
feature (see `Cargo.toml`). For comparison, the shared-ABI spec these three
products follow notes a Go `c-shared` library at 7–17 MB, and llmux's
`libllmux.dylib` on this same machine is **12,787,504 bytes** (~12.8 MB),
measured rather than quoted. That difference is a consequence of the language,
not of doing less:
the offline mock rail here is the same `MockRail` every other patala surface
exercises.

## The surface

```c
const char* patala_abi_version(void);
int         patala_abi_check(const char* expected, char** err);
uint64_t    patala_new(const char* config_json, char** err);   /* 0 = failure */
void        patala_close(uint64_t h);
char*       patala_call(uint64_t h, const char* method, const char* request_json, char** err);
void        patala_free(char* p);
```

[`include/patala.h`](include/patala.h) is the hand-written, supported
declaration, with the full documentation of every method and configuration
document. It is the file to read; this section is the summary.

- **JSON in, JSON out**, and it is the *same* JSON `patala-sidecar` serves,
  built from the same Rust types. A body that works against
  `POST /v1/rails/:id/charge` works against `patala_call(h, "charge", …)`
  unchanged. That is deliberate: the wire contract already has round-trip
  tests over a real socket, and one contract is easier to bind from a dozen
  languages than two.
- **Errors are plain UTF-8 strings**, never JSON, freed with `patala_free`
  like results. One free function for everything the library returns is the
  only rule a binding in twelve languages can be relied on to follow. Do not
  use your own `free()`: this is Rust's allocator.
- **Handles are `uint64` registry keys**, never pointers, and are **never
  reused**. A closed or invented handle is a clean error rather than a
  segfault in your process, and a use-after-close says so instead of landing
  on whatever rail took the slot.
- **`0` success / `-1` failure with `*err` set** for the int-returning
  function. `patala_new` inverts that only because its success value is a
  handle: handles start at 1, so `0` is its failure sentinel.
- **`method` is a string**, not one C function per operation, so the header
  stays stable as patala grows methods:
  `id`, `capabilities`, `quote`, `charge`, `verify`, `validate-destination`,
  `webhook`, `caveat`, `providers`.

There is **no streaming entry point**, and its absence is not an omission:
patala has no streaming operation. llmux's C ABI has `llmux_stream` because
chat streaming is its main event; nothing patala does produces a sequence of
chunks. If that changes, the callback shape to copy is llmux's.

## Money, and two things not to get wrong

Amounts are integer minor units plus a currency string, on both sides of the
boundary. Never a float.

**`verify` returning `{"valid": false}` is not an error.** It is the rail's
honest, fail-closed answer that a receipt does not hold. Gate entitlement on
`{"valid": true}` and nothing else, and do not treat a `false` as a transient
failure to retry — that is how an unpaid order becomes an entitlement.
`patala_call` returns a *result* there, not `NULL`, precisely so the two
cannot be confused.

**`validate-destination` never fails.** "I cannot check this address" comes
back as the verdict `{"status":"Unknown"}`, not as an error, because a caller
must handle it as carefully as a refusal and an error is too easy to swallow.
Read `is_refusal` (do not send) and `human_must_confirm` — which is `true` on
*every* verdict, including `StructurallyValid`. patala does not detect
exchange-owned addresses and will not guess.

## Try it

```bash
# From the workspace root.
cargo build -p patala-ffi
cc -std=c11 -I patala-ffi/include -o /tmp/smoke patala-ffi/ctest/smoke.c   # -ldl on Linux
/tmp/smoke target/debug/libpatala_ffi.dylib "$(cat VERSION)"
```

or, as one command that also builds the everything-on variant:

```bash
make smoke-ffi
```

A 30-line C program that charges and verifies:

```c
#include <stdio.h>
#include "patala.h"

int main(void) {
    char *err = NULL;
    uint64_t h = patala_new("{\"rail\":\"mock\",\"currencies\":[\"USDC\"]}", &err);
    if (!h) { fprintf(stderr, "%s\n", err); patala_free(err); return 1; }

    char *receipt = patala_call(h, "charge",
        "{\"amount_minor\":1250,\"currency\":\"USDC\","
        "\"destination\":\"mock:wallet:alice\",\"reference\":\"order-1\"}", &err);
    if (!receipt) { fprintf(stderr, "%s\n", err); patala_free(err); return 1; }
    printf("receipt: %s\n", receipt);

    char *verdict = patala_call(h, "verify", receipt, &err);
    printf("verify:  %s\n", verdict ? verdict : err);   /* {"valid":true} */

    patala_free(verdict);
    patala_free(receipt);
    patala_close(h);
    return 0;
}
```

## Features

`default = []`, and the default build is fully offline — it links no network
client and not even `patala-uniffi`.

| Feature | Adds |
|---|---|
| `solana` / `stellar` / `hyperswitch` | `{"rail":"solana"}` etc. |
| `fiat` | `{"rail":"fiat","provider":"manual"}` and the `providers` method |
| `fiat-<provider>` | one of `patala-fiat`'s 20 processor adapters |
| `fiat-all` | all twenty |

Asking for a rail this build has no feature for is refused **by name**, naming
the missing feature — never a silent fallback to a different rail.
`scripts/check-features.sh` fails the workspace build if this crate's
`fiat-<name>` list drifts from `patala-fiat/src/`.

## Tests

```bash
cargo test -p patala-ffi                    # 23 tests
cargo test -p patala-ffi --features fiat-all # 25
make smoke-ffi                               # 55 checks through C, twice
```

The Rust tests cover the registry, the configuration (including that a
misspelled field is *refused* rather than defaulted), and every method's
dispatch. They cannot cover the C surface itself — a Rust test calls the Rust
function directly and never crosses the boundary — which is what `ctest/smoke.c`
is for, and why it asserts the number of checks it ran rather than merely
returning 0.

### Verified in this environment (2026-08-09)

- `cargo test -p patala-ffi` — 23/23; `--features fiat-all` — 25/25;
  `--features fiat-all,solana,stellar,hyperswitch` — 25/25.
- `make smoke-ffi` — **55/55 checks** against the default library and 55/55
  against the `fiat-all` one, both dlopened from C.
- The smoke test was mutation-tested, not just run: renaming
  `patala_free`'s export made it fail at `dlsym`, and making a tampered
  `verify` return an error instead of `{"valid":false}` made it report both the
  failed check and a check count of 54 against the expected 55.
- `cargo tree -p patala-ffi -e normal` on the default features pulls in no
  `reqwest`, no `patala-uniffi`, no `patala-fiat`.
- **linux/amd64**: the `.so` is built and `make smoke-ffi` dlopens it from C on
  `ubuntu-latest` in CI's `c abi` job, in both passes — so that row is
  exercised, not assumed. What has never run there is any of the fifteen
  `sdks/` language packages. **UNVERIFIED**: linux/arm64 and darwin/amd64 are
  not built at all, and **Windows has no DLL and nobody has tried** —
  `patala_free`'s "not your `free()`" rule matters most there. Every number on
  this page was measured on macOS arm64.
- **UNVERIFIED AGAINST LIVE** for every real rail, same as the rest of the
  workspace: the C tests only ever drive `MockRail`, and the fiat build is
  exercised by construction only.
