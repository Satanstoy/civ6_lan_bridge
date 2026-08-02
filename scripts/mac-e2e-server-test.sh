#!/usr/bin/env bash
set -Eeuo pipefail

# Starts the normal Rust server binary, creates a short-lived authenticated
# manifest, runs the shared control/envelope protocol test, and leaves a JSON
# report plus server log for the macOS client operator. This local harness uses
# 127.0.0.0/8 aliases as synthetic WireGuard peer addresses; it is not a
# public UDP relay and it does not test Civ VI itself.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="${CIV6_TEST_DIR:-$(mktemp -d /tmp/civ6-lan-bridge-mac-e2e.XXXXXX)}"
OWNED_TEST_DIR=0
if [ -z "${CIV6_TEST_DIR:-}" ]; then
  OWNED_TEST_DIR=1
fi

CONTROL_PORT="${CIV6_TEST_CONTROL_PORT:-18080}"
RELAY_PORT="${CIV6_TEST_RELAY_PORT:-32000}"
CONTROL_BIND="${CIV6_TEST_CONTROL_BIND:-127.0.0.1:${CONTROL_PORT}}"
RELAY_BIND="${CIV6_TEST_RELAY_BIND:-127.0.0.1:${RELAY_PORT}}"
CONTROL_URL="${CIV6_TEST_CONTROL_URL:-http://127.0.0.1:${CONTROL_PORT}}"
RELAY_ADDR="${CIV6_TEST_RELAY_ADDR:-127.0.0.1:${RELAY_PORT}}"
REPORT_PATH="${CIV6_TEST_REPORT:-${ROOT_DIR}/server-test-report.json}"
MANIFEST_PATH="${CIV6_TEST_MANIFEST:-${TEST_DIR}/session-manifest.json}"
REDACTED_MANIFEST_PATH="${CIV6_TEST_REDACTED_MANIFEST:-${TEST_DIR}/session-manifest.redacted.json}"
SERVER_LOG="${CIV6_TEST_SERVER_LOG:-${TEST_DIR}/server.log}"
TOKEN_PATH="${TEST_DIR}/control.token"
SERVER_PID=""

is_private_runtime_path() {
  case "$1" in
    /tmp/*) return 0 ;;
    "${RUNNER_TEMP:-/path-that-does-not-exist}"/*) return 0 ;;
    *) return 1 ;;
  esac
}

if ! is_private_runtime_path "$TEST_DIR" || ! is_private_runtime_path "$MANIFEST_PATH"; then
  echo "CIV6_TEST_DIR and CIV6_TEST_MANIFEST must be under /tmp or RUNNER_TEMP" >&2
  exit 2
fi

mkdir -p "$TEST_DIR"
umask 077

cleanup() {
  local exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ "$OWNED_TEST_DIR" -eq 1 ]; then
    rm -f "$MANIFEST_PATH" "$TOKEN_PATH"
  fi
  exit "$exit_code"
}
trap cleanup EXIT INT TERM

if command -v openssl >/dev/null 2>&1; then
  TOKEN="$(openssl rand -hex 32)"
else
  TOKEN="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
fi
printf '%s' "$TOKEN" > "$TOKEN_PATH"

export CIV6_CONTROL_BIND="$CONTROL_BIND"
export CIV6_RELAY_BIND="$RELAY_BIND"
export CIV6_RELAY_PORT="$RELAY_PORT"
export CIV6_VIRTUAL_IP_PREFIX="${CIV6_TEST_VIRTUAL_IP_PREFIX:-127.0.0}"
export CIV6_CONTROL_BEARER_TOKEN="$TOKEN"
export CIV6_BUILD_COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
export CIV6_TEST_CONTROL_URL="$CONTROL_URL"
export CIV6_TEST_RELAY_ADDR="$RELAY_ADDR"
export CIV6_TEST_MANIFEST="$MANIFEST_PATH"
export CIV6_TEST_REDACTED_MANIFEST="$REDACTED_MANIFEST_PATH"
export CIV6_TEST_REPORT="$REPORT_PATH"
export CIV6_TEST_SERVER_LOG="$SERVER_LOG"
export RUST_LOG="${RUST_LOG:-civ6_lan_server=info,tower_http=info}"

export PATH="${CIV6_RUST_BIN_DIR:-/home/ubuntu/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin}:$PATH"

echo "Starting civ6-lan-server from $ROOT_DIR"
echo "Control endpoint: $CONTROL_URL"
echo "Relay endpoint: udp://$RELAY_ADDR"
echo "UDP relay port: $RELAY_PORT"
echo "Protocol version: 2 (v1 decode compatibility retained)"
echo "Build commit: $CIV6_BUILD_COMMIT"

cargo run --quiet --manifest-path "$ROOT_DIR/server/Cargo.toml" --bin civ6-lan-server \
  > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
echo "Server log: $SERVER_LOG"
echo "Manifest: $MANIFEST_PATH"
echo "Stop command: kill $SERVER_PID"

ready=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$CONTROL_URL/health/live" >/dev/null 2>&1; then
    ready=1
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Server exited before readiness; log follows:" >&2
    sed -n '1,240p' "$SERVER_LOG" >&2 || true
    exit 1
  fi
  sleep 0.25
done
if [ "$ready" -ne 1 ]; then
  echo "Server did not become ready; log follows:" >&2
  sed -n '1,240p' "$SERVER_LOG" >&2 || true
  exit 1
fi

set +e
cargo run --quiet --manifest-path "$ROOT_DIR/server/Cargo.toml" --bin mac_e2e_test
test_exit=$?
set -e

if [ -f "$MANIFEST_PATH" ]; then
  sed "s/${TOKEN}/<destroyed>/g" "$MANIFEST_PATH" > "$REDACTED_MANIFEST_PATH"
  rm -f "$MANIFEST_PATH" "$TOKEN_PATH"
fi

echo "Report: $REPORT_PATH"
echo "Redacted manifest: $REDACTED_MANIFEST_PATH"
echo "Server log: $SERVER_LOG"
if [ -f "$REPORT_PATH" ]; then
  sed -n '1,260p' "$REPORT_PATH"
fi
exit "$test_exit"
