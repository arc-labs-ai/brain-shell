//! `plan` verb.
//!
//! PLAN streams `PLAN_RESP` frames; the terminal frame carries the
//! `plan_status` that the renderer's footer narrates (no-path, budget
//! exhausted, cancelled). We collect the raw frames so we can recover
//! both the flattened steps and that final status.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{PlanBudget, PlanRequest, PlanState};
use brain_db_sdk::BrainClient;

use crate::parser::{parse_txn_id, PlanArgs};
use crate::render::PlanSteps;
use crate::session::Session;

use super::Rendered;

pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: PlanArgs,
) -> Result<Rendered, BrainError> {
    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(client_side)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);

    let session_filter = if args.filter_session.is_empty() {
        None
    } else {
        Some(args.filter_session.clone())
    };

    let request = PlanRequest {
        act_as: None,
        trace: false,
        start: PlanState::ByText(args.from),
        goal: PlanState::ByText(args.to),
        budget: PlanBudget {
            max_steps: args.max_steps,
            max_wall_time_ms: args.max_wall_time_ms,
            max_branches_explored: args.max_branches,
        },
        strategy_hint: args.strategy.map(|s| s.into_wire()),
        session_filter,
        request_id: Some(brain_db_sdk::new_id()),
        txn_id: txn,
    };

    let frames = client.plan_frames(&request).await?;
    let mut steps = Vec::new();
    let mut status = None;
    for frame in frames {
        steps.extend(frame.steps);
        if let Some(s) = frame.plan_status {
            status = Some(s);
        }
    }
    Ok(Box::new(PlanSteps { steps, status }))
}

/// Wrap a shell-side validation failure as a `BrainError` the dispatch
/// loop renders as a client-side error card.
fn client_side(message: String) -> BrainError {
    BrainError::Protocol(message)
}
