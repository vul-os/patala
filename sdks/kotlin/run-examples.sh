#!/usr/bin/env bash
#
# run-examples.sh — compile and RUN the Kotlin SDK: both examples and the
# counted checks.
#
# The direct path is the GENERATED UniFFI binding (uniffi.patala), so this
# script generates it first — via `make generate`, which owns the version pins
# and the assertions on what came out. The sidecar path is unchanged: it is
# HTTP over loopback and still goes through sdks/java's client, so the three
# non-FFM Java classes are compiled here too.
#
# NETWORK: neither example needs one. patala's default rail is MockRail —
# deterministic, offline — and the sidecar's default registry contains exactly
# that one rail, reached over loopback.
#
# MONEY: neither example moves any. This is a payments library.
#
# Fails closed. Usage:
#   sdks/kotlin/run-examples.sh [both|direct|sidecar|checks|compile]
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
java_sdk="${root}/sdks/java"
which="${1:-both}"

# shellcheck source=lib.sh
source "${here}/lib.sh"

fail() { echo "run-examples: FAIL — $*" >&2; exit 1; }

case "${which}" in
  both|direct|sidecar|checks|compile) ;;
  *) fail "unknown mode '${which}' (want: both|direct|sidecar|checks|compile)" ;;
esac

# --- toolchain ---------------------------------------------------------------
#
# A JDK reached only through JAVA_HOME or Homebrew still counts, and macOS's
# /usr/bin/java stub does not — patala_find_jdk_bin runs the candidate before
# believing it.
jdk_bin="$(patala_find_jdk_bin)" || exit 1
PATH="${jdk_bin}:${PATH}"
export PATH
command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH"
command -v kotlinc >/dev/null 2>&1 || fail "kotlinc is not on PATH (brew install kotlin)"

jdk_major="$(patala_jdk_major)"
[ -n "${jdk_major}" ] || fail "could not determine the java version"
[ "${jdk_major}" -ge 11 ] || fail "Java ${jdk_major} is too old — this SDK targets 11+"
echo "run-examples: JDK ${jdk_major} (${jdk_bin}), $(kotlinc -version 2>&1 | head -1)"

# --- 1. the generated bindings -----------------------------------------------
#
# `make generate` pins the uniffi version, builds the cdylib and asserts the
# package clause and the typed surface. Skipped when the Makefile is the one
# that invoked this script.
if [ "${PATALA_KOTLIN_SKIP_GENERATE:-0}" != "1" ]; then
  make -C "${here}" generate
fi
generated="${here}/bindings/uniffi/patala/patala.kt"
[ -f "${generated}" ] || fail "no generated bindings at ${generated} — run: make -C sdks/kotlin generate"

case "$(uname -s)" in
  Darwin) libfile="libpatala_uniffi.dylib" ;;
  *)      libfile="libpatala_uniffi.so" ;;
esac
libpath="${root}/target/release/${libfile}"
[ -f "${libpath}" ] || fail "expected a cdylib at ${libpath} (make -C sdks/kotlin rust-lib)"
echo "run-examples: cdylib $(wc -c < "${libpath}" | tr -d ' ') bytes"

# The JNA version is pinned in ONE place — the Makefile — and read from it
# here, so this script and `make check` can never compile against different
# jars.
jna_version="$(make -s -C "${here}" print-jna-version)"
[ -n "${jna_version}" ] || fail "could not read JNA_VERSION from the Makefile"
jna="$(patala_find_jna "${jna_version}")" || exit 1
stdlib="$(patala_find_kotlin_stdlib)" || exit 1
echo "run-examples: jna $(basename "${jna}")"

# --- 2. the sidecar binary ---------------------------------------------------
bin=""
if [ "${which}" = "both" ] || [ "${which}" = "sidecar" ]; then
  echo "run-examples: building patala-sidecar…"
  tmplog="$(mktemp)"
  ( cd "${root}" && cargo build -p patala-sidecar --release ) >"${tmplog}" 2>&1 \
    || { cat "${tmplog}" >&2; rm -f "${tmplog}"; fail "patala-sidecar did not build"; }
  rm -f "${tmplog}"
  bin="${root}/target/release/patala-sidecar"
  [ -x "${bin}" ] || fail "expected a binary at ${bin}"
fi

# --- 3. compile --------------------------------------------------------------
#
# Everything targets Java 11. The direct path used to need Java 22, because it
# went through sdks/java's java.lang.foreign binding; the generated binding
# uses JNA, which does not.
classes="${here}/build/classes"
rm -rf "${classes}"
mkdir -p "${classes}"

# The sidecar client only. PatalaDirect.java (the FFM binding) is deliberately
# NOT compiled here any more — the direct path is generated UniFFI now.
javac -nowarn -d "${classes}" --release 11 \
  "${java_sdk}/src/main/java/org/vulos/patala/Patala.java" \
  "${java_sdk}/src/main/java/org/vulos/patala/PatalaException.java" \
  "${java_sdk}/src/main/java/org/vulos/patala/Json.java"

kotlinc -nowarn -jvm-target 11 -classpath "${jna}:${classes}" -d "${classes}" \
  "${generated}" \
  "${here}"/src/main/kotlin/org/vulos/patala/kotlin/*.kt \
  "${here}"/examples/*.kt \
  "${here}"/checks/*.kt 2>&1 | grep -v '^warning:' || true

for expected in DirectChargeKt SidecarChargeKt ChecksKt \
                uniffi/patala/PatalaRail org/vulos/patala/kotlin/Patala; do
  [ -f "${classes}/${expected}.class" ] || fail "${expected} did not compile"
done
echo "run-examples: compiled (generated bindings + SDK + examples + checks)"
if [ "${which}" = "compile" ]; then
  echo "run-examples: OK"
  exit 0
fi

# --- 4. run ------------------------------------------------------------------
#
# jna.library.path is belt and braces: the examples call Patala.useLibrary(),
# which sets UniFFI's own libraryOverride to an absolute path. Both are here
# so a copy-pasted java command works without the SDK call.
cp_run="${classes}:${jna}:${stdlib}"
native_access=()
if [ "${jdk_major}" -ge 22 ]; then
  # JDK 22+ warns when a library on the classpath calls System.load; JNA does.
  # It is a warning today and an error in a future release.
  native_access=(--enable-native-access=ALL-UNNAMED)
fi
run_java() {
  java "${native_access[@]}" -Djna.library.path="$(dirname "${libpath}")" -cp "${cp_run}" "$@"
}

status=0
if [ "${which}" = "both" ] || [ "${which}" = "direct" ]; then
  echo
  echo "================ DirectCharge (in-process, generated UniFFI) ======"
  PATALA_LIBRARY="${libpath}" run_java DirectChargeKt || status=1
fi

if [ "${which}" = "both" ] || [ "${which}" = "checks" ]; then
  echo
  echo "================ Checks (counted assertions) ======================"
  PATALA_LIBRARY="${libpath}" run_java ChecksKt || status=1
fi

if [ "${which}" = "both" ] || [ "${which}" = "sidecar" ]; then
  echo
  echo "================ SidecarCharge (child process, HTTP) =============="
  PATALA_SIDECAR_BINARY="${bin}" run_java SidecarChargeKt || status=1
fi

echo
[ "${status}" -eq 0 ] || fail "an example exited non-zero"
echo "run-examples: OK"
