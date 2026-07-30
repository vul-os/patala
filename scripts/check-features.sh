#!/usr/bin/env bash
#
# check-features.sh — keep the fiat processor set and the Cargo feature flags
# that expose it in lock-step.
#
# patala-fiat ships one module directory per processor (patala-fiat/src/<name>/)
# and a Cargo feature per processor. patala-py re-exports each as `fiat-<name>`
# (which must enable exactly `patala-fiat/<name>`), and `fiat-all` must enable
# every one — that is the feature patala-go builds its cdylib with, so a
# processor missing from `fiat-all` is silently absent from the Go binding.
#
# Nothing enforced this before: it held only because each new processor was
# added to every list by hand. This script is that enforcement — three source
# files must agree, or `make check` fails. Pure bash + coreutils, no toolchain.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fiat_toml="$root/patala-fiat/Cargo.toml"
py_toml="$root/patala-py/Cargo.toml"
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

# fiat-all's members, one "fiat-<name>" per line.
fiat_all_members="$(awk '/^fiat-all *= *\[/{f=1} f{print} /\]/{if(f)exit}' "$py_toml" \
  | grep -oE '"fiat-[a-z0-9]+"' | tr -d '"' | sort -u)"

for p in $processors; do
  # 1. patala-fiat defines the per-processor feature.
  grep -qE "^$p *= *\[" "$fiat_toml" \
    || note "patala-fiat/Cargo.toml is missing a [$p] feature for src/$p/"

  # 2. patala-py maps fiat-<p> to exactly patala-fiat/<p>.
  py_line="$(grep -E "^fiat-$p *= *\[" "$py_toml" || true)"
  if [ -z "$py_line" ]; then
    note "patala-py/Cargo.toml is missing the fiat-$p feature"
  elif ! printf '%s' "$py_line" | grep -q "patala-fiat/$p\b"; then
    note "patala-py fiat-$p does not enable patala-fiat/$p (line: $py_line)"
  fi

  # 3. fiat-all includes fiat-<p>.
  printf '%s\n' "$fiat_all_members" | grep -qx "fiat-$p" \
    || note "patala-py fiat-all is missing fiat-$p (patala-go's cdylib would omit it)"

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
    || note "patala-py fiat-all lists $m but patala-fiat/src/$name/ does not exist"
done

if [ "$fail" -ne 0 ]; then
  echo "check-features: FAILED — the fiat processor set and feature flags disagree." >&2
  exit 1
fi

n="$(printf '%s\n' "$processors" | grep -c .)"
echo "check-features: OK — $n fiat processors consistent across patala-fiat + patala-py \
(fiat-all complete, all covered by tests/webhook_coverage.rs)."
