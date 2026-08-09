#!/usr/bin/env bash
#
# uniffi-kotlin-probe.sh — the evidence behind this SDK's build decision.
#
# UniFFI has a first-class Kotlin backend, patala-uniffi carries the workspace's
# one #[uniffi::export] surface under the namespace `patala`, and patala-py and
# patala-go are both generated from it. So the obvious question for a Kotlin SDK
# is: why is this one a wrapper over sdks/java's C-ABI binding instead of
# generated Kotlin with real `PayRequest`/`Receipt`/`DestinationVerdict` types?
#
# Because the generated Kotlin does not compile. This script is that claim,
# executable: it generates the bindings with the bindgen this workspace already
# pins (uniffi 0.29.x, via `cargo run -p patala-uniffi --bin uniffi-bindgen` —
# no separately installed CLI, so nothing here can drift from the scaffolding
# the cdylib carries) and then hands them to kotlinc.
#
# EXIT CODES ARE INVERTED, on purpose:
#
#   0  the generated Kotlin FAILED to compile, as README.md documents.
#   1  the generated Kotlin COMPILED — the blocker is gone, and this SDK's
#      justification for wrapping the Java binding is now stale. Re-decide.
#
# A README that quotes a compiler error is only honest while the error is still
# there. This is the thing that notices when it stops being.
#
# Usage:  sdks/kotlin/uniffi-kotlin-probe.sh
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"

fail() { echo "uniffi-probe: FAIL — $*" >&2; exit 2; }

if ! command -v java >/dev/null 2>&1 && [ -n "${JAVA_HOME:-}" ]; then
  PATH="${JAVA_HOME}/bin:${PATH}"
  export PATH
fi
command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH"
command -v kotlinc >/dev/null 2>&1 || fail "kotlinc is not on PATH"

case "$(uname -s)" in
  Darwin) libext="dylib" ;;
  *)      libext="so" ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# --- 1. the cdylib carrying the UniFFI metadata ------------------------------
echo "uniffi-probe: building patala-uniffi…"
( cd "${root}" && cargo build -q -p patala-uniffi --release ) \
  || fail "patala-uniffi did not build"
lib="${root}/target/release/libpatala_uniffi.${libext}"
[ -f "${lib}" ] || fail "no cdylib at ${lib}"

uniffi_version="$(cd "${root}" && cargo tree -p patala-uniffi -i uniffi --depth 0 2>/dev/null \
  | head -1 | awk '{print $2}')"
echo "uniffi-probe: cdylib $(wc -c < "${lib}" | tr -d ' ') bytes, uniffi ${uniffi_version:-unknown}"

# --- 2. generate the Kotlin bindings -----------------------------------------
#
# --no-format: the generator shells out to ktlint for cosmetics and warns when
# it is absent. Formatting is not what is being measured.
echo "uniffi-probe: generating Kotlin bindings…"
( cd "${root}" && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
    --library "${lib}" --language kotlin --no-format --out-dir "${tmp}/bindings" ) \
  || fail "uniffi-bindgen could not generate Kotlin"

generated="${tmp}/bindings/uniffi/patala/patala.kt"
[ -f "${generated}" ] || fail "expected generated Kotlin at ${generated}"
echo "uniffi-probe: generated $(wc -l < "${generated}" | tr -d ' ') lines at uniffi/patala/patala.kt"

# --- 3. the JNA jar the generated code imports -------------------------------
#
# Generated UniFFI Kotlin is a com.sun.jna.Library. That dependency is itself
# part of the finding — see README.md — but it must be present for the compile
# to be a fair test of anything else.
jna="$(ls "${HOME}"/.m2/repository/net/java/dev/jna/jna/*/jna-*.jar 2>/dev/null | head -1 || true)"
if [ -z "${jna}" ]; then
  if command -v mvn >/dev/null 2>&1; then
    echo "uniffi-probe: fetching net.java.dev.jna:jna…"
    mvn -q dependency:get -Dartifact=net.java.dev.jna:jna:5.14.0 >/dev/null 2>&1 || true
    jna="$(ls "${HOME}"/.m2/repository/net/java/dev/jna/jna/*/jna-*.jar 2>/dev/null | head -1 || true)"
  fi
fi
[ -n "${jna}" ] || fail "no JNA jar available; the generated bindings cannot be compiled without one"
echo "uniffi-probe: jna $(basename "${jna}")"

# --- 4. compile it -----------------------------------------------------------
echo "uniffi-probe: compiling with $(kotlinc -version 2>&1 | head -1)…"
set +e
kotlinc -nowarn -jvm-target 22 -classpath "${jna}" -d "${tmp}/out" "${generated}" \
  >"${tmp}/kotlinc.log" 2>&1
rc=$?
set -e

errors="$(grep -c 'error:' "${tmp}/kotlinc.log" || true)"

echo
if [ "${rc}" -eq 0 ]; then
  echo "uniffi-probe: the generated Kotlin COMPILED."
  echo
  echo "  This SDK wraps sdks/java's C-ABI binding because it did not. If it"
  echo "  does now — a newer uniffi, or patala-uniffi's PatalaError variants"
  echo "  renamed away from a field called \`message\` — then README.md's"
  echo "  justification is stale and the UniFFI route should be reconsidered:"
  echo "  it would give real PayRequest/Receipt/DestinationVerdict types"
  echo "  instead of JSON strings."
  exit 1
fi

echo "uniffi-probe: the generated Kotlin did NOT compile — ${errors} error(s)."
echo "This is the documented state. README.md quotes it."
echo
grep 'error:' "${tmp}/kotlinc.log" | sed 's|^.*/patala.kt|patala.kt|' | head -20
echo
echo "uniffi-probe: OK (expected failure reproduced)"
exit 0
