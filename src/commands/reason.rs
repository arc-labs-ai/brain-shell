//! `reason` verb.
//!
//! REASON streams `REASON_RESP` frames; the renderer only needs the
//! flattened inference steps, so the SDK's `reason` (which collects and
//! flattens for us) is exactly the right shape.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{ObservationInput, ReasonRequest};
use brain_db_sdk::BrainClient;

use crate::parser::ReasonArgs;
use crate::render::ReasonSteps;
use crate::session::Session;

use super::Rendered;

/// Wall-time budget for a reasoning run, in milliseconds. The shell
/// exposes depth / confidence / max-inferences as the operator knobs;
/// the wire still requires a wall-time ceiling, so we apply a sane
/// default the way `plan` does for its branch budget.
const DEFAULT_BUDGET_WALL_TIME_MS: u32 = 5_000;

pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: ReasonArgs,
) -> Result<Rendered, BrainError> {
    let txn = session.effective_txn(None);

    let request = ReasonRequest {
        observation: ObservationInput::ByText(args.observation),
        depth: args.depth,
        confidence_threshold: args.confidence,
        context_filter: None,
        max_inferences: args.max_inferences,
        budget_wall_time_ms: DEFAULT_BUDGET_WALL_TIME_MS,
        request_id: Some(brain_db_sdk::new_id()),
        txn_id: txn,
    };

    let inferences = client.reason(&request).await?;
    Ok(Box::new(ReasonSteps(inferences)))
}
