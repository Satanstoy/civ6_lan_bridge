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

pub const RELAY_PROTOCOL_VERSION: u8 = 1;
pub const RELAY_HEADER_SIZE: usize = 8;
pub const MAX_RELAY_DATAGRAM_SIZE: usize = RELAY_HEADER_SIZE + MAX_CIV6_DATAGRAM_SIZE + 64;

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

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RelayCodecError {
    #[error("malformed relay packet: {0}")]
    InvalidPacket(String),
    #[error("relay packet kind {0} is not recognized")]
    UnexpectedMessageKind(u8),
}

impl RelayMessage {
    pub fn encode(&self) -> Result<Vec<u8>, RelayCodecError> {
        let mut body = Vec::with_capacity(RELAY_HEADER_SIZE + MAX_CIV6_DATAGRAM_SIZE);
        let kind = match self {
            Self::DiscoveryRequest {
                request_id,
                destination_port,
                payload,
            } => {
                write_id(&mut body, request_id.as_uuid());
                write_port(&mut body, *destination_port);
                write_payload(&mut body, payload)?;
                DISCOVERY_REQUEST
            }
            Self::DiscoveryToHost {
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
            Self::DiscoveryResponse {
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
            Self::DiscoveryToClient {
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
            Self::GameplayPacket {
                session_id,
                source_port,
                payload,
            } => {
                write_id(&mut body, session_id.as_uuid());
                write_port(&mut body, *source_port);
                write_payload(&mut body, payload)?;
                GAMEPLAY_PACKET
            }
            Self::GameplayToPeer {
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
            Self::RelayProbe { request_id } => {
                write_id(&mut body, request_id.as_uuid());
                RELAY_PROBE
            }
            Self::RelayProbeAck { request_id } => {
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
        packet.push(RELAY_PROTOCOL_VERSION);
        packet.push(kind);
        packet.extend_from_slice(&(body.len() as u16).to_be_bytes());
        packet.extend_from_slice(&body);
        Ok(packet)
    }

    pub fn decode(packet: &[u8]) -> Result<Self, RelayCodecError> {
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
        if packet[4] != RELAY_PROTOCOL_VERSION {
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
        let message = match kind {
            DISCOVERY_REQUEST => Self::DiscoveryRequest {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
                destination_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            DISCOVERY_TO_HOST => Self::DiscoveryToHost {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
                source_virtual_ip: read_ip(&mut body)?,
                destination_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            DISCOVERY_RESPONSE => Self::DiscoveryResponse {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
                host_session_id: HostSessionId::from_uuid(read_id(&mut body)?),
                source_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            DISCOVERY_TO_CLIENT => Self::DiscoveryToClient {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
                host_virtual_ip: read_ip(&mut body)?,
                source_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            GAMEPLAY_PACKET => Self::GameplayPacket {
                session_id: GameplaySessionId::from_uuid(read_id(&mut body)?),
                source_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            GAMEPLAY_TO_PEER => Self::GameplayToPeer {
                session_id: GameplaySessionId::from_uuid(read_id(&mut body)?),
                source_virtual_ip: read_ip(&mut body)?,
                destination_port: read_port(&mut body)?,
                payload: read_payload(&mut body)?,
            },
            RELAY_PROBE => Self::RelayProbe {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            },
            RELAY_PROBE_ACK => Self::RelayProbeAck {
                request_id: DiscoveryRequestId::from_uuid(read_id(&mut body)?),
            },
            other => return Err(RelayCodecError::UnexpectedMessageKind(other)),
        };
        if !body.is_empty() {
            return Err(RelayCodecError::InvalidPacket(
                "relay envelope contains trailing bytes".to_owned(),
            ));
        }
        Ok(message)
    }
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
}
