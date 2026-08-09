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
# patala-py is a FORWARDER: it declares the same `fiat-<name>` names, enabling
# exactly `patala-uniffi/fiat-<name>`. A forwarder that skips a processor
# cannot build a wheel with that processor in it, however complete
# patala-uniffi is — so it is checked too, in the same loop, rather than being
# trusted to keep up.
#
# Nothing enforced this before: it held only because each new processor was
# added to every list by hand. This script is that enforcement — four source
# files must agree, or `make check` fails. Pure bash + coreutils, no toolchain.
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
)
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

# fiat-all's members in patala-uniffi (the authority), one "fiat-<name>" per
# line. The forwarders' own fiat-all is a single "patala-uniffi/fiat-all", so
# there is only ever one enumerated list to keep in step with src/.
fiat_all_members="$(awk '/^fiat-all *= *\[/{f=1} f{print} /\]/{if(f)exit}' "$uniffi_toml" \
  | grep -oE '"fiat-[a-z0-9]+"' | tr -d '"' | sort -u)"

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

  # 3. fiat-all includes fiat-<p>.
  printf '%s\n' "$fiat_all_members" | grep -qx "fiat-$p" \
    || note "patala-uniffi fiat-all is missing fiat-$p (patala-go's cdylib would omit it)"

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
+ its patala-py forwarder (fiat-all complete, all covered by tests/webhook_coverage.rs)."
