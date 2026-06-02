//! `plan` verb.
//!
//! PLAN streams `PLAN_RESP` frames; the terminal frame carries the
//! `plan_status` that the renderer's footer narrates (no-path, budget
//! exhausted, cancelled). We collect the raw frames so we can recover
//! both the flattened steps and that final status.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{PlanBudget, PlanRequest, PlanState};
use brain_db_sdk::BrainClient;

use crate::parser::PlanArgs;
use crate::render::PlanSteps;
use crate::session::Session;

use super::Rendered;

/// Default branch-exploration budget. PLAN's wire budget requires it but
/// the shell exposes only `--max-steps` / `--max-wall-time-ms`; this is a
/// generous ceiling that lets those two be the operator-facing knobs.
const DEFAULT_MAX_BRANCHES: u32 = 256;

pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: PlanArgs,
) -> Result<Rendered, BrainError> {
    let txn = session.effective_txn(None);

    let request = PlanRequest {
        start: PlanState::ByText(args.from),
        goal: PlanState::ByText(args.to),
        budget: PlanBudget {
            max_steps: args.max_steps,
            max_wall_time_ms: args.max_wall_time_ms,
            max_branches_explored: DEFAULT_MAX_BRANCHES,
        },
        strategy_hint: None,
        context_filter: None,
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
