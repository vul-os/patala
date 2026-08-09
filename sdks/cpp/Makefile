# Makefile for the patala C++ examples.
#
#   make            build both
#   make run        build both and run them
#   make clean
#
# `direct` LINKS libpatala_ffi through the header-only wrapper in patala.hpp.
# `sidecar` links no patala library at all — it needs a socket.
#
# Build the library first:
#   cargo build -p patala-ffi --release
# Override its location with:  make PATALA_LIB_DIR=/path/to/dir

CXX      ?= c++
CXXFLAGS ?= -std=c++17 -O2 -Wall -Wextra -Wpedantic
ROOT     := $(abspath ../..)

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  LIBNAME := libpatala_ffi.dylib
else
  LIBNAME := libpatala_ffi.so
endif

PATALA_LIB_DIR ?= $(if $(wildcard $(ROOT)/target/release/$(LIBNAME)),$(ROOT)/target/release,$(ROOT)/target/debug)

# The version the library must report, from the repo's VERSION file, so a stale
# libpatala_ffi earlier on the load path is caught by patala::require_abi.
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

direct: direct.cpp patala.hpp jsonpeek.hpp | check-lib
	$(CXX) $(CXXFLAGS) $(INCLUDE) $(DEFINES) -o $@ direct.cpp \
	  -L$(PATALA_LIB_DIR) -lpatala_ffi -Wl,-rpath,$(PATALA_LIB_DIR)
ifeq ($(UNAME_S),Darwin)
	@# rustc stamps a cdylib's LC_ID_DYLIB with the ABSOLUTE path of the copy
	@# under target/<profile>/deps/, so a linked executable would hard-code a
	@# path out of somebody's build tree. Rewrite the reference in OUR
	@# executable; a packager should fix the library instead:
	@#   install_name_tool -id @rpath/libpatala_ffi.dylib libpatala_ffi.dylib
	@install_name_tool -change \
	  "$$(otool -D $(PATALA_LIB_DIR)/$(LIBNAME) | tail -1)" \
	  @rpath/$(LIBNAME) $@
endif

sidecar: sidecar.cpp jsonpeek.hpp
	$(CXX) $(CXXFLAGS) $(INCLUDE) -o $@ sidecar.cpp

run: $(BINS)
	./run-demo.sh

clean:
	rm -f $(BINS)
