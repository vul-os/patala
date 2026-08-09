#!/usr/bin/env bash
# Run the patala Rust examples. Offline, MockRail only, no credentials.
#
#   ./sdks/rust/run.sh            # both
#   ./sdks/rust/run.sh direct
#   ./sdks/rust/run.sh sidecar
#
# `direct` needs nothing but a Rust toolchain: patala-core is a path
# dependency of this crate and there is no library to build or locate.
# `sidecar` needs the `patala-sidecar` binary, which this script builds from
# the root workspace if it is not already there.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
want="${1:-both}"

if [[ "$want" == "direct" || "$want" == "both" ]]; then
  echo "==> direct (in-process, no FFI)"
  (cd "$here" && cargo run --quiet --example direct)
fi

if [[ "$want" == "sidecar" || "$want" == "both" ]]; then
  echo
  if [[ -z "${PATALA_SIDECAR_BIN:-}" && ! -x "$root/target/release/patala-sidecar" ]]; then
    echo "==> building patala-sidecar"
    (cd "$root" && cargo build --quiet -p patala-sidecar --release)
  fi
  echo "==> sidecar (child process over HTTP)"
  (cd "$here" && cargo run --quiet --example sidecar)
fi
