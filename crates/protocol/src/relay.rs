//! Shared binary envelope used between desktop adapters and the server relay.
//!
//! This is deliberately independent of Tokio and operating-system packet
//! APIs so Windows WFP, macOS Network Extension, and the server use the same
//! wire format.

use std::net::Ipv4Addr;

use thiserror::Error;
use uuid::Uuid;

use crate::{
    Civ6UdpPort, DiscoveryRequestId, GameplaySessionId, HostSessionId, VirtualIp,
    MAX_CIV6_DATAGRAM_SIZE,
};

pub const LEGACY_RELAY_PROTOCOL_VERSION: u8 = 1;
pub const RELAY_PROTOCOL_VERSION: u8 = 2;
pub const RELAY_HEADER_SIZE: usize = 8;
pub const MAX_RELAY_DATAGRAM_SIZE: usize = RELAY_HEADER_SIZE + MAX_CIV6_DATAGRAM_SIZE + 64;
/// The default payload ceiling that fits through the smallest supported
/// tunnel without relying on IP fragmentation.
pub const DEFAULT_SAFE_RELAY_PAYLOAD_SIZE: usize = 1_200;
pub const WIREGUARD_UDP_PATH_ID: u8 = 1;
pub const QUIC_DATAGRAM_PATH_ID: u8 = 2;

const MAGIC: [u8; 4] = *b"C6LB";
const DISCOVERY_REQUEST: u8 = 1;
const DISCOVERY_TO_HOST: u8 = 2;
const DISCOVERY_RESPONSE: u8 = 3;
const DISCOVERY_TO_CLIENT: u8 = 4;
const GAMEPLAY_PACKET: u8 = 5;
const GAMEPLAY_TO_PEER: u8 = 6;
const RELAY_PROBE: u8 = 7;
const RELAY_PROBE_ACK: u8 = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayMessage {
    DiscoveryRequest {
        request_id: DiscoveryRequestId,
        destination_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    DiscoveryToHost {
        request_id: DiscoveryRequestId,
        source_virtual_ip: VirtualIp,
        destination_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    DiscoveryResponse {
        request_id: DiscoveryRequestId,
        host_session_id: HostSessionId,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    DiscoveryToClient {
        request_id: DiscoveryRequestId,
        host_virtual_ip: VirtualIp,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    GameplayPacket {
        session_id: GameplaySessionId,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    GameplayToPeer {
        session_id: GameplaySessionId,
        source_virtual_ip: VirtualIp,
        destination_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    RelayProbe {
        request_id: DiscoveryRequestId,
    },
    RelayProbeAck {
        request_id: DiscoveryRequestId,
    },
}

/// Metadata carried by the v2 relay envelope.
///
/// `sequence` is sender-local and is intentionally not an acknowledgement
/// number: gameplay datagrams are never replayed by the relay. A non-zero
/// `connection_epoch` identifies the current authenticated path for a peer;
/// the server rejects packets from older epochs after a resume.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelayEnvelopeMeta {
    pub sequence: u64,
    pub connection_epoch: u64,
    /// Milliseconds since the Unix epoch, supplied by the sender for
    /// diagnostics only. It is not used for expiry decisions.
    pub sent_at_ms: u64,
    /// `None` means the default/unknown path. The first concrete path is
    /// WireGuard UDP; QUIC DATAGRAM can use a later value without changing
    /// the room or Civ VI payload protocol.
    pub path_id: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RelayTransportPath {
    WireGuardUdp = WIREGUARD_UDP_PATH_ID,
    QuicDatagram = QUIC_DATAGRAM_PATH_ID,
}

impl RelayTransportPath {
    pub const fn id(self) -> u8 {
        self as u8
    }

    pub const fn from_id(value: u8) -> Option<Self> {
        match value {
            WIREGUARD_UDP_PATH_ID => Some(Self::WireGuardUdp),
            QUIC_DATAGRAM_PATH_ID => Some(Self::QuicDatagram),
            _ => None,
        }
    }
}

impl RelayEnvelopeMeta {
    pub fn new(sequence: u64, connection_epoch: u64, path_id: Option<u8>) -> Self {
        Self {
            sequence,
            connection_epoch,
            sent_at_ms: unix_time_ms(),
            path_id,
        }
    }
}

/// A versioned message plus transport/session metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayEnvelope {
    pub meta: RelayEnvelopeMeta,
    pub message: RelayMessage,
}

impl RelayEnvelope {
    pub fn new(meta: RelayEnvelopeMeta, message: RelayMessage) -> Self {
        Self { meta, message }
    }

    pub fn encode(&self) -> Result<Vec<u8>, RelayCodecError> {
        encode_packet(&self.message, Some(self.meta), RELAY_PROTOCOL_VERSION)
    }

    pub fn decode(packet: &[u8]) -> Result<Self, RelayCodecError> {
        decode_packet(packet)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RelayCodecError {
    #[error("malformed relay packet: {0}")]
    InvalidPacket(String),
    #[error("relay packet kind {0} is not recognized")]
    UnexpectedMessageKind(u8),
}

impl RelayMessage {
    pub fn encode(&self) -> Result<Vec<u8>, RelayCodecError> {
        encode_packet(self, None, LEGACY_RELAY_PROTOCOL_VERSION)
    }

    pub fn encode_with_meta(&self, meta: RelayEnvelopeMeta) -> Result<Vec<u8>, RelayCodecError> {
        encode_packet(self, Some(meta), RELAY_PROTOCOL_VERSION)
    }

    pub fn decode(packet: &[u8]) -> Result<Self, RelayCodecError> {
        Ok(RelayEnvelope::decode(packet)?.message)
    }
}

fn encode_packet(
    message: &RelayMessage,
    meta: Option<RelayEnvelopeMeta>,
    version: u8,
) -> Result<Vec<u8>, RelayCodecError> {
    let mut body = Vec::with_capacity(RELAY_HEADER_SIZE + MAX_CIV6_DATAGRAM_SIZE);
    if let Some(meta) = meta {
        write_meta(&mut body, meta);
    }
    let kind = match message {
        RelayMessage::DiscoveryRequest {
            request_id,
            destination_port,
            payload,
        } => {
            write_id(&mut body, request_id.as_uuid());
            write_port(&mut body, *destination_port);
            write_payload(&mut body, payload)?;
            DISCOVERY_REQUEST
        }
        RelayMessage::DiscoveryToHost {
            request_id,
            source_virtual_ip,
            destination_port,
            payload,
        } => {
            write_id(&mut body, request_id.as_uuid());
            write_ip(&mut body, *source_virtual_ip);
            write_port(&mut body, *destination_port);
            write_payload(&mut body, payload)?;
            DISCOVERY_TO_HOST
        }
        RelayMessage::DiscoveryResponse {
            request_id,
            host_session_id,
            source_port,
            payload,
        } => {
            write_id(&mut body, request_id.as_uuid());
            write_id(&mut body, host_session_id.as_uuid());
            write_port(&mut body, *source_port);
            write_payload(&mut body, payload)?;
            DISCOVERY_RESPONSE
        }
        RelayMessage::DiscoveryToClient {
            request_id,
            host_virtual_ip,
            source_port,
            payload,
        } => {
            write_id(&mut body, request_id.as_uuid());
            write_ip(&mut body, *host_virtual_ip);
            write_port(&mut body, *source_port);
            write_payload(&mut body, payload)?;
            DISCOVERY_TO_CLIENT
        }
        RelayMessage::GameplayPacket {
            session_id,
            source_port,
            payload,
        } => {
            write_id(&mut body, session_id.as_uuid());
            write_port(&mut body, *source_port);
            write_payload(&mut body, payload)?;
            GAMEPLAY_PACKET
        }
        RelayMessage::GameplayToPeer {
            session_id,
            source_virtual_ip,
            destination_port,
            payload,
        } => {
            write_id(&mut body, session_id.as_uuid());
            write_ip(&mut body, *source_virtual_ip);
            write_port(&mut body, *destination_port);
            write_payload(&mut body, payload)?;
            GAMEPLAY_TO_PEER
        }
        RelayMessage::RelayProbe { request_id } => {
            write_id(&mut body, request_id.as_uuid());
            RELAY_PROBE
        }
        RelayMessage::RelayProbeAck { request_id } => {
            write_id(&mut body, request_id.as_uuid());
            RELAY_PROBE_ACK
        }
    };

    if body.len() > u16::MAX as usize {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope body is too large".to_owned(),
        ));
    }
    let mut packet = Vec::with_capacity(RELAY_HEADER_SIZE + body.len());
    packet.extend_from_slice(&MAGIC);
    packet.push(version);
    packet.push(kind);
    packet.extend_from_slice(&(body.len() as u16).to_be_bytes());
    packet.extend_from_slice(&body);
    Ok(packet)
}

fn decode_packet(packet: &[u8]) -> Result<RelayEnvelope, RelayCodecError> {
    if packet.len() > MAX_RELAY_DATAGRAM_SIZE {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope exceeds the maximum datagram size".to_owned(),
        ));
    }
    if packet.len() < RELAY_HEADER_SIZE {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope header is truncated".to_owned(),
        ));
    }
    if packet[..4] != MAGIC {
        return Err(RelayCodecError::InvalidPacket(
            "invalid relay magic".to_owned(),
        ));
    }
    if packet[4] != LEGACY_RELAY_PROTOCOL_VERSION && packet[4] != RELAY_PROTOCOL_VERSION {
        return Err(RelayCodecError::InvalidPacket(format!(
            "unsupported relay version {}",
            packet[4]
        )));
    }

    let declared_body_len = u16::from_be_bytes([packet[6], packet[7]]) as usize;
    if declared_body_len != packet.len() - RELAY_HEADER_SIZE {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope length does not match its header".to_owned(),
        ));
    }

    let kind = packet[5];
    let mut body = &packet[RELAY_HEADER_SIZE..];
    let meta = if packet[4] == RELAY_PROTOCOL_VERSION {
        read_meta(&mut body)?
    } else {
        RelayEnvelopeMeta::default()
    };
    let message = match kind {
        DISCOVERY_REQUEST => RelayMessage::DiscoveryRequest {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            destination_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        DISCOVERY_TO_HOST => RelayMessage::DiscoveryToHost {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            source_virtual_ip: read_ip(&mut body)?,
            destination_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        DISCOVERY_RESPONSE => RelayMessage::DiscoveryResponse {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            host_session_id: HostSessionId::from_uuid(read_id(&mut body)?),
            source_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        DISCOVERY_TO_CLIENT => RelayMessage::DiscoveryToClient {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            host_virtual_ip: read_ip(&mut body)?,
            source_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        GAMEPLAY_PACKET => RelayMessage::GameplayPacket {
            session_id: GameplaySessionId::from_uuid(read_id(&mut body)?),
            source_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        GAMEPLAY_TO_PEER => RelayMessage::GameplayToPeer {
            session_id: GameplaySessionId::from_uuid(read_id(&mut body)?),
            source_virtual_ip: read_ip(&mut body)?,
            destination_port: read_port(&mut body)?,
            payload: read_payload(&mut body)?,
        },
        RELAY_PROBE => RelayMessage::RelayProbe {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
        },
        RELAY_PROBE_ACK => RelayMessage::RelayProbeAck {
            request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
        },
        other => return Err(RelayCodecError::UnexpectedMessageKind(other)),
    };
    if !body.is_empty() {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope contains trailing bytes".to_owned(),
        ));
    }
    Ok(RelayEnvelope { meta, message })
}

fn write_meta(buffer: &mut Vec<u8>, meta: RelayEnvelopeMeta) {
    buffer.extend_from_slice(&meta.sequence.to_be_bytes());
    buffer.extend_from_slice(&meta.connection_epoch.to_be_bytes());
    buffer.extend_from_slice(&meta.sent_at_ms.to_be_bytes());
    match meta.path_id {
        Some(path_id) => {
            buffer.push(1);
            buffer.push(path_id);
        }
        None => buffer.extend_from_slice(&[0, 0]),
    }
}

fn read_meta(input: &mut &[u8]) -> Result<RelayEnvelopeMeta, RelayCodecError> {
    let sequence =
        u64::from_be_bytes(read_exact(input, 8)?.try_into().map_err(|_| {
            RelayCodecError::InvalidPacket("relay sequence is truncated".to_owned())
        })?);
    let connection_epoch = u64::from_be_bytes(read_exact(input, 8)?.try_into().map_err(|_| {
        RelayCodecError::InvalidPacket("relay connection epoch is truncated".to_owned())
    })?);
    let sent_at_ms =
        u64::from_be_bytes(read_exact(input, 8)?.try_into().map_err(|_| {
            RelayCodecError::InvalidPacket("relay timestamp is truncated".to_owned())
        })?);
    let has_path = read_exact(input, 1)?[0];
    let path_id = read_exact(input, 1)?[0];
    match has_path {
        0 => Ok(RelayEnvelopeMeta {
            sequence,
            connection_epoch,
            sent_at_ms,
            path_id: None,
        }),
        1 => Ok(RelayEnvelopeMeta {
            sequence,
            connection_epoch,
            sent_at_ms,
            path_id: Some(path_id),
        }),
        other => Err(RelayCodecError::InvalidPacket(format!(
            "invalid relay path marker {other}"
        ))),
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn write_id(buffer: &mut Vec<u8>, value: Uuid) {
    buffer.extend_from_slice(value.as_bytes());
}

fn write_ip(buffer: &mut Vec<u8>, value: VirtualIp) {
    buffer.extend_from_slice(&u32::from(value.address()).to_be_bytes());
}

fn write_port(buffer: &mut Vec<u8>, value: Civ6UdpPort) {
    buffer.extend_from_slice(&value.0.to_be_bytes());
}

fn write_payload(buffer: &mut Vec<u8>, payload: &[u8]) -> Result<(), RelayCodecError> {
    if payload.len() > MAX_CIV6_DATAGRAM_SIZE {
        return Err(RelayCodecError::InvalidPacket(
            "Civ VI payload exceeds the maximum datagram size".to_owned(),
        ));
    }
    buffer.extend_from_slice(payload);
    Ok(())
}

fn read_exact<'a>(input: &mut &'a [u8], size: usize) -> Result<&'a [u8], RelayCodecError> {
    if input.len() < size {
        return Err(RelayCodecError::InvalidPacket(
            "relay envelope body is truncated".to_owned(),
        ));
    }
    let (head, tail) = input.split_at(size);
    *input = tail;
    Ok(head)
}

fn read_id(input: &mut &[u8]) -> Result<Uuid, RelayCodecError> {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(read_exact(input, 16)?);
    Ok(Uuid::from_bytes(bytes))
}

fn read_ip(input: &mut &[u8]) -> Result<VirtualIp, RelayCodecError> {
    let bytes = read_exact(input, 4)?;
    Ok(VirtualIp::new(Ipv4Addr::new(
        bytes[0], bytes[1], bytes[2], bytes[3],
    )))
}

fn read_port(input: &mut &[u8]) -> Result<Civ6UdpPort, RelayCodecError> {
    let bytes = read_exact(input, 2)?;
    Ok(Civ6UdpPort(u16::from_be_bytes([bytes[0], bytes[1]])))
}

fn read_payload(input: &mut &[u8]) -> Result<Vec<u8>, RelayCodecError> {
    if input.len() > MAX_CIV6_DATAGRAM_SIZE {
        return Err(RelayCodecError::InvalidPacket(
            "Civ VI payload exceeds the maximum datagram size".to_owned(),
        ));
    }
    let payload = input.to_vec();
    *input = &[];
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_magic_and_round_trips_empty_payload() {
        let message = RelayMessage::GameplayPacket {
            session_id: GameplaySessionId::new(),
            source_port: Civ6UdpPort(62_056),
            payload: Vec::new(),
        };
        let packet = message.encode().unwrap();
        assert_eq!(RelayMessage::decode(&packet).unwrap(), message);

        let mut invalid = packet;
        invalid[0] = b'X';
        assert!(matches!(
            RelayMessage::decode(&invalid),
            Err(RelayCodecError::InvalidPacket(_))
        ));
    }

    #[test]
    fn v2_envelope_round_trips_metadata_and_legacy_decode_still_works() {
        let message = RelayMessage::GameplayPacket {
            session_id: GameplaySessionId::new(),
            source_port: Civ6UdpPort(62_056),
            payload: vec![0xc6, 0x6c, 0x62],
        };
        let meta = RelayEnvelopeMeta {
            sequence: 42,
            connection_epoch: 7,
            sent_at_ms: 1_234,
            path_id: Some(1),
        };
        let packet = RelayEnvelope::new(meta, message.clone()).encode().unwrap();
        assert_eq!(
            RelayEnvelope::decode(&packet).unwrap(),
            RelayEnvelope::new(meta, message.clone())
        );
        assert_eq!(RelayMessage::decode(&packet).unwrap(), message);

        let legacy = message.encode().unwrap();
        assert_eq!(
            RelayEnvelope::decode(&legacy).unwrap(),
            RelayEnvelope::new(RelayEnvelopeMeta::default(), message)
        );
    }

    #[test]
    fn transport_path_ids_reserve_wireguard_and_quic_without_aliasing_them() {
        assert_eq!(RelayTransportPath::WireGuardUdp.id(), WIREGUARD_UDP_PATH_ID);
        assert_eq!(RelayTransportPath::QuicDatagram.id(), QUIC_DATAGRAM_PATH_ID);
        assert_ne!(
            RelayTransportPath::WireGuardUdp.id(),
            RelayTransportPath::QuicDatagram.id()
        );
        assert_eq!(
            RelayTransportPath::from_id(QUIC_DATAGRAM_PATH_ID),
            Some(RelayTransportPath::QuicDatagram)
        );
    }
}
