# patala language packages

Use patala from any of fifteen languages, **two ways**:

- **Direct** — in-process. Eleven of the fifteen load a C ABI shared library
  (`patala_new`/`call`/`close`/`free`/`abi_version`/`abi_check`), built by
  [`patala-ffi`](../patala-ffi/README.md). Python, Go and Kotlin instead use
  typed UniFFI bindings generated from [`patala-uniffi`](../patala-uniffi/) —
  packaged as [`patala-py`](../patala-py/) and [`patala-go`](../patala-go/),
  and generated in place by [`kotlin/`](kotlin/). Rust has no boundary at all —
  it takes a Cargo dependency.
- **Sidecar** — [`patala-sidecar`](../patala-sidecar/) as a separate process,
  either one you run or one the package spawns and manages for you on
  `127.0.0.1`.

**There is no streaming API,** in either mode, in any language. patala has no
streaming operation, so there is no `patala_stream` and no `AsyncSequence`,
`Flow`, `IAsyncEnumerable` or `for await` anywhere here. The omission is
deliberate. (llmux shares this ABI shape and *does* have `llmux_stream`; do not
go looking for patala's.)

**The "Default" column is a real recommendation, not a formality** — and for
Java and Kotlin it is **the reverse of what the same page says in llmux and
openrate**. That reversal was measured, not reasoned about; see below.

| Language | Direct | Sidecar | Default |
|---|---|---|---|
| [rust](rust/) | a Cargo dependency — **no FFI, no shared library, no `unsafe`** | ✓ | **direct** — the one language here where that carries no asterisk |
| [c](c/) | ✓ links `libpatala_ffi` | ✓ forks and reaps it | **direct** |
| [cpp](cpp/) | ✓ header-only RAII (`patala.hpp`, `patala::Rail`) | ✓ forks and reaps it | **direct** |
| [swift](swift/) | ✓ `dlopen` + `@convention(c)` — no module map, no `unsafeFlags` | ✓ spawned, `URLSession` | **direct** on macOS |
| [java](java/) | ✓ FFM (JDK 22+) | ✓ managed | **direct** — *reversed from llmux/openrate* |
| [kotlin](kotlin/) | ✓ generated UniFFI over JNA — loads `libpatala_uniffi`, **not** the C ABI | ✓ managed, through the Java client | **direct** — *reversed from llmux/openrate* |
| [node](node/) | ✓ koffi, with a working `callAsync` | ✓ managed | direct — its README argues both; key isolation is the one reason to move |
| [deno](deno/) | ✓ `Deno.dlopen`, `callAsync` declared `nonblocking` | ✓ managed | direct — same, and `--allow-ffi` is effectively "allow anything" |
| [bun](bun/) | ✓ `bun:ffi` — synchronous only, `bun:ffi` offers nothing else | ✓ managed | direct — same |
| [python](python/) | ✓ [`patala-py`](../patala-py/) — **UniFFI, not the C ABI** | ✓ example | direct — the fork reason the siblings cite does not exist here |
| [ruby](ruby/) | ✓ `fiddle` (stdlib — no gem) | ✓ managed | direct — Unicorn and clustered Puma are **not** disqualifying here |
| [php](php/) | ✓ the `FFI` extension (`FFI::cdef`) | ✓ managed | **depends on your `php.ini`** — see README |
| [dotnet](dotnet/) | ✓ `LibraryImport` + `SafeHandle` | ✓ managed | **sidecar** — and the reason is Windows, not the runtime |
| [go](go/) | ✓ [`patala-go`](../patala-go/) — UniFFI **over cgo** | ✓ | **sidecar** if a static binary matters |
| [elixir](elixir/README.md) | ✓ a dirty-IO NIF over the C ABI — **the siblings ship none** | ✓ managed | **sidecar** — for BEAM reasons, not patala reasons |

## Why the defaults differ from llmux's and openrate's

llmux and openrate are **Go**. Their shared libraries are built with
`go build -buildmode=c-shared`, which puts the Go runtime — its GC, its
scheduler, its signal handlers — inside whichever process loads them. Six of
their fifteen packages default to the sidecar because of it.

**patala's core is Rust. It has no runtime to carry.** No GC, no scheduler, no
green threads, no signal handlers, no threads started at load or at any other
time, and nothing that runs before your `main`. That is not a claim inherited
from the language; every package below measured it, and each measurement was
taken against a Go library in the *same* environment as a control.

### Java and Kotlin: direct, and that is the reversal

The whole argument for the sidecar in llmux's and openrate's Java packages is
`libjsig`: loading a Go `c-shared` library replaces five of HotSpot's signal
handlers, `libjsig` fixes it cleanly, and `libjsig` is a flag on the **java
launch command** — which a library cannot add to a process that has already
started. A drop-in dependency becomes an operations change.

[`java/signal-probe.sh`](java/signal-probe.sh) is llmux's own probe pointed at
`libpatala_ffi`, so the two results are comparable rather than merely both
quoted. Run here on OpenJDK 26.0.2, macOS 15.7.3 / arm64:

```text
0 handler(s) replaced, 0 left in place with altered flags

threads in this process:
  before dlopen:        23
  after dlopen:         23
  after a round trip:   23
```

All thirteen probed signals — `SIGSEGV` and `SIGUSR2` included — come back
`unchanged`, before *and* after a full `charge` → `verify` round trip, because
a handle's runtime is created lazily and "nothing at load time" would be the
weaker claim. The same probe against llmux reports **5 replaced, 3 with altered
flags**. Under `-Xcheck:jni` llmux prints `Warning: SIGSEGV handler modified!`
and four more, ending `Consider using jsig library.`; patala prints nothing.

So `libjsig` is not needed, the argument that made the siblings choose the
sidecar does not exist here, and **direct is the recommended default on the
JVM**. Kotlin inherits the verdict but no longer the binding: it used to be a
thin layer over the Java C-ABI classes and is now the *generated UniFFI Kotlin*
(see [Two libraries, not one](#two-libraries-not-one)). The signal measurement
carries over because it is a property of the Rust cdylib, not of which of the
two cdylibs you load.

### Node: `worker_threads` and `callAsync` actually work

llmux's and openrate's Node packages are synchronous-only because a Node thread
that has entered a Go `c-shared` library never terminates — the worker answers
correctly and then hangs the process at exit forever.

Measured in [`node/README.md`](node/README.md), with a ten-line reproduction
carrying no patala logic in it at all:

| | `libpatala_ffi` (Rust) | a Go `c-shared` control |
|---|---|---|
| `worker_threads` worker | **exits 0 in ~33 ms** | answers, then **never exits** — killed at 15 s |
| koffi `.async` | **works, process exits 0** | answers, then hangs — killed at 12 s |
| process threads | **7 → 7** | 7 → 13 |

That is why this package ships `callAsync` and the siblings could not. It is
worth using for a rail that talks to a network and worth nothing for the mock
rail, which answers in 0.44 ms.

### Size

`cargo build -p patala-ffi --release`, macOS arm64, measured here:

| Library | Bytes |
|---|---|
| `libpatala_ffi.dylib`, default — mock rail only, fully offline | **844,656** |
| `libpatala_ffi.dylib`, `--features fiat-all` — 20 processor adapters, UniFFI, reqwest, TLS | 6,330,544 |
| llmux's `libllmux.dylib`, for comparison | 12,787,504 |

**15.1× smaller** on the default build, and it is a consequence of the language
rather than of doing less: the offline mock rail here is the same `MockRail`
every other patala surface exercises. The Kotlin package is where that figure
cuts both ways: generated UniFFI Kotlin is a `com.sun.jna.Library`, so it ships
JNA's own per-platform native stub *as well as* the cdylib — two native
artifacts, which its README states plainly rather than netting out.
`libpatala_uniffi.dylib`, the one Kotlin loads, is **881,696 bytes**.

## Two libraries, not one

Worth knowing before you read a package README and find a filename you did not
expect. patala has **two** in-process surfaces, and the packages split across
them:

| Surface | What it is | Who loads it |
|---|---|---|
| `libpatala_ffi` | a plain `extern "C"` cdylib — JSON in, JSON out, `uint64` handles, six symbols | c, cpp, swift, java, node, deno, bun, ruby, php, dotnet, elixir |
| `libpatala_uniffi` / `patala-py` / `patala-go` | typed UniFFI bindings generated from one `#[uniffi::export]` surface | python, go, kotlin — and, alongside its C-ABI package, [swift/uniffi](swift/uniffi/) |
| — | no boundary | rust |

The JSON the C ABI speaks is the *same* JSON `patala-sidecar` serves, built
from the same Rust types, so a body that works against
`POST /v1/rails/:id/charge` works against `patala_call(h, "charge", …)`
unchanged. The UniFFI route gives real records and real enums instead —
`RailClass`, `DestinationStatus`, five typed `PatalaError` variants — which is
why Python, Go and Kotlin take it rather than binding the C ABI a second time.

UniFFI's **Kotlin** backend was blocked until commit `79e5002`: `patala_core::Error`
had two variants with a field named `message`, and UniFFI renders a flat error
enum as a subclass of `kotlin.Exception`, which already has an open `message`
property — the field was emitted twice and `kotlinc` gave 12 errors. Renaming
the field to `detail` was the honest fix; patala has never been tagged, so this
was the cheapest moment in its life to change a public field name. **That
unblocked the Kotlin package, which is now the generated bindings themselves**
— the old Java-FFM wrapper is deleted, not deprecated — and
[`kotlin/uniffi-kotlin-probe.sh`](kotlin/uniffi-kotlin-probe.sh) keeps the
`detail` rename from being re-litigated: its exit code is **inverted**, so it
fails the moment the upstream bug is fixed and the name can be reconsidered.

**Ruby is a UniFFI backend patala can now generate for and does not.** It was
blocked for the same family of reason as Kotlin: uniffi 0.29.5's Ruby backend
renames an argument colliding with a language keyword in the `def` line but not
in the body, so `RailCapabilities`' `class` field emitted `class = class` and
the whole generated file failed `ruby -c`. Renaming that field to `rail_class`
(commit `1e4374e`) cleared it, and **all five UniFFI backends — Python, Go,
Kotlin, Swift, Ruby — now generate working code for patala.** The upstream bug
itself is untouched: `make probe-ruby`
([`scripts/uniffi-ruby-probe.sh`](../scripts/uniffi-ruby-probe.sh)) still
reproduces it from an isolated UDL, and its exit code is **inverted**, so it
now reports the fixed case rather than the broken one. [ruby/](ruby/) stays on
the C ABI by choice, not by force: `fiddle` is in the stdlib, so direct mode
adds nothing to a Ruby dependency graph and needs no generation step.

## Costs that are real, and are not the siblings' costs

Do not carry llmux's or openrate's caveats over. Fork-unsafety, JVM signal
conflicts, a Node thread that never terminates, `dlclose` hanging — those are
true for a Go-cored library and false here, and each was re-measured rather
than assumed. These are patala's:

- **A current-thread Tokio runtime per handle.** `patala-core`'s trait is
  `async`, so each C ABI handle owns a runtime built in `patala_new` and
  dropped with the handle. It starts no threads — it drives futures on whichever
  thread called in. The consequence worth planning around is that **calls on
  one handle serialise**; calls on different handles run concurrently. Open one
  handle per rail, and more than one if you want parallelism on the same rail.
  **The two surfaces differ here, and the difference is visible in a thread
  count**: `patala-ffi` starts no threads at all, while `patala-py`/`patala-go`
  block on one lazily-started, process-wide, multi-thread runtime that brings up
  **two worker threads on the first call** (construction is inert in both).
  MockRail survived a fork through the UniFFI path too, but only because
  `block_on` drives the future on the calling thread and MockRail spawns
  nothing; a rail doing real network I/O may reach for those workers.
  **UNMEASURED** — no live rail was reachable from this environment, and that is
  a gap rather than a result.
- **cgo, if and only if you choose the Go binding.** `CGO_ENABLED=1` becomes
  mandatory, a C toolchain joins the build, the binary stops being a
  self-contained static executable, and cross-compilation stops being
  `GOOS=… GOARCH=… go build`. **`cackle` is the first real Go consumer in this
  suite and it chose the sidecar for exactly these reasons** —
  [`patala-go/README.md`](../patala-go/README.md) says so in its opening
  paragraph rather than burying it.
- **`rustc` stamps the cdylib's `LC_ID_DYLIB` with an absolute build-tree
  path.** `otool -D` on the release library prints
  `…/target/release/deps/libpatala_ffi.dylib`, which is where a linked binary
  will go looking for it on any other machine. The C and C++ Makefiles fix it
  with `install_name_tool -change … @rpath/libpatala_ffi.dylib`, and a packager
  should fix the library itself with
  `install_name_tool -id @rpath/libpatala_ffi.dylib libpatala_ffi.dylib` — which
  invalidates the code signature, so re-sign afterwards. The packages that
  `dlopen` at runtime rather than linking (swift, deno, bun, node, ruby, php,
  elixir) never meet this.
- **The library is fork-safe; a handle that is *in use* at the moment of the
  fork is not.** Say it in that shape, because the flat claim is too strong and
  the flat denial is llmux's problem, not patala's. The library side is not
  close: same machine, same Python, same harness, `libpatala_ffi` sits at **1
  thread** after `dlopen` and still 1 after a full round trip, and a forked
  child ran charge, verify and a fresh handle in 0.00 s — where `libllmux` goes
  to 7 threads, answers `models` in the child, and then **hangs on `chat`**,
  never answering before the probe's watchdog (5 s by default,
  `PATALA_FORK_TIMEOUT`) `SIGKILL`ed it. Real php-fpm (`pm = static`, `opcache.preload` loading *and
  charging through* the library in the master before the fork) answered **24
  requests, 0 hung**, including on the handle the master itself opened — llmux's
  exact failure scenario with the opposite outcome. The handle side is the real
  rule, and it is now quantified: with four parent threads charging on the same
  handle, over 200 forks, an **inherited handle hung 4–8 times in 200**
  (reproduced independently in Python and Ruby) and a **fresh handle opened in
  the child hung 0 in 200**. The runtime sits behind a mutex and `fork()` copies
  a locked mutex as locked. It is a race against a microsecond-wide window, so
  **a test that forks once is a false green** — which is exactly why the coarse
  claim survives casual checking. Load the library where you like; open the
  handle in the child (Unicorn's `after_fork`, clustered Puma's
  `on_worker_boot`, or simply per request).
- **Latency is not the reason to embed.** In-process is microseconds against
  tens of microseconds over loopback, next to a rail that talks to a chain or a
  processor. The reasons are: no second process, no port, no loopback surface.
  **The reason *not* to embed is the signing key** — route several services
  through one sidecar and the key lives in exactly one narrowly-scoped process
  instead of in all of them.

## Prebuilt libraries — what actually exists

Direct mode needs a shared library for your platform — `libpatala_ffi` for the
eleven C-ABI packages, `libpatala_uniffi` for kotlin, and their own cdylibs for
python and go. Today:

| Target | Status |
|---|---|
| darwin/arm64 | **built and executed** — every number on this page was measured on it |
| linux/amd64 | the `.so` is built and the C smoke test runs on it in CI's `c abi` job; **no package in this directory has been run there.** (CI's `python binding` and `go binding` jobs do build and execute the *UniFFI* bindings on `ubuntu-latest` — `patala-py`'s and `patala-go`'s own suites, not these example scripts.) |
| **linux/arm64** | **not built** |
| **darwin/amd64** | **not built** |
| **windows/amd64** | **does not exist — no DLL ships, and nobody has tried** |

Say the Windows row plainly, because Node, .NET and PHP all have large Windows
install bases: nothing here has been run on Windows. That is the whole reason
the **.NET** package defaults to the sidecar — not the Go-runtime argument its
llmux counterpart uses, which does not apply, but the flat fact that there is
no DLL to load. `patala_free`'s "this is Rust's allocator, not your `free()`"
rule is also the one that would bite hardest there, where a CRT mismatch is a
real crash.

**llmux's and openrate's matrices are different from this one and from each
other.** Do not assume one covers another. Build your own with
`cargo build -p patala-ffi --release` (add `--features fiat-all` for the
processor adapters).

The sidecar path has none of these constraints — it needs only the
`patala-sidecar` binary for your platform.

## The one thing that can override every recommendation above

**The sidecar's rail registry is mock-only today.** `default_registry()`
registers exactly one rail, `"mock"`; a request naming any other `rail_id` gets
a `404`. The server, its auth, its error mapping and all six endpoints are real
and exercised over a real socket — but **direct mode is the only path to a real
rail right now**, whatever a package's Default column says. The per-rail
registration is unwritten work rather than a design problem; adding a rail
changes no route, no handler and no wire format.
[`docs/status.md`](../docs/status.md) tracks it.

## Related documents

- [The C ABI](../docs/c-abi.md) — the six functions, the ownership rules and
  the costs, documented once.
- [Language packages](../docs/language-packages.md) — the same fifteen, with a
  run command for each.
- [Choosing a mode](../docs/choosing-a-mode.md) — crate, binding, C ABI or
  sidecar, side by side.
- [One core, every language](../docs/polyglot.md) — why there is one
  implementation and not fifteen.
