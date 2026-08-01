//! Cross-platform client core shared by the Windows and macOS desktop apps.
//!
//! The UI and packet interception layers are platform-specific. This crate
//! owns the control-plane calls, relay envelope transport, and diagnostics so
//! both clients exercise exactly the same server protocol.

use std::{io, net::SocketAddr, time::Duration};

use async_trait::async_trait;
use civ6_lan_protocol::{
    relay::{RelayCodecError, RelayMessage},
    DiscoveryRequestId, GameplaySessionId, HostSessionId, PeerId, RoomCode, RoomId, VirtualIp,
};
use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;
use tokio::net::UdpSocket;

pub const DEFAULT_RELAY_PORT: u16 = 32_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TestSessionManifest {
    pub session_id: String,
    pub room_id: RoomId,
    pub room_code: RoomCode,
    pub client_id: PeerId,
    pub client_virtual_ip: VirtualIp,
    pub relay_host: String,
    pub relay_port: u16,
    pub control_endpoint: String,
    pub protocol_version: u8,
    pub token: String,
    pub expires_at: u64,
    pub test_mode: bool,
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub control_url: String,
    pub bearer_token: String,
    pub relay_server: SocketAddr,
    pub relay_port: u16,
}

impl ClientConfig {
    pub fn normalize_control_url(mut self) -> Self {
        while self.control_url.ends_with('/') {
            self.control_url.pop();
        }
        self
    }

    pub fn relay_addr(&self) -> SocketAddr {
        SocketAddr::new(self.relay_server.ip(), self.relay_port)
    }
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("control request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("control API returned HTTP {status}: {message}")]
    HttpStatus { status: StatusCode, message: String },
    #[error("relay codec failed: {0}")]
    RelayCodec(#[from] RelayCodecError),
    #[error("relay socket failed: {0}")]
    RelayIo(#[from] std::io::Error),
    #[error("relay response timed out")]
    RelayTimeout,
    #[error("server returned an unexpected response: {0}")]
    InvalidResponse(String),
}

#[derive(Clone)]
pub struct ControlClient {
    http: Client,
    config: ClientConfig,
}

impl ControlClient {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            http: Client::new(),
            config: config.normalize_control_url(),
        }
    }

    pub async fn health_live(&self) -> Result<HealthResponse, ClientError> {
        self.request(reqwest::Method::GET, "/health/live", &())
            .await
    }

    pub async fn create_room(
        &self,
        room_code: Option<RoomCode>,
    ) -> Result<RoomResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            "/v1/rooms",
            &CreateRoomRequest { room_code },
        )
        .await
    }

    pub async fn join_room(
        &self,
        room_code: &RoomCode,
        peer_id: Option<PeerId>,
        wireguard_public_key: Option<String>,
    ) -> Result<PeerResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_code}/join"),
            &JoinRoomRequest {
                peer_id,
                wireguard_public_key,
            },
        )
        .await
    }

    pub async fn register_host(
        &self,
        room_code: &RoomCode,
        peer_id: PeerId,
    ) -> Result<HostResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_code}/hosts"),
            &RegisterHostRequest { peer_id },
        )
        .await
    }

    pub async fn heartbeat_host(
        &self,
        room_code: &RoomCode,
        peer_id: PeerId,
        host_session_id: HostSessionId,
    ) -> Result<HostResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_code}/heartbeat"),
            &HeartbeatRequest {
                peer_id,
                host_session_id,
            },
        )
        .await
    }

    pub async fn create_gameplay_session(
        &self,
        room_code: &RoomCode,
        client_peer_id: PeerId,
        host_session_id: HostSessionId,
    ) -> Result<GameplayResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_code}/gameplay-sessions"),
            &CreateGameplayRequest {
                client_peer_id,
                host_session_id,
            },
        )
        .await
    }

    pub async fn room_status(&self, room_code: &RoomCode) -> Result<RoomResponse, ClientError> {
        self.request(
            reqwest::Method::GET,
            &format!("/v1/rooms/{room_code}/status"),
            &(),
        )
        .await
    }

    pub async fn relay_metrics(&self) -> Result<RelayMetricsResponse, ClientError> {
        self.request(reqwest::Method::GET, "/v1/test/metrics", &())
            .await
    }

    pub async fn delete_host(
        &self,
        room_code: &RoomCode,
        host_session_id: HostSessionId,
    ) -> Result<(), ClientError> {
        self.request_empty(
            reqwest::Method::DELETE,
            &format!("/v1/rooms/{room_code}/hosts/{host_session_id}"),
        )
        .await
    }

    pub async fn delete_peer(
        &self,
        room_code: &RoomCode,
        peer_id: PeerId,
    ) -> Result<(), ClientError> {
        self.request_empty(
            reqwest::Method::DELETE,
            &format!("/v1/rooms/{room_code}/peers/{peer_id}"),
        )
        .await
    }

    pub async fn delete_room(&self, room_code: &RoomCode) -> Result<(), ClientError> {
        self.request_empty(reqwest::Method::DELETE, &format!("/v1/rooms/{room_code}"))
            .await
    }

    async fn request<T: DeserializeOwned, B: Serialize>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &B,
    ) -> Result<T, ClientError> {
        let request = self
            .http
            .request(method, format!("{}{}", self.config.control_url, path))
            .bearer_auth(&self.config.bearer_token)
            .json(body);
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let message = response
                .json::<ApiErrorResponse>()
                .await
                .map(|error| error.message)
                .unwrap_or_else(|_| "control request failed".to_owned());
            return Err(ClientError::HttpStatus { status, message });
        }
        response.json::<T>().await.map_err(ClientError::Http)
    }

    async fn request_empty(&self, method: reqwest::Method, path: &str) -> Result<(), ClientError> {
        let response = self
            .http
            .request(method, format!("{}{}", self.config.control_url, path))
            .bearer_auth(&self.config.bearer_token)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let message = response
                .json::<ApiErrorResponse>()
                .await
                .map(|error| error.message)
                .unwrap_or_else(|_| "control request failed".to_owned());
            return Err(ClientError::HttpStatus { status, message });
        }
        Ok(())
    }
}

/// Datagram transport boundary for the Civ VI relay.
///
/// The Phase 1 protocol is intentionally defined over this interface rather
/// than directly over UDP. The first implementation is WireGuard/UDP, while
/// a QUIC DATAGRAM or UDP-obfuscation transport can be added later without
/// changing room discovery or gameplay routing. Implementations must preserve
/// datagram boundaries and must not silently turn gameplay into a byte stream.
#[async_trait]
pub trait DatagramTransport: Send + Sync {
    async fn send(&self, packet: &[u8]) -> io::Result<()>;
    async fn receive(&self, buffer: &mut [u8]) -> io::Result<usize>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

/// Default datagram transport: one connected UDP socket per relay session.
pub struct UdpDatagramTransport {
    socket: UdpSocket,
}

impl UdpDatagramTransport {
    pub async fn bind(local: SocketAddr, server: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local).await?;
        socket.connect(server).await?;
        Ok(Self { socket })
    }
}

#[async_trait]
impl DatagramTransport for UdpDatagramTransport {
    async fn send(&self, packet: &[u8]) -> io::Result<()> {
        self.socket.send(packet).await.map(|_| ())
    }

    async fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
        self.socket.recv(buffer).await
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

pub struct RelayClient<T = UdpDatagramTransport> {
    transport: T,
}

impl RelayClient<UdpDatagramTransport> {
    pub async fn bind(local: SocketAddr, server: SocketAddr) -> Result<Self, ClientError> {
        Ok(Self {
            transport: UdpDatagramTransport::bind(local, server).await?,
        })
    }
}

impl<T: DatagramTransport> RelayClient<T> {
    pub fn from_transport(transport: T) -> Self {
        Self { transport }
    }

    pub async fn send(&self, message: &RelayMessage) -> Result<(), ClientError> {
        let packet = message.encode()?;
        self.transport.send(&packet).await?;
        Ok(())
    }

    pub async fn receive(&self) -> Result<RelayMessage, ClientError> {
        let mut buffer = vec![0u8; civ6_lan_protocol::relay::MAX_RELAY_DATAGRAM_SIZE];
        let length = self.transport.receive(&mut buffer).await?;
        Ok(RelayMessage::decode(&buffer[..length])?)
    }

    pub async fn exchange(
        &self,
        message: &RelayMessage,
        timeout: Duration,
    ) -> Result<RelayMessage, ClientError> {
        self.send(message).await?;
        tokio::time::timeout(timeout, self.receive())
            .await
            .map_err(|_| ClientError::RelayTimeout)?
    }

    pub async fn probe(&self, timeout: Duration) -> Result<(), ClientError> {
        let request_id = DiscoveryRequestId::new();
        let response = self
            .exchange(&RelayMessage::RelayProbe { request_id }, timeout)
            .await?;
        match response {
            RelayMessage::RelayProbeAck { request_id: actual } if actual == request_id => Ok(()),
            other => Err(ClientError::InvalidResponse(format!(
                "expected relay probe ACK, received {other:?}"
            ))),
        }
    }

    pub async fn local_addr(&self) -> Result<SocketAddr, ClientError> {
        Ok(self.transport.local_addr()?)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub database_configured: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RoomResponse {
    pub room_id: RoomId,
    pub room_code: RoomCode,
    pub member_count: usize,
    pub host_count: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PeerResponse {
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub virtual_ip: VirtualIp,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HostResponse {
    pub room_id: RoomId,
    pub host_session_id: HostSessionId,
    pub peer_id: PeerId,
    pub virtual_ip: VirtualIp,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GameplayResponse {
    pub gameplay_session_id: GameplaySessionId,
    pub room_id: RoomId,
    pub client_peer_id: PeerId,
    pub host_peer_id: PeerId,
    pub client_virtual_ip: VirtualIp,
    pub host_virtual_ip: VirtualIp,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RelayMetricsResponse {
    pub sent_packets: u64,
    pub received_packets: u64,
    pub dropped_packets: u64,
    pub duplicated_packets: u64,
    pub reordered_packets: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_peers: usize,
    pub active_rooms: usize,
    pub active_hosts: usize,
    pub authentication_failures: u64,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    message: String,
}

#[derive(Debug, Serialize)]
struct CreateRoomRequest {
    room_code: Option<RoomCode>,
}

#[derive(Debug, Serialize)]
struct JoinRoomRequest {
    peer_id: Option<PeerId>,
    wireguard_public_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterHostRequest {
    peer_id: PeerId,
}

#[derive(Debug, Serialize)]
struct HeartbeatRequest {
    peer_id: PeerId,
    host_session_id: HostSessionId,
}

#[derive(Debug, Serialize)]
struct CreateGameplayRequest {
    client_peer_id: PeerId,
    host_session_id: HostSessionId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use civ6_lan_protocol::{Civ6UdpPort, GameplaySessionId};
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn relay_client_exchanges_a_versioned_message_over_udp() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = RelayClient::bind("127.0.0.1:0".parse().unwrap(), server_addr)
            .await
            .unwrap();
        let expected = RelayMessage::GameplayPacket {
            session_id: GameplaySessionId::new(),
            source_port: Civ6UdpPort(62_056),
            payload: vec![1, 2, 3],
        };
        let server_task = tokio::spawn(async move {
            let mut buffer = [0u8; 512];
            let (length, source) = server.recv_from(&mut buffer).await.unwrap();
            server.send_to(&buffer[..length], source).await.unwrap();
        });
        let actual = client
            .exchange(&expected, Duration::from_secs(1))
            .await
            .unwrap();
        server_task.await.unwrap();
        assert_eq!(actual, expected);
        assert_eq!(client.local_addr().await.unwrap().ip(), Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn config_normalization_removes_trailing_slashes() {
        let config = ClientConfig {
            control_url: "https://example.test///".to_owned(),
            bearer_token: "token".to_owned(),
            relay_server: "127.0.0.1:32000".parse().unwrap(),
            relay_port: DEFAULT_RELAY_PORT,
        }
        .normalize_control_url();
        assert_eq!(config.control_url, "https://example.test");
    }

    #[test]
    fn relay_port_overrides_the_port_in_the_server_setting() {
        let config = ClientConfig {
            control_url: "http://127.0.0.1:8080".to_owned(),
            bearer_token: "token".to_owned(),
            relay_server: "10.240.0.1:9999".parse().unwrap(),
            relay_port: 32_001,
        };
        assert_eq!(config.relay_addr(), "10.240.0.1:32001".parse().unwrap());
    }
}
