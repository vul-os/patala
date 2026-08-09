#!/usr/bin/env bash
#
# signal-probe.sh — measure what loading libpatala_ffi does to this JVM, and
# whether the JVM still works afterwards.
#
# README.md's "The JVM and patala's shared library" section is the output of
# this script, not a recollection. It is deliberately the same probe llmux
# ships (sdks/java/signal-probe.sh there), pointed at a Rust library instead of
# a Go one, so the two results are comparable rather than merely both quoted.
#
# Re-run it on your JDK and your platform. The answer is allowed to be
# different from ours, which is the whole reason it is a script and not a
# paragraph.
#
# Usage:
#   sdks/java/signal-probe.sh            # plain
#   sdks/java/signal-probe.sh --checkjni # with HotSpot's own handler audit on
#   sdks/java/signal-probe.sh --jsig     # with libjsig preloaded
#
# --jsig exists here only so the comparison with llmux is like-for-like. llmux
# NEEDS libjsig; if this probe reports what we measured, patala does not.
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"

fail() { echo "signal-probe: FAIL — $*" >&2; exit 1; }

# A JDK reached through JAVA_HOME counts. On this machine Homebrew's openjdk is
# not symlinked onto PATH, and "no java" would be a false negative.
if ! command -v java >/dev/null 2>&1 && [ -n "${JAVA_HOME:-}" ]; then
  PATH="${JAVA_HOME}/bin:${PATH}"
  export PATH
fi
command -v java >/dev/null 2>&1 || fail "java is not on PATH and JAVA_HOME is unset"
command -v javac >/dev/null 2>&1 || fail "javac is not on PATH"

jdk_major="$(java -XshowSettings:properties -version 2>&1 \
  | sed -n 's/^ *java\.specification\.version *= *//p' | cut -d. -f1)"
[ -n "${jdk_major}" ] && [ "${jdk_major}" -ge 22 ] \
  || fail "Java 22+ is required (java.lang.foreign); this is Java ${jdk_major:-?}"

use_jsig=0
checkjni=0
for arg in "$@"; do
  case "${arg}" in
    --jsig) use_jsig=1 ;;
    --checkjni) checkjni=1 ;;
    *) fail "unknown argument: ${arg}" ;;
  esac
done

case "$(uname -s)" in
  Darwin) libfile="libpatala_ffi.dylib" ;;
  *)      libfile="libpatala_ffi.so" ;;
esac
libpath="${PATALA_LIBRARY:-${root}/target/release/${libfile}}"
if [ ! -f "${libpath}" ]; then
  echo "signal-probe: building ${libfile}…"
  ( cd "${root}" && cargo build -p patala-ffi --release ) >/dev/null 2>&1 \
    || fail "the shared library did not build"
fi
[ -f "${libpath}" ] || fail "no library at ${libpath}"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
javac -d "${tmp}" "${here}/tools/SignalHandlerProbe.java"

jvm_args=(--enable-native-access=ALL-UNNAMED)
[ "${checkjni}" -eq 1 ] && jvm_args+=(-Xcheck:jni)

if [ "${use_jsig}" -eq 1 ]; then
  java_home="$(java -XshowSettings:properties -version 2>&1 \
    | sed -n 's/^ *java\.home *= *//p')"
  case "$(uname -s)" in
    Darwin) jsig="${java_home}/lib/libjsig.dylib" ;;
    *)      jsig="${java_home}/lib/libjsig.so" ;;
  esac
  [ -f "${jsig}" ] || fail "no libjsig at ${jsig} — this JDK does not ship it"
  echo "signal-probe: preloading ${jsig}"
  if [ "$(uname -s)" = "Darwin" ]; then
    DYLD_INSERT_LIBRARIES="${jsig}" java "${jvm_args[@]}" -cp "${tmp}" \
      SignalHandlerProbe "${libpath}"
  else
    LD_PRELOAD="${jsig}" java "${jvm_args[@]}" -cp "${tmp}" \
      SignalHandlerProbe "${libpath}"
  fi
else
  java "${jvm_args[@]}" -cp "${tmp}" SignalHandlerProbe "${libpath}"
fi
