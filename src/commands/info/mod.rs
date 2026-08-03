//! `\info` (REPL) / bare `info` verb — a diagnostic snapshot.
//!
//! Builds the [`InfoCard`] from the SDK's cached handshake
//! (`client.connection()`), the server-assigned identity, the active
//! credential profile, and the live shell session. No live request is
//! issued: every field comes from state the shell already holds, so the
//! card answers "what's my current state?" without a round trip.

use brain_db_sdk::BrainClient;

use crate::render::{
    SpaceInfo, ConnectionInfo, InfoCard, ServerInfo, ServerWelcomeFields, SessionInfo,
};
use crate::session::Session;

/// Assemble the diagnostic card from cached state. Pure: no wire I/O.
/// `profile` is the name of the credential profile this connection used,
/// if any (the bare-token path has no profile name).
#[must_use]
pub fn collect(client: &BrainClient, session: &Session, profile: Option<&str>) -> InfoCard {
    let snapshot = client.connection();

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

    let space = SpaceInfo {
        // Server-assigned space id, read off the handshake.
        space_id: client.space_id(),
        // Server-derived tenant namespace; the shell only reflects it
        // (it never sends a namespace). Empty == system namespace.
        namespace: client.namespace().to_string(),
        // The local credential profile in play, if any.
        profile: profile.map(str::to_owned),
    };

    let connection = ConnectionInfo {
        authenticated: session.authenticated,
        connected_at_unix_nanos: None,
    };

    let session_info = SessionInfo {
        output: session.output.short_name().to_string(),
        sticky_session: session.sticky_session,
        active_txn: session.active_txn.map(|b| crate::parser::format_txn_id(&b)),
        timing: session.timing,
    };

    InfoCard {
        server,
        space,
        connection,
        session: session_info,
    }
}
