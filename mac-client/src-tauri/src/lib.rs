use std::{
    env,
    fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use civ6_lan_client_core::{ClientConfig, ControlClient, RelayClient};
use civ6_lan_protocol::{HostSessionId, PeerId, RoomCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ClientSettings {
    pub control_url: String,
    pub bearer_token: String,
    pub relay_server: String,
    pub relay_port: u16,
}

impl ClientSettings {
    fn config(&self) -> Result<ClientConfig, String> {
        let control_url = self.control_url.trim();
        if control_url.is_empty()
            || !(control_url.starts_with("http://") || control_url.starts_with("https://"))
        {
            return Err("invalid control endpoint: expected an http:// or https:// URL".to_owned());
        }
        if self.relay_port == 0 {
            return Err("invalid relay port: port must be between 1 and 65535".to_owned());
        }
        let relay_server = self
            .relay_server
            .trim()
            .parse::<SocketAddr>()
            .or_else(|_| {
                self.relay_server
                    .trim()
                    .parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, self.relay_port))
            })
            .map_err(|error| format!("invalid relay address: {error}"))?;

        Ok(ClientConfig {
            control_url: control_url.to_owned(),
            bearer_token: self.bearer_token.clone(),
            relay_server,
            relay_port: self.relay_port,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct RelayProbeResult {
    pub status: &'static str,
    pub local_bind: String,
    pub relay_server: String,
}

#[tauri::command]
fn load_test_manifest(path: Option<String>) -> Result<civ6_lan_client_core::TestSessionManifest, String> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| env::var_os("CIV6_TEST_MANIFEST").map(PathBuf::from))
        .ok_or_else(|| "CIV6_TEST_MANIFEST is not set and no manifest path was supplied".to_owned())?;
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read test manifest {}: {error}", path.display()))?;
    let manifest = serde_json::from_str(&content)
        .map_err(|error| format!("invalid test manifest {}: {error}", path.display()))?;
    Ok(manifest)
}

#[tauri::command]
async fn health_live(settings: ClientSettings) -> Result<civ6_lan_client_core::HealthResponse, String> {
    ControlClient::new(settings.config()?)
        .health_live()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_room(settings: ClientSettings) -> Result<civ6_lan_client_core::RoomResponse, String> {
    ControlClient::new(settings.config()?)
        .create_room(None)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_room(settings: ClientSettings, room_code: String) -> Result<(), String> {
    let room_code = room_code
        .parse::<RoomCode>()
        .map_err(|error| format!("invalid room code: {error}"))?;
    ControlClient::new(settings.config()?)
        .delete_room(&room_code)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn join_room(
    settings: ClientSettings,
    room_code: String,
) -> Result<civ6_lan_client_core::PeerResponse, String> {
    let room_code = room_code
        .parse::<RoomCode>()
        .map_err(|error| format!("invalid room code: {error}"))?;
    ControlClient::new(settings.config()?)
        .join_room(&room_code, None, None)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn register_host(
    settings: ClientSettings,
    room_code: String,
    peer_id: String,
) -> Result<civ6_lan_client_core::HostResponse, String> {
    let room_code = room_code
        .parse::<RoomCode>()
        .map_err(|error| format!("invalid room code: {error}"))?;
    let peer_id = peer_id
        .parse::<PeerId>()
        .map_err(|error| format!("invalid peer id: {error}"))?;
    ControlClient::new(settings.config()?)
        .register_host(&room_code, peer_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_gameplay_session(
    settings: ClientSettings,
    room_code: String,
    peer_id: String,
    host_session_id: String,
) -> Result<civ6_lan_client_core::GameplayResponse, String> {
    let room_code = room_code
        .parse::<RoomCode>()
        .map_err(|error| format!("invalid room code: {error}"))?;
    let peer_id = peer_id
        .parse::<PeerId>()
        .map_err(|error| format!("invalid peer id: {error}"))?;
    let host_session_id = host_session_id
        .parse::<HostSessionId>()
        .map_err(|error| format!("invalid host session id: {error}"))?;
    ControlClient::new(settings.config()?)
        .create_gameplay_session(&room_code, peer_id, host_session_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn relay_probe(
    settings: ClientSettings,
    local_bind: String,
) -> Result<RelayProbeResult, String> {
    let config = settings.config()?;
    let relay_server = config.relay_addr();
    let local_bind = local_bind
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid local bind address: {error}"))?;
    let client = RelayClient::bind(local_bind, relay_server)
        .await
        .map_err(|error| error.to_string())?;
    client
        .probe(Duration::from_secs(3))
        .await
        .map_err(|error| error.to_string())?;
    Ok(RelayProbeResult {
        status: "relay_probe_ack",
        local_bind: local_bind.to_string(),
        relay_server: relay_server.to_string(),
    })
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            health_live,
            create_room,
            delete_room,
            join_room,
            register_host,
            create_gameplay_session,
            relay_probe,
            load_test_manifest
        ])
        .run(tauri::generate_context!())
        .expect("error while running Civ6 LAN Bridge macOS client");
}
