# patala-go build pipeline.
#
# This wraps the exact commands documented in README.md so the whole
# generate -> build -> run flow is one command, but every step is still
# runnable by hand (see README.md "Build & run, step by step").
#
# Builds against patala-uniffi's cdylib (target/debug/libpatala_uniffi.{dylib,so}):
# that crate carries the one #[uniffi::export] surface (PATALA.md §5) and
# declares the UniFFI namespace "patala" — this is the SAME surface patala-py
# ships to Python, targeted at Go instead. No new Rust crate is added by this
# package; patala-uniffi is a workspace member of its own.

SHELL := /bin/bash
ROOT_DIR := $(abspath $(CURDIR)/..)
BINDINGS_DIR := $(CURDIR)/bindings
# uniffi-bindgen-go nests its output one level down, under the UniFFI
# NAMESPACE — which patala-uniffi declares as "patala". So the output lands at
# $(BINDINGS_DIR)/patala/, that is the Go import path's last segment
# (.../patala-go/bindings/patala), AND that is the generated file's `package`
# clause. There is no uniffi.toml here any more: it existed only to rename the
# output directory away from `patala_py`, and the namespace is neutral now.
PKG_DIR := $(BINDINGS_DIR)/patala
UNAME_S := $(shell uname -s)

# Cargo features to build patala-uniffi's cdylib with, e.g. `FEATURES=fiat-all`
# or `FEATURES=solana,stellar,hyperswitch,fiat-stripe`. Empty by default —
# the exact status quo before `patala-fiat` was exposed here: a MockRail-only
# cdylib with zero extra dependencies. `run-example-fiat`/`test-fiat` below
# override this to `fiat-all` for you; set it by hand for any other
# combination (`make FEATURES=fiat-stripe generate`).
FEATURES ?=
FEATURE_FLAG := $(if $(FEATURES),--features $(FEATURES))

# `go test ./...` exits 0 when every package reports `[no test files]`, which
# is what `test`/`test-fiat` used to do here: three "no test files" lines and
# a green exit, having verified nothing. Every `go test` invocation below now
# goes through this wrapper, which fails when zero tests ran, when fewer than
# --min passed, or when a --require'd test did not run. See the script's own
# header, and ./bindingtest for the tests themselves.
GATE := $(CURDIR)/scripts/go-test-gate.sh

# The floors below are floors, not exact counts: adding a test never breaks
# them, removing or silently excluding one does. The --require names are the
# money-critical ones — the WebhookStatus mapping cackle gates entitlement on
# (patala-core/src/webhook.rs: Unconfirmed is PENDING-equivalent and must
# NEVER be read as Settled), the fail-closed verify contract, and the
# destination pre-flight surface (patala-core/src/destination.rs), whose
# failure mode is a customer's refund sent to an address the rail already knew
# was wrong.
GATE_MIN := 19
GATE_REQUIRE := \
	--require TestWebhookStatusDiscriminantsArePinned \
	--require TestOnlySettledMeansPaid \
	--require TestGeneratedWebhookStatusVariantsAreExactlyThree \
	--require TestVerifyFailsClosedOnTamperedReceipt \
	--require TestMockRailReportsWebhookUnsupported \
	--require TestEveryDestinationStatusRoundTripsToGo \
	--require TestDestinationStatusDiscriminantsArePinned \
	--require TestNoVerdictWaivesTheHumanConfirmation \
	--require TestRefusalsAreExactlyTheThreeEstablishedDefects \
	--require TestCompensatingPaymentFlowEndToEndFromGo

GATE_MIN_FIAT := 34
GATE_REQUIRE_FIAT := $(GATE_REQUIRE) \
	--require TestBTCPayWebhookIsUnconfirmedNeverSettled \
	--require TestStripeWebhookPaidSessionIsSettled \
	--require TestStripeWebhookUnpaidSessionIsNotSettled \
	--require TestEveryWebhookStatusVariantIsReachedByARealDelivery \
	--require TestStripeWebhookFailsClosed \
	--require TestFiatProviderCoverage \
	--require TestFiatRailsNeverClaimAStructurallyValidDestination \
	--require TestFiatRailStillRefusesAnEmptyDestination \
	--require TestFiatDestinationSurfaceIsReachableFromGo

ifeq ($(UNAME_S),Darwin)
	LIB_FILE := $(ROOT_DIR)/target/debug/libpatala_uniffi.dylib
	LIB_EXT := dylib
else
	LIB_FILE := $(ROOT_DIR)/target/debug/libpatala_uniffi.so
	LIB_EXT := so
endif

.PHONY: rust-lib generate build run-example run-example-fiat test test-fiat clean check-uniffi-bindgen-go

# 1. Build the cdylib patala-go will link against — patala-uniffi's, the crate
#    that owns the one #[uniffi::export] surface (see README.md's "Which
#    cdylib, which package name?"). Respects $(FEATURES) — see above.
rust-lib:
	cd $(ROOT_DIR) && cargo build -p patala-uniffi $(FEATURE_FLAG)

# 2. Generate the Go bindings from that cdylib's embedded UniFFI metadata.
#    Requires `uniffi-bindgen-go` v0.5.0+v0.29.5 on PATH (matching the
#    workspace's pinned `uniffi = "0.29"` — see README.md "Installing
#    uniffi-bindgen-go" for why the version must match and how to install
#    the correct tag).
check-uniffi-bindgen-go:
	@command -v uniffi-bindgen-go >/dev/null 2>&1 || { \
		echo "uniffi-bindgen-go not found on PATH."; \
		echo "Install the version matching this workspace's uniffi = 0.29.5:"; \
		echo "  cargo install --locked --git https://github.com/NordSecurity/uniffi-bindgen-go --tag v0.5.0+v0.29.5 uniffi-bindgen-go"; \
		exit 1; \
	}

generate: rust-lib check-uniffi-bindgen-go
	rm -rf $(BINDINGS_DIR)
	mkdir -p $(BINDINGS_DIR)
	uniffi-bindgen-go "$(LIB_FILE)" --out-dir $(BINDINGS_DIR) --library
	cp "$(LIB_FILE)" $(PKG_DIR)/
	@# The package clause is the UniFFI namespace. Asserted, not assumed: if it
	@# ever reverts to `patala_py` (someone dropping the explicit namespace from
	@# patala-uniffi's setup_scaffolding!), every import in this module breaks in
	@# a way whose error message points at Go, not at the Rust crate that caused
	@# it. Fail here instead, naming the cause.
	@head -1 $(PKG_DIR)/patala.go | grep -qx 'package patala' || { \
		echo "generate: FAILED — $(PKG_DIR)/patala.go does not start with 'package patala'."; \
		echo "  It starts with: $$(head -1 $(PKG_DIR)/patala.go)"; \
		echo "  The Go package clause IS the UniFFI namespace. Check that"; \
		echo "  patala-uniffi/src/lib.rs still calls uniffi::setup_scaffolding!(\"patala\")."; \
		exit 1; \
	}
	@echo "generate: OK — package clause is 'patala'"

# 3. Build the Go module against the generated bindings. CGO_ENABLED=1 and
#    a C toolchain are required from here on — see README.md's "The cgo
#    cost" section.
build: generate
	cd $(CURDIR) && CGO_ENABLED=1 \
		CGO_LDFLAGS="-lpatala_uniffi -L$(PKG_DIR)" \
		go build ./...

# 4. Build and run the MockRail charge -> verify round-trip example.
run-example: generate
	cd $(CURDIR) && CGO_ENABLED=1 \
		CGO_LDFLAGS="-lpatala_uniffi -L$(PKG_DIR)" \
		DYLD_LIBRARY_PATH="$(PKG_DIR):$$DYLD_LIBRARY_PATH" \
		LD_LIBRARY_PATH="$(PKG_DIR):$$LD_LIBRARY_PATH" \
		go run ./examples/roundtrip

# Runs ./bindingtest against a MockRail-only cdylib, through the gate above:
# a run where nothing executed is a FAILURE, not a pass.
test: generate
	cd $(CURDIR) && CGO_ENABLED=1 \
		CGO_LDFLAGS="-lpatala_uniffi -L$(PKG_DIR)" \
		DYLD_LIBRARY_PATH="$(PKG_DIR):$$DYLD_LIBRARY_PATH" \
		LD_LIBRARY_PATH="$(PKG_DIR):$$LD_LIBRARY_PATH" \
		$(GATE) --min $(GATE_MIN) $(GATE_REQUIRE) -- ./...

# 5. Same as run-example/test above, but forces patala-uniffi's cdylib to be
#    rebuilt with every patala-fiat adapter compiled in (`fiat-all`, see
#    patala-uniffi/Cargo.toml), so `PatalaRailNewFiat`/`PatalaFiatProviders`
#    (examples/fiatroundtrip) are actually reachable. This is the ONLY
#    difference from `run-example`/`test` above — same generate -> build ->
#    run pipeline, just with $(FEATURES) pinned to `fiat-all` regardless of
#    whatever the bare `make generate` was last invoked with.
#
#    `examples/fiatroundtrip/main.go` carries a `//go:build fiat` constraint
#    (Go has no Cargo-style optional-feature-per-dependency mechanism, so
#    this is the Go-side equivalent): without `-tags fiat`, `go build ./...`/
#    `go test ./...` above SKIP that directory entirely, which is exactly
#    what keeps `build`/`test`/`run-example` working unchanged against a
#    plain (non-fiat) cdylib — this target is the only one that opts in.
run-example-fiat:
	$(MAKE) FEATURES=fiat-all generate
	cd $(CURDIR) && CGO_ENABLED=1 \
		CGO_LDFLAGS="-lpatala_uniffi -L$(PKG_DIR)" \
		DYLD_LIBRARY_PATH="$(PKG_DIR):$$DYLD_LIBRARY_PATH" \
		LD_LIBRARY_PATH="$(PKG_DIR):$$LD_LIBRARY_PATH" \
		go run -tags fiat ./examples/fiatroundtrip

# The full Go gate: ./bindingtest's default tests PLUS the `-tags fiat` ones,
# which drive genuinely signed Stripe and BTCPay deliveries through the real
# cdylib and pin every WebhookStatus variant to the delivery that means it.
# This is the target that used to exit 0 having run zero tests.
test-fiat:
	$(MAKE) FEATURES=fiat-all generate
	cd $(CURDIR) && CGO_ENABLED=1 \
		CGO_LDFLAGS="-lpatala_uniffi -L$(PKG_DIR)" \
		DYLD_LIBRARY_PATH="$(PKG_DIR):$$DYLD_LIBRARY_PATH" \
		LD_LIBRARY_PATH="$(PKG_DIR):$$LD_LIBRARY_PATH" \
		$(GATE) --min $(GATE_MIN_FIAT) $(GATE_REQUIRE_FIAT) -- -tags fiat ./...

clean:
	rm -rf $(BINDINGS_DIR)
