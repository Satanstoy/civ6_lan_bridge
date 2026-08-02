//! Cross-platform client core shared by the Windows and macOS desktop apps.
//!
//! The UI and packet interception layers are platform-specific. This crate
//! owns the control-plane calls, relay envelope transport, and diagnostics so
//! both clients exercise exactly the same server protocol.

use std::{
    collections::VecDeque,
    future::Future,
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use civ6_lan_protocol::{
    relay::{
        RelayCodecError, RelayEnvelope, RelayEnvelopeMeta, RelayMessage, RelayTransportPath,
        DEFAULT_SAFE_RELAY_PAYLOAD_SIZE, MAX_RELAY_DATAGRAM_SIZE, WIREGUARD_UDP_PATH_ID,
    },
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
    #[error("relay payload of {size} bytes exceeds the safe limit of {limit} bytes")]
    RelayPayloadTooLarge { size: usize, limit: usize },
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

    pub async fn resume_room(
        &self,
        room_code: &RoomCode,
        peer_id: PeerId,
    ) -> Result<PeerResponse, ClientError> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_code}/resume"),
            &ResumeRoomRequest { peer_id },
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

    /// The path identifier is part of the shared envelope, so a future QUIC
    /// DATAGRAM implementation can use the same session and room protocol.
    fn path_id(&self) -> Option<u8> {
        Some(WIREGUARD_UDP_PATH_ID)
    }
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

    fn path_id(&self) -> Option<u8> {
        Some(RelayTransportPath::WireGuardUdp.id())
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

    pub fn path_id(&self) -> Option<u8> {
        self.transport.path_id()
    }

    pub async fn send(&self, message: &RelayMessage) -> Result<(), ClientError> {
        let packet = message.encode()?;
        self.transport.send(&packet).await?;
        Ok(())
    }

    pub async fn receive(&self) -> Result<RelayMessage, ClientError> {
        Ok(self.receive_envelope().await?.message)
    }

    pub async fn receive_envelope(&self) -> Result<RelayEnvelope, ClientError> {
        let mut buffer = vec![0u8; MAX_RELAY_DATAGRAM_SIZE];
        let length = self.transport.receive(&mut buffer).await?;
        Ok(RelayEnvelope::decode(&buffer[..length])?)
    }

    pub async fn send_envelope(
        &self,
        message: &RelayMessage,
        meta: RelayEnvelopeMeta,
    ) -> Result<(), ClientError> {
        self.transport
            .send(&RelayEnvelope::new(meta, message.clone()).encode()?)
            .await?;
        Ok(())
    }

    pub async fn exchange_envelope(
        &self,
        message: &RelayMessage,
        meta: RelayEnvelopeMeta,
        timeout: Duration,
    ) -> Result<RelayEnvelope, ClientError> {
        self.send_envelope(message, meta).await?;
        tokio::time::timeout(timeout, self.receive_envelope())
            .await
            .map_err(|_| ClientError::RelayTimeout)?
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
        self.send(&RelayMessage::RelayProbe { request_id }).await?;
        let started = Instant::now();
        loop {
            let remaining = timeout
                .checked_sub(started.elapsed())
                .ok_or(ClientError::RelayTimeout)?;
            match tokio::time::timeout(remaining, self.receive()).await {
                Ok(Ok(RelayMessage::RelayProbeAck { request_id: actual }))
                    if actual == request_id =>
                {
                    return Ok(())
                }
                Ok(Ok(_)) => continue,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(ClientError::RelayTimeout),
            }
        }
    }

    pub async fn local_addr(&self) -> Result<SocketAddr, ClientError> {
        Ok(self.transport.local_addr()?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayConnectionState {
    Disconnected,
    Connecting,
    Authenticated,
    RoomJoined,
    Healthy,
    Degraded,
    Reconnecting,
    Resynced,
}

#[derive(Clone, Copy, Debug)]
pub struct RelaySessionConfig {
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    pub degraded_after_failures: u32,
    pub reconnect_initial_backoff: Duration,
    pub reconnect_max_backoff: Duration,
    pub discovery_retry_interval: Duration,
    pub discovery_retry_window: Duration,
    pub safe_payload_size: usize,
    pub path_id: Option<u8>,
}

impl Default for RelaySessionConfig {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(2),
            probe_timeout: Duration::from_millis(750),
            degraded_after_failures: 3,
            reconnect_initial_backoff: Duration::from_millis(250),
            reconnect_max_backoff: Duration::from_secs(8),
            discovery_retry_interval: Duration::from_millis(250),
            discovery_retry_window: Duration::from_secs(5),
            safe_payload_size: DEFAULT_SAFE_RELAY_PAYLOAD_SIZE,
            // Let the transport report its concrete path. A caller can set
            // this explicitly while migrating or forcing a fallback.
            path_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RelaySessionSnapshot {
    pub state: RelayConnectionState,
    pub consecutive_probe_failures: u32,
    pub reconnect_attempts: u32,
    pub connection_epoch: u64,
    pub last_rtt_ms: Option<u64>,
    pub rtt_p50_ms: Option<u64>,
    pub rtt_p95_ms: Option<u64>,
}

impl RelaySessionSnapshot {
    fn new(connection_epoch: u64) -> Self {
        Self {
            state: RelayConnectionState::Disconnected,
            consecutive_probe_failures: 0,
            reconnect_attempts: 0,
            connection_epoch,
            last_rtt_ms: None,
            rtt_p50_ms: None,
            rtt_p95_ms: None,
        }
    }
}

/// A small, transport-agnostic session controller shared by Windows and
/// macOS. It owns retry timing and envelope sequencing, while the caller's
/// reconnect callback performs the authenticated control-plane resume.
pub struct RelaySession<T> {
    client: RelayClient<T>,
    config: RelaySessionConfig,
    snapshot: RelaySessionSnapshot,
    next_sequence: u64,
    rtt_samples_ms: VecDeque<u64>,
}

impl<T: DatagramTransport> RelaySession<T> {
    pub fn new(client: RelayClient<T>, connection_epoch: u64) -> Self {
        Self::with_config(client, connection_epoch, RelaySessionConfig::default())
    }

    pub fn with_config(
        client: RelayClient<T>,
        connection_epoch: u64,
        config: RelaySessionConfig,
    ) -> Self {
        Self {
            client,
            config,
            snapshot: RelaySessionSnapshot::new(connection_epoch),
            next_sequence: 0,
            rtt_samples_ms: VecDeque::with_capacity(128),
        }
    }

    pub fn client(&self) -> &RelayClient<T> {
        &self.client
    }

    pub fn snapshot(&self) -> RelaySessionSnapshot {
        self.snapshot.clone()
    }

    pub fn mark_authenticated(&mut self, connection_epoch: u64) {
        self.snapshot.connection_epoch = connection_epoch;
        self.snapshot.state = RelayConnectionState::Authenticated;
    }

    pub fn mark_room_joined(&mut self) {
        self.snapshot.state = RelayConnectionState::RoomJoined;
    }

    pub fn mark_disconnected(&mut self) {
        self.snapshot.state = RelayConnectionState::Disconnected;
    }

    /// Record a failed relay probe. Returns `true` when the session crosses
    /// the degraded threshold and should start its reconnect backoff.
    pub fn record_probe_failure(&mut self) -> bool {
        self.snapshot.consecutive_probe_failures =
            self.snapshot.consecutive_probe_failures.saturating_add(1);
        if self.snapshot.consecutive_probe_failures >= self.config.degraded_after_failures {
            self.snapshot.state = RelayConnectionState::Degraded;
            true
        } else {
            false
        }
    }

    pub fn reconnect_delay_for_next_attempt(&self) -> Duration {
        self.reconnect_delay()
    }

    pub fn next_envelope_meta(&mut self) -> RelayEnvelopeMeta {
        self.next_sequence = self.next_sequence.saturating_add(1).max(1);
        RelayEnvelopeMeta::new(
            self.next_sequence,
            self.snapshot.connection_epoch,
            self.config.path_id.or_else(|| self.client.path_id()),
        )
    }

    pub async fn probe_once(&mut self) -> Result<Duration, ClientError> {
        if self.snapshot.state == RelayConnectionState::Disconnected {
            self.snapshot.state = RelayConnectionState::Connecting;
        }
        let request_id = DiscoveryRequestId::new();
        let meta = self.next_envelope_meta();
        let started = Instant::now();
        self.client
            .send_envelope(&RelayMessage::RelayProbe { request_id }, meta)
            .await?;
        loop {
            let elapsed = started.elapsed();
            let remaining = self
                .config
                .probe_timeout
                .checked_sub(elapsed)
                .ok_or(ClientError::RelayTimeout)?;
            let response = tokio::time::timeout(remaining, self.client.receive_envelope())
                .await
                .map_err(|_| ClientError::RelayTimeout)??;
            if matches!(
                response.message,
                RelayMessage::RelayProbeAck { request_id: actual } if actual == request_id
            ) {
                let rtt = started.elapsed();
                self.record_probe_success(rtt);
                return Ok(rtt);
            }
        }
    }

    /// Keep the relay path healthy until the task is cancelled. The callback
    /// is invoked only after the failure threshold and should call the control
    /// API's resume endpoint. Returning the fresh epoch invalidates the old
    /// gameplay path without replaying any Civ VI packets.
    pub async fn run_with_reconnect<F, Fut>(&mut self, mut reconnect: F) -> Result<(), ClientError>
    where
        F: FnMut(RelaySessionSnapshot) -> Fut,
        Fut: Future<Output = Result<u64, ClientError>>,
    {
        loop {
            match self.probe_once().await {
                Ok(_) => {
                    if matches!(
                        self.snapshot.state,
                        RelayConnectionState::Resynced | RelayConnectionState::RoomJoined
                    ) {
                        self.snapshot.state = RelayConnectionState::Healthy;
                    }
                    tokio::time::sleep(self.config.probe_interval).await;
                }
                Err(_error) => {
                    if !self.record_probe_failure() {
                        tokio::time::sleep(self.config.probe_interval).await;
                        continue;
                    }

                    loop {
                        self.snapshot.state = RelayConnectionState::Reconnecting;
                        let delay = self.reconnect_delay();
                        tokio::time::sleep(delay).await;
                        self.snapshot.reconnect_attempts =
                            self.snapshot.reconnect_attempts.saturating_add(1);
                        match reconnect(self.snapshot()).await {
                            Ok(connection_epoch) => {
                                self.snapshot.connection_epoch = connection_epoch;
                                self.snapshot.consecutive_probe_failures = 0;
                                self.snapshot.reconnect_attempts = 0;
                                self.next_sequence = 0;
                                self.snapshot.state = RelayConnectionState::Resynced;
                                break;
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
        }
    }

    pub async fn send_gameplay(&mut self, message: &RelayMessage) -> Result<(), ClientError> {
        self.ensure_safe_payload(message)?;
        let meta = self.next_envelope_meta();
        self.client.send_envelope(message, meta).await
    }

    /// Discovery is the only application traffic retried by the client. The
    /// request ID stays stable so the relay can refresh its cached fan-out
    /// without creating a second logical request.
    pub async fn send_discovery_with_retry(
        &mut self,
        message: &RelayMessage,
    ) -> Result<u32, ClientError> {
        let request_id = match message {
            RelayMessage::DiscoveryRequest { request_id, .. } => *request_id,
            _ => {
                return Err(ClientError::InvalidResponse(
                    "discovery retry requires a DiscoveryRequest".to_owned(),
                ))
            }
        };
        let deadline = Instant::now() + self.config.discovery_retry_window;
        let mut sends = 0;
        let mut last_error = None;
        while Instant::now() < deadline {
            self.ensure_safe_payload(message)?;
            let meta = self.next_envelope_meta();
            match self.client.send_envelope(message, meta).await {
                Ok(()) => {
                    sends += 1;
                    last_error = None;
                }
                Err(error) => last_error = Some(error),
            }
            tokio::time::sleep(self.config.discovery_retry_interval).await;
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        let _ = request_id;
        Ok(sends)
    }

    fn record_probe_success(&mut self, rtt: Duration) {
        self.snapshot.consecutive_probe_failures = 0;
        self.snapshot.last_rtt_ms = Some(rtt.as_millis().min(u128::from(u64::MAX)) as u64);
        let rtt_ms = self.snapshot.last_rtt_ms.unwrap_or_default();
        if self.rtt_samples_ms.len() >= 128 {
            self.rtt_samples_ms.pop_front();
        }
        self.rtt_samples_ms.push_back(rtt_ms);
        let mut samples: Vec<_> = self.rtt_samples_ms.iter().copied().collect();
        samples.sort_unstable();
        self.snapshot.rtt_p50_ms = samples.get((samples.len() - 1) * 50 / 100).copied();
        self.snapshot.rtt_p95_ms = samples.get((samples.len() - 1) * 95 / 100).copied();
    }

    fn reconnect_delay(&self) -> Duration {
        let attempt = self.snapshot.reconnect_attempts.min(5);
        let multiplier = 1_u32 << attempt;
        self.config
            .reconnect_initial_backoff
            .saturating_mul(multiplier)
            .min(self.config.reconnect_max_backoff)
    }

    fn ensure_safe_payload(&self, message: &RelayMessage) -> Result<(), ClientError> {
        let payload_size = match message {
            RelayMessage::DiscoveryRequest { payload, .. }
            | RelayMessage::DiscoveryToHost { payload, .. }
            | RelayMessage::DiscoveryResponse { payload, .. }
            | RelayMessage::DiscoveryToClient { payload, .. }
            | RelayMessage::GameplayPacket { payload, .. }
            | RelayMessage::GameplayToPeer { payload, .. } => payload.len(),
            RelayMessage::RelayProbe { .. } | RelayMessage::RelayProbeAck { .. } => 0,
        };
        if payload_size > self.config.safe_payload_size {
            return Err(ClientError::RelayPayloadTooLarge {
                size: payload_size,
                limit: self.config.safe_payload_size,
            });
        }
        Ok(())
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
    pub connection_epoch: u64,
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
    pub sequence_duplicates: u64,
    pub rate_limited_packets: u64,
    pub oversized_packets: u64,
    pub probe_successes: u64,
    pub probe_failures: u64,
    pub last_probe_at_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_peers: usize,
    pub suspended_peers: usize,
    pub active_rooms: usize,
    pub active_hosts: usize,
    pub active_gameplay_sessions: usize,
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
struct ResumeRoomRequest {
    peer_id: PeerId,
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
    use civ6_lan_protocol::{relay::QUIC_DATAGRAM_PATH_ID, Civ6UdpPort, GameplaySessionId};
    use std::{
        collections::VecDeque,
        net::Ipv4Addr,
        sync::{Arc, Mutex},
    };

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

    #[tokio::test]
    async fn relay_session_sequences_probes_and_records_rtt() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = RelayClient::bind("127.0.0.1:0".parse().unwrap(), server_addr)
            .await
            .unwrap();
        let mut session = RelaySession::new(client, 7);
        session.mark_authenticated(7);
        session.mark_room_joined();
        let server_task = tokio::spawn(async move {
            let mut buffer = [0u8; MAX_RELAY_DATAGRAM_SIZE];
            let (length, source) = server.recv_from(&mut buffer).await.unwrap();
            let envelope = RelayEnvelope::decode(&buffer[..length]).unwrap();
            assert_eq!(envelope.meta.sequence, 1);
            assert_eq!(envelope.meta.connection_epoch, 7);
            let ack = RelayEnvelope::new(
                envelope.meta,
                RelayMessage::RelayProbeAck {
                    request_id: match envelope.message {
                        RelayMessage::RelayProbe { request_id } => request_id,
                        other => panic!("unexpected probe: {other:?}"),
                    },
                },
            )
            .encode()
            .unwrap();
            server.send_to(&ack, source).await.unwrap();
        });
        session.probe_once().await.unwrap();
        server_task.await.unwrap();
        let snapshot = session.snapshot();
        assert_eq!(snapshot.state, RelayConnectionState::RoomJoined);
        assert!(snapshot.last_rtt_ms.is_some());
        assert_eq!(session.next_envelope_meta().sequence, 2);
    }

    #[test]
    fn relay_session_backoff_matches_the_reconnect_contract() {
        let client = RelayClient::from_transport(TestTransport);
        let mut session = RelaySession::new(client, 1);
        assert_eq!(
            session.reconnect_delay_for_next_attempt(),
            Duration::from_millis(250)
        );
        session.snapshot.reconnect_attempts = 4;
        assert_eq!(
            session.reconnect_delay_for_next_attempt(),
            Duration::from_secs(4)
        );
        session.snapshot.reconnect_attempts = 8;
        assert_eq!(
            session.reconnect_delay_for_next_attempt(),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn relay_session_can_label_a_quic_fallback_without_changing_the_envelope() {
        let client = RelayClient::from_transport(TestTransport);
        let mut session = RelaySession::with_config(
            client,
            1,
            RelaySessionConfig {
                path_id: Some(QUIC_DATAGRAM_PATH_ID),
                ..RelaySessionConfig::default()
            },
        );
        let meta = session.next_envelope_meta();
        assert_eq!(meta.path_id, Some(QUIC_DATAGRAM_PATH_ID));
        assert_eq!(
            RelayTransportPath::from_id(meta.path_id.unwrap()),
            Some(RelayTransportPath::QuicDatagram)
        );
    }

    #[tokio::test]
    async fn simulated_loss_delay_and_disconnect_recover_without_gameplay_replay() {
        let controller = FaultController::default();
        controller.set_delay(Duration::from_millis(2));
        let client = RelayClient::from_transport(SimulatedTransport {
            controller: controller.clone(),
        });
        let config = RelaySessionConfig {
            probe_timeout: Duration::from_millis(12),
            ..RelaySessionConfig::default()
        };
        let mut session = RelaySession::with_config(client, 1, config);
        session.mark_authenticated(1);
        session.mark_room_joined();

        controller.drop_next(3);
        for _ in 0..3 {
            assert!(session.probe_once().await.is_err());
            session.record_probe_failure();
        }
        assert_eq!(session.snapshot().state, RelayConnectionState::Degraded);
        assert_eq!(session.snapshot().consecutive_probe_failures, 3);

        controller.set_offline(true);
        assert!(session.probe_once().await.is_err());
        controller.set_offline(false);
        assert!(session.probe_once().await.unwrap() >= Duration::from_millis(2));
        assert_eq!(session.snapshot().consecutive_probe_failures, 0);

        let sent = controller.sent_packets();
        assert_eq!(sent, 5);
    }

    struct TestTransport;

    #[async_trait]
    impl DatagramTransport for TestTransport {
        async fn send(&self, _packet: &[u8]) -> io::Result<()> {
            Ok(())
        }

        async fn receive(&self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "test transport"))
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("127.0.0.1:0".parse().unwrap())
        }
    }

    #[derive(Clone, Default)]
    struct FaultController {
        state: Arc<Mutex<FaultState>>,
    }

    #[derive(Default)]
    struct FaultState {
        drop_remaining: usize,
        offline: bool,
        delay: Duration,
        sent_packets: usize,
        received_packets: VecDeque<Vec<u8>>,
    }

    impl FaultController {
        fn drop_next(&self, count: usize) {
            self.state.lock().unwrap().drop_remaining = count;
        }

        fn set_offline(&self, offline: bool) {
            self.state.lock().unwrap().offline = offline;
        }

        fn set_delay(&self, delay: Duration) {
            self.state.lock().unwrap().delay = delay;
        }

        fn sent_packets(&self) -> usize {
            self.state.lock().unwrap().sent_packets
        }
    }

    struct SimulatedTransport {
        controller: FaultController,
    }

    #[async_trait]
    impl DatagramTransport for SimulatedTransport {
        async fn send(&self, packet: &[u8]) -> io::Result<()> {
            let envelope = RelayEnvelope::decode(packet)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            let (delay, should_drop, offline) = {
                let mut state = self.controller.state.lock().unwrap();
                state.sent_packets += 1;
                let should_drop = state.drop_remaining > 0;
                if should_drop {
                    state.drop_remaining -= 1;
                }
                (state.delay, should_drop, state.offline)
            };
            if offline {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "simulated network disconnect",
                ));
            }
            if should_drop {
                return Ok(());
            }
            tokio::time::sleep(delay).await;
            if let RelayMessage::RelayProbe { request_id } = envelope.message {
                let ack =
                    RelayEnvelope::new(envelope.meta, RelayMessage::RelayProbeAck { request_id })
                        .encode()
                        .map_err(|error| {
                            io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                        })?;
                self.controller
                    .state
                    .lock()
                    .unwrap()
                    .received_packets
                    .push_back(ack);
            }
            Ok(())
        }

        async fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
            loop {
                if let Some(packet) = self
                    .controller
                    .state
                    .lock()
                    .unwrap()
                    .received_packets
                    .pop_front()
                {
                    buffer[..packet.len()].copy_from_slice(&packet);
                    return Ok(packet.len());
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("127.0.0.1:0".parse().unwrap())
        }
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
