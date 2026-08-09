#!/usr/bin/env bash
#
# uniffi-ruby-probe.sh — why patala's Ruby SDK is not generated UniFFI.
#
# UniFFI 0.29 lists Ruby among its backends (`uniffi-bindgen generate
# --language ruby`), and patala carries the one #[uniffi::export] surface that
# patala-py and patala-go are generated from. So the obvious question for
# sdks/ruby — which reaches patala through the plain C ABI with `fiddle` — is
# why it is not generated too.
#
# Because the generated Ruby is not valid Ruby. It does not parse. Not "is
# unidiomatic", not "fails at runtime": `ruby -c` rejects the file.
#
# THE BUG, exactly. An interface constructor or method argument named `class`
# is renamed to `_class` in the `def` line and left as `class` in the body, so
# the generator emits
#
#     def self.new_mock(id, _class, currencies, fee_minor, failing)
#       class = class          # <- a class definition, in a method body
#
# patala hits this because `RailCapabilities.class` is the field that decides
# an entire integration's UX (a `CustodialReversible` rail means a card form
# and a refundable pending state; a `NonCustodialFinal` rail means a wallet
# address and a signed final receipt), and `PatalaRail::new_mock` takes it.
#
# It is a Ruby-backend bug and not a patala one, which this script proves by
# generating the SAME UDL to Python, where the identical rename is applied
# consistently and the output parses.
#
# EXIT CODES ARE INVERTED, on purpose:
#
#   0  the Ruby backend still emits invalid Ruby, as documented. sdks/ruby's
#      hand-written C-ABI binding is not a missed opportunity.
#   1  it has been FIXED. Generated Ruby would give real PayRequest / Receipt /
#      DestinationVerdict objects and a typed PatalaError instead of JSON
#      strings, which is worth reconsidering sdks/ruby for.
#   2  the probe could not run — a missing tool, or a CONTROL case failing,
#      which would mean a red result here proves nothing.
#
# Usage:  scripts/uniffi-ruby-probe.sh
#
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() { echo "ruby-probe: FAIL — $*" >&2; exit 2; }

command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH"
command -v ruby >/dev/null 2>&1 || fail "ruby is not on PATH"
command -v python3 >/dev/null 2>&1 || fail "python3 is not on PATH (used as the contrast case)"

echo "ruby-probe: $(ruby --version)"
uniffi_version="$(cd "${root}" && cargo tree -p patala-uniffi -i uniffi --depth 0 2>/dev/null \
  | head -1 | awk '{print $2}')"
echo "ruby-probe: uniffi ${uniffi_version:-unknown}"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# uniffi-bindgen locates a crate root next to the UDL, so each case is a
# throwaway crate directory. Neither is ever compiled by cargo — the Ruby
# backend needs no cdylib, which is what keeps this probe cheap.
#
#   $1 = case name, $2 = the constructor argument, $3 = language
generate_case() {
  local name="$1"
  local arg="$2"
  local language="$3"
  local dir="${tmp}/${name}-${language}"
  mkdir -p "${dir}/src"
  cat > "${dir}/Cargo.toml" <<EOF
[package]
name = "${name}"
version = "0.0.0"
edition = "2021"
EOF
  cat > "${dir}/src/${name}.udl" <<EOF
namespace ${name} {};

interface Rail {
  constructor(string ${arg});
};
EOF
  ( cd "${root}" && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
      "${dir}/src/${name}.udl" --language "${language}" --no-format --crate "${name}" \
      --out-dir "${dir}/out" ) || fail "uniffi-bindgen could not generate ${language} for ${name}"
  echo "${dir}/out"
}

# --- the control: the same interface, with an argument that is not `class` ---
echo
echo "ruby-probe: control — a constructor argument named \`kind\`…"
control="$(generate_case control kind ruby)"
if ruby -c "${control}/control.rb" >/dev/null 2>&1; then
  echo "ruby-probe: control PARSES, as it must. The toolchain is sound."
else
  ruby -c "${control}/control.rb" 2>&1 | head -10 >&2 || true
  fail "the control must parse, or a failure below proves nothing"
fi

# --- the contrast: the SUBJECT UDL, generated to Python ----------------------
echo
echo "ruby-probe: contrast — the same \`class\` argument, generated to Python…"
contrast="$(generate_case subject class python)"
if python3 -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" "${contrast}/subject.py"; then
  echo "ruby-probe: the Python backend renames it consistently and PARSES."
else
  fail "the Python contrast case did not parse — the UDL itself may be at fault"
fi

# --- the subject -------------------------------------------------------------
echo
echo "ruby-probe: subject — a constructor argument named \`class\`, in Ruby…"
subject="$(generate_case subject class ruby)"
if ruby -c "${subject}/subject.rb" >/dev/null 2>&1; then
  echo
  echo "ruby-probe: the \`class\` case PARSES — the Ruby backend bug is FIXED."
  echo
  echo "  Generated Ruby is now worth reconsidering for sdks/ruby: it would"
  echo "  replace JSON strings with real PayRequest / Receipt /"
  echo "  DestinationVerdict objects and a typed PatalaError, the same win"
  echo "  sdks/kotlin took when its own blocker was lifted."
  echo
  echo "  Check patala's whole surface, not just this probe:"
  echo "    cargo build -p patala-uniffi --release"
  echo "    cargo run -p patala-uniffi --bin uniffi-bindgen -- generate \\"
  echo "      --library target/release/libpatala_uniffi.dylib --language ruby --out-dir <dir>"
  echo "    ruby -c <dir>/patala.rb"
  exit 1
fi
echo "ruby-probe: it does NOT parse:"
# `ruby -c` exits non-zero here, which is the point — `|| true` keeps
# `set -e -o pipefail` from treating the expected failure as a probe failure.
ruby -c "${subject}/subject.rb" 2>&1 | grep -E 'syntax error|unexpected' | head -4 | sed 's/^/    /' || true

# --- and patala's own surface, which is the thing that actually matters ------
echo
echo "ruby-probe: patala's own generated Ruby…"
case "$(uname -s)" in
  Darwin) libext="dylib" ;;
  *)      libext="so" ;;
esac
( cd "${root}" && cargo build -q -p patala-uniffi --release ) || fail "patala-uniffi did not build"
lib="${root}/target/release/libpatala_uniffi.${libext}"
[ -f "${lib}" ] || fail "no cdylib at ${lib}"
( cd "${root}" && cargo run -q -p patala-uniffi --bin uniffi-bindgen -- generate \
    --library "${lib}" --language ruby --no-format --out-dir "${tmp}/patala" ) \
  || fail "uniffi-bindgen could not generate Ruby for patala"
[ -f "${tmp}/patala/patala.rb" ] || fail "expected ${tmp}/patala/patala.rb"
if ruby -c "${tmp}/patala/patala.rb" >/dev/null 2>&1; then
  echo
  echo "ruby-probe: patala's OWN generated Ruby parses, but the isolated"
  echo "  \`class\` case above does not. Something has changed shape — re-read"
  echo "  both before trusting either. Treat this as the fixed case."
  exit 1
fi
echo "ruby-probe: it does not parse either, at PatalaRail.new_mock:"
ruby -c "${tmp}/patala/patala.rb" 2>&1 | grep -E 'class = class|unexpected constant path' | head -2 | sed 's/^/    /' || true

echo
echo "ruby-probe: OK (expected failure reproduced; control and contrast both passed)"
exit 0
