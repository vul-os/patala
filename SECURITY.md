# Security Policy

patala is a sovereign payment-rail library that moves value without holding funds. Security reports are taken seriously and handled with priority.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: [GitHub private vulnerability reporting](https://github.com/vul-os/patala/security/advisories/new) on `vul-os/patala`.
- Alternatively, email **vulosorg@gmail.com** with `[patala security]` in the subject.

You will get an acknowledgement within **72 hours** and a status update at least every **14 days** until resolution. Please allow a reasonable window to ship a fix before public disclosure.

## Scope

- **Value movement** — any path that moves, redirects or double-spends value without authorization.
- **Receipts & provenance** — forging or tampering with signed receipts.
- **Key & credential handling** — leaking or mishandling rail credentials or signing keys.
- **Adapter boundaries** — a hostile rail adapter affecting a vendoring product beyond its interface.

Out of scope: vulnerabilities requiring an already-compromised host, and issues in third-party services the operator configures.

## Release artifacts

Pushing a `v*` tag runs `.github/workflows/release.yml`, which publishes exactly four
things and vouches for all of them:

| Asset | What it is |
|---|---|
| `patala_<v>_source.zip` | the archive of the tag |
| `patala_<v>_c-abi_linux_amd64.tar.gz` | `lib/libpatala_ffi.so` + `include/patala.h` |
| `patala_<v>_c-abi_darwin_arm64.tar.gz` | `lib/libpatala_ffi.dylib` + `include/patala.h` |
| `SHA256SUMS` | one line per asset above |

**Verify before you run any of it:**

```sh
curl -fsSLO https://raw.githubusercontent.com/vul-os/patala/<tag>/scripts/verify.sh
bash verify.sh --tag <tag> --attest patala_<v>_c-abi_linux_amd64.tar.gz
```

`scripts/verify.sh` fetches `SHA256SUMS`, looks up the **exact** entry for the asset
(string comparison on field 2, so `…tar.gz` can never be answered by `…tar.gz.sig`) and
compares digests. It has two outcomes: verified, or non-zero with a diagnostic naming what
was wrong. There is **no `--skip-verify`**, and **no path where an absent `SHA256SUMS`
means "nothing to check"** — that shrug converts *"I don't know"* into *"it's fine"*, which
is the bug the file exists not to have. `bash scripts/verify.sh --selftest` proves it:
24 synthetic-origin cases, each asserting an exit code **and** that a diagnostic was
printed. CI runs that matrix on every push, not only at release time.

The release also carries a **sigstore build-provenance attestation**, minted from the
workflow's OIDC identity — no long-lived signing key exists to leak, own or rotate, and
what it binds is *"vul-os/patala's release workflow at this commit"* rather than *"whoever
holds the key"*. Check it with `verify.sh --attest` (needs the `gh` CLI). It is
deliberately not load-bearing: the digest path needs only `curl` and `sha256sum`, and a run
**without** `--attest` says provenance was not checked rather than letting a pass imply
more than it checked.

Two properties worth naming, because they are what make the manifest mean something:

- every published asset is staged into `release/` and the manifest is emitted **over that
  directory**, so "published" and "covered" are the same set by construction rather than
  two hand-maintained lists — and the job fails if the manifest's line count does not equal
  the number of staged assets, or if nothing was staged at all;
- the release job runs `verify.sh` against **its own output** before publishing, so the
  producer and the consumer cannot drift apart without a red release.

### What is still not published

- **The `v0.1.1` tag predates this workflow**, so that release has no assets and no
  manifest. The workflow fires on tag *push*; the first release carrying artifacts will be
  the next tag.
- **linux/amd64 and darwin/arm64 only.** Nothing cross-compiles here: each bundle is built
  on the platform it names, because the packaging step dlopens the library it just built
  and drives a real charge → verify round trip through it (`patala-ffi/ctest/smoke.c`,
  58 counted checks). A platform CI cannot both build *and* execute is a platform patala
  does not claim. linux/arm64 and darwin/amd64 are absent for that reason and no other.
- **No crate on crates.io** — `patala-ffi`, `patala-py`, `patala-sidecar` and
  `patala-uniffi` set `publish = false`, and nothing publishes the rest. Rust consumers
  vendor by path or by git.
- **No Python wheel.** `patala-py/pyproject.toml` still declares `0.1.0` against a `0.1.1`
  crate, and nothing builds a manylinux wheel; a wheel attached to a GitHub Release would
  not be `pip install`-able anyway.
- **No sidecar binary.** `patala-sidecar`'s registry is mock-only today (see
  `patala-sidecar/src/registry.rs`), so publishing it would ship a payments daemon that
  cannot make a payment.
- **No container image, no npm package, no install script.**
- `patala-go` is an in-repo Go module (`github.com/vul-os/patala/patala-go`). If it is ever
  consumed through the module proxy, its integrity comes from Go's own `go.sum` and the
  checksum database, not from anything this repo emits.

## Supported versions

Pre-1.0: only the latest release (and `main`) receives fixes.
