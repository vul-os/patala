# Changelog

All notable changes to patala are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
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
- `make check` (and therefore CI) now runs the feature-gated half of the suite: `cargo test -p patala-fiat --all-features` and `cargo test -p patala-py --features fiat-all`, plus clippy over both. `cargo test --workspace` builds every crate with its *default* features, and `patala-fiat`'s default feature set is deliberately empty — so 480+ existing tests covering the 20 processor adapters had never run in CI. Both passes are gates now.
- `scripts/check-features.sh` additionally requires every adapter to appear in the webhook-coverage test and to enable the private `_adapter` marker feature.
- `patala-fiat`: `httpshared` is gated on the new `_adapter` marker instead of a hand-maintained 20-arm `cfg(any(...))`.

### Fixed
- Docs across the workspace now build clean under `cargo doc -D warnings`: resolved broken intra-doc links in `patala-solana`, `patala-stellar`, `patala-fiat`, `patala-py`, and `patala-hyperswitch` (links to feature-gated, private, or `#[cfg(test)]` items rendered as plain code spans).
- Docs no longer say the `PaymentRail` trait "has no webhook method at all" — that claim was true when written and is now false in ~30 places across `patala-fiat`, `patala-hyperswitch` and `PORTING.md`.
- `README.md` and `site/docs/status.md` omitted `patala-fiat` entirely (20 adapters, the currency table, the `manual` rail) and listed a direct PayFast rail as deferred when `patala-fiat/src/payfast/` is built. Both corrected, along with the test counts, and the sidecar's mock-only rail registry is now stated rather than implied.
- `patala-fiat`'s `Cargo.toml` description and crate docs described "two pilot adapters (Stripe, Paystack)"; there are twenty.

## [0.1.0] — 2026-07-21

First versioned cut of patala — a sovereign, centerless payment-rail substrate: one interface to move value (fiat or crypto) that any product can vendor and self-host, holding no funds and taking no cut.
