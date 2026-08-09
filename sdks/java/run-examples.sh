#!/usr/bin/env bash
#
# run-examples.sh — compile and RUN both Java examples, direct and sidecar.
#
# Builds what they need:
#   * the shared library  (cargo build -p patala-ffi --release)      for DirectCharge
#   * the sidecar binary  (cargo build -p patala-sidecar --release)  for SidecarCharge
#
# NETWORK: neither example needs one. patala's default rail is MockRail, which
# is deterministic and offline, and the sidecar's default registry contains
# exactly that one rail. The sidecar example opens a loopback socket and
# nothing else. This is unlike openrate, whose sidecar example genuinely needs
# a network because its server fetches at startup.
#
# MONEY: neither example moves any, and that is not a coincidence — this is a
# payments library, so an example that moved real value would be a liability
# rather than a demonstration.
#
# Fails closed: a missing toolchain, a library that would not build, or an
# example that exits non-zero is a FAILURE, never a skip.
#
# Usage:  sdks/java/run-examples.sh [direct|sidecar]
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
which="${1:-both}"

fail() { echo "run-examples: FAIL — $*" >&2; exit 1; }

# A JDK reached only through JAVA_HOME still counts — Homebrew's openjdk is not
# symlinked onto PATH on macOS, and treating that as "no java" is a false
# negative.
if ! command -v java >/dev/null 2>&1 && [ -n "${JAVA_HOME:-}" ]; then
  PATH="${JAVA_HOME}/bin:${PATH}"
  export PATH
fi
for tool in cargo javac java; do
  command -v "${tool}" >/dev/null 2>&1 || fail "${tool} is not on PATH"
done

jdk_major="$(java -XshowSettings:properties -version 2>&1 \
  | sed -n 's/^ *java\.specification\.version *= *//p' | cut -d. -f1)"
[ -n "${jdk_major}" ] || fail "could not determine the java version"
if [ "${jdk_major}" -lt 22 ] && [ "${which}" != "sidecar" ]; then
  fail "Java ${jdk_major} is too old for the direct example — java.lang.foreign
       became permanent in Java 22. The SIDECAR example runs on Java 11+:
       sdks/java/run-examples.sh sidecar"
fi
echo "run-examples: JDK ${jdk_major} ($(java -version 2>&1 | head -1))"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# --- the shared library ------------------------------------------------------
case "$(uname -s)" in
  Darwin) libfile="libpatala_ffi.dylib" ;;
  *)      libfile="libpatala_ffi.so" ;;
esac
libpath="${root}/target/release/${libfile}"
if [ "${which}" != "sidecar" ]; then
  echo "run-examples: building ${libfile}…"
  ( cd "${root}" && cargo build -p patala-ffi --release ) >"${tmp}/lib.log" 2>&1 \
    || { cat "${tmp}/lib.log" >&2; fail "the shared library did not build"; }
  [ -f "${libpath}" ] || fail "expected a library at ${libpath}"
  echo "run-examples: library $(wc -c < "${libpath}" | tr -d ' ') bytes at ${libpath}"
fi

# --- the sidecar binary ------------------------------------------------------
if [ "${which}" != "direct" ]; then
  echo "run-examples: building patala-sidecar…"
  ( cd "${root}" && cargo build -p patala-sidecar --release ) >"${tmp}/bin.log" 2>&1 \
    || { cat "${tmp}/bin.log" >&2; fail "patala-sidecar did not build"; }
  bin="${root}/target/release/patala-sidecar"
  [ -x "${bin}" ] || fail "expected a binary at ${bin}"
fi

# --- compile -----------------------------------------------------------------
#
# Two compiler passes, matching pom.xml: the sidecar half targets Java 11 so it
# is usable from an 11 consumer, and PatalaDirect needs 22 for java.lang.foreign.
out="${tmp}/classes"
mkdir -p "${out}"
javac -nowarn -d "${out}" --release 11 \
  "${here}/src/main/java/org/vulos/patala/Patala.java" \
  "${here}/src/main/java/org/vulos/patala/PatalaException.java" \
  "${here}/src/main/java/org/vulos/patala/Json.java"
javac -nowarn -d "${out}" -cp "${out}" --release 22 \
  "${here}/src/main/java/org/vulos/patala/PatalaDirect.java"
javac -nowarn -d "${out}" -cp "${out}" --release 22 "${here}"/examples/*.java
echo "run-examples: compiled"

status=0

if [ "${which}" = "both" ] || [ "${which}" = "direct" ]; then
  echo
  echo "================ DirectCharge (in-process, C ABI) ================"
  PATALA_LIBRARY="${libpath}" \
    java --enable-native-access=ALL-UNNAMED -cp "${out}" DirectCharge || status=1
fi

if [ "${which}" = "both" ] || [ "${which}" = "sidecar" ]; then
  echo
  echo "================ SidecarCharge (child process, HTTP) ============="
  PATALA_SIDECAR_BINARY="${bin}" java -cp "${out}" SidecarCharge || status=1
fi

echo
[ "${status}" -eq 0 ] || fail "an example exited non-zero"
echo "run-examples: OK"
