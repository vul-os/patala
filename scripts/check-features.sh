#!/usr/bin/env bash
#
# check-features.sh — keep the fiat processor set and the Cargo feature flags
# that expose it in lock-step.
#
# patala-fiat ships one module directory per processor (patala-fiat/src/<name>/)
# and a Cargo feature per processor. patala-uniffi re-exports each as
# `fiat-<name>` (which must enable exactly `patala-fiat/<name>`), and
# `fiat-all` must enable every one — that is the feature patala-go builds its
# cdylib with, so a processor missing from `fiat-all` is silently absent from
# the Go binding.
#
# patala-py and patala-ffi are FORWARDERS: each declares the same
# `fiat-<name>` names, enabling exactly `patala-uniffi/fiat-<name>`. A
# forwarder that skips a processor cannot build a wheel or a C library with
# that processor in it, however complete patala-uniffi is — so both are
# checked too, in the same loop, rather than being trusted to keep up.
#
# Nothing enforced this before: it held only because each new processor was
# added to every list by hand. This script is that enforcement — five source
# files must agree, or `make check` fails.
#
# It ALSO builds each processor feature ON ITS OWN (see "single-processor
# builds" below), which the list checks cannot substitute for: every list above
# was correct while `cargo check -p patala-fiat --features yoco` — and thirteen
# more — failed outright, because `src/httpshared.rs` gated itself on six
# feature names while fifteen adapters called into it. `--all-features` and the
# empty default set both compiled, so nothing in `make check` ever built the
# configuration an operator who wants ONE processor actually uses; the workaround
# is `--all-features`, i.e. linking twenty processors into a payments binary.
# Set PATALA_SKIP_FEATURE_BUILDS=1 to run only the (instant) list checks.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fiat_toml="$root/patala-fiat/Cargo.toml"
uniffi_toml="$root/patala-uniffi/Cargo.toml"
# name:manifest:expected-target — the crates that re-export patala-fiat's
# per-processor features. patala-uniffi is the authority (it owns the optional
# `patala-fiat` dependency); the other two forward to it.
forwarders=(
  "patala-uniffi:$uniffi_toml:patala-fiat"
  "patala-py:$root/patala-py/Cargo.toml:patala-uniffi"
  "patala-ffi:$root/patala-ffi/Cargo.toml:patala-uniffi"
)

# The crates whose `fiat-all` ENUMERATES every fiat-<name> (rather than
# forwarding a single `patala-uniffi/fiat-all`). Both have per-adapter code or
# tests of their own behind `#[cfg(feature = "fiat-<name>")]`, so an adapter
# missing from their `fiat-all` is code that the everything-on build silently
# does not compile. patala-py has no such cfg and forwards instead.
fiat_all_tomls=("$uniffi_toml" "$root/patala-ffi/Cargo.toml")
fiat_src="$root/patala-fiat/src"
webhook_cov="$root/patala-fiat/tests/webhook_coverage.rs"

fail=0
note() { echo "check-features: $*" >&2; fail=1; }

# The processors are exactly the module directories under patala-fiat/src/
# (manual lives in manual.rs — it is the always-on default, not feature-gated).
processors="$(find "$fiat_src" -mindepth 1 -maxdepth 1 -type d -exec basename {} \; | sort)"
if [ -z "$processors" ]; then
  echo "check-features: found no processor directories under $fiat_src" >&2
  exit 2
fi

# fiat-all's members, one "fiat-<name>" per line, for each crate that
# enumerates them. Read ONCE per crate, not once per processor.
fiat_all_members_of() {
  awk '/^fiat-all *= *\[/{f=1} f{print} /\]/{if(f)exit}' "$1" \
    | grep -oE '"fiat-[a-z0-9]+"' | tr -d '"' | sort -u
}
# `has_line <haystack> <line>` — exact-line membership without a pipeline.
# The obvious `printf ... | grep -qx` is subtly WRONG under `set -o pipefail`:
# grep exits as soon as it matches, printf takes SIGPIPE, and pipefail then
# reports the pipeline as failed for a check that actually PASSED. That made
# this script report random missing processors on a correct tree. Bash pattern
# matching has no pipeline and no race.
has_line() {
  case $'\n'"$1"$'\n' in
    *$'\n'"$2"$'\n'*) return 0 ;;
    *) return 1 ;;
  esac
}
fiat_all_members="$(fiat_all_members_of "$uniffi_toml")"
declare -a fiat_all_lists=()
for toml in "${fiat_all_tomls[@]}"; do
  fiat_all_lists+=("$(fiat_all_members_of "$toml")")
done

for p in $processors; do
  # 1. patala-fiat defines the per-processor feature.
  grep -qE "^$p *= *\[" "$fiat_toml" \
    || note "patala-fiat/Cargo.toml is missing a [$p] feature for src/$p/"

  # 2. Every re-exporting crate maps fiat-<p> onto the crate below it:
  #    patala-uniffi -> patala-fiat/<p>, and the forwarders -> patala-uniffi/fiat-<p>.
  for entry in "${forwarders[@]}"; do
    crate="${entry%%:*}"
    rest="${entry#*:}"
    toml="${rest%%:*}"
    target="${rest##*:}"
    if [ "$target" = "patala-fiat" ]; then
      want="patala-fiat/$p"
    else
      want="$target/fiat-$p"
    fi
    line="$(grep -E "^fiat-$p *= *\[" "$toml" || true)"
    if [ -z "$line" ]; then
      note "$crate/Cargo.toml is missing the fiat-$p feature"
    elif ! printf '%s' "$line" | grep -q "$want\b"; then
      note "$crate fiat-$p does not enable $want (line: $line)"
    fi
  done

  # 3. Every enumerating crate's fiat-all includes fiat-<p>.
  for i in "${!fiat_all_tomls[@]}"; do
    crate_dir="$(basename "$(dirname "${fiat_all_tomls[$i]}")")"
    has_line "${fiat_all_lists[$i]}" "fiat-$p" \
      || note "$crate_dir fiat-all is missing fiat-$p (the everything-on build would silently omit it)"
  done

  # 4. tests/webhook_coverage.rs names the processor, so the trait-surface
  #    coverage tests actually exercise it -- both verify_webhook and
  #    validate_destination, the two PaymentRail methods that have defaults and
  #    can therefore be silently left unimplemented. That test is
  #    feature-gated, so a count assertion INSIDE it cannot notice an adapter
  #    that was never added to its list -- only this structural check can.
  grep -q "feature = \"$p\"" "$webhook_cov" \
    || note "patala-fiat/tests/webhook_coverage.rs has no #[cfg(feature = \"$p\")] entry \
(src/$p/'s verify_webhook and validate_destination would never be exercised)"

  # 4b. ...and it is classified in that file's dest_shape() table, which says
  #     what the processor's `destination` field actually is (a redirect URL,
  #     the buyer's email, or nothing). dest_shape() panics on an unknown name
  #     rather than defaulting, but only for an adapter that reached it -- this
  #     catches one that was added to neither list.
  grep -q "\"$p\"" "$webhook_cov" \
    || note "patala-fiat/tests/webhook_coverage.rs's dest_shape() does not classify $p \
(decide whether its PayRequest::destination is a redirect URL, the buyer's email, or unread)"

  # 5. The processor feature enables the private `_adapter` marker. That marker
  #    is what compiles `httpshared` in and what gates the webhook-coverage
  #    test's existence; an adapter that forgets it would build a cdylib whose
  #    coverage test can be run without ever seeing that adapter.
  grep -qE "^$p *= *\[.*\"_adapter\"" "$fiat_toml" \
    || note "patala-fiat/Cargo.toml: feature [$p] does not enable \"_adapter\""
done

# Reverse direction: fiat-all must not list a processor that no longer exists.
for m in $fiat_all_members; do
  name="${m#fiat-}"
  [ -d "$fiat_src/$name" ] \
    || note "patala-uniffi fiat-all lists $m but patala-fiat/src/$name/ does not exist"
done

if [ "$fail" -ne 0 ]; then
  echo "check-features: FAILED — the fiat processor set and feature flags disagree." >&2
  exit 1
fi

n="$(printf '%s\n' "$processors" | grep -c .)"
echo "check-features: OK — $n fiat processors consistent across patala-fiat + patala-uniffi \
+ its patala-py/patala-ffi forwarders (fiat-all complete, all covered by tests/webhook_coverage.rs)."

# --- single-processor builds -------------------------------------------------
#
# One processor, alone, is the configuration this crate's whole feature layout
# exists to serve — and it was the one configuration nothing built. `--all-targets`
# so the feature-gated half of tests/webhook_coverage.rs is compiled too, and
# `-D warnings` because a helper left dead by a narrower feature set is how two
# of these broke even after they linked.
#
# Cheap after the first: the twenty runs share one target directory and differ
# only in patala-fiat's own codegen.
if [ "${PATALA_SKIP_FEATURE_BUILDS:-0}" = "1" ]; then
  echo "check-features: single-processor builds SKIPPED (PATALA_SKIP_FEATURE_BUILDS=1)."
  exit 0
fi
command -v cargo >/dev/null 2>&1 || {
  echo "check-features: FAILED — cargo is not on PATH, so the single-processor \
builds cannot run. Set PATALA_SKIP_FEATURE_BUILDS=1 to run only the list checks." >&2
  exit 1
}

built=0
for p in $processors; do
  if ! out="$(cd "$root" && cargo clippy -q -p patala-fiat --features "$p" --all-targets \
                 -- -D warnings 2>&1)"; then
    note "cargo clippy -p patala-fiat --features $p --all-targets FAILED:"
    printf '%s\n' "$out" | sed 's/^/    /' >&2
  else
    built=$((built + 1))
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "check-features: FAILED — $((n - built)) of $n processors do not build on their own. \
An operator who wants one processor must not be pushed to --all-features." >&2
  exit 1
fi
echo "check-features: OK — all $built processor features build and lint ALONE."
