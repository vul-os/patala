# Makefile for the patala C examples.
#
#   make            build both
#   make run        build both and run them
#   make clean
#
# `direct` LINKS libpatala_ffi, which is the shape a program with an installed
# library has. (patala-ffi/ctest/smoke.c dlopens it instead, because a test
# that resolves every symbol by name is what catches a missing #[no_mangle].)
#
# `sidecar` links no patala library at all — it needs a socket and nothing
# else.
#
# The library lives in target/{release,debug}/ after
#   cargo build -p patala-ffi --release
# Override with:  make PATALA_LIB_DIR=/path/to/dir

CC      ?= cc
CFLAGS  ?= -std=c11 -O2 -Wall -Wextra -Wpedantic
ROOT    := $(abspath ../..)

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  LIBNAME := libpatala_ffi.dylib
else
  LIBNAME := libpatala_ffi.so
endif

PATALA_LIB_DIR ?= $(if $(wildcard $(ROOT)/target/release/$(LIBNAME)),$(ROOT)/target/release,$(ROOT)/target/debug)

# The version the library must report. Read from the repo's VERSION file, so a
# stale libpatala_ffi earlier on the load path is caught by patala_abi_check
# rather than by a support ticket.
PATALA_VERSION := $(shell tr -d '[:space:]' < $(ROOT)/VERSION)

INCLUDE := -I$(ROOT)/patala-ffi/include -I.
DEFINES := -DPATALA_EXPECTED_VERSION='"$(PATALA_VERSION)"'

BINS := direct sidecar

.PHONY: all run clean check-lib

all: $(BINS)

check-lib:
	@test -f "$(PATALA_LIB_DIR)/$(LIBNAME)" || { \
	  echo "no $(LIBNAME) in $(PATALA_LIB_DIR)"; \
	  echo "build it:  (cd $(ROOT) && cargo build -p patala-ffi --release)"; \
	  echo "or point at one:  make PATALA_LIB_DIR=/path/to/dir"; \
	  exit 1; }

direct: direct.c jsonpeek.c jsonpeek.h | check-lib
	$(CC) $(CFLAGS) $(INCLUDE) $(DEFINES) -o $@ direct.c jsonpeek.c \
	  -L$(PATALA_LIB_DIR) -lpatala_ffi -Wl,-rpath,$(PATALA_LIB_DIR)
ifeq ($(UNAME_S),Darwin)
	@# rustc stamps a cdylib's LC_ID_DYLIB with the ABSOLUTE path of the copy
	@# under target/<profile>/deps/, so the executable would hard-code a path
	@# out of somebody's build tree and break the moment the library is
	@# installed anywhere else. Rewriting the reference in OUR executable is
	@# the fix that does not require modifying the library; a packager should
	@# instead ship it with a proper install name:
	@#   install_name_tool -id @rpath/libpatala_ffi.dylib libpatala_ffi.dylib
	@install_name_tool -change \
	  "$$(otool -D $(PATALA_LIB_DIR)/$(LIBNAME) | tail -1)" \
	  @rpath/$(LIBNAME) $@
endif

sidecar: sidecar.c mini_http.c mini_http.h jsonpeek.c jsonpeek.h
	$(CC) $(CFLAGS) $(INCLUDE) -o $@ sidecar.c mini_http.c jsonpeek.c

run: $(BINS)
	./run-demo.sh

clean:
	rm -f $(BINS)
