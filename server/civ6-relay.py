#!/usr/bin/env python3
"""A small UDP relay for Civilization VI LAN discovery and gameplay.

Civilization VI uses IPv4 broadcast for LAN discovery.  WireGuard is an L3
tunnel and does not carry that broadcast, so clients that redirect their
discovery packets to the relay can use this process as the stable endpoint.

The relay is deliberately single-host: one configured Civ VI host is enough
for a normal LAN room, and keeping the target explicit prevents this from
becoming an open UDP reflector.  Every client gets its own upstream socket so
responses from the host can be routed back to the correct client even though
all clients use UDP/62056.
"""

from __future__ import annotations

import argparse
import ipaddress
import logging
import os
import selectors
import signal
import socket
import time
from dataclasses import dataclass
from typing import Dict, Iterable, Tuple


Address = Tuple[str, int]
LOGGER = logging.getLogger("civ6-relay")


@dataclass
class Session:
    client: Address
    upstream: socket.socket
    kind: str
    last_seen: float
    return_port: int


class Civ6Relay:
    """Relay Civ VI discovery/game packets between WG clients and one host."""

    def __init__(
        self,
        listen_ip: str,
        host_ip: str,
        discovery_start: int = 62900,
        discovery_end: int = 62999,
        gameplay_port: int = 62056,
        idle_timeout: float = 15.0,
        allowed_networks: Iterable[str] = (),
        buffer_size: int = 65535,
    ) -> None:
        self.listen_ip = _validate_ipv4(listen_ip, "listen_ip")
        self.host_ip = _validate_ipv4(host_ip, "host_ip")
        self.discovery_start = _validate_port(discovery_start, "discovery_start")
        self.discovery_end = _validate_port(discovery_end, "discovery_end")
        self.gameplay_port = _validate_port(gameplay_port, "gameplay_port")
        if self.discovery_start > self.discovery_end:
            raise ValueError("discovery_start must not be greater than discovery_end")
        if self.discovery_start <= self.gameplay_port <= self.discovery_end:
            raise ValueError("gameplay_port must not overlap the discovery range")
        if idle_timeout <= 0:
            raise ValueError("idle_timeout must be greater than zero")
        self.idle_timeout = idle_timeout
        self.buffer_size = buffer_size
        self.allowed_networks = tuple(
            ipaddress.ip_network(network, strict=False) for network in allowed_networks
        )

        self.selector = selectors.DefaultSelector()
        self.front_sockets: Dict[int, socket.socket] = {}
        self.sessions: Dict[Tuple[str, Address], Session] = {}
        self.upstream_sessions: Dict[socket.socket, Session] = {}
        self.running = False

    def start(self) -> None:
        """Bind all public relay sockets."""
        for port in range(self.discovery_start, self.discovery_end + 1):
            self._bind_front_socket(port, "discovery")
        self._bind_front_socket(self.gameplay_port, "gameplay")
        self.running = True
        LOGGER.info(
            "listening on %s for discovery UDP %d-%d and gameplay UDP %d; host=%s",
            self.listen_ip,
            self.discovery_start,
            self.discovery_end,
            self.gameplay_port,
            self.host_ip,
        )

    def run(self) -> None:
        if not self.running:
            self.start()
        try:
            while self.running:
                for key, _ in self.selector.select(timeout=1.0):
                    self._handle_readable(key.fileobj, key.data)
                self._expire_sessions()
        finally:
            self.close()

    def close(self) -> None:
        if not self.running and not self.front_sockets and not self.upstream_sessions:
            return
        self.running = False
        for sock in list(self.front_sockets.values()):
            self._close_socket(sock)
        for sock in list(self.upstream_sessions):
            self._close_socket(sock)
        self.front_sockets.clear()
        self.upstream_sessions.clear()
        self.sessions.clear()
        self.selector.close()

    def _bind_front_socket(self, port: int, kind: str) -> None:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((self.listen_ip, port))
        sock.setblocking(False)
        self.selector.register(sock, selectors.EVENT_READ, (kind, port))
        self.front_sockets[port] = sock

    def _handle_readable(self, sock: socket.socket, metadata: Tuple[str, int]) -> None:
        try:
            data, sender = sock.recvfrom(self.buffer_size)
        except OSError as exc:
            if self.running:
                LOGGER.warning("failed to receive UDP packet: %s", exc)
            return

        if metadata[0] == "upstream":
            self._handle_host_packet(sock, data, sender)
        else:
            self._handle_client_packet(sock, metadata[0], metadata[1], data, sender)

    def _handle_client_packet(
        self,
        front_socket: socket.socket,
        kind: str,
        return_port: int,
        data: bytes,
        client: Address,
    ) -> None:
        if not self._is_allowed(client[0]):
            LOGGER.warning("dropping packet from unauthorized WG address %s", client[0])
            return
        if client[0] == self.host_ip:
            return

        key = (kind, client)
        now = time.monotonic()
        session = self.sessions.get(key)
        if session is None:
            session = self._new_session(kind, client, return_port, now)
            self.sessions[key] = session
            self.upstream_sessions[session.upstream] = session
            LOGGER.debug("new %s session client=%s", kind, client)
        else:
            session.last_seen = now
            session.return_port = return_port

        try:
            session.upstream.sendto(data, (self.host_ip, return_port))
        except OSError as exc:
            LOGGER.warning("failed to forward %s packet from %s: %s", kind, client, exc)
            self._drop_session(key, session)
            return
        LOGGER.debug("client %s -> host %s:%d (%d bytes)", client, self.host_ip, return_port, len(data))

    def _handle_host_packet(self, upstream: socket.socket, data: bytes, sender: Address) -> None:
        session = self.upstream_sessions.get(upstream)
        if session is None or sender[0] != self.host_ip:
            LOGGER.warning("dropping unexpected upstream packet from %s", sender)
            return

        session.last_seen = time.monotonic()
        return_port = sender[1] if sender[1] in self.front_sockets else session.return_port
        front_socket = self.front_sockets.get(return_port)
        if front_socket is None:
            LOGGER.warning("no frontend socket for host response port %d", return_port)
            return
        try:
            front_socket.sendto(data, session.client)
        except OSError as exc:
            LOGGER.warning("failed to return host packet to %s: %s", session.client, exc)
            self._drop_session((session.kind, session.client), session)
            return
        LOGGER.debug("host %s:%d -> client %s (%d bytes)", sender[0], sender[1], session.client, len(data))

    def _new_session(self, kind: str, client: Address, return_port: int, now: float) -> Session:
        upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        upstream.bind((self.listen_ip, 0))
        upstream.setblocking(False)
        self.selector.register(upstream, selectors.EVENT_READ, ("upstream", 0))
        return Session(client, upstream, kind, now, return_port)

    def _expire_sessions(self) -> None:
        cutoff = time.monotonic() - self.idle_timeout
        for key, session in list(self.sessions.items()):
            if session.last_seen < cutoff:
                self._drop_session(key, session)

    def _drop_session(self, key: Tuple[str, Address], session: Session) -> None:
        self.sessions.pop(key, None)
        self.upstream_sessions.pop(session.upstream, None)
        self._close_socket(session.upstream)

    def _close_socket(self, sock: socket.socket) -> None:
        try:
            self.selector.unregister(sock)
        except (KeyError, ValueError):
            pass
        try:
            sock.close()
        except OSError:
            pass

    def _is_allowed(self, address: str) -> bool:
        return not self.allowed_networks or any(
            ipaddress.ip_address(address) in network for network in self.allowed_networks
        )


def _validate_ipv4(value: str, name: str) -> str:
    try:
        address = ipaddress.ip_address(value)
    except ValueError as exc:
        raise ValueError(f"{name} must be an IPv4 address: {value!r}") from exc
    if address.version != 4:
        raise ValueError(f"{name} must be an IPv4 address: {value!r}")
    return str(address)


def _validate_port(value: int, name: str) -> int:
    if not 1 <= value <= 65535:
        raise ValueError(f"{name} must be between 1 and 65535")
    return value


def _env_int(name: str, default: int) -> int:
    value = os.getenv(name)
    return default if value is None else int(value)


def _env_networks() -> Tuple[str, ...]:
    value = os.getenv("CIV6_RELAY_ALLOWED_CIDRS", "")
    return tuple(item.strip() for item in value.split(",") if item.strip())


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen-ip", default=os.getenv("CIV6_RELAY_LISTEN_IP", "10.10.0.1"))
    parser.add_argument("--host-ip", default=os.getenv("CIV6_HOST_WG_IP"))
    parser.add_argument(
        "--discovery-start",
        type=int,
        default=_env_int("CIV6_DISCOVERY_PORT_START", 62900),
    )
    parser.add_argument(
        "--discovery-end",
        type=int,
        default=_env_int("CIV6_DISCOVERY_PORT_END", 62999),
    )
    parser.add_argument(
        "--gameplay-port",
        type=int,
        default=_env_int("CIV6_GAMEPLAY_PORT", 62056),
    )
    parser.add_argument(
        "--idle-timeout",
        type=float,
        default=float(os.getenv("CIV6_RELAY_IDLE_TIMEOUT", "15")),
    )
    parser.add_argument(
        "--allowed-cidr",
        action="append",
        default=None,
        help="Only accept clients in this CIDR; repeat for multiple networks.",
    )
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    if not args.host_ip:
        parser.error("--host-ip or CIV6_HOST_WG_IP is required")
    if args.allowed_cidr is None:
        args.allowed_cidr = list(_env_networks())
    return args


def main() -> int:
    args = parse_args()
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    try:
        relay = Civ6Relay(
            listen_ip=args.listen_ip,
            host_ip=args.host_ip,
            discovery_start=args.discovery_start,
            discovery_end=args.discovery_end,
            gameplay_port=args.gameplay_port,
            idle_timeout=args.idle_timeout,
            allowed_networks=args.allowed_cidr,
        )
    except (TypeError, ValueError) as exc:
        LOGGER.error("invalid configuration: %s", exc)
        return 2

    def stop(_signum: int, _frame: object) -> None:
        relay.running = False

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    try:
        relay.run()
    except OSError as exc:
        LOGGER.error("relay could not start: %s", exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
