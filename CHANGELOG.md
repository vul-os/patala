# Changelog

All notable changes to patala are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- `patala-core`: crate-level doctest showing the offline `MockRail` charge → verify round-trip — executable documentation that runs under `cargo test --doc`.
- `patala-solana`: runnable doctest on `binding_memo` demonstrating the deterministic, domain-separated payer+reference binding (anti-replay).
- Root `Makefile` codifying the workspace quality gates: `make check` runs `fmt --check`, `clippy -D warnings`, `cargo test --workspace`, and a warning-free doc build.
- GitHub Actions CI (`.github/workflows/ci.yml`) running `make check` on every push to `main` and every PR — the same gate, enforced. Pure-Rust (UniFFI bindings, no pyo3), so it needs no Python toolchain.

### Fixed
- Docs across the workspace now build clean under `cargo doc -D warnings`: resolved broken intra-doc links in `patala-solana`, `patala-stellar`, `patala-fiat`, `patala-py`, and `patala-hyperswitch` (links to feature-gated, private, or `#[cfg(test)]` items rendered as plain code spans).

## [0.1.0] — 2026-07-21

First versioned cut of patala — a sovereign, centerless payment-rail substrate: one interface to move value (fiat or crypto) that any product can vendor and self-host, holding no funds and taking no cut.
