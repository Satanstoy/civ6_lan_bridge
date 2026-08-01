import Foundation

struct IPv4UDPDatagram {
    var sourceAddress: UInt32
    var destinationAddress: UInt32
    var sourcePort: UInt16
    var destinationPort: UInt16
    var payload: Data

    var isDiscovery: Bool {
        (62_900...62_999).contains(Int(sourcePort))
            || (62_900...62_999).contains(Int(destinationPort))
    }

    var isGameplay: Bool {
        sourcePort == 62_056 || destinationPort == 62_056
    }

    static func parse(_ packet: Data) -> IPv4UDPDatagram? {
        let bytes = [UInt8](packet)
        guard bytes.count >= 28 else { return nil }
        guard bytes[0] >> 4 == 4 else { return nil }
        let headerLength = Int(bytes[0] & 0x0f) * 4
        guard headerLength >= 20, bytes.count >= headerLength + 8 else { return nil }
        guard bytes[9] == 17 else { return nil }

        let totalLength = Int(readUInt16(bytes, at: 2))
        guard totalLength >= headerLength + 8, totalLength <= bytes.count else { return nil }
        let sourceAddress = readUInt32(bytes, at: 12)
        let destinationAddress = readUInt32(bytes, at: 16)
        let sourcePort = readUInt16(bytes, at: headerLength)
        let destinationPort = readUInt16(bytes, at: headerLength + 2)
        let udpLength = Int(readUInt16(bytes, at: headerLength + 4))
        guard udpLength >= 8, headerLength + udpLength <= totalLength else { return nil }

        return IPv4UDPDatagram(
            sourceAddress: sourceAddress,
            destinationAddress: destinationAddress,
            sourcePort: sourcePort,
            destinationPort: destinationPort,
            payload: Data(bytes[(headerLength + 8)..<(headerLength + udpLength)])
        )
    }

    func encoded() -> Data {
        let headerLength = 20
        let udpLength = 8 + payload.count
        let totalLength = headerLength + udpLength
        var packet = [UInt8](repeating: 0, count: totalLength)

        packet[0] = 0x45
        writeUInt16(UInt16(totalLength), into: &packet, at: 2)
        writeUInt16(0, into: &packet, at: 4)
        writeUInt16(0, into: &packet, at: 6)
        packet[8] = 64
        packet[9] = 17
        writeUInt32(sourceAddress, into: &packet, at: 12)
        writeUInt32(destinationAddress, into: &packet, at: 16)

        writeUInt16(sourcePort, into: &packet, at: headerLength)
        writeUInt16(destinationPort, into: &packet, at: headerLength + 2)
        writeUInt16(UInt16(udpLength), into: &packet, at: headerLength + 4)
        packet.replaceSubrange((headerLength + 8)..<totalLength, with: payload)

        writeUInt16(checksum(packet[0..<headerLength]), into: &packet, at: 10)
        var pseudoHeader = [UInt8]()
        appendUInt32(sourceAddress, to: &pseudoHeader)
        appendUInt32(destinationAddress, to: &pseudoHeader)
        pseudoHeader.append(0)
        pseudoHeader.append(17)
        appendUInt16(UInt16(udpLength), to: &pseudoHeader)
        pseudoHeader.append(contentsOf: packet[headerLength..<totalLength])
        writeUInt16(checksum(pseudoHeader), into: &packet, at: headerLength + 6)
        return Data(packet)
    }
}

private func readUInt16(_ bytes: [UInt8], at offset: Int) -> UInt16 {
    UInt16(bytes[offset]) << 8 | UInt16(bytes[offset + 1])
}

private func readUInt32(_ bytes: [UInt8], at offset: Int) -> UInt32 {
    UInt32(bytes[offset]) << 24
        | UInt32(bytes[offset + 1]) << 16
        | UInt32(bytes[offset + 2]) << 8
        | UInt32(bytes[offset + 3])
}

private func writeUInt16(_ value: UInt16, into bytes: inout [UInt8], at offset: Int) {
    bytes[offset] = UInt8(value >> 8)
    bytes[offset + 1] = UInt8(value & 0xff)
}

private func writeUInt32(_ value: UInt32, into bytes: inout [UInt8], at offset: Int) {
    bytes[offset] = UInt8(value >> 24)
    bytes[offset + 1] = UInt8((value >> 16) & 0xff)
    bytes[offset + 2] = UInt8((value >> 8) & 0xff)
    bytes[offset + 3] = UInt8(value & 0xff)
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

private func checksum<S: Sequence>(_ bytes: S) -> UInt16 where S.Element == UInt8 {
    var sum: UInt32 = 0
    var pending: UInt8?
    for byte in bytes {
        if let high = pending {
            sum += UInt32(high) << 8 | UInt32(byte)
            pending = nil
        } else {
            pending = byte
        }
    }
    if let high = pending {
        sum += UInt32(high) << 8
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16)
    }
    return ~UInt16(sum & 0xffff)
}
