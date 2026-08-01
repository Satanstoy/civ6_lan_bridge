//! In-memory room/session routing model.
//!
//! The relay data plane will use this state machine to decide where a Civ VI
//! datagram may go. It intentionally does not perform I/O or inspect Civ VI
//! payloads. That keeps room isolation and expiry behavior testable without a
//! live WireGuard interface.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use civ6_lan_protocol::{
    is_discovery_port, Civ6UdpPort, DiscoveryRequestId, GameplaySessionId, HostSessionId, PeerId,
    RoomCode, RoomId, VirtualIp, GAMEPLAY_PORT, MAX_CIV6_DATAGRAM_SIZE,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub struct RouterConfig {
    pub host_ttl: Duration,
    pub discovery_ttl: Duration,
    pub gameplay_idle_ttl: Duration,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            host_ttl: Duration::from_secs(15),
            discovery_ttl: Duration::from_secs(5),
            gameplay_idle_ttl: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RouterError {
    #[error("room {0} was not found")]
    RoomNotFound(RoomId),
    #[error("room code {0} is already in use")]
    DuplicateRoomCode(RoomCode),
    #[error("room {0} already exists")]
    DuplicateRoomId(RoomId),
    #[error("room {0} is not empty")]
    RoomNotEmpty(RoomId),
    #[error("peer {0} is already a member of a room")]
    PeerAlreadyInRoom(PeerId),
    #[error("peer {0} is not a member of room {1}")]
    PeerNotInRoom(PeerId, RoomId),
    #[error("peer {0} is not registered")]
    PeerNotFound(PeerId),
    #[error("virtual IP {0} is already in use")]
    VirtualIpInUse(VirtualIp),
    #[error("host session {0} was not found")]
    HostNotFound(HostSessionId),
    #[error("host session {0} does not belong to room {1}")]
    HostNotInRoom(HostSessionId, RoomId),
    #[error("host session {0} is expired")]
    HostExpired(HostSessionId),
    #[error("discovery request {0} was not found")]
    DiscoveryNotFound(DiscoveryRequestId),
    #[error("discovery request {0} is expired")]
    DiscoveryExpired(DiscoveryRequestId),
    #[error("gameplay session {0} was not found")]
    GameplayNotFound(GameplaySessionId),
    #[error("peer {0} is not authorized for gameplay session {1}")]
    UnauthorizedGameplayPeer(PeerId, GameplaySessionId),
    #[error("UDP port {0} is not a Civ VI discovery port")]
    InvalidDiscoveryPort(u16),
    #[error("UDP port {0} is not the Civ VI gameplay port")]
    InvalidGameplayPort(u16),
    #[error("UDP datagram size {0} exceeds the configured limit")]
    DatagramTooLarge(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryTarget {
    pub host_session_id: HostSessionId,
    pub host_peer_id: PeerId,
    pub host_virtual_ip: VirtualIp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomSnapshot {
    pub room_id: RoomId,
    pub code: RoomCode,
    pub member_count: usize,
    pub host_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostSnapshot {
    pub host_session_id: HostSessionId,
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub virtual_ip: VirtualIp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameplaySnapshot {
    pub session_id: GameplaySessionId,
    pub room_id: RoomId,
    pub client_peer_id: PeerId,
    pub host_peer_id: PeerId,
    pub client_virtual_ip: VirtualIp,
    pub host_virtual_ip: VirtualIp,
}

#[derive(Debug)]
pub enum RelayIngress {
    Discovery {
        source_peer_id: PeerId,
        request_id: DiscoveryRequestId,
        destination_port: Civ6UdpPort,
        payload: Vec<u8>,
        now: Instant,
    },
    DiscoveryResponse {
        host_session_id: HostSessionId,
        request_id: DiscoveryRequestId,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
        now: Instant,
    },
    Gameplay {
        session_id: GameplaySessionId,
        source_peer_id: PeerId,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
        now: Instant,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub enum RelayAction {
    DiscoveryFanout {
        request_id: DiscoveryRequestId,
        source_peer_id: PeerId,
        destination_port: Civ6UdpPort,
        targets: Vec<DiscoveryTarget>,
        payload: Vec<u8>,
    },
    DiscoveryUnicast {
        request_id: DiscoveryRequestId,
        destination_peer_id: PeerId,
        source_virtual_ip: VirtualIp,
        source_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
    GameplayUnicast {
        route: GameplayRoute,
        destination_port: Civ6UdpPort,
        payload: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameplayRoute {
    pub session_id: GameplaySessionId,
    pub room_id: RoomId,
    pub source_peer_id: PeerId,
    pub destination_peer_id: PeerId,
    pub destination_virtual_ip: VirtualIp,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExpirationReport {
    pub expired_hosts: usize,
    pub expired_discoveries: usize,
    pub expired_gameplay_sessions: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RouterStats {
    pub active_rooms: usize,
    pub active_peers: usize,
    pub active_hosts: usize,
    pub active_gameplay_sessions: usize,
}

#[derive(Clone, Debug)]
struct RoomState {
    code: RoomCode,
    members: HashSet<PeerId>,
    host_sessions: HashSet<HostSessionId>,
}

#[derive(Clone, Copy, Debug)]
struct PeerState {
    room_id: RoomId,
    virtual_ip: VirtualIp,
}

#[derive(Clone, Copy, Debug)]
struct HostState {
    room_id: RoomId,
    peer_id: PeerId,
    virtual_ip: VirtualIp,
    last_heartbeat: Instant,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct DiscoveryState {
    room_id: RoomId,
    client_peer_id: PeerId,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct GameplayState {
    room_id: RoomId,
    client_peer_id: PeerId,
    host_peer_id: PeerId,
    host_virtual_ip: VirtualIp,
    client_virtual_ip: VirtualIp,
    last_activity: Instant,
    expires_at: Instant,
}

/// Authoritative in-memory routing state for one relay process.
#[derive(Debug)]
pub struct RoomRouter {
    config: RouterConfig,
    rooms: HashMap<RoomId, RoomState>,
    rooms_by_code: HashMap<RoomCode, RoomId>,
    peers: HashMap<PeerId, PeerState>,
    virtual_ips: HashMap<VirtualIp, PeerId>,
    hosts: HashMap<HostSessionId, HostState>,
    host_by_peer: HashMap<(RoomId, PeerId), HostSessionId>,
    discoveries: HashMap<DiscoveryRequestId, DiscoveryState>,
    gameplay: HashMap<GameplaySessionId, GameplayState>,
}

impl RoomRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self {
            config,
            rooms: HashMap::new(),
            rooms_by_code: HashMap::new(),
            peers: HashMap::new(),
            virtual_ips: HashMap::new(),
            hosts: HashMap::new(),
            host_by_peer: HashMap::new(),
            discoveries: HashMap::new(),
            gameplay: HashMap::new(),
        }
    }

    pub fn dispatch(&mut self, ingress: RelayIngress) -> Result<RelayAction, RouterError> {
        match ingress {
            RelayIngress::Discovery {
                source_peer_id,
                request_id,
                destination_port,
                payload,
                now,
            } => {
                self.validate_payload(&payload)?;
                let room_id = self
                    .room_for_peer(source_peer_id)
                    .ok_or(RouterError::PeerNotFound(source_peer_id))?;
                let targets = self.begin_discovery(
                    room_id,
                    source_peer_id,
                    request_id,
                    destination_port,
                    now,
                )?;
                Ok(RelayAction::DiscoveryFanout {
                    request_id,
                    source_peer_id,
                    destination_port,
                    targets,
                    payload,
                })
            }
            RelayIngress::DiscoveryResponse {
                host_session_id,
                request_id,
                source_port,
                payload,
                now,
            } => {
                self.validate_payload(&payload)?;
                if !is_discovery_port(source_port.0) {
                    return Err(RouterError::InvalidDiscoveryPort(source_port.0));
                }
                let destination_peer_id =
                    self.route_discovery_response(request_id, host_session_id, now)?;
                let source_virtual_ip = self
                    .hosts
                    .get(&host_session_id)
                    .ok_or(RouterError::HostNotFound(host_session_id))?
                    .virtual_ip;
                Ok(RelayAction::DiscoveryUnicast {
                    request_id,
                    destination_peer_id,
                    source_virtual_ip,
                    source_port,
                    payload,
                })
            }
            RelayIngress::Gameplay {
                session_id,
                source_peer_id,
                source_port,
                payload,
                now,
            } => {
                self.validate_payload(&payload)?;
                if source_port.0 != GAMEPLAY_PORT {
                    return Err(RouterError::InvalidGameplayPort(source_port.0));
                }
                let route = self.route_gameplay_datagram(session_id, source_peer_id, now)?;
                Ok(RelayAction::GameplayUnicast {
                    route,
                    destination_port: source_port,
                    payload,
                })
            }
        }
    }

    pub fn create_room(&mut self, code: RoomCode) -> Result<RoomId, RouterError> {
        let room_id = RoomId::new();
        self.create_room_with_id(room_id, code)?;
        Ok(room_id)
    }

    pub fn create_room_with_id(
        &mut self,
        room_id: RoomId,
        code: RoomCode,
    ) -> Result<(), RouterError> {
        if self.rooms_by_code.contains_key(&code) {
            return Err(RouterError::DuplicateRoomCode(code));
        }
        if self.rooms.contains_key(&room_id) {
            return Err(RouterError::DuplicateRoomId(room_id));
        }
        self.rooms.insert(
            room_id,
            RoomState {
                code: code.clone(),
                members: HashSet::new(),
                host_sessions: HashSet::new(),
            },
        );
        self.rooms_by_code.insert(code, room_id);
        Ok(())
    }

    pub fn remove_empty_room(&mut self, room_id: RoomId) -> Result<(), RouterError> {
        let room = self
            .rooms
            .get(&room_id)
            .ok_or(RouterError::RoomNotFound(room_id))?;
        if !room.members.is_empty() || !room.host_sessions.is_empty() {
            return Err(RouterError::RoomNotEmpty(room_id));
        }
        let code = room.code.clone();
        self.rooms.remove(&room_id);
        self.rooms_by_code.remove(&code);
        Ok(())
    }

    pub fn room_id_for_code(&self, code: &RoomCode) -> Option<RoomId> {
        self.rooms_by_code.get(code).copied()
    }

    pub fn room_code(&self, room_id: RoomId) -> Result<&RoomCode, RouterError> {
        self.rooms
            .get(&room_id)
            .map(|room| &room.code)
            .ok_or(RouterError::RoomNotFound(room_id))
    }

    pub fn room_snapshot(&self, room_id: RoomId) -> Result<RoomSnapshot, RouterError> {
        let room = self
            .rooms
            .get(&room_id)
            .ok_or(RouterError::RoomNotFound(room_id))?;
        Ok(RoomSnapshot {
            room_id,
            code: room.code.clone(),
            member_count: room.members.len(),
            host_count: room.host_sessions.len(),
        })
    }

    pub fn room_for_peer(&self, peer_id: PeerId) -> Option<RoomId> {
        self.peers.get(&peer_id).map(|peer| peer.room_id)
    }

    pub fn peer_virtual_ip(&self, peer_id: PeerId) -> Option<VirtualIp> {
        self.peers.get(&peer_id).map(|peer| peer.virtual_ip)
    }

    pub fn peer_for_virtual_ip(&self, virtual_ip: VirtualIp) -> Option<PeerId> {
        self.virtual_ips.get(&virtual_ip).copied()
    }

    pub fn is_virtual_ip_available(&self, virtual_ip: VirtualIp) -> bool {
        !self.virtual_ips.contains_key(&virtual_ip)
    }

    pub fn join_room(
        &mut self,
        room_id: RoomId,
        peer_id: PeerId,
        virtual_ip: VirtualIp,
    ) -> Result<(), RouterError> {
        if !self.rooms.contains_key(&room_id) {
            return Err(RouterError::RoomNotFound(room_id));
        }
        if self.peers.contains_key(&peer_id) {
            return Err(RouterError::PeerAlreadyInRoom(peer_id));
        }
        if self.virtual_ips.contains_key(&virtual_ip) {
            return Err(RouterError::VirtualIpInUse(virtual_ip));
        }

        let room = self
            .rooms
            .get_mut(&room_id)
            .expect("room existence was checked above");
        room.members.insert(peer_id);
        self.peers.insert(
            peer_id,
            PeerState {
                room_id,
                virtual_ip,
            },
        );
        self.virtual_ips.insert(virtual_ip, peer_id);
        Ok(())
    }

    pub fn restore_peer(
        &mut self,
        room_id: RoomId,
        peer_id: PeerId,
        virtual_ip: VirtualIp,
    ) -> Result<(), RouterError> {
        self.join_room(room_id, peer_id, virtual_ip)
    }

    pub fn leave_room(&mut self, room_id: RoomId, peer_id: PeerId) -> Result<(), RouterError> {
        let room = self
            .rooms
            .get(&room_id)
            .ok_or(RouterError::RoomNotFound(room_id))?;
        if !room.members.contains(&peer_id) {
            return Err(RouterError::PeerNotInRoom(peer_id, room_id));
        }

        let host_ids: Vec<_> = self
            .hosts
            .iter()
            .filter_map(|(id, host)| {
                (host.room_id == room_id && host.peer_id == peer_id).then_some(*id)
            })
            .collect();
        for host_id in host_ids {
            self.remove_host(host_id);
        }

        let gameplay_ids: Vec<_> = self
            .gameplay
            .iter()
            .filter_map(|(id, session)| {
                (session.room_id == room_id
                    && (session.client_peer_id == peer_id || session.host_peer_id == peer_id))
                    .then_some(*id)
            })
            .collect();
        for session_id in gameplay_ids {
            self.gameplay.remove(&session_id);
        }

        let discovery_ids: Vec<_> = self
            .discoveries
            .iter()
            .filter_map(|(id, discovery)| {
                (discovery.room_id == room_id && discovery.client_peer_id == peer_id).then_some(*id)
            })
            .collect();
        for request_id in discovery_ids {
            self.discoveries.remove(&request_id);
        }

        let room = self
            .rooms
            .get_mut(&room_id)
            .expect("room was checked above");
        room.members.remove(&peer_id);
        let peer = self.peers.remove(&peer_id).expect("peer was checked above");
        self.virtual_ips.remove(&peer.virtual_ip);
        Ok(())
    }

    pub fn register_host(
        &mut self,
        room_id: RoomId,
        peer_id: PeerId,
        now: Instant,
    ) -> Result<HostSessionId, RouterError> {
        let peer = self.member_state(room_id, peer_id)?;
        let host_ttl = self.config.host_ttl;

        if let Some(existing_id) = self.host_by_peer.get(&(room_id, peer_id)).copied() {
            if let Some(existing) = self.hosts.get_mut(&existing_id) {
                if existing.expires_at > now {
                    existing.last_heartbeat = now;
                    existing.expires_at = now + host_ttl;
                    return Ok(existing_id);
                }
            }
            self.remove_host(existing_id);
        }

        let session_id = HostSessionId::new();
        let host = HostState {
            room_id,
            peer_id,
            virtual_ip: peer.virtual_ip,
            last_heartbeat: now,
            expires_at: now + host_ttl,
        };
        self.hosts.insert(session_id, host);
        self.host_by_peer.insert((room_id, peer_id), session_id);
        self.rooms
            .get_mut(&room_id)
            .expect("room was checked above")
            .host_sessions
            .insert(session_id);
        Ok(session_id)
    }

    pub fn heartbeat_host(
        &mut self,
        host_session_id: HostSessionId,
        now: Instant,
    ) -> Result<(), RouterError> {
        let host = self
            .hosts
            .get_mut(&host_session_id)
            .ok_or(RouterError::HostNotFound(host_session_id))?;
        if host.expires_at <= now {
            return Err(RouterError::HostExpired(host_session_id));
        }
        host.last_heartbeat = now;
        host.expires_at = now + self.config.host_ttl;
        Ok(())
    }

    pub fn heartbeat_host_for_room(
        &mut self,
        room_id: RoomId,
        peer_id: PeerId,
        host_session_id: HostSessionId,
        now: Instant,
    ) -> Result<(), RouterError> {
        let host = self
            .hosts
            .get(&host_session_id)
            .copied()
            .ok_or(RouterError::HostNotFound(host_session_id))?;
        if host.room_id != room_id || host.peer_id != peer_id {
            return Err(RouterError::HostNotInRoom(host_session_id, room_id));
        }
        self.heartbeat_host(host_session_id, now)
    }

    pub fn host_snapshot(
        &self,
        host_session_id: HostSessionId,
    ) -> Result<HostSnapshot, RouterError> {
        let host = self
            .hosts
            .get(&host_session_id)
            .copied()
            .ok_or(RouterError::HostNotFound(host_session_id))?;
        Ok(HostSnapshot {
            host_session_id,
            room_id: host.room_id,
            peer_id: host.peer_id,
            virtual_ip: host.virtual_ip,
        })
    }

    pub fn unregister_host(&mut self, host_session_id: HostSessionId) -> Result<(), RouterError> {
        if !self.hosts.contains_key(&host_session_id) {
            return Err(RouterError::HostNotFound(host_session_id));
        }
        self.remove_host(host_session_id);
        Ok(())
    }

    pub fn begin_discovery(
        &mut self,
        room_id: RoomId,
        client_peer_id: PeerId,
        request_id: DiscoveryRequestId,
        destination_port: Civ6UdpPort,
        now: Instant,
    ) -> Result<Vec<DiscoveryTarget>, RouterError> {
        if !is_discovery_port(destination_port.0) {
            return Err(RouterError::InvalidDiscoveryPort(destination_port.0));
        }
        self.member_state(room_id, client_peer_id)?;
        self.discoveries.insert(
            request_id,
            DiscoveryState {
                room_id,
                client_peer_id,
                expires_at: now + self.config.discovery_ttl,
            },
        );

        let targets = self
            .rooms
            .get(&room_id)
            .expect("room was checked above")
            .host_sessions
            .iter()
            .filter_map(|host_id| self.hosts.get(host_id).map(|host| (*host_id, *host)))
            .filter(|(_, host)| host.expires_at > now)
            .map(|(host_session_id, host)| DiscoveryTarget {
                host_session_id,
                host_peer_id: host.peer_id,
                host_virtual_ip: host.virtual_ip,
            })
            .collect();

        Ok(targets)
    }

    pub fn route_discovery_response(
        &self,
        request_id: DiscoveryRequestId,
        host_session_id: HostSessionId,
        now: Instant,
    ) -> Result<PeerId, RouterError> {
        let discovery = self
            .discoveries
            .get(&request_id)
            .ok_or(RouterError::DiscoveryNotFound(request_id))?;
        if discovery.expires_at <= now {
            return Err(RouterError::DiscoveryExpired(request_id));
        }

        let host = self
            .hosts
            .get(&host_session_id)
            .ok_or(RouterError::HostNotFound(host_session_id))?;
        if host.expires_at <= now {
            return Err(RouterError::HostExpired(host_session_id));
        }
        if host.room_id != discovery.room_id {
            return Err(RouterError::HostNotInRoom(
                host_session_id,
                discovery.room_id,
            ));
        }
        Ok(discovery.client_peer_id)
    }

    pub fn select_host(
        &mut self,
        room_id: RoomId,
        client_peer_id: PeerId,
        host_session_id: HostSessionId,
        now: Instant,
    ) -> Result<GameplaySessionId, RouterError> {
        let client = self.member_state(room_id, client_peer_id)?;
        let host = self
            .hosts
            .get(&host_session_id)
            .copied()
            .ok_or(RouterError::HostNotFound(host_session_id))?;
        if host.room_id != room_id {
            return Err(RouterError::HostNotInRoom(host_session_id, room_id));
        }
        if host.expires_at <= now {
            return Err(RouterError::HostExpired(host_session_id));
        }

        let session_id = GameplaySessionId::new();
        self.gameplay.insert(
            session_id,
            GameplayState {
                room_id,
                client_peer_id,
                host_peer_id: host.peer_id,
                host_virtual_ip: host.virtual_ip,
                client_virtual_ip: client.virtual_ip,
                last_activity: now,
                expires_at: now + self.config.gameplay_idle_ttl,
            },
        );
        Ok(session_id)
    }

    pub fn route_gameplay_datagram(
        &mut self,
        session_id: GameplaySessionId,
        source_peer_id: PeerId,
        now: Instant,
    ) -> Result<GameplayRoute, RouterError> {
        let session = self
            .gameplay
            .get_mut(&session_id)
            .ok_or(RouterError::GameplayNotFound(session_id))?;
        if session.expires_at <= now {
            return Err(RouterError::GameplayNotFound(session_id));
        }

        let (destination_peer_id, destination_virtual_ip) =
            if source_peer_id == session.client_peer_id {
                (session.host_peer_id, session.host_virtual_ip)
            } else if source_peer_id == session.host_peer_id {
                (session.client_peer_id, session.client_virtual_ip)
            } else {
                return Err(RouterError::UnauthorizedGameplayPeer(
                    source_peer_id,
                    session_id,
                ));
            };

        session.last_activity = now;
        session.expires_at = now + self.config.gameplay_idle_ttl;
        Ok(GameplayRoute {
            session_id,
            room_id: session.room_id,
            source_peer_id,
            destination_peer_id,
            destination_virtual_ip,
        })
    }

    pub fn gameplay_snapshot(
        &self,
        session_id: GameplaySessionId,
    ) -> Result<GameplaySnapshot, RouterError> {
        let session = self
            .gameplay
            .get(&session_id)
            .copied()
            .ok_or(RouterError::GameplayNotFound(session_id))?;
        Ok(GameplaySnapshot {
            session_id,
            room_id: session.room_id,
            client_peer_id: session.client_peer_id,
            host_peer_id: session.host_peer_id,
            client_virtual_ip: session.client_virtual_ip,
            host_virtual_ip: session.host_virtual_ip,
        })
    }

    pub fn remove_gameplay_session(
        &mut self,
        session_id: GameplaySessionId,
    ) -> Result<(), RouterError> {
        if self.gameplay.remove(&session_id).is_some() {
            Ok(())
        } else {
            Err(RouterError::GameplayNotFound(session_id))
        }
    }

    pub fn expire(&mut self, now: Instant) -> ExpirationReport {
        let expired_hosts: Vec<_> = self
            .hosts
            .iter()
            .filter_map(|(id, host)| (host.expires_at <= now).then_some(*id))
            .collect();
        let expired_host_count = expired_hosts.len();
        for host_id in expired_hosts {
            self.remove_host(host_id);
        }

        let expired_discoveries: Vec<_> = self
            .discoveries
            .iter()
            .filter_map(|(id, discovery)| (discovery.expires_at <= now).then_some(*id))
            .collect();
        let expired_discovery_count = expired_discoveries.len();
        for request_id in expired_discoveries {
            self.discoveries.remove(&request_id);
        }

        let expired_gameplay: Vec<_> = self
            .gameplay
            .iter()
            .filter_map(|(id, session)| (session.expires_at <= now).then_some(*id))
            .collect();
        let expired_gameplay_count = expired_gameplay.len();
        for session_id in expired_gameplay {
            self.gameplay.remove(&session_id);
        }

        ExpirationReport {
            expired_hosts: expired_host_count,
            expired_discoveries: expired_discovery_count,
            expired_gameplay_sessions: expired_gameplay_count,
        }
    }

    pub fn host_count(&self, room_id: RoomId) -> Result<usize, RouterError> {
        Ok(self
            .rooms
            .get(&room_id)
            .ok_or(RouterError::RoomNotFound(room_id))?
            .host_sessions
            .len())
    }

    pub fn stats(&self) -> RouterStats {
        RouterStats {
            active_rooms: self.rooms.len(),
            active_peers: self.peers.len(),
            active_hosts: self.hosts.len(),
            active_gameplay_sessions: self.gameplay.len(),
        }
    }

    fn member_state(&self, room_id: RoomId, peer_id: PeerId) -> Result<PeerState, RouterError> {
        let room = self
            .rooms
            .get(&room_id)
            .ok_or(RouterError::RoomNotFound(room_id))?;
        if !room.members.contains(&peer_id) {
            return Err(RouterError::PeerNotInRoom(peer_id, room_id));
        }
        let peer = self
            .peers
            .get(&peer_id)
            .copied()
            .ok_or(RouterError::PeerNotInRoom(peer_id, room_id))?;
        if peer.room_id != room_id {
            return Err(RouterError::PeerNotInRoom(peer_id, room_id));
        }
        Ok(peer)
    }

    fn validate_payload(&self, payload: &[u8]) -> Result<(), RouterError> {
        if payload.len() > MAX_CIV6_DATAGRAM_SIZE {
            return Err(RouterError::DatagramTooLarge(payload.len()));
        }
        Ok(())
    }

    fn remove_host(&mut self, host_session_id: HostSessionId) {
        let Some(host) = self.hosts.remove(&host_session_id) else {
            return;
        };
        self.host_by_peer.remove(&(host.room_id, host.peer_id));
        if let Some(room) = self.rooms.get_mut(&host.room_id) {
            room.host_sessions.remove(&host_session_id);
        }
        self.gameplay.retain(|_, session| {
            session.host_peer_id != host.peer_id || session.room_id != host.room_id
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use civ6_lan_protocol::{Civ6UdpPort, DiscoveryRequestId, GAMEPLAY_PORT};
    use pretty_assertions::assert_eq;
    use std::net::Ipv4Addr;

    fn code(value: &str) -> RoomCode {
        RoomCode::parse(value).unwrap()
    }

    fn ip(value: &str) -> VirtualIp {
        VirtualIp::new(value.parse::<Ipv4Addr>().unwrap())
    }

    fn setup_router() -> (RoomRouter, RoomId, RoomId, PeerId, PeerId, PeerId) {
        let mut router = RoomRouter::new(RouterConfig::default());
        let room_a = router.create_room(code("RMAAAA")).unwrap();
        let room_b = router.create_room(code("RMBBBB")).unwrap();
        let peer_a1 = PeerId::new();
        let peer_a2 = PeerId::new();
        let peer_b1 = PeerId::new();
        router.join_room(room_a, peer_a1, ip("10.240.0.2")).unwrap();
        router.join_room(room_a, peer_a2, ip("10.240.0.3")).unwrap();
        router.join_room(room_b, peer_b1, ip("10.240.0.4")).unwrap();
        (router, room_a, room_b, peer_a1, peer_a2, peer_b1)
    }

    #[test]
    fn discovery_fanout_is_room_scoped_and_keeps_host_identity() {
        let (mut router, room_a, room_b, peer_a1, peer_a2, peer_b1) = setup_router();
        let now = Instant::now();
        let host_a = router.register_host(room_a, peer_a1, now).unwrap();
        let host_b = router.register_host(room_b, peer_b1, now).unwrap();
        let request_id = DiscoveryRequestId::new();

        let targets = router
            .begin_discovery(room_a, peer_a2, request_id, Civ6UdpPort(62_900), now)
            .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].host_session_id, host_a);
        assert_eq!(
            router.route_discovery_response(request_id, host_a, now),
            Ok(peer_a2)
        );
        assert_eq!(
            router.route_discovery_response(request_id, host_b, now),
            Err(RouterError::HostNotInRoom(host_b, room_a))
        );
    }

    #[test]
    fn multiple_members_can_host_and_selected_gameplay_is_bidirectional() {
        let (mut router, room_a, _, peer_a1, peer_a2, _) = setup_router();
        let now = Instant::now();
        let host_a1 = router.register_host(room_a, peer_a1, now).unwrap();
        let host_a2 = router.register_host(room_a, peer_a2, now).unwrap();
        assert_eq!(router.host_count(room_a), Ok(2));

        let request_id = DiscoveryRequestId::new();
        let targets = router
            .begin_discovery(room_a, peer_a1, request_id, Civ6UdpPort(62_901), now)
            .unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets
            .iter()
            .any(|target| target.host_session_id == host_a1));
        assert!(targets
            .iter()
            .any(|target| target.host_session_id == host_a2));

        let gameplay_id = router.select_host(room_a, peer_a1, host_a2, now).unwrap();
        let to_host = router
            .route_gameplay_datagram(gameplay_id, peer_a1, now)
            .unwrap();
        assert_eq!(to_host.destination_peer_id, peer_a2);
        assert_eq!(to_host.destination_virtual_ip, ip("10.240.0.3"));

        let to_client = router
            .route_gameplay_datagram(gameplay_id, peer_a2, now)
            .unwrap();
        assert_eq!(to_client.destination_peer_id, peer_a1);
        assert_eq!(to_client.destination_virtual_ip, ip("10.240.0.2"));
        assert_eq!(to_client.room_id, room_a);

        let unrelated = PeerId::new();
        assert_eq!(
            router.route_gameplay_datagram(gameplay_id, unrelated, now),
            Err(RouterError::UnauthorizedGameplayPeer(
                unrelated,
                gameplay_id
            ))
        );
        assert_eq!(GAMEPLAY_PORT, 62_056);
    }

    #[test]
    fn expired_hosts_and_sessions_are_removed_without_touching_other_rooms() {
        let config = RouterConfig {
            host_ttl: Duration::from_secs(10),
            discovery_ttl: Duration::from_secs(15),
            gameplay_idle_ttl: Duration::from_secs(20),
        };
        let mut router = RoomRouter::new(config);
        let room_a = router.create_room(code("RMCCCC")).unwrap();
        let room_b = router.create_room(code("RMDDDD")).unwrap();
        let peer_a = PeerId::new();
        let peer_b = PeerId::new();
        router.join_room(room_a, peer_a, ip("10.240.0.5")).unwrap();
        router.join_room(room_b, peer_b, ip("10.240.0.6")).unwrap();
        let start = Instant::now();
        let host_a = router.register_host(room_a, peer_a, start).unwrap();
        router
            .register_host(room_b, peer_b, start + Duration::from_secs(5))
            .unwrap();
        let request_id = DiscoveryRequestId::new();
        router
            .begin_discovery(room_a, peer_a, request_id, Civ6UdpPort(62_902), start)
            .unwrap();

        let report = router.expire(start + Duration::from_secs(11));
        assert_eq!(report.expired_hosts, 1);
        assert_eq!(report.expired_discoveries, 0);
        assert_eq!(router.host_count(room_a), Ok(0));
        assert_eq!(router.host_count(room_b), Ok(1));
        assert_eq!(
            router.route_discovery_response(request_id, host_a, start + Duration::from_secs(11)),
            Err(RouterError::HostNotFound(host_a))
        );

        let discovery_expiry = router.expire(start + Duration::from_secs(16));
        assert_eq!(discovery_expiry.expired_discoveries, 1);
    }

    #[test]
    fn gameplay_sessions_expire_on_idle_without_expiring_the_host() {
        let config = RouterConfig {
            host_ttl: Duration::from_secs(60),
            discovery_ttl: Duration::from_secs(5),
            gameplay_idle_ttl: Duration::from_secs(20),
        };
        let mut router = RoomRouter::new(config);
        let room = router.create_room(code("RMGAME")).unwrap();
        let client = PeerId::new();
        let host = PeerId::new();
        router.join_room(room, client, ip("10.240.0.8")).unwrap();
        router.join_room(room, host, ip("10.240.0.9")).unwrap();

        let start = Instant::now();
        let host_session = router.register_host(room, host, start).unwrap();
        let gameplay = router
            .select_host(room, client, host_session, start)
            .unwrap();
        router
            .route_gameplay_datagram(gameplay, client, start + Duration::from_secs(10))
            .unwrap();

        let report = router.expire(start + Duration::from_secs(31));
        assert_eq!(report.expired_hosts, 0);
        assert_eq!(report.expired_gameplay_sessions, 1);
        assert_eq!(router.host_count(room), Ok(1));
        assert_eq!(
            router.route_gameplay_datagram(gameplay, client, start + Duration::from_secs(31)),
            Err(RouterError::GameplayNotFound(gameplay))
        );
    }

    #[test]
    fn a_peer_cannot_join_two_rooms_or_reuse_a_virtual_ip() {
        let (mut router, room_a, room_b, peer_a1, _, _) = setup_router();
        assert_eq!(
            router.join_room(room_b, peer_a1, ip("10.240.0.7")),
            Err(RouterError::PeerAlreadyInRoom(peer_a1))
        );

        let new_peer = PeerId::new();
        assert_eq!(
            router.join_room(room_b, new_peer, ip("10.240.0.2")),
            Err(RouterError::VirtualIpInUse(ip("10.240.0.2")))
        );
        assert_eq!(router.room_code(room_a).unwrap(), &code("RMAAAA"));
    }

    #[test]
    fn dispatch_layer_rejects_unknown_peers_and_oversized_datagrams() {
        let (mut router, room_a, _, peer_a1, peer_a2, _) = setup_router();
        let now = Instant::now();
        router.register_host(room_a, peer_a1, now).unwrap();

        let unknown = PeerId::new();
        assert_eq!(
            router.dispatch(RelayIngress::Discovery {
                source_peer_id: unknown,
                request_id: DiscoveryRequestId::new(),
                destination_port: Civ6UdpPort(62_900),
                payload: vec![1],
                now,
            }),
            Err(RouterError::PeerNotFound(unknown))
        );

        assert_eq!(
            router.dispatch(RelayIngress::Discovery {
                source_peer_id: peer_a2,
                request_id: DiscoveryRequestId::new(),
                destination_port: Civ6UdpPort(62_900),
                payload: vec![0; MAX_CIV6_DATAGRAM_SIZE + 1],
                now,
            }),
            Err(RouterError::DatagramTooLarge(MAX_CIV6_DATAGRAM_SIZE + 1))
        );
    }
}
