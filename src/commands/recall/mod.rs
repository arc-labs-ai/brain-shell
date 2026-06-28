//! `recall` verb.
//!
//! With `--include-graph`, per-hit knowledge enrichment (mentioned
//! entities + sourced statements + incident relations) rides inline on
//! each `MemoryResult.graph` field; the renderer consumes the wire shape
//! directly. RECALL streams its results — the SDK's `recall` collects
//! every `RECALL_RESP` frame and flattens them into one ordered list,
//! which is exactly the shape the result-list renderer wants.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::RecallRequest;
use brain_db_sdk::BrainClient;

use crate::core::MemoryId;
use crate::parser::{parse_txn_id, RecallArgs};
use crate::render::RecallResults;
use crate::session::Session;

use super::Rendered;

/// Send a `RECALL` and render the memories the smart router returned.
/// Pushes every returned id onto the session's recent-id list.
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: RecallArgs,
) -> Result<Rendered, BrainError> {
    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(client_side)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);

    let context_filter = if args.filter_context.is_empty() {
        None
    } else {
        Some(args.filter_context.clone())
    };

    let agent_filter = args
        .filter_agent
        .iter()
        .map(|s| parse_agent_id(s))
        .collect::<Result<Vec<_>, _>>()?;

    let kind_filter = if args.filter_kind.is_empty() {
        None
    } else {
        Some(args.filter_kind.iter().map(|k| k.into_wire()).collect())
    };

    let age_bound_unix_nanos = args.max_age_seconds.map(|secs| {
        // The wire field is an absolute lower-bound timestamp ("keep
        // memories created at or after this point"). Compute it from the
        // client's clock; nanos because that's the wire unit.
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(u64::MAX);
        now_nanos.saturating_sub(secs.saturating_mul(1_000_000_000))
    });

    let request = RecallRequest {
        cue_text: args.query,
        // Empty subject: the server resolves subject + relation from the cue.
        subject_name: String::new(),
        max_results: args.max_results,
        confidence_threshold: args.confidence,
        context_filter,
        age_bound_unix_nanos,
        as_of_record_time_unix_nanos: None,
        kind_filter,
        salience_floor: args.salience_floor,
        include_edges: args.include_edges,
        include_graph: args.include_graph,
        include_text: args.include_text,
        request_id: Some(brain_db_sdk::new_id()),
        txn_id: txn,
        agent_filter,
        include_other_agents: args.include_other_agents,
    };

    // The smart router returns the memories it chose; `answer_kind` is an
    // internal shape signal the shell renders as a memory list.
    let answer = client.recall(&request).await?;
    for m in answer.memories() {
        session.push_recent_id(MemoryId::from_raw(m.memory_id));
    }

    Ok(Box::new(RecallResults(answer.memories)))
}

/// Parse an agent-id string into a raw 16-byte wire id. Accepts the
/// hyphenated UUID form (`0191b6f0-1234-7890-abcd-ef0123456789`) and the
/// bare 32-hex form (`0191b6f012347890abcdef0123456789`).
fn parse_agent_id(s: &str) -> Result<[u8; 16], BrainError> {
    let s = s.trim();
    uuid::Uuid::parse_str(s)
        .map(|u| *u.as_bytes())
        .map_err(|e| client_side(format!("invalid --filter-agent value `{s}`: {e}")))
}

/// Wrap a shell-side validation failure as a `BrainError` the dispatch
/// loop renders as a client-side error card.
fn client_side(message: String) -> BrainError {
    BrainError::Protocol(message)
}
