use std::{net::Ipv4Addr, string::FromUtf8Error};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use civ6_lan_protocol::VirtualIp;
use thiserror::Error;
use tokio::process::Command;

pub const PERSISTENT_KEEPALIVE_SECONDS: u16 = 25;

#[derive(Clone, Debug)]
pub struct WireGuardManager {
    interface: String,
}

#[derive(Debug, Error)]
pub enum WireGuardError {
    #[error("WireGuard public key must decode to exactly 32 bytes")]
    InvalidPublicKey,
    #[error("WireGuard command failed: {0}")]
    CommandFailed(String),
    #[error("failed to execute wg: {0}")]
    Io(#[from] std::io::Error),
    #[error("WireGuard command output was not valid UTF-8: {0}")]
    InvalidOutput(#[from] FromUtf8Error),
}

impl WireGuardManager {
    pub fn new(interface: impl Into<String>) -> Self {
        Self {
            interface: interface.into(),
        }
    }

    pub fn interface(&self) -> &str {
        &self.interface
    }

    pub fn validate_public_key(public_key: &str) -> Result<(), WireGuardError> {
        let decoded = STANDARD
            .decode(public_key)
            .map_err(|_| WireGuardError::InvalidPublicKey)?;
        if decoded.len() == 32 {
            Ok(())
        } else {
            Err(WireGuardError::InvalidPublicKey)
        }
    }

    pub fn add_peer_args(&self, public_key: &str, virtual_ip: VirtualIp) -> Vec<String> {
        vec![
            "set".to_owned(),
            self.interface.clone(),
            "peer".to_owned(),
            public_key.to_owned(),
            "allowed-ips".to_owned(),
            format!("{virtual_ip}/32"),
            "persistent-keepalive".to_owned(),
            PERSISTENT_KEEPALIVE_SECONDS.to_string(),
        ]
    }

    pub fn remove_peer_args(&self, public_key: &str) -> Vec<String> {
        vec![
            "set".to_owned(),
            self.interface.clone(),
            "peer".to_owned(),
            public_key.to_owned(),
            "remove".to_owned(),
        ]
    }

    pub async fn add_peer(
        &self,
        public_key: &str,
        virtual_ip: VirtualIp,
    ) -> Result<(), WireGuardError> {
        Self::validate_public_key(public_key)?;
        self.run(self.add_peer_args(public_key, virtual_ip)).await
    }

    pub async fn remove_peer(&self, public_key: &str) -> Result<(), WireGuardError> {
        Self::validate_public_key(public_key)?;
        self.run(self.remove_peer_args(public_key)).await
    }

    async fn run(&self, args: Vec<String>) -> Result<(), WireGuardError> {
        let output = Command::new("wg").args(args).output().await?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8(output.stderr)?;
            Err(WireGuardError::CommandFailed(stderr.trim().to_owned()))
        }
    }
}

pub fn virtual_ip_cidr(ip: VirtualIp) -> String {
    let address: Ipv4Addr = ip.address();
    format!("{address}/32")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    #[test]
    fn validates_wireguard_public_key_length() {
        let key = STANDARD.encode([0_u8; 32]);
        assert!(WireGuardManager::validate_public_key(&key).is_ok());
        assert!(WireGuardManager::validate_public_key("not-a-key").is_err());
    }

    #[test]
    fn builds_argument_vectors_without_shell_interpolation() {
        let manager = WireGuardManager::new("wg0");
        let key = STANDARD.encode([1_u8; 32]);
        let args = manager.add_peer_args(&key, VirtualIp::new("10.240.0.2".parse().unwrap()));
        assert_eq!(args[0], "set");
        assert_eq!(args[1], "wg0");
        assert_eq!(args[2], "peer");
        assert_eq!(args[3], key);
        assert_eq!(args[4], "allowed-ips");
        assert_eq!(args[5], "10.240.0.2/32");
        assert_eq!(args[6], "persistent-keepalive");
        assert_eq!(args[7], "25");
        assert_eq!(
            virtual_ip_cidr(VirtualIp::new("10.240.0.2".parse().unwrap())),
            "10.240.0.2/32"
        );
    }
}
