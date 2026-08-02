import Foundation

enum RelayEnvelopeError: Error {
    case malformed
    case unsupportedVersion
    case oversized
    case invalidMessage
}

struct RelayEnvelopeMetadata: Equatable {
    let sequence: UInt64
    let connectionEpoch: UInt64
    let sentAtMilliseconds: UInt64
    let pathID: UInt8?
}

struct DecodedRelayEnvelope {
    let envelope: RelayEnvelope
    let metadata: RelayEnvelopeMetadata?
}

enum RelayEnvelope {
    case discoveryRequest(requestID: UUID, destinationPort: UInt16, payload: Data)
    case discoveryToHost(requestID: UUID, sourceAddress: UInt32, destinationPort: UInt16, payload: Data)
    case discoveryResponse(requestID: UUID, hostSessionID: UUID, sourcePort: UInt16, payload: Data)
    case discoveryToClient(requestID: UUID, hostAddress: UInt32, sourcePort: UInt16, payload: Data)
    case gameplayPacket(sessionID: UUID, sourcePort: UInt16, payload: Data)
    case gameplayToPeer(sessionID: UUID, sourceAddress: UInt32, destinationPort: UInt16, payload: Data)
    case relayProbe(requestID: UUID)
    case relayProbeAck(requestID: UUID)

    private static let magic: [UInt8] = [0x43, 0x36, 0x4c, 0x42]
    private static let legacyVersion: UInt8 = 1
    private static let version: UInt8 = 2
    private static let maxPayload = 4 * 1024
    private static let maxDatagramSize = 8 + maxPayload + 64

    func encoded() throws -> Data {
        try encoded(metadata: nil)
    }

    func encoded(metadata: RelayEnvelopeMetadata) throws -> Data {
        try encoded(metadata: Optional(metadata))
    }

    private func encoded(metadata: RelayEnvelopeMetadata?) throws -> Data {
        var body = [UInt8]()
        if let metadata {
            appendUInt64(metadata.sequence, to: &body)
            appendUInt64(metadata.connectionEpoch, to: &body)
            appendUInt64(metadata.sentAtMilliseconds, to: &body)
            if let pathID = metadata.pathID {
                body.append(1)
                body.append(pathID)
            } else {
                body.append(contentsOf: [0, 0])
            }
        }
        let kind: UInt8
        switch self {
        case let .discoveryRequest(requestID, destinationPort, payload):
            kind = 1
            appendUUID(requestID, to: &body)
            appendUInt16(destinationPort, to: &body)
            try appendPayload(payload, to: &body)
        case let .discoveryToHost(requestID, sourceAddress, destinationPort, payload):
            kind = 2
            appendUUID(requestID, to: &body)
            appendUInt32(sourceAddress, to: &body)
            appendUInt16(destinationPort, to: &body)
            try appendPayload(payload, to: &body)
        case let .discoveryResponse(requestID, hostSessionID, sourcePort, payload):
            kind = 3
            appendUUID(requestID, to: &body)
            appendUUID(hostSessionID, to: &body)
            appendUInt16(sourcePort, to: &body)
            try appendPayload(payload, to: &body)
        case let .discoveryToClient(requestID, hostAddress, sourcePort, payload):
            kind = 4
            appendUUID(requestID, to: &body)
            appendUInt32(hostAddress, to: &body)
            appendUInt16(sourcePort, to: &body)
            try appendPayload(payload, to: &body)
        case let .gameplayPacket(sessionID, sourcePort, payload):
            kind = 5
            appendUUID(sessionID, to: &body)
            appendUInt16(sourcePort, to: &body)
            try appendPayload(payload, to: &body)
        case let .gameplayToPeer(sessionID, sourceAddress, destinationPort, payload):
            kind = 6
            appendUUID(sessionID, to: &body)
            appendUInt32(sourceAddress, to: &body)
            appendUInt16(destinationPort, to: &body)
            try appendPayload(payload, to: &body)
        case let .relayProbe(requestID):
            kind = 7
            appendUUID(requestID, to: &body)
        case let .relayProbeAck(requestID):
            kind = 8
            appendUUID(requestID, to: &body)
        }

        guard body.count <= Int(UInt16.max) else { throw RelayEnvelopeError.oversized }
        let packetVersion = metadata == nil ? Self.legacyVersion : Self.version
        var packet = Self.magic + [packetVersion, kind, UInt8(body.count >> 8), UInt8(body.count & 0xff)]
        packet.append(contentsOf: body)
        return Data(packet)
    }

    static func decode(_ data: Data) throws -> RelayEnvelope {
        try decodeWithMetadata(data).envelope
    }

    static func decodeWithMetadata(_ data: Data) throws -> DecodedRelayEnvelope {
        let bytes = [UInt8](data)
        guard bytes.count <= maxDatagramSize else { throw RelayEnvelopeError.oversized }
        guard bytes.count >= 8, Array(bytes[0..<4]) == magic else {
            throw RelayEnvelopeError.malformed
        }
        guard bytes[4] == legacyVersion || bytes[4] == version else {
            throw RelayEnvelopeError.unsupportedVersion
        }
        let bodyLength = Int(UInt16(bytes[6]) << 8 | UInt16(bytes[7]))
        guard bodyLength == bytes.count - 8 else { throw RelayEnvelopeError.malformed }

        var cursor = RelayCursor(bytes: Array(bytes.dropFirst(8)))
        let metadata = bytes[4] == version ? try cursor.metadata() : nil
        let message: RelayEnvelope
        switch bytes[5] {
        case 1:
            message = .discoveryRequest(
                requestID: try cursor.uuid(),
                destinationPort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 2:
            message = .discoveryToHost(
                requestID: try cursor.uuid(),
                sourceAddress: try cursor.u32(),
                destinationPort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 3:
            message = .discoveryResponse(
                requestID: try cursor.uuid(),
                hostSessionID: try cursor.uuid(),
                sourcePort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 4:
            message = .discoveryToClient(
                requestID: try cursor.uuid(),
                hostAddress: try cursor.u32(),
                sourcePort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 5:
            message = .gameplayPacket(
                sessionID: try cursor.uuid(),
                sourcePort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 6:
            message = .gameplayToPeer(
                sessionID: try cursor.uuid(),
                sourceAddress: try cursor.u32(),
                destinationPort: try cursor.u16(),
                payload: try cursor.payload()
            )
        case 7:
            message = .relayProbe(requestID: try cursor.uuid())
        case 8:
            message = .relayProbeAck(requestID: try cursor.uuid())
        default:
            throw RelayEnvelopeError.invalidMessage
        }
        guard cursor.isAtEnd else { throw RelayEnvelopeError.malformed }
        return DecodedRelayEnvelope(envelope: message, metadata: metadata)
    }
}

private struct RelayCursor {
    var bytes: [UInt8]
    var offset = 0

    var isAtEnd: Bool { offset == bytes.count }

    mutating func take(_ count: Int) throws -> [UInt8] {
        guard count >= 0, offset + count <= bytes.count else { throw RelayEnvelopeError.malformed }
        defer { offset += count }
        return Array(bytes[offset..<(offset + count)])
    }

    mutating func u16() throws -> UInt16 {
        let value = try take(2)
        return UInt16(value[0]) << 8 | UInt16(value[1])
    }

    mutating func u32() throws -> UInt32 {
        let value = try take(4)
        return UInt32(value[0]) << 24
            | UInt32(value[1]) << 16
            | UInt32(value[2]) << 8
            | UInt32(value[3])
    }

    mutating func u64() throws -> UInt64 {
        let value = try take(8)
        return UInt64(value[0]) << 56
            | UInt64(value[1]) << 48
            | UInt64(value[2]) << 40
            | UInt64(value[3]) << 32
            | UInt64(value[4]) << 24
            | UInt64(value[5]) << 16
            | UInt64(value[6]) << 8
            | UInt64(value[7])
    }

    mutating func metadata() throws -> RelayEnvelopeMetadata {
        let sequence = try u64()
        let connectionEpoch = try u64()
        let sentAtMilliseconds = try u64()
        let hasPath = try take(1)[0]
        let pathID = try take(1)[0]
        switch hasPath {
        case 0:
            return RelayEnvelopeMetadata(
                sequence: sequence,
                connectionEpoch: connectionEpoch,
                sentAtMilliseconds: sentAtMilliseconds,
                pathID: nil
            )
        case 1:
            return RelayEnvelopeMetadata(
                sequence: sequence,
                connectionEpoch: connectionEpoch,
                sentAtMilliseconds: sentAtMilliseconds,
                pathID: pathID
            )
        default:
            throw RelayEnvelopeError.malformed
        }
    }

    mutating func uuid() throws -> UUID {
        let value = try take(16)
        return UUID(uuid: (
            value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
            value[8], value[9], value[10], value[11], value[12], value[13], value[14], value[15]
        ))
    }

    mutating func payload() throws -> Data {
        let value = try take(bytes.count - offset)
        guard value.count <= 4 * 1024 else { throw RelayEnvelopeError.oversized }
        return Data(value)
    }
}

private func appendUUID(_ uuid: UUID, to bytes: inout [UInt8]) {
    withUnsafeBytes(of: uuid.uuid) { bytes.append(contentsOf: $0) }
}

private func appendUInt16(_ value: UInt16, to bytes: inout [UInt8]) {
    bytes.append(UInt8(value >> 8))
    bytes.append(UInt8(value & 0xff))
}

private func appendUInt32(_ value: UInt32, to bytes: inout [UInt8]) {
    bytes.append(UInt8(value >> 24))
    bytes.append(UInt8((value >> 16) & 0xff))
    bytes.append(UInt8((value >> 8) & 0xff))
    bytes.append(UInt8(value & 0xff))
}

private func appendUInt64(_ value: UInt64, to bytes: inout [UInt8]) {
    bytes.append(UInt8((value >> 56) & 0xff))
    bytes.append(UInt8((value >> 48) & 0xff))
    bytes.append(UInt8((value >> 40) & 0xff))
    bytes.append(UInt8((value >> 32) & 0xff))
    bytes.append(UInt8((value >> 24) & 0xff))
    bytes.append(UInt8((value >> 16) & 0xff))
    bytes.append(UInt8((value >> 8) & 0xff))
    bytes.append(UInt8(value & 0xff))
}

private func appendPayload(_ payload: Data, to bytes: inout [UInt8]) throws {
    guard payload.count <= 4 * 1024 else { throw RelayEnvelopeError.oversized }
    bytes.append(contentsOf: payload)
}
