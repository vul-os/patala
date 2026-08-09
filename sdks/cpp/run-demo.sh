#!/usr/bin/env bash
# Build and run the patala C++ examples. Offline, MockRail only, no credentials.
#
#   ./sdks/cpp/run-demo.sh            # both
#   ./sdks/cpp/run-demo.sh direct
#   ./sdks/cpp/run-demo.sh sidecar
#
# Builds what is missing: libpatala_ffi for `direct`, the patala-sidecar binary
# for `sidecar`. Both come from the root cargo workspace, so a Rust toolchain
# is needed to produce them — not to consume them.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
want="${1:-both}"

case "$(uname -s)" in
  Darwin) libname="libpatala_ffi.dylib" ;;
  *)      libname="libpatala_ffi.so" ;;
esac

if [[ "$want" == "direct" || "$want" == "both" ]]; then
  if [[ ! -f "$root/target/release/$libname" ]]; then
    echo "==> building libpatala_ffi"
    (cd "$root" && cargo build --quiet -p patala-ffi --release)
  fi
  echo "==> building the C++ direct example"
  make -C "$here" --no-print-directory direct
  echo
  echo "==> direct (in-process, C ABI via patala.hpp)"
  "$here/direct"
fi

if [[ "$want" == "sidecar" || "$want" == "both" ]]; then
  if [[ ! -x "$root/target/release/patala-sidecar" ]]; then
    echo "==> building patala-sidecar"
    (cd "$root" && cargo build --quiet -p patala-sidecar --release)
  fi
  echo "==> building the C++ sidecar example"
  make -C "$here" --no-print-directory sidecar
  echo
  echo "==> sidecar (child process over HTTP)"
  "$here/sidecar" "$root/target/release/patala-sidecar"
fi
