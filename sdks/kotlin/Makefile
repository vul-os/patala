# sdks/kotlin — generate, compile and RUN the Kotlin SDK.
#
# The binding is generated: `uniffi.patala` comes out of patala-uniffi's one
# #[uniffi::export] surface, the same surface patala-py and patala-go are
# generated from (PATALA.md §5). Nothing under bindings/ is checked in, for
# the same reason /target is not — it is build output, and the point of this
# file is that anybody can reproduce it byte for byte.
#
# This follows patala-go/Makefile, which is the house pattern: pin the
# bindgen, refuse to run without it, and ASSERT the shape of what came out
# instead of assuming it. A generated binding nobody can regenerate
# identically is a liability.
#
#   make -C sdks/kotlin check     everything below, in order
#   make -C sdks/kotlin generate  bindings only
#   make -C sdks/kotlin examples  direct + sidecar, run for real
#   make -C sdks/kotlin checks    the counted assertions
#   make -C sdks/kotlin probe     the inverted upstream-bug probe
#
# There is deliberately NO build.gradle.kts. Nothing in this repo runs Gradle,
# so a build file would be an unexecuted claim about how the module builds.
# kotlinc is driven directly, and it is driven for real.

SHELL := /bin/bash
HERE := $(CURDIR)
ROOT_DIR := $(abspath $(CURDIR)/../..)
BINDINGS_DIR := $(HERE)/bindings

# --- the pins ----------------------------------------------------------------
#
# UNIFFI_VERSION is not the bindgen's own idea of its version — it is what this
# workspace's Cargo.lock resolves, asserted below. The bindgen is
# `cargo run -p patala-uniffi --bin uniffi-bindgen`, i.e. this workspace's own
# binary, so unlike patala-go (which needs the separate NordSecurity
# uniffi-bindgen-go on PATH) there is nothing here to install and nothing that
# can drift from the scaffolding compiled into the cdylib. The assertion is
# still worth making: a `cargo update` that moves uniffi is a change to every
# generated binding in the repo, and it should be a deliberate one.
UNIFFI_VERSION := 0.29.5

# The generated Kotlin is a com.sun.jna.Library. Pinned so a machine with
# three JNAs in ~/.m2 compiles against the one CI does. Override with
# PATALA_JNA_JAR.
JNA_VERSION := 5.14.0

# The UniFFI namespace — uniffi::setup_scaffolding!("patala") — decides BOTH
# the output path and the package clause. Asserted after generation.
NAMESPACE := patala
GENERATED := $(BINDINGS_DIR)/uniffi/$(NAMESPACE)/$(NAMESPACE).kt
PACKAGE_CLAUSE := package uniffi.$(NAMESPACE)

# Cargo features for the cdylib, e.g. `make FEATURES=fiat-all check`. Empty by
# default: a MockRail-only cdylib with zero extra dependencies.
FEATURES ?=
FEATURE_FLAG := $(if $(FEATURES),--features $(FEATURES))

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
	LIB_EXT := dylib
else
	LIB_EXT := so
endif
LIB_FILE := $(ROOT_DIR)/target/release/libpatala_uniffi.$(LIB_EXT)

.PHONY: check generate build examples direct sidecar checks probe rust-lib \
        check-tools check-uniffi-version print-jna-version clean

# The JNA pin, for run-examples.sh — so the version lives in exactly one file.
print-jna-version:
	@echo $(JNA_VERSION)

# The full gate for this SDK.
check: generate build examples checks probe

# --- 0. the toolchain --------------------------------------------------------
#
# Named with install hints, the way patala-go names uniffi-bindgen-go. A
# missing kotlinc must not look like a broken binding.
check-tools:
	@command -v cargo >/dev/null 2>&1 || { \
		echo "cargo not found on PATH. Install Rust: https://rustup.rs"; exit 1; }
	@command -v kotlinc >/dev/null 2>&1 || { \
		echo "kotlinc not found on PATH."; \
		echo "  brew install kotlin        (or https://kotlinlang.org/docs/command-line.html)"; \
		exit 1; }
	@source $(HERE)/lib.sh && patala_find_jdk_bin >/dev/null
	@source $(HERE)/lib.sh && patala_find_kotlin_stdlib >/dev/null
	@source $(HERE)/lib.sh && patala_find_jna $(JNA_VERSION) >/dev/null

# The pin, asserted rather than described. `cargo tree -i` reports the version
# actually resolved for patala-uniffi's `uniffi` dependency.
check-uniffi-version:
	@resolved="$$(cd $(ROOT_DIR) && cargo tree -p patala-uniffi -i uniffi --depth 0 2>/dev/null \
		| head -1 | awk '{print $$2}' | sed 's/^v//')"; \
	if [ "$$resolved" != "$(UNIFFI_VERSION)" ]; then \
		echo "generate: FAILED — uniffi is $$resolved, this Makefile pins $(UNIFFI_VERSION)."; \
		echo "  The generated Kotlin, the scaffolding in the cdylib and the JNA"; \
		echo "  contract all come from that one version. If the bump is deliberate,"; \
		echo "  regenerate, re-run 'make -C sdks/kotlin check', and update"; \
		echo "  UNIFFI_VERSION here and the toolchain table in README.md."; \
		exit 1; \
	fi; \
	echo "generate: uniffi $$resolved (pinned)"

# --- 1. the cdylib the bindings load -----------------------------------------
rust-lib:
	cd $(ROOT_DIR) && cargo build -p patala-uniffi --release $(FEATURE_FLAG)

# --- 2. generate -------------------------------------------------------------
#
# --no-format: the generator shells out to ktlint for cosmetics and warns when
# it is absent. Formatting is not what is being shipped.
generate: check-tools check-uniffi-version rust-lib
	rm -rf $(BINDINGS_DIR)
	mkdir -p $(BINDINGS_DIR)
	cd $(ROOT_DIR) && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
		--library "$(LIB_FILE)" --language kotlin --no-format --out-dir $(BINDINGS_DIR)
	@# The output PATH and the package clause are both the UniFFI namespace.
	@# Asserted, not assumed: if it ever reverts to `patala_uniffi` (someone
	@# dropping the explicit namespace from setup_scaffolding!), every import in
	@# this SDK breaks in a way whose error message points at Kotlin, not at the
	@# Rust crate that caused it. Fail here instead, naming the cause.
	@test -f $(GENERATED) || { \
		echo "generate: FAILED — expected $(GENERATED)."; \
		echo "  Generated instead:"; find $(BINDINGS_DIR) -name '*.kt' | sed 's/^/    /'; \
		echo "  The output directory IS the UniFFI namespace. Check that"; \
		echo "  patala-uniffi/src/lib.rs still calls uniffi::setup_scaffolding!(\"$(NAMESPACE)\")."; \
		exit 1; \
	}
	@grep -qx '$(PACKAGE_CLAUSE)' $(GENERATED) || { \
		echo "generate: FAILED — $(GENERATED) has no '$(PACKAGE_CLAUSE)' line."; \
		echo "  It declares: $$(grep -m1 '^package ' $(GENERATED))"; \
		exit 1; \
	}
	@# The typed surface is the entire reason this SDK is generated. If a
	@# regeneration produced a file without these, it is not the binding this
	@# README describes and the examples will not compile anyway — say so here,
	@# where the cause is one line up, rather than in kotlinc's output.
	@for symbol in 'data class PayRequest' 'data class Receipt' \
	               'data class DestinationVerdict' 'enum class RailClass' \
	               'sealed class PatalaException'; do \
		grep -q "^$$symbol" $(GENERATED) || { \
			echo "generate: FAILED — no '$$symbol' in the generated Kotlin."; \
			exit 1; \
		}; \
	done
	@echo "generate: OK — $(PACKAGE_CLAUSE), $$(wc -l < $(GENERATED) | tr -d ' ') lines, typed surface present"

# --- 3. compile --------------------------------------------------------------
build:
	PATALA_KOTLIN_SKIP_GENERATE=1 $(HERE)/run-examples.sh compile

# --- 4. run ------------------------------------------------------------------
examples:
	PATALA_KOTLIN_SKIP_GENERATE=1 $(HERE)/run-examples.sh both

direct:
	PATALA_KOTLIN_SKIP_GENERATE=1 $(HERE)/run-examples.sh direct

sidecar:
	PATALA_KOTLIN_SKIP_GENERATE=1 $(HERE)/run-examples.sh sidecar

checks:
	PATALA_KOTLIN_SKIP_GENERATE=1 $(HERE)/run-examples.sh checks

# --- 5. the probe ------------------------------------------------------------
#
# Exit codes INVERTED: 0 means the upstream codegen bug still reproduces, which
# is why patala-uniffi's error variants carry `detail` and not `message`.
# Non-zero means it has been fixed upstream and that constraint can be lifted.
probe:
	$(HERE)/uniffi-kotlin-probe.sh

clean:
	rm -rf $(BINDINGS_DIR) $(HERE)/build
