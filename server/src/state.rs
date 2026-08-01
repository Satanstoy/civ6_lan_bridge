use std::{collections::HashMap, net::Ipv4Addr, sync::Arc};

use civ6_lan_protocol::{PeerId, RoomCode, VirtualIp};
use civ6_lan_router::{RoomRouter, RouterConfig, RouterError};
use tokio::sync::RwLock;

use crate::db::{Database, PersistedState};
use crate::metrics::RelayMetrics;
use crate::wireguard::{WireGuardError, WireGuardManager};

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<RwLock<RoomRouter>>,
    pub bearer_token: Arc<str>,
    pub database: Option<Database>,
    pub wireguard: Option<WireGuardManager>,
    pub peer_keys: Arc<RwLock<HashMap<PeerId, String>>>,
    pub metrics: RelayMetrics,
    pub virtual_ip_prefix: [u8; 3],
}

impl AppState {
    pub fn new(bearer_token: impl Into<String>) -> Self {
        Self {
            router: Arc::new(RwLock::new(RoomRouter::new(RouterConfig::default()))),
            bearer_token: Arc::from(bearer_token.into()),
            database: None,
            wireguard: None,
            peer_keys: Arc::new(RwLock::new(HashMap::new())),
            metrics: RelayMetrics::default(),
            virtual_ip_prefix: [10, 240, 0],
        }
    }

    pub fn with_database(mut self, database: Database) -> Self {
        self.database = Some(database);
        self
    }

    pub fn with_wireguard(mut self, wireguard: WireGuardManager) -> Self {
        self.wireguard = Some(wireguard);
        self
    }

    pub fn with_virtual_ip_prefix(mut self, prefix: [u8; 3]) -> Self {
        self.virtual_ip_prefix = prefix;
        self
    }

    pub async fn restore_persisted_state(
        &self,
        persisted: PersistedState,
    ) -> Result<(), RouterError> {
        let mut router = self.router.write().await;
        for room in persisted.rooms {
            router.create_room_with_id(room.room_id, room.room_code)?;
        }
        let mut peer_keys = self.peer_keys.write().await;
        for peer in persisted.peers {
            router.restore_peer(peer.room_id, peer.peer_id, peer.virtual_ip)?;
            peer_keys.insert(peer.peer_id, peer.wireguard_public_key);
        }
        Ok(())
    }

    pub async fn restore_wireguard_peers(&self) -> Result<usize, WireGuardError> {
        let Some(manager) = self.wireguard.clone() else {
            return Ok(0);
        };
        let peer_keys = self.peer_keys.read().await.clone();
        let router = self.router.read().await;
        let mut restored = 0;
        for (peer_id, public_key) in peer_keys {
            if let Some(virtual_ip) = router.peer_virtual_ip(peer_id) {
                manager.add_peer(&public_key, virtual_ip).await?;
                restored += 1;
            }
        }
        Ok(restored)
    }
}

pub fn allocate_virtual_ip(router: &RoomRouter, prefix: [u8; 3]) -> Option<VirtualIp> {
    (2..=254)
        .map(|last_octet| {
            VirtualIp::new(Ipv4Addr::new(prefix[0], prefix[1], prefix[2], last_octet))
        })
        .find(|candidate| router.is_virtual_ip_available(*candidate))
}

pub fn parse_room_code(value: &str) -> Result<RoomCode, String> {
    RoomCode::parse(value).map_err(|error| error.to_string())
}
