import Foundation

struct Civ6PacketRouter {
    private let localAddress: UInt32
    private let hostSessionID: UUID?
    private var gameplaySessionID: UUID?
    private var requestIDsBySourceAddress: [UInt32: UUID] = [:]

    init(localAddress: UInt32, hostSessionID: UUID? = nil, gameplaySessionID: UUID? = nil) {
        self.localAddress = localAddress
        self.hostSessionID = hostSessionID
        self.gameplaySessionID = gameplaySessionID
    }

    mutating func setGameplaySession(_ sessionID: UUID?) {
        gameplaySessionID = sessionID
    }

    mutating func outbound(_ packet: Data) -> RelayEnvelope? {
        guard let datagram = IPv4UDPDatagram.parse(packet) else { return nil }

        if datagram.isDiscovery && datagram.destinationAddress == 0xffffffff {
            let requestID = UUID()
            return .discoveryRequest(
                requestID: requestID,
                destinationPort: datagram.destinationPort,
                payload: datagram.payload
            )
        }

        if datagram.isDiscovery,
           let hostSessionID,
           let requestID = requestIDsBySourceAddress[datagram.destinationAddress] {
            return .discoveryResponse(
                requestID: requestID,
                hostSessionID: hostSessionID,
                sourcePort: datagram.sourcePort,
                payload: datagram.payload
            )
        }

        if datagram.isGameplay, let gameplaySessionID {
            return .gameplayPacket(
                sessionID: gameplaySessionID,
                sourcePort: 62_056,
                payload: datagram.payload
            )
        }
        return nil
    }

    mutating func inbound(_ envelope: RelayEnvelope) -> Data? {
        switch envelope {
        case let .discoveryToHost(requestID, sourceAddress, destinationPort, payload):
            requestIDsBySourceAddress[sourceAddress] = requestID
            return IPv4UDPDatagram(
                sourceAddress: sourceAddress,
                destinationAddress: 0xffffffff,
                sourcePort: destinationPort,
                destinationPort: destinationPort,
                payload: payload
            ).encoded()
        case let .discoveryToClient(_, hostAddress, sourcePort, payload):
            return IPv4UDPDatagram(
                sourceAddress: hostAddress,
                destinationAddress: localAddress,
                sourcePort: sourcePort,
                destinationPort: sourcePort,
                payload: payload
            ).encoded()
        case let .gameplayToPeer(_, sourceAddress, destinationPort, payload):
            return IPv4UDPDatagram(
                sourceAddress: sourceAddress,
                destinationAddress: localAddress,
                sourcePort: destinationPort,
                destinationPort: destinationPort,
                payload: payload
            ).encoded()
        case .relayProbeAck:
            return nil
        case .discoveryRequest, .discoveryResponse, .gameplayPacket, .relayProbe:
            return nil
        }
    }
}
