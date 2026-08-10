# Changelog

All notable changes to patala are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

## [0.1.2] — 2026-08-10

**The first patala release that publishes anything.** `v0.1.1` was tagged before
a release workflow existed, so it produced no artifacts and nothing a consumer
could verify. This tag is the first one that does.

### Added

- **`.github/workflows/release.yml`.** A `v*` tag now builds, checksums, attests
  and publishes: a source archive, C ABI bundles for linux/amd64 and
  darwin/arm64, and `SHA256SUMS` over the staged directory. Assets are staged
  into `release/` and the manifest is emitted **over that directory**, so
  "published" and "covered" are the same set by construction rather than two
  hand-maintained lists. The job is red if nothing staged, or if the manifest's
  line count is not the staged count.

  The C ABI bundle ships the library **and** the header in one archive —
  separable downloads eventually get paired wrong — built with all rails on,
  because a prebuilt-library consumer cannot add one later. Two platforms only,
  and for a structural reason rather than a to-do: the packaging step `dlopen`s
  the library it just built and drives a real charge → verify round trip through
  it, so nothing cross-compiles.

  Excluded and stated in `SECURITY.md`: the Python wheel (a wheel on a Release
  page is not `pip install`-able anyway) and the sidecar binary (its registry is
  mock-only — it would be a payments daemon that cannot make a payment).
- **`scripts/verify.sh`** — the consumer half. Exact field-2 name match, no
  `--skip-verify`, and **no path where an absent `SHA256SUMS` means "nothing to
  check"**; that shrug turns "I don't know" into "it's fine". 24-case
  synthetic-origin failure matrix under `--selftest`.

  Worth recording why the name match is exact: swapping it for a substring
  `grep` was mutation-tested and **reported VERIFIED for bytes vouched for only
  as a `.sig`**.
- **`scripts/release-stage.sh`** — refuses on tag/`VERSION` disagreement, a
  stale crate version, a host that is not the platform the asset name claims,
  a rail missing from the published feature set, wrong library magic bytes, or a
  tarball whose contents are not exactly the expected set. Every refusal was
  broken and observed before being trusted.
- **CI job `release-tooling` and `make release-selftest`** — both failure
  matrices run on every push, not only at release time.

### Fixed

- **`patala-py/pyproject.toml` declared `0.1.0` against a `0.1.1` crate.** The
  wheel would have reported a version its own library did not — the same class
  of lie as a stale ABI string. The stager's version sweep now reads
  `pyproject.toml` alongside the nine `Cargo.toml`s, since the Cargo sweep
  structurally could not see it.

### Changed

- `SECURITY.md`'s "Release artifacts: there are none" is now accurate, along
  with the six pages and the landing that repeated it.

## [0.1.1] — 2026-08-10

**patala's first published release.** `[0.1.0]` below is a real entry — a
versioned cut recorded on 2026-07-21 — but it was never tagged, so nothing was
ever fetchable. Rather than retroactively tag a section describing a different
tree, this is cut as 0.1.1 and 0.1.0 stays as the development record it is.

Two public field names changed on the way here, and both were free only because
nothing had shipped: `PatalaError`'s `message` → `detail`, and
`RailCapabilities`'s `class` → `rail_class`. Each unblocked a UniFFI backend
whose generated code did not compile — Kotlin in the first case, Ruby in the
second — and UniFFI mangles both per language (`_class` in Python, `Class` in
Go), so after a tag either rename would have broken every binding at once. All
five UniFFI backends now generate and run: Python, Go, Kotlin, Swift, Ruby.

### Security

Everything here was found by a review commissioned specifically because patala
had never been tagged — the one moment when changing a public contract costs
nothing. All of it is mutation-tested; the quoted strings are the actual
failures the tests emit when the fix is reverted.

- **A non-UTF-8 byte anywhere in a `patala_new` configuration silently produced
  a `MockRail`.** `as_str` collapsed invalid UTF-8 to `""`, and an empty
  document means "the offline default". Demonstrated against the real ABI: a
  config asking for Stripe returned `handle=1` with `err=NULL`, then reported
  `id = mock`, `charge` succeeded for 999,999 minor units and `verify` returned
  `{"valid":true}` — a settled receipt for money that never moved. It was the
  only place in the library where an error mapped onto *valid*, which is the
  direction the whole fail-closed design exists to prevent. A host whose config
  bytes are latin-1 hits it without doing anything exotic.
- **`isRefusal` failed open in two SDKs**, in the direction that costs money.
  The .NET and Kotlin sidecar helpers scanned for `"is_refusal":true` without
  skipping whitespace after the colon, so a verdict reformatted by
  `System.Text.Json` or any proxy yielded `" true"` and a **`Malformed` verdict
  reported as not-a-refusal** — after which each README's own gate sends the
  payout. Two lines away, `IsValid` used the same shape and failed *closed*: the
  polarity was inverted. Kotlin's helper was deleted rather than repaired,
  because `Direct.kt` had already deleted the identical function as a defect and
  a better hand-rolled scan keeps the bug class alive. .NET kept its API and
  moved to a real parser, which was in the shared framework all along.
- **iyzico accepted a completely unauthenticated callback.** Its
  `retrieveCheckoutForm` round trip *is* the signature check, and the error was
  discarded — so `POST token=anything` produced a `WebhookEvent` with a negative
  status, which `patala-core`'s own contract forbids outright. No money could be
  fabricated, but an anonymous request could drive a consumer's cancel-order or
  release-inventory path.
- **Eight rails could emit a delivery with no dedup key** (`event_id` empty, or
  `"0"` for two of them), which the webhook contract says is impossible. The id
  was validated only inside the *settled* arm, so a correctly signed
  non-settling redelivery had nothing to suppress it by.
- **Three rails read an absent settlement-status field as settled.** Not
  reachable today, but a processor payload change would have read as paid.
- **Fourteen of twenty single-processor builds did not compile**, pushing
  operators to `--all-features` and linking all twenty processors into a
  payments binary. The root cause was `cfg` drift in two places; the fix makes
  the marker features name their own dependencies, so they cannot drift again.
- Smaller: `*err` is cleared on entry (a stale message otherwise outlives its
  call); a malformed key seed reported the offending *character* back to the
  caller and now reports its position; a pre-epoch clock made every replay
  window measure against `now = 0`; Ruby, Elixir, Node, Bun and Deno accepted a
  non-JSON 2xx body or a truthy non-`true` value where they must not.

- **Key material is now zeroised.** There was none anywhere: `SigningKey` wiped
  the seed it kept, but every copy on the way to it — file contents, the decoded
  vector, the base58/StrKey text, the `Vec<u8>` UniFFI allocates — was dropped
  intact. `HyperswitchConfig` also derived `Debug` while holding an API key and
  a webhook secret, against its own field doc; it was the sole outlier among 21
  configs. And the sidecar forwarded **every** request header into
  `WebhookDelivery`, which meant its own bearer token — the credential whose
  isolation is the process's entire reason to exist — was handed to arbitrary
  rail code.

**Examined and found clean**, which is worth recording alongside the above: 300k
mutated JSON documents and 400k generated fiat configs across 21 providers,
constructing 19,089 live rails, produced **zero panics**. Every MAC comparison
in all 22 webhook implementations goes through a constant-time path — not one
`==` on a MAC. No `*Rail` struct in any of 23 crates derives `Debug`. All 15
managed sidecars mint 32 CSPRNG bytes and pass them in the child's environment,
never argv. `cargo audit`: 0 advisories across 268 dependencies. The
fail-closed contract holds end to end across `patala-core`, `patala-uniffi`,
`patala-ffi`, the sidecar and all fifteen packages — no binding maps `Ok(false)`
onto an exception, and none maps an error onto valid.

### Added

- **`patala-ffi` — a plain `extern "C"` shared library over `patala-core`.** UniFFI has no backend for C, C++, Node/Deno/Bun, PHP or Elixir; those languages now load a hand-written C ABI instead: JSON in and JSON out (the *same* JSON `patala-sidecar` already serves), `uint64` registry handles that are never reused, errors as plain UTF-8 strings freed with `patala_free`, `0`/`-1` returns with `*err` set. Six symbols: `patala_abi_version`, `patala_abi_check`, `patala_new`, `patala_close`, `patala_call`, `patala_free`. It matches the ABI convention `llmux` and `openrate` shipped, so a reader who learns one has learned all three — **minus their Go-runtime caveats, which do not apply**: patala is Rust, so loading this library starts no threads, installs no signal handlers, runs no GC or scheduler, and is fork-safe. The default (mock-only, fully offline) release artifact is **844,656 bytes**, against ~13 MB for the Go-based equivalent.
- **`patala-ffi/ctest/smoke.c` + `scripts/ffi-ctest.sh` + CI job `c-abi`.** Every Rust test in `patala-ffi` calls the Rust functions directly and would pass with a missing `#[no_mangle]` or a header that had drifted. The smoke test `dlopen`s the built artifact, resolves each symbol by name and drives a real `MockRail` charge → verify round trip through `include/patala.h` — 55 checks, and it asserts that all 55 *ran*, so a C program that exits `0` having executed three of them fails. It also counts the process's threads across `dlopen` and across a full round trip, which turns "no runtime in your process" from a README sentence into an enforced fact; on a platform it cannot count threads on it fails rather than skipping.

### Changed
- **The UniFFI surface moved out of `patala-py` into a new `patala-uniffi` crate, and the binding namespace is now `patala`.** UniFFI derives a namespace from the crate that calls `setup_scaffolding!()`, and while the surface lived in `patala-py` that namespace was `patala_py` — so `uniffi-bindgen-go` emitted a file beginning `package patala_py`, `patala-go` carried a `uniffi.toml` that renamed only the output directory, and every Go call site needed an import alias. `patala-go/README.md` had named the fix and deferred it; with ten more languages being generated from this surface, all ten would have inherited a Python-flavoured module name. `patala-uniffi` now declares `uniffi::setup_scaffolding!("patala")` explicitly. Nothing about the exported shape changed.
  - `patala-py` is now `pub use patala_uniffi::*;` plus the wheel packaging. Its cdylib still carries the scaffolding (rustc re-exports a linked rlib's `#[no_mangle]` symbols from a cdylib), which `make smoke-python` proves empirically under a real interpreter. **The generated Python module is `patala.py`: `from patala import PatalaRail`, not `from patala_py import …`.**
  - `patala-go` generates from `libpatala_uniffi` directly, `uniffi.toml` is deleted, no file in the module aliases the import any more, and `make generate` now **asserts** the package clause is `patala` and names the Rust cause when it is not. Both gates are unchanged and still pass: `make test` 19/19 with its 10 required tests, `make test-fiat` 34/34 with its 19.
  - `patala-py/tests/scaffolding.rs` pins the namespace at **link** time by referencing `UNIFFI_META_NAMESPACE_PATALA`; dropping the explicit namespace makes that test target fail to link.
- **`scripts/check-features.sh` covers all three re-exporting crates** — `patala-uniffi` as the authority for the 20 fiat features, with `patala-py` and `patala-ffi` checked as forwarders, and both enumerated `fiat-all` lists checked against `patala-fiat/src/`. It also had a latent bug: `printf … | grep -qx` under `set -o pipefail` reports a *passing* check as failed whenever `grep` exits before `printf` finishes writing, which made the script report random missing processors on a correct tree. Replaced with pattern matching, no pipeline.

- **`patala-go/bindingtest/` — the Go binding has real tests.** 24 `testing.T` tests (12 of them behind `-tags fiat`) against the generated UniFFI surface: `PatalaRailNewMock`/`PatalaRailNewFiat`, capabilities, `Quote`/`Charge`/`Verify`, fail-closed verify against four kinds of receipt tampering, the typed error variants, and `VerifyWebhook` driven against genuinely signed Stripe and BTCPay deliveries. Previously this module had no `_test.go` files at all — its only executable checks were two `package main` examples run via `go run`.
- **`WebhookStatus` is pinned three ways, because `Unconfirmed` must never be read as payment.** UniFFI lowers enum variants to their ordinal position, so reordering `patala_core::WebhookStatus` and regenerating silently re-points every Go constant while every call site still compiles. `Unconfirmed` means "authentic delivery, asserts nothing about money" — PENDING-equivalent — and `cackle` consumes these bindings, so a flip to `Settled` marks unpaid orders paid. The three layers: constant assertions (catch renumbering), a scan of the generated source asserting the variant *set* is exactly the three known ones (catches an added/removed variant, which renumbering assertions cannot see), and live round-trips through the real cdylib reaching each variant via a delivery that means it (catches a rail mapping its own outcome wrongly on the Rust side, which neither static check can see).
- **`patala-go/scripts/go-test-gate.sh`** — `go test ./...` exits `0` when every package reports `[no test files]`, which is exactly what `make test` and `make test-fiat` did: green, having executed zero assertions. Both targets now run through this gate, which fails on zero tests, on fewer than a floor of passing top-level tests, and on any *required* test that did not run and pass (skipped or excluded by a build tag both count as failure). It has no skip path.
- **Root `make smoke-go`** — the Go counterpart to `make smoke-python`: `make test-fiat` + `make test` in `patala-go`, plus `gofmt`.
- **CI job `go-binding`** — the Go binding is enforced, not just executed by hand. Installs `uniffi-bindgen-go` at the pinned `v0.5.0+v0.29.5` tag (cached on the tag) and runs `make smoke-go`. The two stated reasons it was previously excluded — a pinned bindgen and a C toolchain — are a cached `cargo install` and a compiler `ubuntu-latest` already ships.
- `patala-sidecar`: `registry::tests::registry_is_mock_only` pins the mock-only rail registry, so the claim stated in four documents cannot rot in either direction.
- **`patala-go/bindingtest/destination_test.go` and `destination_fiat_test.go` — the offline `validate_destination` surface (§3a), exercised from Go through the real cdylib.** 7 tests against the always-on surface (all five `DestinationStatus` variants round-tripping, the `human_must_confirm`/`exchange_deposit_caveat` invariants, the compensating-payment flow end to end) plus 3 more behind `-tags fiat` (fiat rails never claiming `StructurallyValid`). This was missed from the changelog when it landed; recorded now because it changes the running total below. Combined with the two bullets above, `patala-go/bindingtest` now totals **34 `testing.T` tests**: **19 top-level** (`go test ./...`, no build tag) plus **15 more behind `-tags fiat`** (12 in `fiat_webhook_test.go`, 3 in `destination_fiat_test.go`) for **34 with `-tags fiat` set** — not the 24 total / 12 gated first recorded when only `binding_test.go`, `fiat_webhook_test.go` and `webhook_status_test.go` existed.

### Added (earlier in this cycle)
- **`PaymentRail::verify_webhook`** — inbound webhook verification is now part of the seam, alongside `quote`/`charge`/`verify`/`refund`, with `WebhookDelivery`, `WebhookEvent` and `WebhookStatus` in `patala-core`. Default is `Unsupported`, so a rail with no push delivery (`MockRail`, `patala-fiat`'s `manual`) says so rather than faking one. `WebhookStatus` is three-valued — `Settled`/`NotSettled`/`Unconfirmed` — because several real schemes authenticate a notification without asserting anything about money, and reporting those as "did not settle" would be a false claim.
- All 20 `patala-fiat` adapters and `patala-hyperswitch` implement it, each delegating to the verification code that already existed beside them.
- The UniFFI surface exports `PatalaRail.verify_webhook(delivery)`, so Python, Go, Swift and Kotlin consumers can reach it; `patala-sidecar` exposes `POST /v1/rails/:rail_id/webhook`, forwarding the request body byte-for-byte (re-encoding it would invalidate every genuine signature).
- `patala-fiat/README.md` — consumer-facing docs for the crate, with the ISO-4217 currency table (147 currencies) documented as the reusable piece it is.
- `patala-fiat/tests/currency_table.rs` — pins the currency table against a checksum, an entry count, and the full zero-decimal and three-decimal code lists, so drift in money-critical data is detected and has to be justified. Runs in the default feature set.
- `patala-fiat/tests/webhook_coverage.rs` — asserts every compiled-in adapter implements `verify_webhook`, fails closed on a forged delivery, and reads the header names it documents. Names every adapter it did *not* verify, out loud.
- `patala-py/examples/smoke_test.py` and `patala-go/examples/fiatroundtrip` now verify a genuinely signed Stripe webhook delivery end to end, from Python and from Go.
- `patala-core`: crate-level doctest showing the offline `MockRail` charge → verify round-trip — executable documentation that runs under `cargo test --doc`.
- `patala-solana`: runnable doctest on `binding_memo` demonstrating the deterministic, domain-separated payer+reference binding (anti-replay).
- `patala-stellar`: runnable doctest on `tx::memo_hash` demonstrating the deterministic 32-byte `Memo::Hash` binding and its length-prefixed anti-ambiguity (sibling parity with `patala-solana`).
- Root `Makefile` codifying the workspace quality gates: `make check` runs `fmt --check`, `clippy -D warnings`, `cargo test --workspace`, and a warning-free doc build.
- GitHub Actions CI (`.github/workflows/ci.yml`) running `make check` on every push to `main` and every PR — the same gate, enforced. Pure-Rust (UniFFI bindings, no pyo3), so it needs no Python toolchain.

### Changed
- **`Cargo.lock` is now checked in.** It was gitignored while this workspace ships a binary (`patala-sidecar`), so a clean clone resolved its own dependency versions and did not reproduce the build anyone tested — for a process whose reason to exist is key isolation for a payment substrate, that is the wrong default. Cargo's own guidance (commit it for a binary, ignore it for a library) points the same way.
- `patala-sidecar/src/registry.rs`, its README and its `Cargo.toml` now state plainly that the rail registry is **mock-only** and that per-rail registration is **unwritten**, and correct the stale reason given for it: the note said the rail crates "don't exist in this tree", which stopped being true when `patala-solana`, `patala-stellar`, `patala-hyperswitch` and `patala-fiat` became workspace members. The remaining work is named instead (optional dependencies behind per-rail features, a per-rail config-source decision with a fail-closed story, and extending the lint/test targets to the feature-on build).
- `make check` (and therefore CI) now runs the feature-gated half of the suite: `cargo test -p patala-fiat --all-features` and `cargo test -p patala-py --features fiat-all`, plus clippy over both. `cargo test --workspace` builds every crate with its *default* features, and `patala-fiat`'s default feature set is deliberately empty — so 480+ existing tests covering the 20 processor adapters had never run in CI. Both passes are gates now.
- `scripts/check-features.sh` additionally requires every adapter to appear in the webhook-coverage test and to enable the private `_adapter` marker feature.
- `patala-fiat`: `httpshared` is gated on the new `_adapter` marker instead of a hand-maintained 20-arm `cfg(any(...))`.

### Fixed
- Docs across the workspace now build clean under `cargo doc -D warnings`: resolved broken intra-doc links in `patala-solana`, `patala-stellar`, `patala-fiat`, `patala-py`, and `patala-hyperswitch` (links to feature-gated, private, or `#[cfg(test)]` items rendered as plain code spans).
- Docs no longer say the `PaymentRail` trait "has no webhook method at all" — that claim was true when written and is now false in ~30 places across `patala-fiat`, `patala-hyperswitch` and `PORTING.md`.
- `README.md` and `site/docs/status.md` omitted `patala-fiat` entirely (20 adapters, the currency table, the `manual` rail) and listed a direct PayFast rail as deferred when `patala-fiat/src/payfast/` is built. Both corrected, along with the test counts, and the sidecar's mock-only rail registry is now stated rather than implied.
- `patala-fiat`'s `Cargo.toml` description and crate docs described "two pilot adapters (Stripe, Paystack)"; there are twenty.
- `README.md`, `site/docs/status.md`, the root `Makefile` and `.github/workflows/ci.yml` all said the Go binding was run by hand and not enforced by CI. That is no longer true — corrected in all four. `patala-go/README.md`'s dated verification note recording `make test-fiat` reporting `[no test files]` is kept (it is what that run observed) and now says so was fixed.

### Fixed (later in this cycle)
- **The license-metadata mismatch noted below as "not changed" has since been changed.** All seven crates now declare `license = "MIT OR Apache-2.0"` in their `Cargo.toml`, matching the pair the repo actually offers (verified via `cargo metadata`, and again directly against every crate's manifest during this sweep — see README.md's "License" section). The README's "Open owner decision" note referenced below has been removed, because the decision was made.

### Noted for the owner — not changed (superseded, see "Fixed" above)
- ~~**All seven crates declare `license = "MIT"` while the repo ships MIT OR Apache-2.0.**~~ The metadata was *narrower* than what is offered, so nothing was over-claimed and nobody was misled into relying on a grant they did not have — which is why this was recorded rather than fixed at the time. It was still a mismatch with real consequences: licence scanners and SBOM generators reading crate metadata would not see the Apache-2.0 option, and Apache-2.0 is the one carrying an explicit patent grant. The ecosystem convention is `license = "MIT OR Apache-2.0"`, a one-line change per manifest that would broaden, never narrow, what is granted. Which licences are offered is the copyright holder's decision, not a drive-by consistency fix — the owner has since made it; see "Fixed" above.

## [0.1.0] — 2026-07-21

First versioned cut of patala — a sovereign, centerless payment-rail substrate: one interface to move value (fiat or crypto) that any product can vendor and self-host, holding no funds and taking no cut.
