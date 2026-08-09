# patala (Go)

**There is no binding in this directory.** [`../../patala-go`](../../patala-go)
is the Go binding — `uniffi-bindgen-go` output from the one
`#[uniffi::export]` surface in `patala-uniffi` — and this directory is runnable
examples of it, plus the sidecar alternative. Read
[`../../patala-go/README.md`](../../patala-go/README.md) for the binding: how
the bindings are generated, the pinned `uniffi-bindgen-go` tag, the `fiat`
build tag, and its test gate.

| mode | what it is | example | cgo |
| --- | --- | --- | --- |
| **Direct** | `patala-go` linked into your binary | [`examples/direct/main.go`](examples/direct/main.go) | **required** |
| **Sidecar** | `patala-sidecar` as a separate process, HTTP over loopback | [`examples/sidecar/main.go`](examples/sidecar/main.go) | none — `CGO_ENABLED=0` |

Both drive `MockRail`: deterministic, offline, no credentials. patala is a
payments library, so an example that moves real value is not an example.

```sh
sdks/go/examples/run.sh            # both
sdks/go/examples/run.sh direct
sdks/go/examples/run.sh sidecar
```

## Which one to pick — and here Go is the odd one out

For most languages patala's in-process path is close to free: it is Rust, so
there is no runtime in your process, no GC, no scheduler, no signal handlers,
no fork hazard, and the mock-only C ABI is 844,656 bytes. Go does not get that
deal, because reaching a Rust library from Go means **cgo**, and cgo costs the
things Go people chose Go for:

- `CGO_ENABLED=1` is mandatory. Verified here, and it fails cleanly rather than
  producing a mystery:

  ```
  $ CGO_ENABLED=0 go build ./examples/direct
  package github.com/vul-os/patala/sdks/go/examples/direct
      imports github.com/vul-os/patala/patala-go/bindings/patala:
      build constraints exclude all Go files in .../patala-go/bindings/patala
  ```

- A C toolchain at build time, not just a Go one.
- **No static single binary.** The result dynamically links
  `libpatala_uniffi.{dylib,so}`, at the build-time absolute path on macOS, so
  the library travels with the binary or you fix up the install name.
- Cross-compilation stops being `GOOS=… GOARCH=… go build`.

So, unusually for this SDK set: **if your Go program prizes a static binary,
take the sidecar.** `cackle` is the concrete case in this suite, and
`patala-go/README.md` says so in its own opening paragraph rather than burying
it. Take the direct path when you want native Go types (`uint64` minor units,
`RailClass`, typed `PatalaError` variants) and control the build environment.

The sidecar's other argument is the same one it has everywhere and is worth
more than latency: **key isolation.** A non-custodial rail's signing key lives
in whichever process calls `charge`. One sidecar means one process holds it.
See [`../../patala-sidecar/README.md`](../../patala-sidecar/README.md#threat-model),
including what it does not defend against.

## Direct

Needs the generated bindings (`make -C patala-go generate`, which needs
`uniffi-bindgen-go` at the pinned tag `v0.5.0+v0.29.5`). `examples/run.sh
direct` generates them if they are missing.

```sh
cd sdks/go
CGO_ENABLED=1 \
  CGO_LDFLAGS="-lpatala_uniffi -L../../patala-go/bindings/patala" \
  DYLD_LIBRARY_PATH="../../patala-go/bindings/patala:$DYLD_LIBRARY_PATH" \
  LD_LIBRARY_PATH="../../patala-go/bindings/patala:$LD_LIBRARY_PATH" \
  go run ./examples/direct
```

Real output, 2026-08-09, go1.25 on darwin/arm64 (`ld: warning: ignoring
duplicate libraries` before it is the linker, harmless, and comes from the
`-lpatala_uniffi` that the generated cgo preamble already supplies):

```
go go1.25.6 on darwin/arm64, cgo linked in (this binary cannot be built with CGO_ENABLED=0)

capabilities
  ok  Id() == "mock"
  ok  Class is NonCustodialFinal — a wallet address and a final receipt, not a card form
  ok  HoldsFunds is false — patala never holds funds
  ok  Reversible is false — there is no refund on this rail

pre-flight: ValidateDestination, before any money moves
  ok  a well-formed address gives StructurallyValid (printed as 4 — see the comment)
  ok  IsRefusal is false — a field, never re-derived from Status with a switch that can fall through
  ok  HumanMustConfirm is true even here — patala does not detect exchange-owned addresses
  ok  an empty destination is a Malformed refusal — returned as a verdict, never as an error

Quote -> Charge -> Verify
  ok  TotalMinor == 1250, a uint64 of minor units — never a float
  ok  Charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies true
  ok  a tampered receipt verifies (false, nil) — fail-closed, and false is DATA, not an error

errors are typed, never a panic
  ok  an unsupported currency is PatalaError.InvalidRequest: PatalaError: InvalidRequest: Message=rail mock does not support currency EUR

webhooks: a rail with no push delivery says so
  ok  the mock refuses with Unsupported rather than inventing a WebhookEvent

ALL 14 GO DIRECT ASSERTIONS PASSED
```

`Status` printing as `4` rather than `StructurallyValid` is UniFFI's enum
lowering: variants become ordinals and the generated Go type is integer-backed
with no `String()`. Compare against the named constant, never against the
number — the number is a position in a Rust enum, and `patala-go`'s
`bindingtest` pins those positions for exactly this reason.

## Sidecar

```sh
cargo build -p patala-sidecar
cd sdks/go && CGO_ENABLED=0 go run ./examples/sidecar
```

The example generates a token, picks a free port, spawns the server, waits for
`/healthz`, runs the round trip with `net/http` + `encoding/json`, and
terminates it. Real output, same date:

```
binary:   /Users/pc/code/vulos/patala/target/debug/patala-sidecar
listening on http://127.0.0.1:56516 (loopback only — the bind address is not configurable)
go go1.25.6 on darwin/arm64, CGO_ENABLED is irrelevant here

capabilities
  ok  GET /v1/rails/mock -> 200
  ok  class is "NonCustodialFinal" — decide the whole UX off this, not off a provider name
  ok  holds_funds is false

pre-flight: validate-destination, before any money moves
  ok  a well-formed address -> 200 "StructurallyValid"
  ok  is_refusal is false — read the body, not just the status code
  ok  human_must_confirm is true even on StructurallyValid
  ok  an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal

quote -> charge -> verify
  ok  total_minor == 1250, decoded into a uint64 — minor units, never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies {"valid": true}
  ok  a tampered receipt is 200 {"valid": false} — fail-closed, and NOT an HTTP error

the error surface, so you can tell these four apart
  ok  an unsupported currency -> 400 "invalid_request"
  ok  an unknown rail_id -> 404 "unknown_rail"
  ok  the mock has no push delivery -> 501 "unsupported", never an invented event
  ok  no Authorization header -> 401 on a READ-ONLY route too

sidecar terminated; nothing left running

ALL 15 GO SIDECAR ASSERTIONS PASSED
```

Two things worth copying out of that file into your own code:

- **Keep `proof` as `json.RawMessage`.** A `Receipt` is an opaque token you
  hand back to `/verify` unchanged. Model it loosely, re-encode it, and a
  genuine receipt stops verifying.
- **A non-2xx is an answer, not a transport failure.** `{"valid": false}`
  arrives as `200`; `501` means this rail has no push delivery; `401` guards
  the read-only capabilities route too. Branch on all four.

**The sidecar's rail registry is mock-only today** — any other `rail_id` is a
`404`. That is a gap in the sidecar, not in these examples.

## Hazards that do NOT apply here

llmux's and openrate's Go SDK pages carry a list of Go-runtime caveats. Those
products *are* Go, and their C ABIs are `-buildmode=c-shared`; the caveats are
theirs. Coming the other way — Go calling Rust — none of it applies, and it has
not been copied:

- **No second runtime.** `libpatala_uniffi` starts no GC and installs no signal
  handlers, so Go's preemption and its `SIGURG`/`SIGPROF` machinery are
  untouched.
- **No fork hazard.** Go programs rarely fork without exec, but the property is
  real and was measured from Python, where forking is routine: a `fork()`ed
  child ran charge and verify through the C ABI in 0.00 s, where the same probe
  against `libllmux` hung on the first real call. See
  [`../python/README.md`](../python/README.md).
- One cost *is* real and shared with the other bindings: the UniFFI surface
  blocks on a **lazily started, process-wide multi-thread tokio runtime** with
  two workers, created on the first call rather than at load.

## What was actually built

`libpatala_uniffi.dylib` for **darwin/arm64**, on this machine, by
`make -C patala-go generate`. Nothing else was produced here: no Linux `.so`,
no Windows DLL, no darwin/amd64. The CI job builds the Linux side; Windows is
untried. The sidecar path needs only the `patala-sidecar` binary for your
platform and has no such matrix.

## Files

| file | mode | what it shows |
| --- | --- | --- |
| `examples/direct/main.go` | direct | capabilities, destination pre-flight, quote → charge → verify, tamper detection, typed errors, `VerifyWebhook` refusing with `Unsupported` |
| `examples/sidecar/main.go` | sidecar | spawn + healthz + shutdown, the same round trip over HTTP, and all four error codes (400 / 404 / 501 / 401) |
| `examples/run.sh` | both | generates bindings / builds the sidecar if missing, then runs either or both |
