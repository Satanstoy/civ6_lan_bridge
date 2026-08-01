use std::{env, net::SocketAddr};

use civ6_lan_server::{
    build_router,
    db::connect_and_migrate,
    protocol::relay::RELAY_PROTOCOL_VERSION,
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

    let virtual_ip_prefix = match env::var("CIV6_VIRTUAL_IP_PREFIX") {
        Ok(value) => parse_ipv4_prefix(&value)?,
        Err(env::VarError::NotPresent) => [10, 240, 0],
        Err(error) => return Err(error.into()),
    };
    let mut state = AppState::new(bearer_token).with_virtual_ip_prefix(virtual_ip_prefix);
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
    let control_bind = listener.local_addr()?;
    let build_commit = option_env!("CIV6_BUILD_COMMIT")
        .map(ToOwned::to_owned)
        .or_else(|| env::var("CIV6_BUILD_COMMIT").ok())
        .unwrap_or_else(|| "unknown".to_owned());

    let api = build_router(state.clone());
    if let Ok(relay_bind) = env::var("CIV6_RELAY_BIND") {
        let mut relay_bind: SocketAddr = relay_bind.parse()?;
        let relay_port = env::var("CIV6_RELAY_PORT")
            .ok()
            .map(|value| value.parse::<u16>())
            .transpose()?;
        let relay_port = relay_port.unwrap_or_else(|| {
            if relay_bind.port() == 0 {
                DEFAULT_RELAY_PORT
            } else {
                relay_bind.port()
            }
        });
        if relay_bind.port() == 0 {
            relay_bind.set_port(relay_port);
        } else if relay_bind.port() != relay_port {
            return Err(format!(
                "CIV6_RELAY_BIND port {} does not match CIV6_RELAY_PORT {}",
                relay_bind.port(),
                relay_port
            )
            .into());
        }
        let relay = RelayServer::bind(relay_bind, state, relay_port).await?;
        let relay_endpoint = format!("udp://{}", relay.local_addr()?);
        info!(
            control_endpoint = %format!("http://{}", control_bind),
            relay_endpoint = %relay_endpoint,
            relay_port,
            protocol_version = RELAY_PROTOCOL_VERSION,
            build_commit = %build_commit,
            pid = std::process::id(),
            "Civ6 server ready"
        );
        tokio::select! {
            result = relay.run() => result?,
            result = axum::serve(listener, api) => result?,
        }
    } else {
        info!(
            control_endpoint = %format!("http://{}", control_bind),
            relay_endpoint = "disabled",
            relay_port = 0u16,
            protocol_version = RELAY_PROTOCOL_VERSION,
            build_commit = %build_commit,
            pid = std::process::id(),
            "Civ6 server ready; UDP relay disabled"
        );
        axum::serve(listener, api).await?;
    }
    Ok(())
}

fn parse_ipv4_prefix(value: &str) -> Result<[u8; 3], Box<dyn std::error::Error>> {
    let octets: Vec<u8> = value.split('.').map(str::parse).collect::<Result<_, _>>()?;
    match octets.as_slice() {
        [first, second, third] => Ok([*first, *second, *third]),
        _ => Err("CIV6_VIRTUAL_IP_PREFIX must contain exactly three IPv4 octets".into()),
    }
}
