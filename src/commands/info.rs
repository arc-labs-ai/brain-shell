//! `\info` (REPL) / bare `info` verb — a diagnostic snapshot.
//!
//! Builds the [`InfoCard`] from the SDK's cached handshake
//! (`client.session()`), the resolved agent identity, and the live shell
//! session. No live request is issued: every field comes from state the
//! shell already holds, so the card answers "what's my current state?"
//! without a round trip.

use brain_db_sdk::BrainClient;

use crate::cli::agent::AgentIdSource;
use crate::core::AgentId;
use crate::render::{
    AgentInfo, ConnectionInfo, InfoCard, ServerInfo, ServerWelcomeFields, SessionInfo,
};
use crate::session::Session;

/// Assemble the diagnostic card from cached state. Pure: no wire I/O.
#[must_use]
pub fn collect(
    client: &BrainClient,
    session: &Session,
    agent_id: AgentId,
    agent_source: &AgentIdSource,
) -> InfoCard {
    let snapshot = client.session();

    let features = &snapshot.server_features;
    let welcome = ServerWelcomeFields {
        server_id: snapshot.server_id.clone(),
        wire_version: snapshot.chosen_version,
        // The SDK handshake snapshot doesn't carry a server wall clock;
        // surface 0 so the card renders the (epoch) row rather than
        // inventing a skew reading.
        server_time_unix_nanos: 0,
        bound_shard: snapshot.bound_shard_id,
        // Capability flags the shell can read off the negotiated
        // features. The handshake exposes payload/stream limits rather
        // than discrete streaming/zstd/push booleans, so derive what we
        // can and leave the rest false.
        streaming: features.max_concurrent_streams > 1,
        compression_zstd: false,
        server_push: false,
    };

    let server = ServerInfo {
        address: session.server.to_string(),
        welcome: Some(welcome),
    };

    let agent = AgentInfo {
        name: source_name(agent_source),
        agent_id: agent_id.to_bytes(),
        source_label: source_label(agent_source),
        default: matches!(agent_source, AgentIdSource::DefaultFromConfig { .. }),
        note: String::new(),
        created_at: None,
    };

    let connection = ConnectionInfo {
        authenticated: true,
        connected_at_unix_nanos: None,
    };

    let session_info = SessionInfo {
        output: session.output.short_name().to_string(),
        sticky_context: session.sticky_context,
        active_txn: session.active_txn.map(|b| crate::parser::format_txn_id(&b)),
        timing: session.timing,
    };

    InfoCard {
        server,
        agent,
        connection,
        session: session_info,
    }
}

/// Name (if any) of the agent the session is bound to.
fn source_name(s: &AgentIdSource) -> Option<String> {
    match s {
        AgentIdSource::NamedFlag { name, .. }
        | AgentIdSource::NamedEnv { name, .. }
        | AgentIdSource::ActiveFromConfig { name, .. }
        | AgentIdSource::DefaultFromConfig { name, .. }
        | AgentIdSource::AutoMinted { name, .. } => Some(name.clone()),
        AgentIdSource::IdFlag | AgentIdSource::IdEnv | AgentIdSource::Ephemeral => None,
    }
}

/// Human-readable source label shared with the connect banner.
fn source_label(s: &AgentIdSource) -> String {
    match s {
        AgentIdSource::NamedFlag { name, file } => {
            format!("flag --agent {name} ({})", file.display())
        }
        AgentIdSource::IdFlag => "flag --agent-id".into(),
        AgentIdSource::NamedEnv { name, file } => {
            format!("env BRAIN_AGENT={name} ({})", file.display())
        }
        AgentIdSource::IdEnv => "env BRAIN_AGENT_ID".into(),
        AgentIdSource::ActiveFromConfig { name, file } => {
            format!("config active = {name} ({})", file.display())
        }
        AgentIdSource::DefaultFromConfig { name, file } => {
            format!("config default = {name} ({})", file.display())
        }
        AgentIdSource::AutoMinted { name, file } => {
            format!("auto-minted {name} ({})", file.display())
        }
        AgentIdSource::Ephemeral => "ephemeral (no config file path available)".into(),
    }
}
