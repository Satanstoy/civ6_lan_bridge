#!/usr/bin/env bash
set -Eeuo pipefail

WG_INTERFACE="${WG_INTERFACE:-wg0}"
HOST_IP="${1:-${CIV6_HOST_WG_IP:-}}"

if [ -z "$HOST_IP" ]; then
  echo "usage: sudo $0 <host-wireguard-ip>" >&2
  exit 2
fi

command -v tcpdump >/dev/null || {
  echo "tcpdump is required" >&2
  exit 1
}

echo "Capturing Civ VI discovery/game packets on ${WG_INTERFACE} for ${HOST_IP}. Press Ctrl-C to stop."
exec tcpdump -ni "$WG_INTERFACE" "host ${HOST_IP} and (udp port 62056 or udp portrange 62900-62999)"
