import NetworkExtension

/// Network Extension lifecycle and routing boundary for the macOS adapter.
///
/// The packet-to-relay codec is intentionally owned by the shared Rust core;
/// this provider owns only the system tunnel and packet-flow lifecycle. The
/// production target must embed the Rust transport sidecar and call it for
/// Civ6 UDP datagrams instead of treating the packet flow as a generic proxy.
final class PacketTunnelProvider: NEPacketTunnelProvider {
    private var stopping = false

    override func startTunnel(
        options: [String: NSObject]?,
        completionHandler: @escaping (Error?) -> Void
    ) {
        let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "10.240.0.1")
        let ipv4 = NEIPv4Settings(
            addresses: ["10.240.0.2"],
            subnetMasks: ["255.255.255.0"]
        )

        // The exact /32 broadcast route makes Civ6's limited broadcast
        // visible to packetFlow. The provider must then filter UDP ports
        // 62900-62999 and 62056 before handing datagrams to the relay core.
        ipv4.includedRoutes = [
            NEIPv4Route(destinationAddress: "255.255.255.255", subnetMask: "255.255.255.255"),
            NEIPv4Route(destinationAddress: "10.240.0.0", subnetMask: "255.255.255.0")
        ]
        settings.ipv4Settings = ipv4
        settings.mtu = NSNumber(value: 1280)

        setTunnelNetworkSettings(settings) { [weak self] error in
            guard let self else {
                completionHandler(error)
                return
            }
            guard error == nil else {
                completionHandler(error)
                return
            }

            self.readPackets()
            completionHandler(nil)
        }
    }

    override func stopTunnel(
        with reason: NEProviderStopReason,
        completionHandler: @escaping () -> Void
    ) {
        stopping = true
        completionHandler()
    }

    private func readPackets() {
        guard !stopping else { return }

        packetFlow.readPackets { [weak self] packets, protocols in
            guard let self, !self.stopping else { return }

            // TODO(phase-3): parse IPv4/UDP, keep only Civ6 discovery and
            // gameplay ports, then hand the payload to the shared Rust
            // DatagramTransport sidecar. Non-Civ6 packets must never be
            // forwarded by this provider.
            _ = packets
            _ = protocols
            self.readPackets()
        }
    }
}
