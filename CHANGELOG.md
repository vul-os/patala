# Changelog

All notable changes to patala are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
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
