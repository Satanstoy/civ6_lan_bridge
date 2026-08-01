use std::{env, net::SocketAddr};

use civ6_lan_server::{
    build_router,
    db::connect_and_migrate,
    relay::{RelayServer, DEFAULT_RELAY_PORT},
    state::AppState,
    wireguard::WireGuardManager,
};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG")
                .unwrap_or_else(|_| "civ6_lan_server=info,tower_http=info".to_owned()),
        )
        .init();

    let bearer_token = env::var("CIV6_CONTROL_BEARER_TOKEN")
        .map_err(|_| "CIV6_CONTROL_BEARER_TOKEN must be set")?;
    if bearer_token.len() < 32 {
        return Err("CIV6_CONTROL_BEARER_TOKEN must be at least 32 characters".into());
    }

    let mut state = AppState::new(bearer_token);
    if let Ok(database_url) = env::var("CIV6_DATABASE_URL") {
        let pool = connect_and_migrate(&database_url).await?;
        let persisted = pool.load_state().await?;
        pool.clear_ephemeral_sessions().await?;
        state.restore_persisted_state(persisted).await?;
        info!("PostgreSQL migrations applied");
        state = state.with_database(pool);
    } else {
        info!("CIV6_DATABASE_URL is not set; control state is in-memory and readiness is ready_in_memory");
    }

    if let Ok(interface) = env::var("CIV6_WIREGUARD_INTERFACE") {
        state = state.with_wireguard(WireGuardManager::new(interface));
        let restored = state.restore_wireguard_peers().await?;
        info!(restored, "WireGuard peers restored from PostgreSQL");
    } else {
        info!("CIV6_WIREGUARD_INTERFACE is not set; WireGuard peer management is disabled");
    }

    let bind: SocketAddr = env::var("CIV6_CONTROL_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    let listener = TcpListener::bind(bind).await?;
    info!(%bind, "Civ6 control API listening");

    let api = build_router(state.clone());
    if let Ok(relay_bind) = env::var("CIV6_RELAY_BIND") {
        let relay_bind: SocketAddr = relay_bind.parse()?;
        let relay = RelayServer::bind(relay_bind, state, DEFAULT_RELAY_PORT).await?;
        info!(%relay_bind, relay_port = DEFAULT_RELAY_PORT, "Civ6 UDP relay listening");
        tokio::select! {
            result = relay.run() => result?,
            result = axum::serve(listener, api) => result?,
        }
    } else {
        info!("CIV6_RELAY_BIND is not set; Civ6 UDP relay is disabled");
        axum::serve(listener, api).await?;
    }
    Ok(())
}
