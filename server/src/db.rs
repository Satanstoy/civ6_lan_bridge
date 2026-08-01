use std::net::Ipv4Addr;

use civ6_lan_protocol::{GameplaySessionId, HostSessionId, PeerId, RoomCode, RoomId, VirtualIp};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

#[derive(Clone, Debug)]
pub struct PersistedRoom {
    pub room_id: RoomId,
    pub room_code: RoomCode,
}

#[derive(Clone, Debug)]
pub struct PersistedPeer {
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub virtual_ip: VirtualIp,
    pub wireguard_public_key: String,
}

#[derive(Clone, Debug, Default)]
pub struct PersistedState {
    pub rooms: Vec<PersistedRoom>,
    pub peers: Vec<PersistedPeer>,
}

impl Database {
    pub async fn connect_and_migrate(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn insert_room(
        &self,
        room_id: RoomId,
        room_code: &RoomCode,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO rooms (room_id, room_code) VALUES ($1, $2)")
            .bind(room_id.as_uuid())
            .bind(room_code.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_room(&self, room_id: RoomId) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM rooms WHERE room_id = $1")
            .bind(room_id.as_uuid())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_peer(
        &self,
        room_id: RoomId,
        peer_id: PeerId,
        virtual_ip: VirtualIp,
        wireguard_public_key: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO peers \
             (peer_id, room_id, virtual_ip, wireguard_public_key) \
             VALUES ($1, $2, $3::inet, $4)",
        )
        .bind(peer_id.as_uuid())
        .bind(room_id.as_uuid())
        .bind(virtual_ip.to_string())
        .bind(wireguard_public_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_peer(&self, peer_id: PeerId) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM peers WHERE peer_id = $1")
            .bind(peer_id.as_uuid())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_host(
        &self,
        room_id: RoomId,
        peer_id: PeerId,
        host_session_id: HostSessionId,
        ttl: Duration,
    ) -> Result<(), sqlx::Error> {
        let ttl_seconds = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM host_sessions \
             WHERE room_id = $1 AND peer_id = $2 AND host_session_id <> $3",
        )
        .bind(room_id.as_uuid())
        .bind(peer_id.as_uuid())
        .bind(host_session_id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO host_sessions \
             (host_session_id, room_id, peer_id, last_heartbeat_at, expires_at) \
             VALUES ($1, $2, $3, NOW(), NOW() + ($4 * INTERVAL '1 second')) \
             ON CONFLICT (host_session_id) DO UPDATE SET \
                 last_heartbeat_at = EXCLUDED.last_heartbeat_at, \
                 expires_at = EXCLUDED.expires_at",
        )
        .bind(host_session_id.as_uuid())
        .bind(room_id.as_uuid())
        .bind(peer_id.as_uuid())
        .bind(ttl_seconds)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn heartbeat_host(
        &self,
        room_id: RoomId,
        peer_id: PeerId,
        host_session_id: HostSessionId,
        ttl: Duration,
    ) -> Result<(), sqlx::Error> {
        let ttl_seconds = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
        let result = sqlx::query(
            "UPDATE host_sessions \
             SET last_heartbeat_at = NOW(), \
                 expires_at = NOW() + ($4 * INTERVAL '1 second') \
             WHERE host_session_id = $1 AND room_id = $2 AND peer_id = $3",
        )
        .bind(host_session_id.as_uuid())
        .bind(room_id.as_uuid())
        .bind(peer_id.as_uuid())
        .bind(ttl_seconds)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    pub async fn delete_host(&self, host_session_id: HostSessionId) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM host_sessions WHERE host_session_id = $1")
            .bind(host_session_id.as_uuid())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_gameplay(
        &self,
        room_id: RoomId,
        client_peer_id: PeerId,
        host_session_id: HostSessionId,
        gameplay_session_id: GameplaySessionId,
        ttl: Duration,
    ) -> Result<(), sqlx::Error> {
        let ttl_seconds = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO gameplay_sessions \
             (gameplay_session_id, room_id, client_peer_id, host_session_id, \
              last_activity_at, expires_at) \
             VALUES ($1, $2, $3, $4, NOW(), NOW() + ($5 * INTERVAL '1 second'))",
        )
        .bind(gameplay_session_id.as_uuid())
        .bind(room_id.as_uuid())
        .bind(client_peer_id.as_uuid())
        .bind(host_session_id.as_uuid())
        .bind(ttl_seconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_gameplay(
        &self,
        gameplay_session_id: GameplaySessionId,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM gameplay_sessions WHERE gameplay_session_id = $1")
            .bind(gameplay_session_id.as_uuid())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Host and gameplay sessions are deliberately not restored after a
    /// process restart. Clients must re-register and re-create their routes.
    pub async fn clear_ephemeral_sessions(&self) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM gameplay_sessions")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM host_sessions")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn load_state(&self) -> Result<PersistedState, sqlx::Error> {
        let room_rows = sqlx::query(
            "SELECT room_id, room_code \
             FROM rooms \
             WHERE expires_at IS NULL OR expires_at > NOW() \
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        let peer_rows = sqlx::query(
            "SELECT peer_id, room_id, virtual_ip::text AS virtual_ip, wireguard_public_key \
             FROM peers \
             WHERE left_at IS NULL \
             ORDER BY joined_at",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut state = PersistedState::default();
        for row in room_rows {
            let room_id: Uuid = row.try_get("room_id")?;
            let room_code: String = row.try_get("room_code")?;
            let room_code = RoomCode::parse(room_code)
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
            state.rooms.push(PersistedRoom {
                room_id: RoomId::from_uuid(room_id),
                room_code,
            });
        }
        for row in peer_rows {
            let peer_id: Uuid = row.try_get("peer_id")?;
            let room_id: Uuid = row.try_get("room_id")?;
            let virtual_ip: String = row.try_get("virtual_ip")?;
            let virtual_ip = virtual_ip
                .parse::<Ipv4Addr>()
                .map(VirtualIp::new)
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
            let wireguard_public_key: String = row.try_get("wireguard_public_key")?;
            state.peers.push(PersistedPeer {
                room_id: RoomId::from_uuid(room_id),
                peer_id: PeerId::from_uuid(peer_id),
                virtual_ip,
                wireguard_public_key,
            });
        }
        Ok(state)
    }
}

pub async fn connect_and_migrate(database_url: &str) -> Result<Database, sqlx::Error> {
    Database::connect_and_migrate(database_url).await
}
