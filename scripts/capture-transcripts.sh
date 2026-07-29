#!/usr/bin/env bash
# capture-transcripts.sh — run patala's real, honest surfaces and save their
# true terminal output as plain text, for render-shots.mjs to turn into the
# styled panes on site/index.html.
#
# This is the ONLY place those panes' text comes from. Nothing downstream
# is allowed to invent or edit a line of output — if a command's output
# changes, re-run this script and re-run render-shots.mjs, don't hand-edit
# the .txt files or the rendered panes.
#
# Usage:
#   ./scripts/capture-transcripts.sh
#
# Requires: cargo (workspace already buildable), python3, go + the
# uniffi-bindgen-go binary patala-go/Makefile expects (see its README).
# Everything this script cannot run, it skips and says so on stderr — it
# never fabricates a transcript.

set -uo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/site/assets/shots/transcripts"
mkdir -p "$OUT_DIR"
cd "$ROOT_DIR"

echo "==> patala/scripts/capture-transcripts.sh — capturing real output into $OUT_DIR"

# ---------------------------------------------------------------------------
# 1. Sidecar boot + real HTTP round-trips over curl.
# ---------------------------------------------------------------------------
SIDECAR_PORT="${PATALA_SIDECAR_PORT:-8420}"
TOKEN="$(openssl rand -hex 32)"
BOOT_LOG="$(mktemp)"

echo "==> booting patala-sidecar on 127.0.0.1:${SIDECAR_PORT}"
PATALA_SIDECAR_TOKEN="$TOKEN" PATALA_SIDECAR_PORT="$SIDECAR_PORT" \
  cargo run -p patala-sidecar >"$BOOT_LOG" 2>&1 &
SIDECAR_PID=$!

cleanup() { kill "$SIDECAR_PID" >/dev/null 2>&1; wait "$SIDECAR_PID" 2>/dev/null; }
trap cleanup EXIT

# Wait for the real boot line rather than a fixed sleep.
for _ in $(seq 1 60); do
  grep -q "listening on" "$BOOT_LOG" 2>/dev/null && break
  sleep 0.5
done

if grep -q "listening on" "$BOOT_LOG"; then
  {
    printf '$ export PATALA_SIDECAR_TOKEN=$(openssl rand -hex 32)\n'
    printf '$ cargo run -p patala-sidecar\n'
    grep -E "Compiling patala-sidecar|Finished .dev. profile|Running .target|listening on" "$BOOT_LOG" \
      | sed -E 's#/Users/[^ ]*/patala#patala#' \
      | sed -E $'s/\x1b\\[[0-9;]*m//g'
  } > "$OUT_DIR/sidecar-boot.txt"
  echo "==> wrote sidecar-boot.txt"

  {
    printf '$ curl -s http://127.0.0.1:%s/healthz\n' "$SIDECAR_PORT"
    curl -s "http://127.0.0.1:${SIDECAR_PORT}/healthz"
    printf '\n\n'

    printf '$ curl -s http://127.0.0.1:%s/v1/rails/mock \\\n    -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN"\n' "$SIDECAR_PORT"
    curl -s "http://127.0.0.1:${SIDECAR_PORT}/v1/rails/mock" \
      -H "Authorization: Bearer $TOKEN"
    printf '\n\n'

    printf '$ RECEIPT=$(curl -s http://127.0.0.1:%s/v1/rails/mock/charge \\\n    -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN" \\\n    -d '"'"'{"amount_minor":1250,"currency":"USDC","destination":"dest-wallet-abc123","reference":"order-42"}'"'"')\n' "$SIDECAR_PORT"
    RECEIPT="$(curl -s "http://127.0.0.1:${SIDECAR_PORT}/v1/rails/mock/charge" \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d '{"amount_minor":1250,"currency":"USDC","destination":"dest-wallet-abc123","reference":"order-42"}')"
    printf '$ echo "$RECEIPT"\n'
    echo "$RECEIPT"
    printf '\n\n'

    printf '$ curl -s http://127.0.0.1:%s/v1/rails/mock/verify \\\n    -H "Authorization: Bearer $PATALA_SIDECAR_TOKEN" -d "$RECEIPT"\n' "$SIDECAR_PORT"
    curl -s "http://127.0.0.1:${SIDECAR_PORT}/v1/rails/mock/verify" \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d "$RECEIPT"
    printf '\n\n'

    printf '$ curl -s -o /dev/null -w "%%{http_code}\\n" http://127.0.0.1:%s/v1/rails/mock\n' "$SIDECAR_PORT"
    curl -s -o /dev/null -w "%{http_code}  (no Authorization header — fail-closed)\n" \
      "http://127.0.0.1:${SIDECAR_PORT}/v1/rails/mock"
  } > "$OUT_DIR/sidecar-roundtrip.txt"
  echo "==> wrote sidecar-roundtrip.txt"
else
  echo "!! sidecar never printed 'listening on' — skipping sidecar transcripts (not fabricating)" >&2
  cat "$BOOT_LOG" >&2
fi

cleanup
trap - EXIT
rm -f "$BOOT_LOG"

# ---------------------------------------------------------------------------
# 2. The offline test suite, for real, with a real computed total.
# ---------------------------------------------------------------------------
echo "==> running cargo test --workspace (this is the real default-build suite)"
TEST_LOG="$(mktemp)"
if cargo test --workspace >"$TEST_LOG" 2>&1; then
  {
    printf '$ cargo test --workspace | tee test.log\n'
    grep -E "^     Running|^   Doc-tests|^test result:" "$TEST_LOG"
    printf '\n$ awk '"'"'/^test result: ok\\./{n=$4; gsub(/[^0-9]/,"",n); sum+=n} END{print "TOTAL PASSED:", sum}'"'"' test.log\n'
    awk '/^test result: ok\./{n=$4; gsub(/[^0-9]/,"",n); sum+=n} END{print "TOTAL PASSED:", sum}' "$TEST_LOG"
  } > "$OUT_DIR/test-suite.txt"
  echo "==> wrote test-suite.txt"
else
  echo "!! cargo test --workspace failed — skipping test-suite transcript (not fabricating)" >&2
  tail -40 "$TEST_LOG" >&2
fi
rm -f "$TEST_LOG"

# ---------------------------------------------------------------------------
# 3. Python binding — real interpreter, real round-trip.
# ---------------------------------------------------------------------------
echo "==> make smoke-python"
PY_LOG="$(mktemp)"
if command -v python3 >/dev/null 2>&1 && make smoke-python >"$PY_LOG" 2>&1; then
  {
    printf '$ make smoke-python\n'
    printf '$ PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py\n'
    sed -n '/^capabilities OK/,$p' "$PY_LOG"
  } > "$OUT_DIR/binding-python.txt"
  echo "==> wrote binding-python.txt"
else
  echo "!! make smoke-python unavailable/failed — skipping (not fabricating)" >&2
  tail -40 "$PY_LOG" >&2
fi
rm -f "$PY_LOG"

# ---------------------------------------------------------------------------
# 4. Go binding — real cgo build, real test binary.
# ---------------------------------------------------------------------------
echo "==> patala-go: make test"
GO_LOG="$(mktemp)"
if command -v go >/dev/null 2>&1 && command -v uniffi-bindgen-go >/dev/null 2>&1 \
  && (cd patala-go && make test) >"$GO_LOG" 2>&1; then
  {
    printf '$ cd patala-go && make test\n'
    grep -E "^=== RUN|^--- PASS|^--- FAIL|^PASS$|^ok  |^go-test-gate:" "$GO_LOG"
  } > "$OUT_DIR/binding-go.txt"
  echo "==> wrote binding-go.txt"
else
  echo "!! patala-go make test unavailable/failed — skipping (not fabricating)" >&2
  tail -40 "$GO_LOG" >&2
fi
rm -f "$GO_LOG"

echo "==> done. Transcripts in $OUT_DIR — now run scripts/render-shots.mjs to turn them into PNGs."
