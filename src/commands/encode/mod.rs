//! `encode` verb.
//!
//! Source modes (in precedence order — clap enforces mutual exclusivity
//! at parse time):
//!   * `--from-file <path>` (path `-` = stdin).
//!   * `--from-stdin` (shortcut for `--from-file -`).
//!   * positional `TEXT`.
//!
//! `--wait` blocks after the ENCODE ack until every background stage the
//! write queued (auto_edge, temporal_edge, extractor) reports back, then
//! amends the card with one line per stage. `--wait-auto-edges-ms N`
//! additionally lists the individual auto-edges the AutoEdgeWorker lands
//! in an N-millisecond window. Both observe the server's SUBSCRIBE push
//! stream, replayed from the encode's LSN so a stage that finishes in the
//! gap between ack and subscribe isn't missed.

use std::io::Read;
use std::time::{Duration, Instant};

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    EdgeKindWire, EncodeRequest, EncodeResponse, EventType, StageAuditStatus, StageKind,
    StageOutcome, StagePayload, SubscribeRequest, SubscriptionEvent, SubscriptionFilter,
};
use brain_db_sdk::BrainClient;
use uuid::Uuid;

use crate::core::{MemoryId, RequestId};
use crate::parser::{parse_txn_id, EncodeArgs};
use crate::render::{
    AutoEdgeSummary, AutoEdgesDelta, EncodeRendered, StageKindLabel, StageOutcomeLabel,
    StageResult, StageResultsDelta,
};
use crate::session::Session;

use super::Rendered;

/// Upper bound on how long `--wait` blocks for the queued stages. The
/// background workers cycle on the order of 100 ms; 10 s is generous
/// headroom for the extractor (which may call an LLM) while still
/// guaranteeing the command returns. Stages that miss it render under a
/// "timed out waiting for" line rather than hanging.
const STAGES_TIMEOUT: Duration = Duration::from_secs(10);

/// Send an `ENCODE`. Inherits the session's active txn + sticky context
/// when the caller didn't override them. Pushes the resulting memory id
/// onto the recent-id list. When `--wait*` is set and the write actually
/// hit the log, blocks on the SUBSCRIBE stream to amend the card with
/// background-stage outcomes.
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: EncodeArgs,
) -> Result<Rendered, BrainError> {
    let text = resolve_source_text(&args)?;
    let request_id = parse_request_id(args.request_id.as_deref())?;

    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(client_side)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);
    let context_id = session.effective_context(args.context);

    // Memory kind, salience, edges, and near-duplicate consolidation are all
    // decided server-side — the client supplies only the text and routing
    // metadata. There is no client knob for any of them.
    let request = EncodeRequest {
        text: text.clone(),
        context_id,
        request_id: request_id.map(RequestId::to_bytes).unwrap_or_else(new_id),
        txn_id: txn,
        occurred_at_unix_nanos: None,
    };

    let resp: EncodeResponse = client.encode(&request).await?;
    session.push_recent_id(MemoryId::from_raw(resp.memory_id));

    let auto_edge_window = (args.wait_auto_edges_ms > 0)
        .then(|| Duration::from_millis(u64::from(args.wait_auto_edges_ms)));
    let mut rendered = EncodeRendered::new(resp.clone()).with_source(text);

    if args.wait || auto_edge_window.is_some() {
        match watch_plan(&resp) {
            WatchPlan::Skip(reason) => {
                eprintln!("note: --wait* has nothing to observe — {reason}.");
            }
            WatchPlan::Watch => {
                let (stages, auto_edges) =
                    watch_after_encode(client, &resp, args.wait, auto_edge_window).await;
                if let Some(stages) = stages {
                    rendered = rendered.with_stage_results(stages);
                }
                if let Some(auto_edges) = auto_edges {
                    rendered = rendered.with_auto_edges_delta(auto_edges);
                }
            }
        }
    }

    Ok(Box::new(rendered))
}

/// Whether watching the post-encode stream can produce anything.
enum WatchPlan {
    Watch,
    Skip(&'static str),
}

/// A dedup hit did no write (no stages, no edges); a txn-buffered write
/// has an LSN of 0 and won't broadcast until commit — waiting on either
/// would only burn the deadline and return nothing.
fn watch_plan(resp: &EncodeResponse) -> WatchPlan {
    if resp.was_deduplicated {
        WatchPlan::Skip("the write was a dedup hit, so no background work runs")
    } else if resp.lsn == 0 {
        WatchPlan::Skip("the write is buffered in a txn until commit")
    } else {
        WatchPlan::Watch
    }
}

/// Observe the SUBSCRIBE stream after an encode and collect the requested
/// deltas. Returns `(stage_results, auto_edges)`; either is `None` when
/// the corresponding flag wasn't set.
///
/// The subscription replays from `resp.lsn + 1` (the position right after
/// the encode) so stages that completed in the gap between the ack and
/// this subscribe are still delivered. Events are matched to this memory
/// client-side because the wire filter has no per-memory selector.
async fn watch_after_encode(
    client: &BrainClient,
    resp: &EncodeResponse,
    wait_stages: bool,
    auto_edge_window: Option<Duration>,
) -> (Option<StageResultsDelta>, Option<AutoEdgesDelta>) {
    let pending = &resp.pending_stages;

    // Nothing queued and no edge window: build an empty (but honest)
    // stage delta without opening a stream.
    if wait_stages && pending.is_empty() && auto_edge_window.is_none() {
        return (
            Some(StageResultsDelta {
                elapsed_ms: 0,
                results: Vec::new(),
                timed_out: Vec::new(),
            }),
            None,
        );
    }

    let request = SubscribeRequest {
        filter: SubscriptionFilter {
            contexts: Some(vec![resp.context_id]),
            kinds: None,
            similar_to: None,
            // Nil agent is the anonymous/test path; a Some([nil]) filter
            // would exclude everything, so leave it open in that case.
            agents: (resp.agent_id != [0u8; 16]).then(|| vec![resp.agent_id]),
        },
        include_history: true,
        from_lsn: Some(resp.lsn.saturating_add(1)),
        max_inflight: 0,
    };

    let mut subscription = match client.subscribe(&request).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("note: --wait* could not open the event stream: {e}");
            return (None, None);
        }
    };

    let deadline = {
        let stages = if wait_stages {
            STAGES_TIMEOUT
        } else {
            Duration::ZERO
        };
        stages.max(auto_edge_window.unwrap_or(Duration::ZERO))
    };

    let started = Instant::now();
    let mut results: Vec<StageResult> = Vec::new();
    let mut seen_stages: Vec<StageKind> = Vec::new();
    let mut auto_edges: Vec<AutoEdgeSummary> = Vec::new();

    loop {
        // Early exit: all queued stages reported and we aren't also
        // holding the stream open for the auto-edge window.
        if wait_stages
            && auto_edge_window.is_none()
            && pending.iter().all(|s| seen_stages.contains(s))
        {
            break;
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, subscription.next()).await {
            Ok(Some(Ok(event))) => {
                if event.memory_id != resp.memory_id {
                    continue;
                }
                if wait_stages {
                    if let Some(res) = stage_result_from_event(&event) {
                        if let Some(kind) = stage_kind_of(&event) {
                            if !seen_stages.contains(&kind) {
                                seen_stages.push(kind);
                                results.push(res);
                            }
                        }
                    }
                }
                if auto_edge_window.is_some() {
                    if let Some(edge) = auto_edge_from_event(&event) {
                        auto_edges.push(edge);
                    }
                }
            }
            // Stream error or close: stop and report what we have.
            Ok(Some(Err(_)) | None) => break,
            // Deadline reached.
            Err(_) => break,
        }
    }

    let _ = subscription.unsubscribe().await;
    drop(subscription);

    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let stage_delta = wait_stages.then(|| {
        let timed_out = pending
            .iter()
            .filter(|s| !seen_stages.contains(s))
            .map(|s| label_for_kind(*s))
            .collect();
        StageResultsDelta {
            elapsed_ms,
            results,
            timed_out,
        }
    });
    let auto_delta = auto_edge_window.map(|_| AutoEdgesDelta {
        elapsed_ms,
        edges: auto_edges,
    });

    (stage_delta, auto_delta)
}

/// The `StageKind` a `StageCompleted` event carries, or `None` if the
/// event isn't a stage completion.
fn stage_kind_of(event: &SubscriptionEvent) -> Option<StageKind> {
    if event.event_type != EventType::StageCompleted {
        return None;
    }
    event.stage_kind
}

/// Map a `StageCompleted` event into the render layer's stage result.
/// Returns `None` for non-stage events.
fn stage_result_from_event(event: &SubscriptionEvent) -> Option<StageResult> {
    let kind = stage_kind_of(event)?;
    let outcome = outcome_label(event.stage_outcome.unwrap_or(StageOutcome::Empty));
    let summary = stage_summary(event, outcome);
    Some(StageResult {
        kind: label_for_kind(kind),
        outcome,
        summary,
    })
}

/// One-line human summary of a stage, derived from its typed payload.
/// The extractor wording mirrors the count phrasing the render layer's
/// `augment_extractor_summary` keys off ("N statements, N relations").
fn stage_summary(event: &SubscriptionEvent, outcome: StageOutcomeLabel) -> String {
    match &event.stage_payload {
        Some(StagePayload::AutoEdge(p)) => edge_summary(p.edges_written, "SimilarTo"),
        Some(StagePayload::TemporalEdge(p)) => edge_summary(p.edges_written, "FollowedBy"),
        Some(StagePayload::Extractor(p)) => {
            let base = format!(
                "{} {}, {} {}, {} {} · {}",
                p.entity_count,
                plural(p.entity_count, "entity", "entities"),
                p.statement_count,
                plural(p.statement_count, "statement", "statements"),
                p.relation_count,
                plural(p.relation_count, "relation", "relations"),
                audit_word(p.audit_status),
            );
            if p.audit_status == StageAuditStatus::Failed && !p.error_message.is_empty() {
                format!("{base} — {}", p.error_message)
            } else {
                base
            }
        }
        // No typed payload: fall back to the outcome word so the row
        // still says something truthful.
        None => match outcome {
            StageOutcomeLabel::Ok => "completed".into(),
            StageOutcomeLabel::Empty => "nothing to add".into(),
            StageOutcomeLabel::Failed => "failed".into(),
        },
    }
}

/// `"N <kind> edges written"` / `"no <kind> edges"` for the two edge
/// stages, matching the phrasing the render tests assert on.
fn edge_summary(count: u32, kind: &str) -> String {
    if count == 0 {
        format!("no {kind} edges (below threshold or zero-vector)")
    } else {
        format!("{count} {kind} {} written", plural(count, "edge", "edges"))
    }
}

/// Pull an auto-derived edge out of an `EdgeAdded` event. Returns `None`
/// for non-edge events and for explicit (non-auto) edges — the watcher
/// only reports the ones the AutoEdgeWorker derived.
fn auto_edge_from_event(event: &SubscriptionEvent) -> Option<AutoEdgeSummary> {
    if event.event_type != EventType::EdgeAdded {
        return None;
    }
    let payload = event.edge_payload.as_ref()?;
    // origin: 0 = EXPLICIT, 1 = AUTO_DERIVED.
    if payload.origin != 1 {
        return None;
    }
    Some(AutoEdgeSummary {
        // Wire ids are 16 big-endian bytes; the render layer wants the
        // u128 form (matches `fmt_id`'s `0x{raw:032x}`).
        target: u128::from_be_bytes(payload.to_id),
        kind: edge_kind_name(payload.edge_kind_byte),
        weight: payload.weight,
    })
}

/// Render-side label for a wire `StageKind`.
fn label_for_kind(kind: StageKind) -> StageKindLabel {
    match kind {
        StageKind::AutoEdge => StageKindLabel::AutoEdge,
        StageKind::TemporalEdge => StageKindLabel::TemporalEdge,
        StageKind::Extractor => StageKindLabel::Extractor,
    }
}

/// Render-side label for a wire `StageOutcome`.
fn outcome_label(outcome: StageOutcome) -> StageOutcomeLabel {
    match outcome {
        StageOutcome::Ok => StageOutcomeLabel::Ok,
        StageOutcome::Empty => StageOutcomeLabel::Empty,
        StageOutcome::Failed => StageOutcomeLabel::Failed,
    }
}

/// Human word for an extractor audit verdict.
fn audit_word(status: StageAuditStatus) -> &'static str {
    match status {
        StageAuditStatus::Succeeded => "succeeded",
        StageAuditStatus::PartiallyApplied => "partially applied",
        StageAuditStatus::Failed => "failed",
        StageAuditStatus::Skipped => "skipped",
    }
}

/// Name an edge kind from its wire byte, falling back to the raw byte
/// for any kind this client doesn't recognise (forward-compat).
fn edge_kind_name(byte: u8) -> String {
    let kind = match byte {
        0 => EdgeKindWire::Caused,
        1 => EdgeKindWire::FollowedBy,
        2 => EdgeKindWire::DerivedFrom,
        3 => EdgeKindWire::SimilarTo,
        4 => EdgeKindWire::Contradicts,
        5 => EdgeKindWire::Supports,
        6 => EdgeKindWire::References,
        7 => EdgeKindWire::PartOf,
        other => return format!("kind#{other}"),
    };
    crate::render::render::fmt_edge_kind(kind).to_string()
}

fn plural(n: u32, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// Pull text from whichever source the user picked. Errors when no
/// source resolves to a non-empty payload.
fn resolve_source_text(args: &EncodeArgs) -> Result<String, BrainError> {
    if args.from_stdin {
        return read_stdin();
    }
    if let Some(path) = &args.from_file {
        if path == "-" {
            return read_stdin();
        }
        if path.ends_with(".jsonl") {
            return Err(client_side(
                ".jsonl batching is not supported. Encode files line-by-line via the shell \
                 or use --from-stdin with a single payload."
                    .to_string(),
            ));
        }
        return std::fs::read_to_string(path).map_err(|e| client_side(format!("read {path}: {e}")));
    }
    match &args.text {
        Some(t) if !t.is_empty() => Ok(t.clone()),
        _ => Err(client_side(
            "encode requires a TEXT positional or one of --from-file / --from-stdin".to_string(),
        )),
    }
}

fn read_stdin() -> Result<String, BrainError> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| client_side(format!("read stdin: {e}")))?;
    Ok(s)
}

fn parse_request_id(arg: Option<&str>) -> Result<Option<RequestId>, BrainError> {
    let Some(s) = arg else { return Ok(None) };
    let uuid = Uuid::parse_str(s.trim())
        .map_err(|e| client_side(format!("bad --request-id `{s}`: {e}")))?;
    Ok(Some(RequestId(uuid)))
}

/// Mint a fresh 16-byte request id when the caller didn't supply one.
fn new_id() -> [u8; 16] {
    brain_db_sdk::new_id()
}

/// Wrap a shell-side validation failure as a `BrainError` the dispatch
/// loop renders as a client-side error card. `Protocol` is the closest
/// non-server variant for "the shell rejected your input before the wire".
fn client_side(message: String) -> BrainError {
    BrainError::Protocol(message)
}

#[cfg(test)]
mod tests;
