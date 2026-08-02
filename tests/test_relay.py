import importlib.util
import pathlib
import socket
import sys
import threading
import time
import unittest


MODULE_PATH = pathlib.Path(__file__).parents[1] / "server" / "civ6-relay.py"
SPEC = importlib.util.spec_from_file_location("civ6_relay", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class RelayValidationTests(unittest.TestCase):
    def test_rejects_invalid_addresses_and_overlapping_ports(self):
        with self.assertRaises(ValueError):
            MODULE.Civ6Relay("10.10.0.1", "not-an-ip")
        with self.assertRaises(ValueError):
            MODULE.Civ6Relay("10.10.0.1", "10.10.0.11", 62056, 62056, 62056)

    def test_accepts_default_civ6_ports(self):
        relay = MODULE.Civ6Relay(
            "10.10.0.1",
            "10.10.0.11",
            allowed_networks=("10.10.0.0/24",),
        )
        self.assertEqual(relay.discovery_start, 62900)
        self.assertEqual(relay.discovery_end, 62999)
        self.assertEqual(relay.gameplay_port, 62056)
        self.assertTrue(relay._is_allowed("10.10.0.12"))
        self.assertFalse(relay._is_allowed("192.168.1.20"))
        relay.close()


class RelayLoopbackTests(unittest.TestCase):
    def test_discovery_and_gameplay_are_forwarded_with_separate_sessions(self):
        relay = MODULE.Civ6Relay(
            "127.0.0.1",
            "127.0.0.2",
            discovery_start=62900,
            discovery_end=62900,
            gameplay_port=62056,
            idle_timeout=2,
            allowed_networks=("127.0.0.0/8",),
        )
        host_discovery = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        host_gameplay = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        gameplay_client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sockets = (host_discovery, host_gameplay, client, gameplay_client)
        thread = None
        try:
            host_discovery.bind(("127.0.0.2", 62900))
            host_gameplay.bind(("127.0.0.2", 62056))
            client.bind(("127.0.0.3", 0))
            gameplay_client.bind(("127.0.0.4", 62056))
            relay.start()
            thread = threading.Thread(target=relay.run, daemon=True)
            thread.start()

            client.sendto(b"DISCOVERY", ("127.0.0.1", 62900))
            discovery, upstream = host_discovery.recvfrom(1024)
            self.assertEqual(discovery, b"DISCOVERY")
            host_discovery.sendto(b"ROOM", upstream)
            response, response_from = client.recvfrom(1024)
            self.assertEqual(response, b"ROOM")
            self.assertEqual(response_from, ("127.0.0.1", 62900))

            gameplay_client.sendto(b"GAME", ("127.0.0.1", 62056))
            gameplay, upstream = host_gameplay.recvfrom(1024)
            self.assertEqual(gameplay, b"GAME")
            host_gameplay.sendto(b"GAME-REPLY", upstream)
            response, response_from = gameplay_client.recvfrom(1024)
            self.assertEqual(response, b"GAME-REPLY")
            self.assertEqual(response_from, ("127.0.0.1", 62056))
        finally:
            relay.running = False
            if thread is not None:
                thread.join(timeout=2)
            relay.close()
            for sock in sockets:
                sock.close()
            time.sleep(0.01)


class PlatformRoutingContractTests(unittest.TestCase):
    def test_macos_route_is_split_and_keeps_safe_mtu(self):
        provider = (pathlib.Path(__file__).parents[1] / "mac-client" / "PacketTunnel" / "Sources" / "PacketTunnelProvider.swift").read_text()
        self.assertIn('destinationAddress: "255.255.255.255"', provider)
        self.assertIn('destinationAddress: "10.240.0.0"', provider)
        self.assertIn("settings.mtu = NSNumber(value: 1280)", provider)
        self.assertNotIn('destinationAddress: "0.0.0.0"', provider)

    def test_windows_contract_is_outbound_civ6_only(self):
        contract = (pathlib.Path(__file__).parents[1] / "win-client" / "wfp" / "README.md").read_text()
        self.assertIn("outbound", contract)
        self.assertIn("transport layer", contract)
        self.assertIn("62900-62999", contract)
        self.assertIn("62056/UDP", contract)
        self.assertIn("loop-prevention metadata", contract)


if __name__ == "__main__":
    unittest.main()
