# patala — workspace quality gates.
#
# `make check` runs the full bar the crates are held to: formatting, clippy as
# an error, the whole test suite (unit + integration + doctests, default AND
# feature-gated — see `test-features`), and a warning-free doc build (no broken
# intra-doc links). Every target passes on a clean checkout with a stable Rust
# toolchain that has the rustfmt and clippy components. This is the one command
# CI calls, and the one to run before pushing.
#
# (patala-go has its own Makefile for the UniFFI binding generation — this one
# is the Rust workspace.)

.PHONY: check fmt fmt-check lint test test-features doc features smoke-python smoke-go clean

# The full gate. Run before pushing.
check: fmt-check lint test test-features doc features

# Rewrite formatting in place.
fmt:
	cargo fmt --all

# Fail if anything is unformatted (what a CI job checks; does not rewrite).
fmt-check:
	cargo fmt --all --check

# Clippy across every crate and target, warnings promoted to errors. The
# second invocation is not redundant: `--workspace` uses each crate's DEFAULT
# feature set, and patala-fiat's default feature set is empty — so without
# this, twenty processor adapters are never linted at all.
lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p patala-fiat --all-features --all-targets -- -D warnings
	cargo clippy -p patala-py --features fiat-all --all-targets -- -D warnings

# The whole workspace suite. Doctests run here too; the live-network rail tests
# stay #[ignore]d unless PATALA_SOLANA_LIVE_RPC / PATALA_STELLAR_LIVE are set.
test:
	cargo test --workspace

# The feature-gated half of the suite. `cargo test --workspace` builds every
# crate with its DEFAULT features, and patala-fiat's default feature set is
# deliberately empty (that is what keeps the default build offline) — so a
# plain `--workspace` run executes the currency table, the registry and the
# `manual` rail, and NONE of the twenty processor adapters. That was hundreds
# of tests that existed and never ran in CI. These two lines are the fix; they
# are part of `check`, not an optional extra.
test-features:
	cargo test -p patala-fiat --all-features
	cargo test -p patala-py --features fiat-all

# Docs must build clean — a broken intra-doc link fails the build.
doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Keep the fiat processor set in lock-step with the Cargo feature flags that
# expose it — a new patala-fiat processor left out of patala-py's `fiat-all`
# would silently vanish from the Go binding's cdylib. Pure bash + coreutils.
features:
	./scripts/check-features.sh

# The Python binding, actually executed: build the cdylib with a real fiat
# adapter compiled in, generate the UniFFI wrapper, and run the smoke test
# under a real interpreter. Needs python3 (any 3.x) and nothing else — the
# bindgen is this workspace's own `cargo run --bin uniffi-bindgen`, not an
# separately installed CLI. Not part of `check` because `check` is the
# pure-cargo gate; CI runs this as its own job.
LIB_EXT := $(if $(filter Darwin,$(shell uname -s)),dylib,so)

smoke-python:
	cargo build -p patala-py --features fiat-stripe
	cargo run -p patala-py --bin uniffi-bindgen -- generate \
		--library target/debug/libpatala_py.$(LIB_EXT) \
		--language python \
		--out-dir patala-py/bindings/python
	cp target/debug/libpatala_py.$(LIB_EXT) patala-py/bindings/python/
	PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py

# The Go binding, actually executed — `smoke-python`'s counterpart, and CI's
# third job. Two passes, both through `patala-go/scripts/go-test-gate.sh`,
# which FAILS when zero tests ran (that target used to exit 0 having run
# none):
#
#   test-fiat: cdylib with `--features fiat-all`, so patala-fiat's adapters
#              and `PatalaRail.VerifyWebhook` are reachable. This is where
#              every `WebhookStatus` variant is pinned against a genuinely
#              signed, offline delivery — cackle gates entitlement on that
#              mapping.
#   test:      the same suite against a MockRail-only cdylib (no features),
#              which is also what proves the `//go:build fiat` exclusion
#              works rather than being decorative.
#
# Needs `uniffi-bindgen-go` at the tag matching this workspace's uniffi
# version, plus a C toolchain (cgo) — see patala-go/README.md. Not part of
# `check` for the same reason `smoke-python` is not: `check` is the pure-cargo
# gate, and these have toolchain prerequisites beyond cargo.
smoke-go:
	$(MAKE) -C patala-go test-fiat
	$(MAKE) -C patala-go test
	@unformatted="$$(gofmt -l patala-go/bindingtest patala-go/examples)"; \
	if [ -n "$$unformatted" ]; then \
		echo "gofmt: not formatted:" >&2; echo "$$unformatted" >&2; exit 1; \
	fi; \
	echo "gofmt: clean (patala-go/bindingtest, patala-go/examples)"

clean:
	cargo clean
