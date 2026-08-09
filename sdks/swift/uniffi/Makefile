# sdks/swift/uniffi — generate, build and RUN the UniFFI Swift bindings.
#
# This sits ALONGSIDE the SwiftPM package one directory up, which reaches
# patala through dlopen and the plain C ABI. Both are real; see ../README.md's
# "UniFFI" section for which to pick. The short version: that package can be a
# dependency of another package because it carries no `unsafeFlags`, and
# linking libpatala_uniffi needs `-L`, which is unsafeFlags. So the generated
# path is driven by swiftc directly — the same call to reproducibility the
# Kotlin SDK makes by driving kotlinc rather than shipping a Gradle file
# nothing in this repo runs.
#
# Follows patala-go/Makefile, the house pattern: pin the bindgen, refuse to run
# without it, and ASSERT the shape of what came out.
#
#   make -C sdks/swift/uniffi check      generate + typecheck + run + checks
#   make -C sdks/swift/uniffi generate   bindings only
#   make -C sdks/swift/uniffi run        the charge -> verify example
#   make -C sdks/swift/uniffi checks     the counted assertions
#
# Nothing under bindings/ or build/ is checked in: it is build output,
# reproduced by these commands, exactly like /target.

SHELL := /bin/bash
HERE := $(CURDIR)
ROOT_DIR := $(abspath $(CURDIR)/../../..)
BINDINGS_DIR := $(HERE)/bindings
BUILD_DIR := $(HERE)/build

# --- the pin ------------------------------------------------------------------
#
# The bindgen is this workspace's own binary (`cargo run -p patala-uniffi --bin
# uniffi-bindgen`), so there is nothing to install and nothing that can drift
# from the scaffolding compiled into the cdylib. The version is still asserted:
# a `cargo update` that moves uniffi changes every generated binding in the
# repo and should be deliberate.
UNIFFI_VERSION := 0.29.5

# The UniFFI namespace — uniffi::setup_scaffolding!("patala") — decides the
# generated file names and the C module name. Asserted after generation.
NAMESPACE := patala
GENERATED := $(BINDINGS_DIR)/$(NAMESPACE).swift
MODULEMAP := $(BINDINGS_DIR)/$(NAMESPACE)FFI.modulemap
HEADER := $(BINDINGS_DIR)/$(NAMESPACE)FFI.h

FEATURES ?=
FEATURE_FLAG := $(if $(FEATURES),--features $(FEATURES))

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
	LIB_EXT := dylib
	LIB_PATH_VAR := DYLD_LIBRARY_PATH
else
	LIB_EXT := so
	LIB_PATH_VAR := LD_LIBRARY_PATH
endif
LIB_DIR := $(ROOT_DIR)/target/release
LIB_FILE := $(LIB_DIR)/libpatala_uniffi.$(LIB_EXT)

SWIFTC_FLAGS := -Xcc -fmodule-map-file=$(MODULEMAP) -I $(BINDINGS_DIR)
LINK_FLAGS := -L $(LIB_DIR) -lpatala_uniffi

.PHONY: check generate typecheck build run checks rust-lib check-tools \
        check-uniffi-version clean

check: generate typecheck build run checks

# --- 0. the toolchain ----------------------------------------------------------
check-tools:
	@command -v cargo >/dev/null 2>&1 || { \
		echo "cargo not found on PATH. Install Rust: https://rustup.rs"; exit 1; }
	@command -v swiftc >/dev/null 2>&1 || { \
		echo "swiftc not found on PATH."; \
		echo "  xcode-select --install     (Command Line Tools are enough; Xcode is not)"; \
		exit 1; }
	@echo "swift: $$(swiftc --version 2>&1 | head -1)"

check-uniffi-version:
	@resolved="$$(cd $(ROOT_DIR) && cargo tree -p patala-uniffi -i uniffi --depth 0 2>/dev/null \
		| head -1 | awk '{print $$2}' | sed 's/^v//')"; \
	if [ "$$resolved" != "$(UNIFFI_VERSION)" ]; then \
		echo "generate: FAILED — uniffi is $$resolved, this Makefile pins $(UNIFFI_VERSION)."; \
		echo "  The generated Swift and the scaffolding in the cdylib come from that"; \
		echo "  one version. If the bump is deliberate, regenerate, re-run"; \
		echo "  'make -C sdks/swift/uniffi check', and update UNIFFI_VERSION here"; \
		echo "  and the toolchain table in ../README.md."; \
		exit 1; \
	fi; \
	echo "generate: uniffi $$resolved (pinned)"

# --- 1. the cdylib -------------------------------------------------------------
rust-lib:
	cd $(ROOT_DIR) && cargo build -p patala-uniffi --release $(FEATURE_FLAG)

# --- 2. generate ---------------------------------------------------------------
generate: check-tools check-uniffi-version rust-lib
	rm -rf $(BINDINGS_DIR)
	mkdir -p $(BINDINGS_DIR)
	cd $(ROOT_DIR) && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
		--library "$(LIB_FILE)" --language swift --no-format --out-dir $(BINDINGS_DIR)
	@# The file names ARE the UniFFI namespace. If it ever reverts to
	@# `patala_uniffi` (someone dropping the explicit namespace from
	@# setup_scaffolding!), the module map, the header and the import all move at
	@# once, and swiftc's error would point at Swift rather than at the Rust crate
	@# that caused it. Fail here instead, naming the cause.
	@for f in $(GENERATED) $(MODULEMAP) $(HEADER); do \
		test -f "$$f" || { \
			echo "generate: FAILED — expected $$f."; \
			echo "  Generated instead:"; ls -1 $(BINDINGS_DIR) | sed 's/^/    /'; \
			echo "  The file stem IS the UniFFI namespace. Check that"; \
			echo "  patala-uniffi/src/lib.rs still calls uniffi::setup_scaffolding!(\"$(NAMESPACE)\")."; \
			exit 1; \
		}; \
	done
	@grep -q '^module $(NAMESPACE)FFI {' $(MODULEMAP) || { \
		echo "generate: FAILED — $(MODULEMAP) does not declare 'module $(NAMESPACE)FFI'."; \
		echo "  It declares: $$(head -1 $(MODULEMAP))"; \
		exit 1; \
	}
	@# The typed surface is the entire reason to generate rather than dlopen.
	@for symbol in 'public struct PayRequest' 'public struct Receipt' \
	               'public struct DestinationVerdict' 'public enum RailClass' \
	               'public enum PatalaError'; do \
		grep -q "^$$symbol" $(GENERATED) || { \
			echo "generate: FAILED — no '$$symbol' in the generated Swift."; \
			exit 1; \
		}; \
	done
	@echo "generate: OK — module $(NAMESPACE)FFI, $$(wc -l < $(GENERATED) | tr -d ' ') lines, typed surface present"

# --- 3. typecheck --------------------------------------------------------------
#
# Separate from the build because it needs no cdylib to link against and is the
# cheapest honest answer to "does the generated Swift compile?".
typecheck:
	swiftc -typecheck $(SWIFTC_FLAGS) $(GENERATED) $(HERE)/roundtrip/main.swift
	swiftc -typecheck $(SWIFTC_FLAGS) $(GENERATED) $(HERE)/checks/main.swift
	@echo "typecheck: OK — zero warnings, zero errors"

# --- 4. build ------------------------------------------------------------------
build:
	mkdir -p $(BUILD_DIR)
	swiftc -O $(SWIFTC_FLAGS) $(LINK_FLAGS) -o $(BUILD_DIR)/roundtrip \
		$(GENERATED) $(HERE)/roundtrip/main.swift
	swiftc -O $(SWIFTC_FLAGS) $(LINK_FLAGS) -o $(BUILD_DIR)/checks \
		$(GENERATED) $(HERE)/checks/main.swift
	@echo "build: OK — $(BUILD_DIR)/roundtrip, $(BUILD_DIR)/checks"

# --- 5. run --------------------------------------------------------------------
run: build
	$(LIB_PATH_VAR)="$(LIB_DIR):$$$(LIB_PATH_VAR)" $(BUILD_DIR)/roundtrip

checks: build
	$(LIB_PATH_VAR)="$(LIB_DIR):$$$(LIB_PATH_VAR)" $(BUILD_DIR)/checks

clean:
	rm -rf $(BINDINGS_DIR) $(BUILD_DIR)
