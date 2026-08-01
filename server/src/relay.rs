//! UDP relay transport for the desktop network adapters.
//!
//! The packet sent over WireGuard is a small, versioned envelope rather than
//! a raw `255.255.255.255` packet. That is intentional: the server must carry
//! the request/session identity across the L3 tunnel so that replies from
//! multiple Civ VI hosts remain distinguishable.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use civ6_lan_protocol::{
    relay::{RelayCodecError, RelayMessage, MAX_RELAY_DATAGRAM_SIZE},
    HostSessionId, PeerId, VirtualIp,
};
use civ6_lan_router::{RelayAction, RelayIngress, RoomRouter, RouterError};
use thiserror::Error;
use tokio::net::UdpSocket;

use crate::state::AppState;

pub const DEFAULT_RELAY_PORT: u16 = 32_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundDatagram {
    pub destination: SocketAddr,
    pub message: RelayMessage,
}

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("malformed relay packet: {0}")]
    InvalidPacket(String),
    #[error(transparent)]
    Codec(#[from] RelayCodecError),
    #[error("relay source {0} is not an IPv4 WireGuard peer")]
    InvalidSource(SocketAddr),
    #[error("relay source virtual IP {0} is not registered")]
    UnknownSource(Ipv4Addr),
    #[error("host session {host_session_id} is not owned by source peer {source_peer_id}")]
    SourcePeerMismatch {
        host_session_id: HostSessionId,
        source_peer_id: PeerId,
    },
    #[error(transparent)]
    Router(#[from] RouterError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct RelayDispatcher {
    state: AppState,
    relay_port: u16,
}

impl RelayDispatcher {
    pub fn new(state: AppState, relay_port: u16) -> Self {
        Self { state, relay_port }
    }

    pub async fn handle_datagram(
        &self,
        source: SocketAddr,
        packet: &[u8],
    ) -> Result<Vec<OutboundDatagram>, RelayError> {
        let message = RelayMessage::decode(packet)?;
        let source_ip = match source.ip() {
            IpAddr::V4(value) => value,
            IpAddr::V6(_) => return Err(RelayError::InvalidSource(source)),
        };

        let mut router = self.state.router.write().await;
        let source_peer_id = router
            .peer_for_virtual_ip(VirtualIp::new(source_ip))
            .ok_or(RelayError::UnknownSource(source_ip))?;
        let message = match message {
            RelayMessage::RelayProbe { request_id } => {
                return Ok(vec![OutboundDatagram {
                    destination: source,
                    message: RelayMessage::RelayProbeAck { request_id },
                }]);
            }
            other => other,
        };
        let now = Instant::now();
        let action = match message {
            RelayMessage::DiscoveryRequest {
                request_id,
                destination_port,
                payload,
            } => router.dispatch(RelayIngress::Discovery {
                source_peer_id,
                request_id,
                destination_port,
                payload,
                now,
            })?,
            RelayMessage::DiscoveryResponse {
                request_id,
                host_session_id,
                source_port,
                payload,
            } => {
                let host = router.host_snapshot(host_session_id)?;
                if host.peer_id != source_peer_id {
                    return Err(RelayError::SourcePeerMismatch {
                        host_session_id,
                        source_peer_id,
                    });
                }
                router.dispatch(RelayIngress::DiscoveryResponse {
                    host_session_id,
                    request_id,
                    source_port,
                    payload,
                    now,
                })?
            }
            RelayMessage::GameplayPacket {
                session_id,
                source_port,
                payload,
            } => router.dispatch(RelayIngress::Gameplay {
                session_id,
                source_peer_id,
                source_port,
                payload,
                now,
            })?,
            RelayMessage::DiscoveryToHost { .. }
            | RelayMessage::DiscoveryToClient { .. }
            | RelayMessage::GameplayToPeer { .. }
            | RelayMessage::RelayProbe { .. }
            | RelayMessage::RelayProbeAck { .. } => {
                return Err(RelayError::InvalidPacket(
                    "server received an outbound relay envelope".to_owned(),
                ));
            }
        };

        self.action_to_datagrams(&router, action)
    }

    fn action_to_datagrams(
        &self,
        router: &RoomRouter,
        action: RelayAction,
    ) -> Result<Vec<OutboundDatagram>, RelayError> {
        match action {
            RelayAction::DiscoveryFanout {
                request_id,
                source_peer_id,
                destination_port,
                targets,
                payload,
            } => {
                let source_virtual_ip = router
                    .peer_virtual_ip(source_peer_id)
                    .ok_or(RelayError::UnknownSource(Ipv4Addr::UNSPECIFIED))?;
                Ok(targets
                    .into_iter()
                    .map(|target| OutboundDatagram {
                        destination: relay_destination(target.host_virtual_ip, self.relay_port),
                        message: RelayMessage::DiscoveryToHost {
                            request_id,
                            source_virtual_ip,
                            destination_port,
                            payload: payload.clone(),
                        },
                    })
                    .collect())
            }
            RelayAction::DiscoveryUnicast {
                request_id,
                destination_peer_id,
                source_virtual_ip,
                source_port,
                payload,
            } => {
                let destination = router
                    .peer_virtual_ip(destination_peer_id)
                    .ok_or(RelayError::UnknownSource(Ipv4Addr::UNSPECIFIED))?;
                Ok(vec![OutboundDatagram {
                    destination: relay_destination(destination, self.relay_port),
                    message: RelayMessage::DiscoveryToClient {
                        request_id,
                        host_virtual_ip: source_virtual_ip,
                        source_port,
                        payload,
                    },
                }])
            }
            RelayAction::GameplayUnicast {
                route,
                destination_port,
                payload,
            } => {
                let source_virtual_ip = router
                    .peer_virtual_ip(route.source_peer_id)
                    .ok_or(RelayError::UnknownSource(Ipv4Addr::UNSPECIFIED))?;
                Ok(vec![OutboundDatagram {
                    destination: relay_destination(route.destination_virtual_ip, self.relay_port),
                    message: RelayMessage::GameplayToPeer {
                        session_id: route.session_id,
                        source_virtual_ip,
                        destination_port,
                        payload,
                    },
                }])
            }
        }
    }
}

pub struct RelayServer {
    socket: UdpSocket,
    dispatcher: RelayDispatcher,
}

impl RelayServer {
    pub async fn bind(
        bind: SocketAddr,
        state: AppState,
        relay_port: u16,
    ) -> Result<Self, io::Error> {
        Ok(Self {
            socket: UdpSocket::bind(bind).await?,
            dispatcher: RelayDispatcher::new(state, relay_port),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.socket.local_addr()
    }

    pub async fn run(self) -> Result<(), RelayError> {
        let mut socket_buffer = vec![0u8; MAX_RELAY_DATAGRAM_SIZE];
        let mut expiration = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                    received = self.socket.recv_from(&mut socket_buffer) => {
                        let (length, source) = received?;
                        self.dispatcher.state.metrics.record_received(&socket_buffer[..length]);
                        match self.dispatcher.handle_datagram(source, &socket_buffer[..length]).await {
                            Ok(outbound) => {
                                for datagram in outbound {
                                    let packet = datagram.message.encode()?;
                                    self.socket.send_to(&packet, datagram.destination).await?;
                                    self.dispatcher.state.metrics.record_sent(packet.len());
                                }
                            }
                        Err(error) => {
                            let authentication_failure = matches!(
                                &error,
                                RelayError::UnknownSource(_)
                                    | RelayError::SourcePeerMismatch { .. }
                                    | RelayError::Router(RouterError::UnauthorizedGameplayPeer(..))
                            );
                            self.dispatcher.state.metrics.record_drop(authentication_failure);
                            tracing::debug!(%source, %error, "dropping invalid relay datagram");
                        }
                    }
                }
                _ = expiration.tick() => {
                    self.dispatcher.state.router.write().await.expire(Instant::now());
                }
            }
        }
    }
}

fn relay_destination(ip: VirtualIp, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(ip.address()), port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use civ6_lan_protocol::{
        relay::RelayCodecError, Civ6UdpPort, DiscoveryRequestId, GameplaySessionId, RoomCode,
        RoomId,
    };
    use std::net::Ipv4Addr;

    fn ip(value: &str) -> VirtualIp {
        VirtualIp::new(value.parse::<Ipv4Addr>().unwrap())
    }

    async fn setup() -> (AppState, RoomId, PeerId, PeerId, HostSessionId) {
        let state = AppState::new("test-bearer-token");
        let mut router = state.router.write().await;
        let room_id = router
            .create_room(RoomCode::parse("RELAY2").unwrap())
            .unwrap();
        let client_peer_id = PeerId::new();
        let host_peer_id = PeerId::new();
        router
            .join_room(room_id, client_peer_id, ip("10.240.0.2"))
            .unwrap();
        router
            .join_room(room_id, host_peer_id, ip("10.240.0.3"))
            .unwrap();
        let host_session_id = router
            .register_host(room_id, host_peer_id, Instant::now())
            .unwrap();
        drop(router);
        (
            state,
            room_id,
            client_peer_id,
            host_peer_id,
            host_session_id,
        )
    }

    #[test]
    fn envelope_round_trips_and_rejects_trailing_bytes() {
        let message = RelayMessage::GameplayPacket {
            session_id: GameplaySessionId::new(),
            source_port: Civ6UdpPort(62_056),
            payload: vec![1, 2, 3],
        };
        let mut packet = message.encode().unwrap();
        assert_eq!(RelayMessage::decode(&packet).unwrap(), message);
        packet.push(4);
        assert!(matches!(
            RelayMessage::decode(&packet),
            Err(RelayCodecError::InvalidPacket(_))
        ));
    }

    #[test]
    fn server_envelope_matches_shared_client_codec() {
        let request_id = DiscoveryRequestId::new();
        let server_packet = RelayMessage::RelayProbe { request_id }.encode().unwrap();
        let client_packet = civ6_lan_protocol::relay::RelayMessage::RelayProbe { request_id }
            .encode()
            .unwrap();
        assert_eq!(server_packet, client_packet);
    }

    #[tokio::test]
    async fn discovery_keeps_host_identity_in_both_directions() {
        let (state, _, _, _, host_session_id) = setup().await;
        let dispatcher = RelayDispatcher::new(state, DEFAULT_RELAY_PORT);
        let request_id = DiscoveryRequestId::new();
        let request = RelayMessage::DiscoveryRequest {
            request_id,
            destination_port: Civ6UdpPort(62_900),
            payload: vec![9, 8, 7],
        };
        let outbound = dispatcher
            .handle_datagram(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 240, 0, 2)), DEFAULT_RELAY_PORT),
                &request.encode().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(outbound.len(), 1);
        assert_eq!(
            outbound[0].destination.ip(),
            IpAddr::V4(Ipv4Addr::new(10, 240, 0, 3))
        );
        assert_eq!(
            outbound[0].message,
            RelayMessage::DiscoveryToHost {
                request_id,
                source_virtual_ip: ip("10.240.0.2"),
                destination_port: Civ6UdpPort(62_900),
                payload: vec![9, 8, 7],
            }
        );

        let response = RelayMessage::DiscoveryResponse {
            request_id,
            host_session_id,
            source_port: Civ6UdpPort(62_900),
            payload: vec![1, 2, 3, 4],
        };
        let outbound = dispatcher
            .handle_datagram(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 240, 0, 3)), DEFAULT_RELAY_PORT),
                &response.encode().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(outbound.len(), 1);
        assert_eq!(
            outbound[0].destination.ip(),
            IpAddr::V4(Ipv4Addr::new(10, 240, 0, 2))
        );
        assert_eq!(
            outbound[0].message,
            RelayMessage::DiscoveryToClient {
                request_id,
                host_virtual_ip: ip("10.240.0.3"),
                source_port: Civ6UdpPort(62_900),
                payload: vec![1, 2, 3, 4],
            }
        );
    }

    #[tokio::test]
    async fn relay_probe_returns_an_ack_to_the_registered_peer() {
        let (state, _, _, _, _) = setup().await;
        let dispatcher = RelayDispatcher::new(state, DEFAULT_RELAY_PORT);
        let request_id = DiscoveryRequestId::new();
        let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 240, 0, 2)), DEFAULT_RELAY_PORT);
        let outbound = dispatcher
            .handle_datagram(
                source,
                &RelayMessage::RelayProbe { request_id }.encode().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].destination, source);
        assert_eq!(
            outbound[0].message,
            RelayMessage::RelayProbeAck { request_id }
        );
    }

    #[tokio::test]
    async fn client_core_probe_exchanges_with_the_live_server_socket() {
        let state = AppState::new("test-bearer-token");
        {
            let mut router = state.router.write().await;
            let room_id = router
                .create_room(RoomCode::parse("RELAY2").unwrap())
                .unwrap();
            router
                .join_room(room_id, PeerId::new(), VirtualIp::new(Ipv4Addr::LOCALHOST))
                .unwrap();
        }

        let server = RelayServer::bind("127.0.0.1:0".parse().unwrap(), state, DEFAULT_RELAY_PORT)
            .await
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let server_task = tokio::spawn(server.run());
        let client =
            civ6_lan_client_core::RelayClient::bind("127.0.0.1:0".parse().unwrap(), server_addr)
                .await
                .unwrap();

        client.probe(Duration::from_secs(1)).await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn two_live_client_sockets_exchange_discovery_and_gameplay() {
        let state = AppState::new("test-bearer-token");
        let (room_id, client_peer_id, host_peer_id, host_session_id) = {
            let mut router = state.router.write().await;
            let room_id = router
                .create_room(RoomCode::parse("LAVA42").unwrap())
                .unwrap();
            let client_peer_id = PeerId::new();
            let host_peer_id = PeerId::new();
            router
                .join_room(room_id, client_peer_id, ip("127.0.0.2"))
                .unwrap();
            router
                .join_room(room_id, host_peer_id, ip("127.0.0.3"))
                .unwrap();
            let host_session_id = router
                .register_host(room_id, host_peer_id, Instant::now())
                .unwrap();
            (room_id, client_peer_id, host_peer_id, host_session_id)
        };
        let gameplay_session_id = {
            let mut router = state.router.write().await;
            router
                .select_host(room_id, client_peer_id, host_session_id, Instant::now())
                .unwrap()
        };

        let server = RelayServer::bind("127.0.0.1:0".parse().unwrap(), state, DEFAULT_RELAY_PORT)
            .await
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let server_task = tokio::spawn(server.run());
        let client = civ6_lan_client_core::RelayClient::bind(
            "127.0.0.2:32000".parse().unwrap(),
            server_addr,
        )
        .await
        .unwrap();
        let host = civ6_lan_client_core::RelayClient::bind(
            "127.0.0.3:32000".parse().unwrap(),
            server_addr,
        )
        .await
        .unwrap();

        let request_id = DiscoveryRequestId::new();
        client
            .send(&RelayMessage::DiscoveryRequest {
                request_id,
                destination_port: Civ6UdpPort(62_900),
                payload: vec![1, 2, 3],
            })
            .await
            .unwrap();
        let to_host = tokio::time::timeout(Duration::from_secs(1), host.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            to_host,
            RelayMessage::DiscoveryToHost {
                request_id,
                source_virtual_ip: ip("127.0.0.2"),
                destination_port: Civ6UdpPort(62_900),
                payload: vec![1, 2, 3],
            }
        );

        host.send(&RelayMessage::DiscoveryResponse {
            request_id,
            host_session_id,
            source_port: Civ6UdpPort(62_900),
            payload: vec![4, 5, 6],
        })
        .await
        .unwrap();
        let to_client = tokio::time::timeout(Duration::from_secs(1), client.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            to_client,
            RelayMessage::DiscoveryToClient {
                request_id,
                host_virtual_ip: ip("127.0.0.3"),
                source_port: Civ6UdpPort(62_900),
                payload: vec![4, 5, 6],
            }
        );

        client
            .send(&RelayMessage::GameplayPacket {
                session_id: gameplay_session_id,
                source_port: Civ6UdpPort(62_056),
                payload: vec![7, 8],
            })
            .await
            .unwrap();
        let gameplay_to_host = tokio::time::timeout(Duration::from_secs(1), host.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            gameplay_to_host,
            RelayMessage::GameplayToPeer {
                session_id: gameplay_session_id,
                source_virtual_ip: ip("127.0.0.2"),
                destination_port: Civ6UdpPort(62_056),
                payload: vec![7, 8],
            }
        );

        host.send(&RelayMessage::GameplayPacket {
            session_id: gameplay_session_id,
            source_port: Civ6UdpPort(62_056),
            payload: vec![9, 10],
        })
        .await
        .unwrap();
        let gameplay_to_client = tokio::time::timeout(Duration::from_secs(1), client.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            gameplay_to_client,
            RelayMessage::GameplayToPeer {
                session_id: gameplay_session_id,
                source_virtual_ip: ip("127.0.0.3"),
                destination_port: Civ6UdpPort(62_056),
                payload: vec![9, 10],
            }
        );
        assert_ne!(client_peer_id, host_peer_id);
        server_task.abort();
    }

    #[tokio::test]
    async fn gameplay_is_bidirectional_and_rejects_spoofed_host_responses() {
        let (state, room_id, client_peer_id, host_peer_id, host_session_id) = setup().await;
        let gameplay_session_id = {
            let mut router = state.router.write().await;
            router
                .select_host(room_id, client_peer_id, host_session_id, Instant::now())
                .unwrap()
        };
        let dispatcher = RelayDispatcher::new(state, DEFAULT_RELAY_PORT);
        let packet = RelayMessage::GameplayPacket {
            session_id: gameplay_session_id,
            source_port: Civ6UdpPort(62_056),
            payload: vec![5, 6],
        };
        let outbound = dispatcher
            .handle_datagram(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 240, 0, 2)), DEFAULT_RELAY_PORT),
                &packet.encode().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            outbound[0].destination.ip(),
            IpAddr::V4(Ipv4Addr::new(10, 240, 0, 3))
        );
        assert_eq!(
            outbound[0].message,
            RelayMessage::GameplayToPeer {
                session_id: gameplay_session_id,
                source_virtual_ip: ip("10.240.0.2"),
                destination_port: Civ6UdpPort(62_056),
                payload: vec![5, 6],
            }
        );

        let spoofed = RelayMessage::DiscoveryResponse {
            request_id: DiscoveryRequestId::new(),
            host_session_id,
            source_port: Civ6UdpPort(62_900),
            payload: vec![1],
        };
        let error = dispatcher
            .handle_datagram(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 240, 0, 2)), DEFAULT_RELAY_PORT),
                &spoofed.encode().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            RelayError::SourcePeerMismatch {
                host_session_id: id,
                source_peer_id: peer,
            } if id == host_session_id && peer == client_peer_id
        ));
        assert_ne!(client_peer_id, host_peer_id);
    }
}
