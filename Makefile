# patala — workspace quality gates.
#
# `make check` runs the full bar the crates are held to: formatting, clippy as
# an error, the whole test suite (unit + integration + doctests), and a
# warning-free doc build (no broken intra-doc links). Every target passes on a
# clean checkout with a stable Rust toolchain that has the rustfmt and clippy
# components. This is the one command a future CI job should call, and the one
# to run before pushing.
#
# (patala-go has its own Makefile for the UniFFI binding generation — this one
# is the Rust workspace.)

.PHONY: check fmt fmt-check lint test doc clean

# The full gate. Run before pushing.
check: fmt-check lint test doc

# Rewrite formatting in place.
fmt:
	cargo fmt --all

# Fail if anything is unformatted (what a CI job checks; does not rewrite).
fmt-check:
	cargo fmt --all --check

# Clippy across every crate and target, warnings promoted to errors.
lint:
	cargo clippy --workspace --all-targets -- -D warnings

# The whole workspace suite. Doctests run here too; the live-network rail tests
# stay #[ignore]d unless PATALA_SOLANA_LIVE_RPC / PATALA_STELLAR_LIVE are set.
test:
	cargo test --workspace

# Docs must build clean — a broken intra-doc link fails the build.
doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

clean:
	cargo clean
