//! Versioned, platform-neutral identifiers and Civ VI network constants.
//!
//! This crate deliberately contains no socket or operating-system code. The
//! Windows and macOS adapters and the server use these types to agree on
//! room/session identity without sharing packet interception implementation.

use std::{fmt, net::Ipv4Addr, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub mod relay;

pub const DISCOVERY_PORT_START: u16 = 62_900;
pub const DISCOVERY_PORT_END: u16 = 62_999;
pub const GAMEPLAY_PORT: u16 = 62_056;
pub const MAX_CIV6_DATAGRAM_SIZE: usize = 4 * 1024;

pub fn is_discovery_port(port: u16) -> bool {
    (DISCOVERY_PORT_START..=DISCOVERY_PORT_END).contains(&port)
}

pub fn is_civ6_port(port: u16) -> bool {
    is_discovery_port(port) || port == GAMEPLAY_PORT
}

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(value)?))
            }
        }
    };
}

uuid_id!(RoomId);
uuid_id!(PeerId);
uuid_id!(HostSessionId);
uuid_id!(DiscoveryRequestId);
uuid_id!(GameplaySessionId);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RoomCode(String);

impl RoomCode {
    pub const MIN_LEN: usize = 6;
    pub const MAX_LEN: usize = 12;

    pub fn parse(value: impl AsRef<str>) -> Result<Self, RoomCodeError> {
        let value = value.as_ref().to_ascii_uppercase();

        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&value.len()) {
            return Err(RoomCodeError::InvalidLength {
                min: Self::MIN_LEN,
                max: Self::MAX_LEN,
            });
        }

        if let Some(character) = value
            .chars()
            .find(|character| !"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(*character))
        {
            return Err(RoomCodeError::InvalidCharacter(character));
        }

        Ok(Self(value))
    }

    pub fn random() -> Self {
        const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let bytes = Uuid::new_v4().into_bytes();
        let value: String = bytes[..8]
            .iter()
            .map(|byte| ALPHABET[usize::from(*byte & 31)] as char)
            .collect();
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RoomCode {
    type Err = RoomCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for RoomCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

impl fmt::Display for RoomCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RoomCodeError {
    #[error("room code length must be between {min} and {max} characters")]
    InvalidLength { min: usize, max: usize },
    #[error("room code contains invalid character {0:?}")]
    InvalidCharacter(char),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct VirtualIp(Ipv4Addr);

impl VirtualIp {
    pub const fn new(value: Ipv4Addr) -> Self {
        Self(value)
    }

    pub const fn address(self) -> Ipv4Addr {
        self.0
    }
}

impl fmt::Display for VirtualIp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Civ6UdpPort(pub u16);

impl Civ6UdpPort {
    pub fn validate(self) -> Result<Self, InvalidCiv6Port> {
        if is_civ6_port(self.0) {
            Ok(self)
        } else {
            Err(InvalidCiv6Port(self.0))
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("UDP port {0} is not a supported Civ VI port")]
pub struct InvalidCiv6Port(pub u16);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_range_is_inclusive() {
        assert!(is_discovery_port(DISCOVERY_PORT_START));
        assert!(is_discovery_port(DISCOVERY_PORT_END));
        assert!(!is_discovery_port(DISCOVERY_PORT_START - 1));
        assert!(!is_discovery_port(DISCOVERY_PORT_END + 1));
    }

    #[test]
    fn room_codes_are_normalized_and_reject_ambiguous_characters() {
        assert_eq!(RoomCode::parse("ab2345").unwrap().as_str(), "AB2345");
        assert!(RoomCode::random()
            .as_str()
            .chars()
            .all(|character| { "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(character) }));
        assert_eq!(
            RoomCode::parse("abcde").unwrap_err(),
            RoomCodeError::InvalidLength {
                min: RoomCode::MIN_LEN,
                max: RoomCode::MAX_LEN,
            }
        );
        assert_eq!(
            RoomCode::parse("ABC1EF").unwrap_err(),
            RoomCodeError::InvalidCharacter('1')
        );
        assert_eq!(
            RoomCode::parse("ABC0EF").unwrap_err(),
            RoomCodeError::InvalidCharacter('0')
        );
    }

    #[test]
    fn only_civ6_ports_are_accepted() {
        assert!(Civ6UdpPort(DISCOVERY_PORT_START).validate().is_ok());
        assert!(Civ6UdpPort(GAMEPLAY_PORT).validate().is_ok());
        assert_eq!(
            Civ6UdpPort(12345).validate().unwrap_err(),
            InvalidCiv6Port(12345)
        );
    }
}
