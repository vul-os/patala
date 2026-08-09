#!/usr/bin/env bash
#
# run-examples.sh — compile and RUN both Kotlin examples, direct and sidecar.
#
# The Kotlin SDK wraps the Java one, so this compiles the Java sources with
# javac first and puts them on kotlinc's classpath. That is a deliberate
# structure, not a shortcut: two bindings to one C ABI is two places for a
# use-after-free. Why it is not generated UniFFI Kotlin is a separate question
# with a measured answer — run ./uniffi-kotlin-probe.sh.
#
# NETWORK: neither example needs one. patala's default rail is MockRail —
# deterministic, offline — and the sidecar's default registry contains exactly
# that one rail, reached over loopback.
#
# MONEY: neither example moves any. This is a payments library.
#
# There is no coroutines dependency here: patala has no streaming, so this SDK
# needs nothing but kotlin-stdlib. See README.md.
#
# Fails closed. Usage:  sdks/kotlin/run-examples.sh [direct|sidecar]
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
java_sdk="${root}/sdks/java"
which="${1:-both}"

fail() { echo "run-examples: FAIL — $*" >&2; exit 1; }

# A JDK reached only through JAVA_HOME still counts — Homebrew's openjdk is not
# symlinked onto PATH on macOS.
if ! command -v java >/dev/null 2>&1 && [ -n "${JAVA_HOME:-}" ]; then
  PATH="${JAVA_HOME}/bin:${PATH}"
  export PATH
fi
for tool in cargo javac java kotlinc; do
  command -v "${tool}" >/dev/null 2>&1 || fail "${tool} is not on PATH"
done

jdk_major="$(java -XshowSettings:properties -version 2>&1 \
  | sed -n 's/^ *java\.specification\.version *= *//p' | cut -d. -f1)"
[ -n "${jdk_major}" ] || fail "could not determine the java version"
[ "${jdk_major}" -ge 22 ] || fail "Java ${jdk_major} is too old — the Kotlin SDK compiles against
       org.vulos.patala.PatalaDirect, which is a Java 22 class file"
echo "run-examples: JDK ${jdk_major}, $(kotlinc -version 2>&1 | head -1)"

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
  echo "run-examples: library $(wc -c < "${libpath}" | tr -d ' ') bytes"
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
classes="${tmp}/classes"
mkdir -p "${classes}"
javac -nowarn -d "${classes}" --release 11 \
  "${java_sdk}/src/main/java/org/vulos/patala/Patala.java" \
  "${java_sdk}/src/main/java/org/vulos/patala/PatalaException.java" \
  "${java_sdk}/src/main/java/org/vulos/patala/Json.java"
javac -nowarn -d "${classes}" -cp "${classes}" --release 22 \
  "${java_sdk}/src/main/java/org/vulos/patala/PatalaDirect.java"

kotlinc -nowarn -jvm-target 22 -classpath "${classes}" -d "${classes}" \
  "${here}"/src/main/kotlin/org/vulos/patala/kotlin/*.kt 2>&1 | grep -v '^warning:' || true
kotlinc -nowarn -jvm-target 22 -classpath "${classes}" -d "${classes}" \
  "${here}"/examples/*.kt 2>&1 | grep -v '^warning:' || true

[ -f "${classes}/DirectChargeKt.class" ] || fail "DirectCharge.kt did not compile"
[ -f "${classes}/SidecarChargeKt.class" ] || fail "SidecarCharge.kt did not compile"
echo "run-examples: compiled"

# kotlin-stdlib.jar has to be on the RUN classpath; kotlinc only puts it on the
# compile one. Finding it means resolving through however kotlinc was installed
# — a Homebrew symlink, an SDKMAN shim, or a plain unpacked distribution — so
# each candidate is checked and the failure names every path tried.
find_stdlib() {
  local candidates=() c bin real
  [ -n "${KOTLIN_HOME:-}" ] && candidates+=("${KOTLIN_HOME}/lib/kotlin-stdlib.jar")
  bin="$(command -v kotlinc)"
  real="${bin}"
  # Follow the symlink chain by hand: `readlink -f` is GNU and absent on some
  # macOS versions, and a missing tool here would look like a missing jar.
  while [ -L "${real}" ]; do
    local target; target="$(readlink "${real}")"
    case "${target}" in
      /*) real="${target}" ;;
      *)  real="$(dirname "${real}")/${target}" ;;
    esac
  done
  for c in "$(dirname "$(dirname "${real}")")" "$(dirname "$(dirname "${bin}")")"; do
    candidates+=("${c}/lib/kotlin-stdlib.jar" "${c}/libexec/lib/kotlin-stdlib.jar")
  done
  if command -v brew >/dev/null 2>&1; then
    candidates+=("$(brew --prefix kotlin 2>/dev/null)/libexec/lib/kotlin-stdlib.jar")
  fi
  for c in "${candidates[@]}"; do
    if [ -f "${c}" ]; then echo "${c}"; return 0; fi
  done
  printf 'run-examples: FAIL — could not find kotlin-stdlib.jar. Tried:\n' >&2
  printf '  %s\n' "${candidates[@]}" >&2
  printf 'Set KOTLIN_HOME to the Kotlin distribution root.\n' >&2
  exit 1
}
stdlib="$(find_stdlib)"

cp_run="${classes}:${stdlib}"
status=0

if [ "${which}" = "both" ] || [ "${which}" = "direct" ]; then
  echo
  echo "================ DirectCharge (in-process, C ABI) ================"
  PATALA_LIBRARY="${libpath}" \
    java --enable-native-access=ALL-UNNAMED -cp "${cp_run}" DirectChargeKt || status=1
fi

if [ "${which}" = "both" ] || [ "${which}" = "sidecar" ]; then
  echo
  echo "================ SidecarCharge (child process, HTTP) ============="
  PATALA_SIDECAR_BINARY="${bin}" java -cp "${cp_run}" SidecarChargeKt || status=1
fi

echo
[ "${status}" -eq 0 ] || fail "an example exited non-zero"
echo "run-examples: OK"
