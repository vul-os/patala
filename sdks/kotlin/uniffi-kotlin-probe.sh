#!/usr/bin/env bash
#
# uniffi-kotlin-probe.sh — the upstream codegen bug that decides a field name
# in patala-uniffi, reproduced from first principles.
#
# HISTORY. This script used to generate patala's OWN Kotlin bindings and hand
# them to kotlinc, because they did not compile: `patala-uniffi`'s error enum
# had two variants carrying a field called `message`, UniFFI's Kotlin backend
# renders an error enum as a subclass of `kotlin.Exception` with a synthesised
# `override val message`, and a class cannot declare `message` twice. That was
# 12 kotlinc errors, and it is why this SDK was a wrapper over sdks/java's
# C-ABI binding, passing JSON strings around.
#
# The field was renamed to `detail` (commit 79e5002). patala's generated Kotlin
# now compiles, this SDK IS that generated Kotlin, and the probe's original
# subject is gone.
#
# The probe is not, because the CONSTRAINT is not. `detail` is a slightly worse
# public field name than `message` in every language patala generates, and the
# only reason it is not `message` is this bug. A constraint that lives in a
# commit message is a constraint the next person re-litigates; so this script
# now reproduces the bug ITSELF, in isolation, from a six-line UDL that has
# nothing to do with patala:
#
#   [Error]
#   interface ProbeError {
#     Rail(string message);        <- the subject: must NOT compile
#     InvalidRequest(string detail);
#   };
#
# It needs no cdylib and no cargo build — uniffi-bindgen generates Kotlin
# straight from the UDL — so it stays fast and stays honest.
#
# EXIT CODES ARE INVERTED, on purpose:
#
#   0  a `message` field still breaks UniFFI's Kotlin backend, as documented.
#      patala-uniffi's `detail` naming is still load-bearing. Nothing to do.
#   1  it has been FIXED upstream. `PatalaError`'s variants can go back to
#      `message`, which reads better everywhere; the rename is a public API
#      change across every generated binding, so it is a decision, not a
#      cleanup. README.md's "why `detail`" section is now stale.
#   2  the probe itself could not run — a missing tool, or the CONTROL case
#      (the identical UDL with only a `detail` field) failing to compile,
#      which would mean a red result here proves nothing.
#
# The control is the whole reason this is evidence rather than a coin flip: a
# probe whose only outcome is "kotlinc said no" cannot tell a codegen bug from
# a missing JNA jar.
#
# Usage:  sdks/kotlin/uniffi-kotlin-probe.sh
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"

# shellcheck source=lib.sh
source "${here}/lib.sh"

fail() { echo "uniffi-probe: FAIL — $*" >&2; exit 2; }

jdk_bin="$(patala_find_jdk_bin)" || exit 2
PATH="${jdk_bin}:${PATH}"
export PATH
command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH"
command -v kotlinc >/dev/null 2>&1 || fail "kotlinc is not on PATH (brew install kotlin)"

jna_version="$(patala_jna_version "${here}")" || exit 2
jna="$(patala_find_jna "${jna_version}")" || exit 2

uniffi_version="$(cd "${root}" && cargo tree -p patala-uniffi -i uniffi --depth 0 2>/dev/null \
  | head -1 | awk '{print $2}')"
echo "uniffi-probe: uniffi ${uniffi_version:-unknown}, $(kotlinc -version 2>&1 | head -1)"
echo "uniffi-probe: jna $(basename "${jna}")"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# uniffi-bindgen locates a crate root next to the UDL, so each case is a
# throwaway crate directory. Neither is ever compiled by cargo.
#
#   $1 = case name, $2 = the error variant line
generate_case() {
  local name="$1"
  local variant="$2"
  # Separate `local` statements on purpose: the builtin's arguments are
  # expanded before it runs, so `local a=1 b=$a` reads the OUTER a — which,
  # under `set -u`, is an unbound-variable abort rather than a wrong value.
  local dir="${tmp}/${name}"
  mkdir -p "${dir}/src"
  cat > "${dir}/Cargo.toml" <<EOF
[package]
name = "${name}"
version = "0.0.0"
edition = "2021"
EOF
  cat > "${dir}/src/${name}.udl" <<EOF
namespace ${name} {};

[Error]
interface ProbeError {
  ${variant}
};
EOF
  ( cd "${root}" && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
      "${dir}/src/${name}.udl" --language kotlin --no-format --crate "${name}" \
      --out-dir "${dir}/out" ) || fail "uniffi-bindgen could not generate Kotlin for ${name}"
  local generated="${dir}/out/uniffi/${name}/${name}.kt"
  [ -f "${generated}" ] || fail "expected generated Kotlin at ${generated}"
  echo "${generated}"
}

# $1 = generated file, $2 = log path; prints nothing, returns kotlinc's status
compile_case() {
  local generated="$1" log="$2" rc=0
  set +e
  kotlinc -nowarn -jvm-target 11 -classpath "${jna}" -d "$(dirname "${log}")/out" \
    "${generated}" >"${log}" 2>&1
  rc=$?
  set -e
  return "${rc}"
}

# --- the control: the same shape, with a field name that is not `message` ----
echo
echo "uniffi-probe: control — an error variant with a \`detail\` field…"
control="$(generate_case control 'InvalidRequest(string detail);')"
if compile_case "${control}" "${tmp}/control.log"; then
  echo "uniffi-probe: control COMPILED, as it must. The toolchain is sound."
else
  echo "uniffi-probe: the CONTROL case did not compile:" >&2
  grep 'error:' "${tmp}/control.log" | head -10 >&2
  fail "the control must compile, or a failure below proves nothing"
fi

# --- the subject: a field named `message` ------------------------------------
echo
echo "uniffi-probe: subject — an error variant with a \`message\` field…"
subject="$(generate_case subject 'Rail(string message);')"
if compile_case "${subject}" "${tmp}/subject.log"; then
  echo
  echo "uniffi-probe: the \`message\` case COMPILED — the upstream bug is FIXED."
  echo
  echo "  This is a real change, not a flake. UniFFI ${uniffi_version} no longer"
  echo "  emits a duplicate \`message\` declaration for an error variant that"
  echo "  carries one, which means patala-uniffi's PatalaError variants can go"
  echo "  back to \`message\` — a better name in every generated language, and"
  echo "  the only reason they are called \`detail\` is this bug."
  echo
  echo "  That rename is a public API change across every binding (Kotlin,"
  echo "  Swift, Python, Go), so it is a decision to make deliberately. Start"
  echo "  with README.md's \"Why the error field is called detail\" section,"
  echo "  which this probe exists to keep honest."
  exit 1
fi

errors="$(grep -c 'error:' "${tmp}/subject.log" || true)"
echo
echo "uniffi-probe: the \`message\` case did NOT compile — ${errors} error(s)."
echo "This is the documented state, and it is why patala-uniffi's error"
echo "variants carry \`detail\`. README.md quotes it."
echo
grep 'error:' "${tmp}/subject.log" | sed 's|^.*/subject.kt|subject.kt|' | head -10
echo
echo "uniffi-probe: OK (expected failure reproduced; control compiled)"
exit 0
