import Foundation

@main
struct ProtocolSmoke {
    static func main() throws {
        let original = IPv4UDPDatagram(
            sourceAddress: 0x0a000002,
            destinationAddress: 0xffffffff,
            sourcePort: 50_000,
            destinationPort: 62_900,
            payload: Data([1, 2, 3, 4])
        )
        let parsed = try require(IPv4UDPDatagram.parse(original.encoded()))
        precondition(parsed.sourceAddress == original.sourceAddress)
        precondition(parsed.destinationAddress == original.destinationAddress)
        precondition(parsed.sourcePort == original.sourcePort)
        precondition(parsed.destinationPort == original.destinationPort)
        precondition(parsed.payload == original.payload)
        precondition(parsed.isDiscovery)

        let requestID = UUID()
        let envelope = RelayEnvelope.discoveryRequest(
            requestID: requestID,
            destinationPort: 62_900,
            payload: original.payload
        )
        let decoded = try RelayEnvelope.decode(try envelope.encoded())
        switch decoded {
        case let .discoveryRequest(decodedID, decodedPort, decodedPayload):
            precondition(decodedID == requestID)
            precondition(decodedPort == 62_900)
            precondition(decodedPayload == original.payload)
        default:
            preconditionFailure("relay envelope decoded to the wrong message")
        }

        let metadata = RelayEnvelopeMetadata(
            sequence: 42,
            connectionEpoch: 7,
            sentAtMilliseconds: 1_234,
            pathID: 2
        )
        let decodedV2 = try RelayEnvelope.decodeWithMetadata(
            try envelope.encoded(metadata: metadata)
        )
        precondition(decodedV2.metadata == metadata)
        guard case let .discoveryRequest(decodedV2ID, decodedV2Port, decodedV2Payload) = decodedV2.envelope else {
            preconditionFailure("v2 relay envelope decoded to the wrong message")
        }
        precondition(decodedV2ID == requestID)
        precondition(decodedV2Port == 62_900)
        precondition(decodedV2Payload == original.payload)

        let hostSessionID = UUID()
        var clientRouter = Civ6PacketRouter(
            localAddress: 0x0a000002,
            gameplaySessionID: UUID()
        )
        let request = try require(clientRouter.outbound(original.encoded()))
        guard case let .discoveryRequest(decodedRequestID, decodedPort, decodedPayload) = request else {
            preconditionFailure("broadcast packet was not mapped to a discovery request")
        }
        precondition(decodedPort == original.destinationPort)
        precondition(decodedPayload == original.payload)

        var hostRouter = Civ6PacketRouter(
            localAddress: 0x0a000003,
            hostSessionID: hostSessionID,
            gameplaySessionID: UUID()
        )
        let forwardedToHost = try require(hostRouter.inbound(
            .discoveryToHost(
                requestID: decodedRequestID,
                sourceAddress: original.sourceAddress,
                destinationPort: original.destinationPort,
                payload: original.payload
            )
        ))
        let hostDatagram = try require(IPv4UDPDatagram.parse(forwardedToHost))
        precondition(hostDatagram.sourceAddress == original.sourceAddress)
        precondition(hostDatagram.destinationAddress == 0xffffffff)
        precondition(hostDatagram.destinationPort == original.destinationPort)

        let hostResponsePacket = IPv4UDPDatagram(
            sourceAddress: 0x0a000003,
            destinationAddress: original.sourceAddress,
            sourcePort: original.destinationPort,
            destinationPort: original.sourcePort,
            payload: Data([5, 6, 7])
        ).encoded()
        let response = try require(hostRouter.outbound(hostResponsePacket))
        guard case let .discoveryResponse(responseID, responseHostID, responsePort, responsePayload) = response else {
            preconditionFailure("host response was not mapped to a discovery response")
        }
        precondition(responseID == decodedRequestID)
        precondition(responseHostID == hostSessionID)
        precondition(responsePort == original.destinationPort)
        precondition(responsePayload == Data([5, 6, 7]))

        let forwardedToClient = try require(clientRouter.inbound(
            .discoveryToClient(
                requestID: responseID,
                hostAddress: 0x0a000003,
                sourcePort: responsePort,
                payload: responsePayload
            )
        ))
        let clientResponse = try require(IPv4UDPDatagram.parse(forwardedToClient))
        precondition(clientResponse.sourceAddress == 0x0a000003)
        precondition(clientResponse.destinationAddress == 0x0a000002)
        precondition(clientResponse.sourcePort == responsePort)
        precondition(clientResponse.payload == Data([5, 6, 7]))

        let gameplayPacket = IPv4UDPDatagram(
            sourceAddress: 0x0a000002,
            destinationAddress: 0x0a000003,
            sourcePort: 51_000,
            destinationPort: 62_056,
            payload: Data([8, 9])
        ).encoded()
        let gameplay = try require(clientRouter.outbound(gameplayPacket))
        guard case let .gameplayPacket(sessionID, gameplayPort, gameplayPayload) = gameplay else {
            preconditionFailure("gameplay packet was not mapped to a gameplay envelope")
        }
        precondition(gameplayPort == 62_056)
        precondition(gameplayPayload == Data([8, 9]))
        let gameplayToHost = try require(hostRouter.inbound(
            .gameplayToPeer(
                sessionID: sessionID,
                sourceAddress: 0x0a000002,
                destinationPort: 62_056,
                payload: gameplayPayload
            )
        ))
        let hostGameplay = try require(IPv4UDPDatagram.parse(gameplayToHost))
        precondition(hostGameplay.sourceAddress == 0x0a000002)
        precondition(hostGameplay.destinationAddress == 0x0a000003)
        precondition(hostGameplay.destinationPort == 62_056)
        precondition(hostGameplay.payload == Data([8, 9]))
    }

    private static func require<T>(_ value: T?) throws -> T {
        guard let value else { throw SmokeError.missingPacket }
        return value
    }
}

private enum SmokeError: Error {
    case missingPacket
}
