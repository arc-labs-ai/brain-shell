//! Thin wrapper around [`brain_db_sdk::BrainClient`] applying the shell's
//! defaults (agent identity, per-request timeout).
//!
//! The mux client multiplexes concurrent streams over one socket, so the shell
//! needs only a single connection — a held SUBSCRIBE stream no longer starves
//! concurrent ENCODE / RECALL calls the way it did with the old serial client.

use std::net::SocketAddr;
use std::time::Duration;

use brain_db_sdk::error::BrainError;
use brain_db_sdk::{Auth, BrainClient, ClientConfig};

/// Connect to `addr` as `agent_id`, with a per-request `timeout`.
pub async fn connect(
    addr: SocketAddr,
    agent_id: [u8; 16],
    timeout: Duration,
) -> Result<BrainClient, BrainError> {
    let config = ClientConfig {
        agent_id,
        auth: Auth::Anonymous,
        request_timeout: Some(timeout),
        ..ClientConfig::default()
    };
    BrainClient::connect_with(addr, config).await
}
