#!/usr/bin/env bash
# =============================================================================
# release-stage.sh — build, PROVE and package patala's C ABI bundle for one
# platform, into the directory the release manifest covers.
#
# WHY THIS IS A SCRIPT AND NOT A WORKFLOW STEP
# ────────────────────────────────────────────
# `.github/workflows/release.yml` cannot be run on a developer's machine, so
# any guard written inline there is a guard nobody has ever seen fail. Every
# patala-specific assertion the release depends on lives here instead, where
# `bash scripts/release-stage.sh --version "$(cat VERSION)" ...` runs it for
# real. The workflow keeps only the parts that are genuinely CI-shaped: the
# manifest emit (copied verbatim from the suite's RELEASE-TEMPLATE.md), the
# attestation, and the upload.
#
# WHAT IT PRODUCES
# ────────────────
#   <outdir>/patala_<version>_c-abi_<os>_<arch>.tar.gz
#
# containing lib/, include/, both licences, VERSION and the C ABI's README.
# The header and the library travel in ONE archive on purpose: they are a
# matched pair, and a consumer who can download them separately will
# eventually pair a new header with an old library — which is precisely the
# drift `patala_abi_check()` exists to catch at runtime and which nothing
# catches at download time.
#
# WHAT IT REFUSES (each of these is a release that must not happen)
# ─────────────────────────────────────────────────────────────────
#   2  usage error
#   3  ./VERSION missing, empty, or not equal to the version being released
#   4  a workspace crate's Cargo.toml version disagrees with ./VERSION
#   5  the host is not the platform the asset name would claim
#   6  the published feature set does not cover every rail patala-ffi exposes
#   7  the built library is missing, empty, or not a shared object for the
#      platform the asset name claims
#   8  the finished tarball does not contain exactly the expected files
#
# Exit 0 means: this tarball was built on the platform it names, from sources
# whose declared version matches the tag, with every rail compiled in, and the
# library inside it was dlopened and driven through a real charge -> verify
# round trip by patala-ffi/ctest/smoke.c before it was packaged.
#
# USAGE
#   scripts/release-stage.sh --version 0.1.1 --os linux  --arch amd64
#   scripts/release-stage.sh --version 0.1.1 --os darwin --arch arm64 --outdir release
# =============================================================================
set -euo pipefail

E_USAGE=2
E_VERSION=3
E_CRATE_VERSION=4
E_PLATFORM=5
E_FEATURES=6
E_LIBRARY=7
E_CONTENTS=8

SELF="release-stage.sh"

die() {
  local code="$1"; shift
  printf '%s: FATAL: %s\n' "$SELF" "$1" >&2
  shift
  local line
  for line in "$@"; do printf '        %s\n' "$line" >&2; done
  exit "$code"
}
info() { printf '%s: %s\n' "$SELF" "$*"; }

# The feature set the PUBLISHED library is built with. Every rail patala-ffi
# can expose, on: a consumer who downloads a prebuilt C library cannot add a
# rail later without a Rust toolchain, which is the whole reason this artifact
# exists. `fiat-all` covers the twenty processor adapters one for one (see
# scripts/check-features.sh); the three network rails are named individually
# because patala-ffi has no "every rail" feature and deliberately should not
# grow one — the DEFAULT build staying offline and small is a property the
# README quotes a size for.
PUBLISH_FEATURES="fiat-all,solana,stellar,hyperswitch"

VERSION=""
OS=""
ARCH=""
OUTDIR="release"
SELFTEST=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version)  VERSION="${2:-}"; shift 2 ;;
    --version=*) VERSION="${1#*=}"; shift ;;
    --os)       OS="${2:-}";      shift 2 ;;
    --os=*)     OS="${1#*=}";     shift ;;
    --arch)     ARCH="${2:-}";    shift 2 ;;
    --arch=*)   ARCH="${1#*=}";   shift ;;
    --outdir)   OUTDIR="${2:-}";  shift 2 ;;
    --outdir=*) OUTDIR="${1#*=}"; shift ;;
    --selftest) SELFTEST=1;       shift ;;
    -h|--help)  sed -n '/^# USAGE/,$p' -- "$0" | sed 's/^# \{0,1\}//' | head -5; exit 0 ;;
    *) die "$E_USAGE" "unknown argument: $1" "Run with --help for usage." ;;
  esac
done

# ── --selftest: the refusals that do not need a build, run for real ──────────
# Everything above the cargo invocation can be exercised in a second and with
# no toolchain, so CI does it on every push. A guard that has quietly stopped
# refusing is indistinguishable, from a green pipeline, from one that works.
#
# It is deliberately PARTIAL and says so: the feature-coverage, magic-byte and
# tarball-content refusals need a built library and are therefore only
# exercised by the release job itself. They are listed in the output as not
# covered rather than left for a reader to assume.
if [ "$SELFTEST" -eq 1 ]; then
  self_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  real_version="$(tr -d '[:space:]' < "${self_root}/VERSION")"
  st_fail=0
  st_case() { # <label> <want-exit> -- <args...>
    local label="$1" want="$2"; shift 2
    [ "${1:-}" = "--" ] && shift
    local out rc=0
    out="$(bash "${BASH_SOURCE[0]}" "$@" 2>&1)" || rc=$?
    local diag
    diag="$(printf '%s' "$out" | grep -m1 'FATAL:' | sed 's/.*FATAL: *//' || true)"
    local verdict="ok"
    if [ "$rc" -ne "$want" ]; then
      verdict="FAIL(exit ${rc}, want ${want})"; st_fail=$((st_fail + 1))
    elif [ -z "$diag" ]; then
      # Aborting with no message reads as a crash, not a refusal.
      verdict="FAIL(silent)"; st_fail=$((st_fail + 1)); diag="(NO DIAGNOSTIC PRINTED)"
    fi
    printf '  %-38s exit %-3s %-22s %s\n' "$label" "$rc" "$verdict" "$diag"
  }

  printf '\n%s selftest — refusals that need no build\n\n' "$SELF"
  printf '  %-38s %-8s %-22s %s\n' "CASE" "EXIT" "VERDICT" "DIAGNOSTIC (first line)"
  printf '  %s\n' "----------------------------------------------------------------------------"
  st_case "no --version"                "$E_USAGE"    -- --os linux --arch amd64
  st_case "no --os / --arch"            "$E_USAGE"    -- --version "$real_version"
  st_case "unknown argument"            "$E_USAGE"    -- --version "$real_version" --os linux --arch amd64 --sign-it-anyway
  st_case "tag disagrees with ./VERSION" "$E_VERSION" -- --version "${real_version}-not-the-tree" --os linux --arch amd64
  st_case "host is not the named OS"     "$E_PLATFORM" -- --version "$real_version" --os plan9 --arch "$(uname -m)"
  st_case "host is not the named arch"   "$E_PLATFORM" -- --version "$real_version" --os "$(uname -s | tr '[:upper:]' '[:lower:]')" --arch sparc64
  printf '\n'
  if [ "$st_fail" -ne 0 ]; then
    die 1 "selftest: ${st_fail} case(s) did not behave as specified."
  fi
  printf '%s: selftest passed — 6 cases, each refused with its own exit code and its own diagnostic.\n' "$SELF"
  printf '%s: NOT covered here (they need a built library, and run for real in the release job):\n' "$SELF"
  printf '%s\n' "  - a crate manifest left behind at the previous version (exit ${E_CRATE_VERSION})" \
                "  - a rail missing from the published feature set (exit ${E_FEATURES})" \
                "  - a library whose magic bytes are not the claimed platform's (exit ${E_LIBRARY})" \
                "  - a tarball that does not contain exactly the expected files (exit ${E_CONTENTS})"
  exit 0
fi

[ -n "$VERSION" ] || die "$E_USAGE" "--version is required." \
  "Pass the version being released, WITHOUT the leading 'v'."
[ -n "$OS" ] && [ -n "$ARCH" ] || die "$E_USAGE" \
  "--os and --arch are both required." \
  "They name the platform the asset claims to be for, and are checked against" \
  "this host rather than trusted."

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/.." && pwd)"
cd "$root"

# ── 1. The version being released must be the version in the tree ────────────
# The tag is the only thing a user sees; ./VERSION is what gets compiled into
# the library and what patala_abi_check() compares a caller's bindings
# against. If they disagree, the release ships a library that will refuse the
# very version string the release page tells people to expect.
[ -f VERSION ] || die "$E_VERSION" "./VERSION does not exist." \
  "The C ABI's version probe is checked against this file; without it there is" \
  "nothing to pin the tag to."
file_version="$(tr -d '[:space:]' < VERSION)"
[ -n "$file_version" ] || die "$E_VERSION" "./VERSION is empty." \
  "An empty version would make the abi-version check compare against nothing."
if [ "$file_version" != "$VERSION" ]; then
  die "$E_VERSION" \
    "./VERSION says '${file_version}' but this release is '${VERSION}'." \
    "Releasing anyway would publish a library whose patala_abi_version() does" \
    "not match the tag it is published under, so every consumer's" \
    "patala_abi_check() would refuse it."
fi

# ── 2. …and the version every crate in the workspace declares ────────────────
# One crate left behind at the previous version is a release where `cargo
# build -p patala-ffi` and the tag mean different things.
mismatched=()
while IFS= read -r manifest; do
  crate_version="$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/,"");sub(/^version=/,"");print;exit}' "$manifest")"
  [ "$crate_version" = "$VERSION" ] || mismatched+=("${manifest} declares '${crate_version}'")
done < <(find . -mindepth 2 -maxdepth 2 -name Cargo.toml -path './patala-*' | LC_ALL=C sort)

# A zero-crate scan would pass this check while checking nothing — the exact
# shape of guard this repo has shipped before. Pin the count.
crate_count="$(find . -mindepth 2 -maxdepth 2 -name Cargo.toml -path './patala-*' | wc -l | tr -d ' ')"
if [ "$crate_count" -lt 9 ]; then
  die "$E_CRATE_VERSION" \
    "found only ${crate_count} workspace crate manifest(s) — expected at least 9." \
    "The version scan matched almost nothing, so passing it would mean nothing."
fi
if [ "${#mismatched[@]}" -ne 0 ]; then
  die "$E_CRATE_VERSION" \
    "${#mismatched[@]} crate(s) do not declare version '${VERSION}':" \
    "${mismatched[@]}" \
    "Bump them, or tag the version the workspace actually is."
fi
info "version ${VERSION} agrees with ./VERSION and all ${crate_count} crate manifests"

# ── 3. The host must be the platform the asset name claims ───────────────────
# An asset called ..._linux_amd64.tar.gz containing a darwin/arm64 dylib is a
# lie a user only discovers at dlopen time. This is not hypothetical: the whole
# reason this matrix exists is that GitHub's `macos-latest` silently changed
# architecture once already.
host_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
host_arch="$(uname -m)"
case "$host_arch" in
  x86_64|amd64) host_arch="amd64" ;;
  arm64|aarch64) host_arch="arm64" ;;
esac
if [ "$host_os" != "$OS" ] || [ "$host_arch" != "$ARCH" ]; then
  die "$E_PLATFORM" \
    "this host is ${host_os}/${host_arch} but the asset would be named ${OS}_${ARCH}." \
    "Nothing here cross-compiles: the C smoke test below dlopens the library it" \
    "just built, which is only possible on the target platform. Run this on a" \
    "${OS}/${ARCH} machine, or fix the matrix leg that scheduled it here."
fi

# ── 4. The published build must expose every rail patala-ffi has ─────────────
# A new rail wired into patala-ffi but left out of PUBLISH_FEATURES would
# vanish from every prebuilt library while the source kept advertising it, and
# nothing else in the repo would notice. scripts/check-features.sh guards the
# twenty fiat processors; this guards the rails above them.
declared_rails="$(awk '/^\[features\]/{p=1;next} /^\[/{p=0}
  p && /^[a-z0-9-]+ *= *\[/ {
    name=$1
    if (name == "default" || name == "fiat") next
    if (name ~ /^fiat-/ && name != "fiat-all") next
    print name
  }' patala-ffi/Cargo.toml | LC_ALL=C sort)"
[ -n "$declared_rails" ] || die "$E_FEATURES" \
  "found no rail features in patala-ffi/Cargo.toml." \
  "The parse matched nothing, so this check would pass while checking nothing."
published_rails="$(printf '%s\n' "${PUBLISH_FEATURES//,/$'\n'}" | LC_ALL=C sort)"
if [ "$declared_rails" != "$published_rails" ]; then
  die "$E_FEATURES" \
    "PUBLISH_FEATURES does not match the rails patala-ffi declares." \
    "declared:   $(printf '%s' "$declared_rails" | tr '\n' ' ')" \
    "publishing: $(printf '%s' "$published_rails" | tr '\n' ' ')" \
    "A rail that exists in the source but not in the published library is a" \
    "feature every prebuilt consumer silently does not have."
fi
info "publishing with every declared rail: ${PUBLISH_FEATURES}"

# ── 5. Build it, and prove the built bytes work before packaging them ────────
# ffi-ctest.sh builds the cdylib and then dlopens THAT artifact from C,
# resolving every symbol by name through include/patala.h and driving a real
# charge -> verify round trip. Packaging and proving are one step on purpose:
# there is no ordering in which this script can tar up a library that was not
# just exercised.
info "building and exercising the C ABI (release, --features ${PUBLISH_FEATURES}) ..."
./scripts/ffi-ctest.sh --release --features "$PUBLISH_FEATURES"

case "$OS" in
  darwin) libfile="libpatala_ffi.dylib" ;;
  *)      libfile="libpatala_ffi.so" ;;
esac
libpath="target/release/${libfile}"
[ -s "$libpath" ] || die "$E_LIBRARY" \
  "no library at ${libpath} (or it is empty) after a successful build." \
  "Refusing to publish a bundle whose whole point is that library."

# ── 6. The library must really be a shared object for the claimed platform ───
# Read the magic bytes rather than shelling out to `file`, whose wording is not
# a stable interface. Belt and braces with the uname check above: that one
# catches a mis-scheduled runner, this one catches a stale artifact left in
# target/ by a previous build for another target.
magic="$(head -c 20 -- "$libpath" | od -An -tx1 | tr -d ' \n')"
case "${OS}/${ARCH}" in
  linux/amd64)
    # ELF, e_machine == 0x3E (x86-64) at byte offset 18.
    [ "${magic:0:8}" = "7f454c46" ] && [ "${magic:36:4}" = "3e00" ] || die "$E_LIBRARY" \
      "${libpath} is not an x86-64 ELF shared object (magic ${magic:0:8}, e_machine ${magic:36:4})." \
      "The asset would be named linux_amd64 and would not load on one."
    ;;
  darwin/arm64)
    # Mach-O 64 (cffaedfe little-endian), cputype 0x0100000C == arm64.
    [ "${magic:0:8}" = "cffaedfe" ] && [ "${magic:8:8}" = "0c000001" ] || die "$E_LIBRARY" \
      "${libpath} is not an arm64 Mach-O shared library (magic ${magic:0:8}, cputype ${magic:8:8})." \
      "The asset would be named darwin_arm64 and would not load on one."
    ;;
  *)
    die "$E_PLATFORM" "no magic-byte expectation is written for ${OS}/${ARCH}." \
      "Add one rather than publishing an unchecked binary: a platform nobody" \
      "verified is a platform this release should not claim."
    ;;
esac
info "library is a ${OS}/${ARCH} shared object, $(wc -c < "$libpath" | tr -d ' ') bytes"

# ── 7. Stage the bundle ──────────────────────────────────────────────────────
bundle="patala-${VERSION}-c-abi-${OS}-${ARCH}"
tarball="patala_${VERSION}_c-abi_${OS}_${ARCH}.tar.gz"
staging="$(mktemp -d "${TMPDIR:-/tmp}/patala-stage.XXXXXX")"
trap 'rm -rf -- "$staging"' EXIT

mkdir -p "${staging}/${bundle}/lib" "${staging}/${bundle}/include"
cp -- "$libpath"                    "${staging}/${bundle}/lib/${libfile}"
cp -- patala-ffi/include/patala.h   "${staging}/${bundle}/include/patala.h"
cp -- patala-ffi/README.md          "${staging}/${bundle}/README.md"
cp -- VERSION                       "${staging}/${bundle}/VERSION"
cp -- LICENSE-MIT                   "${staging}/${bundle}/LICENSE-MIT"
cp -- LICENSE-APACHE                "${staging}/${bundle}/LICENSE-APACHE"

mkdir -p -- "$OUTDIR"
outpath="$(cd "$OUTDIR" && pwd)/${tarball}"
# COPYFILE_DISABLE: macOS bsdtar otherwise packs an AppleDouble `._x` sibling
# for every file carrying an extended attribute. The content assertion below
# would catch them; not creating them is better than reporting them.
COPYFILE_DISABLE=1 tar -czf "$outpath" -C "$staging" "$bundle"

# ── 8. The tarball must contain exactly what it is supposed to contain ───────
# `tar czf` of a half-populated directory succeeds and produces a tarball. The
# only thing that distinguishes "packaged the bundle" from "packaged whatever
# happened to be there" is comparing the finished archive's file list against
# the expected one — not a count, which a substitution satisfies, but the set.
expected="$(printf '%s\n' \
  "${bundle}/LICENSE-APACHE" \
  "${bundle}/LICENSE-MIT" \
  "${bundle}/README.md" \
  "${bundle}/VERSION" \
  "${bundle}/include/patala.h" \
  "${bundle}/lib/${libfile}" | LC_ALL=C sort)"
actual="$(tar -tzf "$outpath" | grep -v '/$' | LC_ALL=C sort)"
if [ "$expected" != "$actual" ]; then
  # die() prints one argument per line, so the two listings are expanded into
  # one argument each rather than passed as two embedded newline blobs — a
  # diagnostic nobody can read is most of the way to no diagnostic at all.
  msg=("${tarball} does not contain the expected files." "--- expected ---")
  while IFS= read -r l; do msg+=("  ${l}"); done <<< "$expected"
  msg+=("--- actual ---")
  if [ -n "$actual" ]; then
    while IFS= read -r l; do msg+=("  ${l}"); done <<< "$actual"
  else
    msg+=("  (nothing)")
  fi
  msg+=("The tarball has been deleted so no later step can publish it.")
  rm -f -- "$outpath"
  die "$E_CONTENTS" "${msg[@]}"
fi

info "staged ${OUTDIR}/${tarball} ($(wc -c < "$outpath" | tr -d ' ') bytes, $(printf '%s\n' "$actual" | wc -l | tr -d ' ') files)"
