#!/usr/bin/env bash
#
# ffi-ctest.sh — build libpatala_ffi and run the C smoke test against it.
#
# This is the step that makes patala's C ABI verified rather than asserted.
# Every Rust test in patala-ffi calls the Rust functions directly and would
# pass just as happily with a missing #[no_mangle], a renamed symbol, or a
# header that no longer matches the library. Only a program that dlopens the
# artifact and calls it through include/patala.h can catch that.
#
# What it does:
#   1. builds the cdylib (cargo build -p patala-ffi, plus any --features given),
#   2. compiles patala-ffi/ctest/smoke.c against patala-ffi/include/patala.h,
#   3. runs it with the library path and the version from ./VERSION.
#
# It fails closed: no compiler, no library, or a smoke test that ran the wrong
# number of checks is a FAILURE, never a skip.
#
# Usage:
#   scripts/ffi-ctest.sh [--release] [--features <list>]
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/.." && pwd)"
ffi_dir="${root}/patala-ffi"

profile="debug"
cargo_profile_flag=()
features=()
while [ $# -gt 0 ]; do
  case "$1" in
    --release)
      profile="release"
      cargo_profile_flag=(--release)
      shift
      ;;
    --features)
      features=(--features "${2:?--features needs a value}")
      shift 2
      ;;
    *)
      echo "ffi-ctest: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if ! command -v cc >/dev/null 2>&1; then
  echo "ffi-ctest: FAIL — cc is not on PATH. The C ABI cannot be verified without a C" \
       "compiler; this is a failure, not a skip." >&2
  exit 1
fi

version="$(tr -d '[:space:]' < "${root}/VERSION")"
if [ -z "${version}" ]; then
  echo "ffi-ctest: FAIL — ./VERSION is empty, so the abi-version check would compare" \
       "against nothing" >&2
  exit 1
fi

# --- 1. the library ----------------------------------------------------------
( cd "${root}" && cargo build -p patala-ffi "${cargo_profile_flag[@]}" "${features[@]}" )

case "$(uname -s)" in
  Darwin) libfile="libpatala_ffi.dylib" ;;
  *)      libfile="libpatala_ffi.so" ;;
esac
libpath="${root}/target/${profile}/${libfile}"
if [ ! -f "${libpath}" ]; then
  echo "ffi-ctest: FAIL — expected a library at ${libpath} and there is none" >&2
  exit 1
fi
echo "ffi-ctest: library $(wc -c < "${libpath}" | tr -d ' ') bytes at ${libpath}"

# --- 2. compile the C test ---------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

ldflags=()
if [ "$(uname -s)" != "Darwin" ]; then
  ldflags+=(-ldl)   # macOS has dlopen in libc; glibc needs -ldl on older toolchains
fi

# -Werror on purpose: a warning in a 400-line test that exists to catch drift
# is drift.
cc -std=c11 -Wall -Wextra -Werror -O1 \
   -I "${ffi_dir}/include" \
   -o "${tmp}/smoke" "${ffi_dir}/ctest/smoke.c" ${ldflags[@]+"${ldflags[@]}"}

# --- 3. run it ---------------------------------------------------------------
"${tmp}/smoke" "${libpath}" "${version}"
