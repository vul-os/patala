#!/usr/bin/env bash
# Run the Go examples.
#
#   sdks/go/examples/run.sh            # both
#   sdks/go/examples/run.sh direct     # in-process, through ../../patala-go (cgo)
#   sdks/go/examples/run.sh sidecar    # loopback HTTP, CGO_ENABLED=0
#
# The direct example needs the generated bindings and the cdylib they link
# against, so this generates them if they are missing (`make -C patala-go
# generate`, which needs uniffi-bindgen-go at the pinned tag — see
# patala-go/README.md). The sidecar example needs `cargo build -p
# patala-sidecar`, which this runs for you if the binary is not there.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdk="$(dirname "$here")"
repo="$(cd "$sdk/../.." && pwd)"
bindings="$repo/patala-go/bindings/patala"

want="${1:-both}"

lib_name="libpatala_uniffi.so"
[[ "$(uname -s)" == "Darwin" ]] && lib_name="libpatala_uniffi.dylib"

run_direct() {
  if [[ ! -f "$bindings/patala.go" || ! -f "$bindings/$lib_name" ]]; then
    echo "==> generating the Go bindings (make -C patala-go generate)"
    make -C "$repo/patala-go" generate
  fi
  echo "==> direct (cgo, in-process)"
  cd "$sdk"
  CGO_ENABLED=1 \
    CGO_LDFLAGS="-lpatala_uniffi -L$bindings" \
    DYLD_LIBRARY_PATH="$bindings:${DYLD_LIBRARY_PATH:-}" \
    LD_LIBRARY_PATH="$bindings:${LD_LIBRARY_PATH:-}" \
    go run ./examples/direct
}

run_sidecar() {
  if [[ -z "${PATALA_SIDECAR_BIN:-}" && ! -x "$repo/target/debug/patala-sidecar" \
        && ! -x "$repo/target/release/patala-sidecar" ]]; then
    echo "==> building patala-sidecar"
    cargo build --manifest-path "$repo/Cargo.toml" -p patala-sidecar
  fi
  echo "==> sidecar (pure Go, CGO_ENABLED=0)"
  cd "$sdk"
  CGO_ENABLED=0 go run ./examples/sidecar
}

case "$want" in
  direct) run_direct ;;
  sidecar) run_sidecar ;;
  both) run_direct; echo; run_sidecar ;;
  *) echo "usage: run.sh [direct|sidecar|both]" >&2; exit 2 ;;
esac
