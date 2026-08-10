#!/usr/bin/env bash
#
# run-examples.sh — compile and RUN both .NET examples, direct and sidecar.
#
# Builds what they need:
#   * the shared library  (cargo build -p patala-ffi --release)      for direct
#   * the sidecar binary  (cargo build -p patala-sidecar --release)  for sidecar
#
# NETWORK: neither example needs one. patala's default rail is MockRail —
# deterministic, offline — and the sidecar's default registry contains exactly
# that one rail, reached over loopback. This is unlike openrate, whose sidecar
# example genuinely needs a network because its server fetches at startup.
#
# MONEY: neither example moves any. This is a payments library.
#
# Fails closed: a missing toolchain, a library that would not build, or an
# example that exits non-zero is a FAILURE, never a skip.
#
# Usage:  sdks/dotnet/run-examples.sh [direct|sidecar|checks]
#
# `checks` is the counted assertion suite over the pure-C# half of this SDK —
# Json.Quote, Json.Field, Json.Flag and the IsRefusal helpers over them. It
# needs no library and no child process, and it exists because that half is
# where this SDK made its own decisions and where IsRefusal fail-OPENed on a
# reformatted verdict. Included in the default `both` run.
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
which="${1:-both}"

export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_NOLOGO=1

fail() { echo "run-examples: FAIL — $*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH"
command -v dotnet >/dev/null 2>&1 || fail "dotnet is not on PATH"
echo "run-examples: dotnet $(dotnet --version), $(cargo --version)"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# --- the shared library ------------------------------------------------------
case "$(uname -s)" in
  Darwin) libfile="libpatala_ffi.dylib" ;;
  *)      libfile="libpatala_ffi.so" ;;
esac
libpath="${root}/target/release/${libfile}"
if [ "${which}" != "sidecar" ] && [ "${which}" != "checks" ]; then
  echo "run-examples: building ${libfile}…"
  ( cd "${root}" && cargo build -p patala-ffi --release ) >"${tmp}/lib.log" 2>&1 \
    || { cat "${tmp}/lib.log" >&2; fail "the shared library did not build"; }
  [ -f "${libpath}" ] || fail "expected a library at ${libpath}"
  echo "run-examples: library $(wc -c < "${libpath}" | tr -d ' ') bytes"
fi

# --- the sidecar binary ------------------------------------------------------
bin="${root}/target/release/patala-sidecar"
if [ "${which}" != "direct" ] && [ "${which}" != "checks" ]; then
  echo "run-examples: building patala-sidecar…"
  ( cd "${root}" && cargo build -p patala-sidecar --release ) >"${tmp}/bin.log" 2>&1 \
    || { cat "${tmp}/bin.log" >&2; fail "patala-sidecar did not build"; }
  [ -x "${bin}" ] || fail "expected a binary at ${bin}"
fi

# --- build -------------------------------------------------------------------
dotnet build "${here}/examples/Examples.csproj" -v q -c Release -o "${tmp}/out" \
  >"${tmp}/dotnet.log" 2>&1 || { cat "${tmp}/dotnet.log" >&2; fail "the examples did not build"; }
echo "run-examples: built"

# --- run ---------------------------------------------------------------------
status=0
echo
PATALA_LIBRARY="${libpath}" PATALA_SIDECAR_BINARY="${bin}" \
  dotnet "${tmp}/out/patala-examples.dll" "${which}" || status=1

echo
[ "${status}" -eq 0 ] || fail "an example exited non-zero"
echo "run-examples: OK"
