use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use civ6_lan_protocol::{GameplaySessionId, HostSessionId, PeerId, RoomCode, RoomId, VirtualIp};
use civ6_lan_router::{GameplaySnapshot, HostSnapshot, RoomSnapshot, RouterError};
use serde::{Deserialize, Serialize};

use crate::{
    metrics::RelayMetricsSnapshot,
    state::{allocate_virtual_ip, parse_room_code, AppState},
    wireguard::WireGuardError,
};

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
    message: String,
}

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden(String),
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl ApiError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = self.status_and_code();
        let message = match &self {
            Self::Unauthorized => "a valid bearer token is required".to_owned(),
            Self::Forbidden(message)
            | Self::BadRequest(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Internal(message) => message.clone(),
        };
        (status, Json(ErrorResponse { error, message })).into_response()
    }
}

impl From<RouterError> for ApiError {
    fn from(error: RouterError) -> Self {
        match error {
            RouterError::RoomNotFound(_)
            | RouterError::PeerNotInRoom(_, _)
            | RouterError::PeerNotFound(_)
            | RouterError::HostNotFound(_)
            | RouterError::DiscoveryNotFound(_)
            | RouterError::GameplayNotFound(_) => Self::NotFound(error.to_string()),
            RouterError::DuplicateRoomCode(_)
            | RouterError::DuplicateRoomId(_)
            | RouterError::RoomNotEmpty(_)
            | RouterError::PeerAlreadyInRoom(_)
            | RouterError::VirtualIpInUse(_) => Self::Conflict(error.to_string()),
            RouterError::HostNotInRoom(_, _) | RouterError::UnauthorizedGameplayPeer(_, _) => {
                Self::Forbidden(error.to_string())
            }
            RouterError::HostExpired(_)
            | RouterError::DiscoveryExpired(_)
            | RouterError::InvalidDiscoveryPort(_)
            | RouterError::InvalidGameplayPort(_)
            | RouterError::DatagramTooLarge(_) => Self::BadRequest(error.to_string()),
        }
    }
}

impl From<WireGuardError> for ApiError {
    fn from(error: WireGuardError) -> Self {
        match error {
            WireGuardError::InvalidPublicKey => Self::BadRequest(error.to_string()),
            WireGuardError::CommandFailed(_)
            | WireGuardError::Io(_)
            | WireGuardError::InvalidOutput(_) => {
                Self::Internal("WireGuard peer operation failed".to_owned())
            }
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CreateRoomRequest {
    pub room_code: Option<RoomCode>,
}

#[derive(Debug, Deserialize, Default)]
pub struct JoinRoomRequest {
    pub peer_id: Option<PeerId>,
    pub wireguard_public_key: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterHostRequest {
    pub peer_id: PeerId,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HeartbeatRequest {
    pub peer_id: PeerId,
    pub host_session_id: HostSessionId,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateGameplayRequest {
    pub client_peer_id: PeerId,
    pub host_session_id: HostSessionId,
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
pub struct HealthResponse {
    pub status: &'static str,
    pub database_configured: bool,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/v1/test/metrics", get(test_metrics))
        .route("/v1/rooms", post(create_room))
        .route("/v1/rooms/{code}/join", post(join_room))
        .route("/v1/rooms/{code}/status", get(room_status))
        .route("/v1/rooms/{code}/hosts", post(register_host))
        .route("/v1/rooms/{code}/heartbeat", post(heartbeat_host))
        .route(
            "/v1/rooms/{code}/gameplay-sessions",
            post(create_gameplay_session),
        )
        .route(
            "/v1/rooms/{code}/hosts/{host_session_id}",
            delete(delete_host),
        )
        .route("/v1/rooms/{code}/peers/{peer_id}", delete(delete_peer))
        .route("/v1/rooms/{code}", delete(delete_room))
        .with_state(state)
}

async fn health_live(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        database_configured: state.database.is_some(),
    })
}

async fn health_ready(State(state): State<AppState>) -> Response {
    let database_configured = state.database.is_some();
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: if database_configured {
                "ready"
            } else {
                "ready_in_memory"
            },
            database_configured,
        }),
    )
        .into_response()
}

async fn test_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<RelayMetricsSnapshot>, ApiError> {
    require_bearer(&headers, &state)?;
    let router = state.router.read().await;
    Ok(Json(state.metrics.snapshot(router.stats())))
}

async fn create_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateRoomRequest>,
) -> Result<(StatusCode, Json<RoomResponse>), ApiError> {
    require_bearer(&headers, &state)?;
    let code = payload.room_code.unwrap_or_else(RoomCode::random);
    let snapshot = {
        let mut router = state.router.write().await;
        let room_id = router.create_room(code)?;
        router.room_snapshot(room_id)?
    };
    if let Some(database) = state.database.clone() {
        if database
            .insert_room(snapshot.room_id, &snapshot.code)
            .await
            .is_err()
        {
            let mut router = state.router.write().await;
            let _ = router.remove_empty_room(snapshot.room_id);
            return Err(ApiError::Internal("database write failed".to_owned()));
        }
    }
    Ok((StatusCode::CREATED, Json(room_response(snapshot))))
}

async fn join_room(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<JoinRoomRequest>,
) -> Result<(StatusCode, Json<PeerResponse>), ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let peer_id = payload.peer_id.unwrap_or_else(PeerId::new);
    let database = state.database.clone();
    let wireguard = state.wireguard.clone();
    if (database.is_some() || wireguard.is_some()) && payload.wireguard_public_key.is_none() {
        return Err(ApiError::BadRequest(
            "wireguard_public_key is required when persistent or WireGuard mode is enabled"
                .to_owned(),
        ));
    }
    let public_key = payload.wireguard_public_key.clone();
    if let Some(public_key) = public_key.as_deref() {
        crate::wireguard::WireGuardManager::validate_public_key(public_key)?;
    }

    let (room_id, virtual_ip) = {
        let mut router = state.router.write().await;
        let room_id = router
            .room_id_for_code(&room_code)
            .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
        let virtual_ip = allocate_virtual_ip(&router, state.virtual_ip_prefix)
            .ok_or_else(|| ApiError::Conflict("virtual IP pool is exhausted".to_owned()))?;
        router.join_room(room_id, peer_id, virtual_ip)?;
        (room_id, virtual_ip)
    };

    if let Some(manager) = wireguard.as_ref() {
        let public_key = public_key
            .as_deref()
            .expect("WireGuard public key was checked above");
        if let Err(error) = manager.add_peer(public_key, virtual_ip).await {
            let mut router = state.router.write().await;
            let _ = router.leave_room(room_id, peer_id);
            return Err(error.into());
        }
    }

    if let Some(database) = database.as_ref() {
        let public_key = public_key
            .as_deref()
            .expect("persistent mode requires a public key");
        if database
            .insert_peer(room_id, peer_id, virtual_ip, public_key)
            .await
            .is_err()
        {
            if let Some(manager) = wireguard.as_ref() {
                let _ = manager.remove_peer(public_key).await;
            }
            let mut router = state.router.write().await;
            let _ = router.leave_room(room_id, peer_id);
            return Err(ApiError::Internal("database write failed".to_owned()));
        }
    }

    if let Some(public_key) = public_key {
        state.peer_keys.write().await.insert(peer_id, public_key);
    }

    Ok((
        StatusCode::CREATED,
        Json(PeerResponse {
            room_id,
            peer_id,
            virtual_ip,
        }),
    ))
}

async fn room_status(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Json<RoomResponse>, ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let router = state.router.read().await;
    let room_id = router
        .room_id_for_code(&room_code)
        .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
    Ok(Json(room_response(router.room_snapshot(room_id)?)))
}

async fn register_host(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<RegisterHostRequest>,
) -> Result<(StatusCode, Json<HostResponse>), ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let now = Instant::now();
    let mut router = state.router.write().await;
    let room_id = router
        .room_id_for_code(&room_code)
        .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
    let host_session_id = router.register_host(room_id, payload.peer_id, now)?;
    let snapshot = router.host_snapshot(host_session_id)?;
    drop(router);
    if let Some(database) = state.database.clone() {
        if database
            .upsert_host(
                room_id,
                payload.peer_id,
                host_session_id,
                Duration::from_secs(15),
            )
            .await
            .is_err()
        {
            let mut router = state.router.write().await;
            let _ = router.unregister_host(host_session_id);
            return Err(ApiError::Internal("database write failed".to_owned()));
        }
    }
    Ok((StatusCode::CREATED, Json(host_response(snapshot, 15))))
}

async fn heartbeat_host(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<HeartbeatRequest>,
) -> Result<Json<HostResponse>, ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let now = Instant::now();
    let mut router = state.router.write().await;
    let room_id = router
        .room_id_for_code(&room_code)
        .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
    router.heartbeat_host_for_room(room_id, payload.peer_id, payload.host_session_id, now)?;
    let snapshot = router.host_snapshot(payload.host_session_id)?;
    drop(router);
    if let Some(database) = state.database.clone() {
        if database
            .heartbeat_host(
                room_id,
                payload.peer_id,
                payload.host_session_id,
                Duration::from_secs(15),
            )
            .await
            .is_err()
        {
            return Err(ApiError::Internal("database write failed".to_owned()));
        }
    }
    Ok(Json(host_response(snapshot, 15)))
}

async fn create_gameplay_session(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<CreateGameplayRequest>,
) -> Result<(StatusCode, Json<GameplayResponse>), ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let now = Instant::now();
    let mut router = state.router.write().await;
    let room_id = router
        .room_id_for_code(&room_code)
        .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
    let session_id = router.select_host(
        room_id,
        payload.client_peer_id,
        payload.host_session_id,
        now,
    )?;
    let snapshot = router.gameplay_snapshot(session_id)?;
    drop(router);
    if let Some(database) = state.database.clone() {
        if database
            .insert_gameplay(
                room_id,
                payload.client_peer_id,
                payload.host_session_id,
                session_id,
                Duration::from_secs(30),
            )
            .await
            .is_err()
        {
            let mut router = state.router.write().await;
            let _ = router.remove_gameplay_session(session_id);
            return Err(ApiError::Internal("database write failed".to_owned()));
        }
    }
    Ok((StatusCode::CREATED, Json(gameplay_response(snapshot))))
}

async fn delete_host(
    State(state): State<AppState>,
    Path((code, host_session_id)): Path<(String, HostSessionId)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let router = state.router.write().await;
    let room_id = router
        .room_id_for_code(&room_code)
        .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
    let host = router.host_snapshot(host_session_id)?;
    if host.room_id != room_id {
        return Err(ApiError::Forbidden(
            "host session belongs to another room".to_owned(),
        ));
    }
    drop(router);
    if let Some(database) = state.database.clone() {
        if database.delete_host(host_session_id).await.is_err() {
            return Err(ApiError::Internal("database delete failed".to_owned()));
        }
    }
    let mut router = state.router.write().await;
    router.unregister_host(host_session_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_peer(
    State(state): State<AppState>,
    Path((code, peer_id)): Path<(String, PeerId)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let (room_id, virtual_ip) = {
        let router = state.router.read().await;
        let room_id = router
            .room_id_for_code(&room_code)
            .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
        let peer_room_id = router
            .room_for_peer(peer_id)
            .ok_or_else(|| ApiError::NotFound("peer does not exist".to_owned()))?;
        if peer_room_id != room_id {
            return Err(ApiError::Forbidden(
                "peer belongs to another room".to_owned(),
            ));
        }
        let virtual_ip = router
            .peer_virtual_ip(peer_id)
            .ok_or_else(|| ApiError::NotFound("peer does not exist".to_owned()))?;
        (room_id, virtual_ip)
    };

    let database = state.database.clone();
    let wireguard = state.wireguard.clone();
    let public_key = state.peer_keys.read().await.get(&peer_id).cloned();
    if (database.is_some() || wireguard.is_some()) && public_key.is_none() {
        return Err(ApiError::Internal(
            "peer has no persisted WireGuard public key".to_owned(),
        ));
    }

    if let (Some(manager), Some(public_key)) = (wireguard.as_ref(), public_key.as_deref()) {
        manager.remove_peer(public_key).await?;
    }
    if let Some(database) = database.as_ref() {
        if database.delete_peer(peer_id).await.is_err() {
            if let (Some(manager), Some(public_key)) = (wireguard.as_ref(), public_key.as_deref()) {
                let _ = manager.add_peer(public_key, virtual_ip).await;
            }
            return Err(ApiError::Internal("database delete failed".to_owned()));
        }
    }

    let leave_result = {
        let mut router = state.router.write().await;
        router.leave_room(room_id, peer_id)
    };
    if let Err(error) = leave_result {
        if let (Some(database), Some(public_key)) = (database.as_ref(), public_key.as_deref()) {
            let _ = database
                .insert_peer(room_id, peer_id, virtual_ip, public_key)
                .await;
        }
        if let (Some(manager), Some(public_key)) = (wireguard.as_ref(), public_key.as_deref()) {
            let _ = manager.add_peer(public_key, virtual_ip).await;
        }
        return Err(error.into());
    }
    state.peer_keys.write().await.remove(&peer_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_room(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_bearer(&headers, &state)?;
    let room_code = parse_room_code(&code).map_err(ApiError::BadRequest)?;
    let room_id = {
        let router = state.router.read().await;
        let room_id = router
            .room_id_for_code(&room_code)
            .ok_or_else(|| ApiError::NotFound("room does not exist".to_owned()))?;
        let snapshot = router.room_snapshot(room_id)?;
        if snapshot.member_count != 0 || snapshot.host_count != 0 {
            return Err(ApiError::Conflict("room is not empty".to_owned()));
        }
        room_id
    };
    if let Some(database) = state.database.as_ref() {
        database
            .delete_room(room_id)
            .await
            .map_err(|_| ApiError::Internal("database delete failed".to_owned()))?;
    }
    let mut router = state.router.write().await;
    router.remove_empty_room(room_id)?;
    Ok(StatusCode::NO_CONTENT)
}

fn require_bearer(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    let expected = format!("Bearer {}", state.bearer_token);
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if provided == Some(expected.as_str()) {
        Ok(())
    } else {
        state.metrics.record_authentication_failure();
        Err(ApiError::Unauthorized)
    }
}

fn room_response(snapshot: RoomSnapshot) -> RoomResponse {
    RoomResponse {
        room_id: snapshot.room_id,
        room_code: snapshot.code,
        member_count: snapshot.member_count,
        host_count: snapshot.host_count,
    }
}

fn host_response(snapshot: HostSnapshot, expires_in_seconds: u64) -> HostResponse {
    HostResponse {
        room_id: snapshot.room_id,
        host_session_id: snapshot.host_session_id,
        peer_id: snapshot.peer_id,
        virtual_ip: snapshot.virtual_ip,
        expires_in_seconds,
    }
}

fn gameplay_response(snapshot: GameplaySnapshot) -> GameplayResponse {
    GameplayResponse {
        gameplay_session_id: snapshot.session_id,
        room_id: snapshot.room_id,
        client_peer_id: snapshot.client_peer_id,
        host_peer_id: snapshot.host_peer_id,
        client_virtual_ip: snapshot.client_virtual_ip,
        host_virtual_ip: snapshot.host_virtual_ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use civ6_lan_client_core::{ClientConfig, ControlClient};
    use civ6_lan_protocol::RoomCode;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    fn authorized_request(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn mutable_endpoints_require_bearer_token() {
        let app = build_router(AppState::new("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rooms")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_code":"RMAAAA"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn metrics_endpoint_requires_bearer_and_reports_auth_failures() {
        let app = build_router(AppState::new("test-token"));
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/test/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(authorized_request("GET", "/v1/test/metrics", "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let metrics: RelayMetricsSnapshot = serde_json::from_slice(&body).unwrap();
        assert!(metrics.authentication_failures >= 1);
        assert_eq!(metrics.active_rooms, 0);
    }

    #[tokio::test]
    async fn room_delete_requires_an_empty_room() {
        let app = build_router(AppState::new("test-token"));
        let created = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms",
                r#"{"room_code":"RMDELE"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let joined = app
            .clone()
            .oneshot(authorized_request("POST", "/v1/rooms/RMDELE/join", "{}"))
            .await
            .unwrap();
        assert_eq!(joined.status(), StatusCode::CREATED);
        let body = joined.into_body().collect().await.unwrap().to_bytes();
        let peer: PeerResponse = serde_json::from_slice(&body).unwrap();

        let non_empty_delete = app
            .clone()
            .oneshot(authorized_request("DELETE", "/v1/rooms/RMDELE", "{}"))
            .await
            .unwrap();
        assert_eq!(non_empty_delete.status(), StatusCode::CONFLICT);

        let peer_delete = app
            .clone()
            .oneshot(authorized_request(
                "DELETE",
                &format!("/v1/rooms/RMDELE/peers/{}", peer.peer_id),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(peer_delete.status(), StatusCode::NO_CONTENT);

        let room_delete = app
            .clone()
            .oneshot(authorized_request("DELETE", "/v1/rooms/RMDELE", "{}"))
            .await
            .unwrap();
        assert_eq!(room_delete.status(), StatusCode::NO_CONTENT);

        let status = app
            .oneshot(authorized_request("GET", "/v1/rooms/RMDELE/status", "{}"))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn in_memory_mode_is_ready_without_postgres() {
        let app = build_router(AppState::new("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "ready_in_memory");
        assert_eq!(health["database_configured"], false);
    }

    #[tokio::test]
    async fn duplicate_room_codes_are_rejected_by_control_api() {
        let app = build_router(AppState::new("test-token"));
        let first = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms",
                r#"{"room_code":"RMAAAA"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);

        let second = app
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms",
                r#"{"room_code":"RMAAAA"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cross_room_gameplay_selection_is_forbidden_by_control_api() {
        let app = build_router(AppState::new("test-token"));
        for code in ["RMAAAA", "RMBBBB"] {
            let response = app
                .clone()
                .oneshot(authorized_request(
                    "POST",
                    "/v1/rooms",
                    &format!(r#"{{"room_code":"{code}"}}"#),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        let client_response = app
            .clone()
            .oneshot(authorized_request("POST", "/v1/rooms/RMAAAA/join", "{}"))
            .await
            .unwrap();
        let client_body = client_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let client_peer: PeerResponse = serde_json::from_slice(&client_body).unwrap();

        let host_peer_response = app
            .clone()
            .oneshot(authorized_request("POST", "/v1/rooms/RMBBBB/join", "{}"))
            .await
            .unwrap();
        let host_peer_body = host_peer_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let host_peer: PeerResponse = serde_json::from_slice(&host_peer_body).unwrap();
        let host_response = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms/RMBBBB/hosts",
                &serde_json::to_string(&RegisterHostRequest {
                    peer_id: host_peer.peer_id,
                })
                .unwrap(),
            ))
            .await
            .unwrap();
        let host_body = host_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let host: HostResponse = serde_json::from_slice(&host_body).unwrap();

        let response = app
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms/RMAAAA/gameplay-sessions",
                &serde_json::to_string(&CreateGameplayRequest {
                    client_peer_id: client_peer.peer_id,
                    host_session_id: host.host_session_id,
                })
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cross_room_peer_delete_is_forbidden_and_preserves_peer() {
        let app = build_router(AppState::new("test-token"));
        for code in ["RMAAAA", "RMBBBB"] {
            let response = app
                .clone()
                .oneshot(authorized_request(
                    "POST",
                    "/v1/rooms",
                    &format!(r#"{{"room_code":"{code}"}}"#),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        let joined = app
            .clone()
            .oneshot(authorized_request("POST", "/v1/rooms/RMAAAA/join", "{}"))
            .await
            .unwrap();
        assert_eq!(joined.status(), StatusCode::CREATED);
        let peer_body = joined.into_body().collect().await.unwrap().to_bytes();
        let peer: PeerResponse = serde_json::from_slice(&peer_body).unwrap();

        let cross_room_delete = app
            .clone()
            .oneshot(authorized_request(
                "DELETE",
                &format!("/v1/rooms/RMBBBB/peers/{}", peer.peer_id),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(cross_room_delete.status(), StatusCode::FORBIDDEN);

        let status = app
            .oneshot(authorized_request("GET", "/v1/rooms/RMAAAA/status", "{}"))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status_body = status.into_body().collect().await.unwrap().to_bytes();
        let room: RoomResponse = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(room.member_count, 1);
    }

    #[tokio::test]
    async fn room_join_host_and_gameplay_flow_uses_router_state() {
        let app = build_router(AppState::new("test-token"));
        let response = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms",
                r#"{"room_code":"RMAAAA"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = app
            .clone()
            .oneshot(authorized_request("POST", "/v1/rooms/RMAAAA/join", "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let peer: PeerResponse = serde_json::from_slice(&body).unwrap();

        let response = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms/RMAAAA/hosts",
                &serde_json::to_string(&RegisterHostRequest {
                    peer_id: peer.peer_id,
                })
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let host: HostResponse = serde_json::from_slice(&body).unwrap();

        let response = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms/RMAAAA/gameplay-sessions",
                &serde_json::to_string(&CreateGameplayRequest {
                    client_peer_id: peer.peer_id,
                    host_session_id: host.host_session_id,
                })
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = app
            .oneshot(authorized_request("GET", "/v1/rooms/RMAAAA/status", "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let status: RoomResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.room_code, RoomCode::parse("RMAAAA").unwrap());
        assert_eq!(status.member_count, 1);
        assert_eq!(status.host_count, 1);
    }

    #[tokio::test]
    async fn missing_wireguard_key_does_not_partially_join_peer() {
        let state = AppState::new("test-token")
            .with_wireguard(crate::wireguard::WireGuardManager::new("wg0"));
        let inspector = state.clone();
        let app = build_router(state);

        let response = app
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/v1/rooms",
                r#"{"room_code":"RMKEYS"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = app
            .oneshot(authorized_request("POST", "/v1/rooms/RMKEYS/join", "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let router = inspector.router.read().await;
        let room_id = router
            .room_id_for_code(&RoomCode::parse("RMKEYS").unwrap())
            .unwrap();
        assert_eq!(router.room_snapshot(room_id).unwrap().member_count, 0);
    }

    #[tokio::test]
    async fn client_core_exchanges_control_flow_with_live_http_server() {
        let app = build_router(AppState::new("test-token"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = ControlClient::new(ClientConfig {
            control_url: format!("http://{address}"),
            bearer_token: "test-token".to_owned(),
            relay_server: "127.0.0.1:32000".parse().unwrap(),
            relay_port: 32_000,
        });
        let room = client
            .create_room(Some(RoomCode::parse("RMAAAA").unwrap()))
            .await
            .unwrap();
        let peer = client.join_room(&room.room_code, None, None).await.unwrap();
        let host = client
            .register_host(&room.room_code, peer.peer_id)
            .await
            .unwrap();
        let gameplay = client
            .create_gameplay_session(&room.room_code, peer.peer_id, host.host_session_id)
            .await
            .unwrap();

        assert_eq!(room.member_count, 0);
        assert_eq!(peer.room_id, room.room_id);
        assert_eq!(host.peer_id, peer.peer_id);
        assert_eq!(gameplay.client_peer_id, peer.peer_id);
        assert_eq!(gameplay.host_peer_id, peer.peer_id);
        server_task.abort();
    }
}
