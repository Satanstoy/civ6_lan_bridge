#!/usr/bin/env bash
set -Eeuo pipefail

WG_INTERFACE="${WG_INTERFACE:-wg0}"
DISCOVERY_START="${CIV6_DISCOVERY_PORT_START:-62900}"
DISCOVERY_END="${CIV6_DISCOVERY_PORT_END:-62999}"
GAMEPLAY_PORT="${CIV6_GAMEPLAY_PORT:-62056}"

failures=0
warn() { printf 'WARN: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*"; failures=$((failures + 1)); }
pass() { printf 'PASS: %s\n' "$*"; }

command -v ip >/dev/null || fail "ip command is missing"
command -v wg >/dev/null || fail "wg command is missing"
command -v ss >/dev/null || fail "ss command is missing"

if ip link show "$WG_INTERFACE" >/dev/null 2>&1; then
  pass "$WG_INTERFACE exists"
else
  fail "$WG_INTERFACE does not exist"
fi

if wg show "$WG_INTERFACE" >/dev/null 2>&1; then
  pass "$WG_INTERFACE is readable"
else
  fail "$WG_INTERFACE is not readable; run this script with sudo"
fi

forwarding="$(sysctl -n net.ipv4.ip_forward 2>/dev/null || echo 0)"
if [ "$forwarding" = "1" ]; then
  pass "IPv4 forwarding is enabled"
else
  fail "net.ipv4.ip_forward is $forwarding"
fi

if systemctl is-active --quiet "wg-quick@${WG_INTERFACE}"; then
  pass "wg-quick@${WG_INTERFACE} is active"
else
  warn "wg-quick@${WG_INTERFACE} is not active according to systemd; the interface may have been started manually"
fi

if ss -lun | awk -v p=":${GAMEPLAY_PORT}" '$5 ~ p"$" { found=1 } END { exit !found }'; then
  pass "UDP ${GAMEPLAY_PORT} has a listener"
else
  warn "UDP ${GAMEPLAY_PORT} has no visible listener; Civ VI creates it when a room is hosted"
fi

if command -v nft >/dev/null 2>&1; then
  rules="$(nft list ruleset 2>/dev/null || true)"
  if grep -Eq "udp dport ${GAMEPLAY_PORT}.*accept|udp dport ${GAMEPLAY_PORT}.*counter" <<<"$rules"; then
    pass "firewall mentions UDP ${GAMEPLAY_PORT}"
  else
    warn "could not confirm a firewall rule for UDP ${GAMEPLAY_PORT}"
  fi
  if grep -Eq "udp dport ${DISCOVERY_START}-${DISCOVERY_END}.*accept|udp dport ${DISCOVERY_START}-${DISCOVERY_END}.*counter" <<<"$rules"; then
    pass "firewall mentions UDP ${DISCOVERY_START}-${DISCOVERY_END}"
  else
    warn "could not confirm a firewall rule for UDP ${DISCOVERY_START}-${DISCOVERY_END}"
  fi
else
  warn "nft is not installed; skipped firewall inspection"
fi

if command -v udp2raw >/dev/null 2>&1; then
  pass "udp2raw is installed"
else
  warn "udp2raw is not installed; WireGuard can still work directly over UDP"
fi

if [ "$failures" -gt 0 ]; then
  printf '%s\n' "${failures} blocking preflight check(s) failed. No changes were made."
  exit 1
fi

printf '%s\n' 'Preflight completed. No changes were made.'
