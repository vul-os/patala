#!/usr/bin/env bash
# Run the patala Swift examples. Offline, MockRail only, no credentials.
#
#   ./sdks/swift/run.sh            # both examples
#   ./sdks/swift/run.sh direct
#   ./sdks/swift/run.sh sidecar
#   ./sdks/swift/run.sh checks     # the assertions behind the README
#
# Builds what is missing from the root cargo workspace: libpatala_ffi for
# `direct` and `checks`, the patala-sidecar binary for `sidecar`. A Rust
# toolchain is needed to produce those, not to consume them.
#
# Note there is no `swift test` here: XCTest ships with Xcode, and the machine
# this was written on has Command Line Tools only. `checks` is an executable
# for exactly that reason — see Sources/patala-checks/main.swift.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
want="${1:-both}"

case "$(uname -s)" in
  Darwin) libname="libpatala_ffi.dylib" ;;
  *)      libname="libpatala_ffi.so" ;;
esac
lib="${PATALA_LIBRARY:-$root/target/release/$libname}"
version="$(tr -d '[:space:]' < "$root/VERSION")"

needs_library() { [[ "$want" == "direct" || "$want" == "checks" || "$want" == "both" ]]; }

if needs_library && [[ ! -f "$lib" ]]; then
  echo "==> building libpatala_ffi"
  (cd "$root" && cargo build --quiet -p patala-ffi --release)
fi

echo "==> swift build -c release"
(cd "$here" && swift build -c release 2>&1 | tail -1)

if [[ "$want" == "checks" ]]; then
  echo
  PATALA_LIBRARY="$lib" "$here/.build/release/patala-checks"
  exit 0
fi

if [[ "$want" == "direct" || "$want" == "both" ]]; then
  echo
  echo "==> direct (in-process, C ABI via dlopen)"
  PATALA_LIBRARY="$lib" PATALA_VERSION="$version" "$here/.build/release/patala-direct-example"
fi

if [[ "$want" == "sidecar" || "$want" == "both" ]]; then
  if [[ ! -x "$root/target/release/patala-sidecar" ]]; then
    echo "==> building patala-sidecar"
    (cd "$root" && cargo build --quiet -p patala-sidecar --release)
  fi
  echo
  echo "==> sidecar (child process over HTTP)"
  "$here/.build/release/patala-sidecar-example" "$root/target/release/patala-sidecar"
fi
