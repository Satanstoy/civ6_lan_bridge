//! Server-side control plane and routing adapters.

pub use civ6_lan_protocol as protocol;
pub use civ6_lan_router as router;

pub mod api;
pub mod db;
pub mod metrics;
pub mod relay;
pub mod state;
pub mod wireguard;

pub use api::build_router;
pub use state::AppState;
