//! `reason` verb.
//!
//! REASON streams `REASON_RESP` frames; the renderer only needs the
//! flattened inference steps, so the SDK's `reason` (which collects and
//! flattens for us) is exactly the right shape.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{ObservationInput, ReasonRequest};
use brain_db_sdk::BrainClient;

use crate::parser::{parse_txn_id, ReasonArgs};
use crate::render::ReasonSteps;
use crate::session::Session;

use super::Rendered;

pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: ReasonArgs,
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

    let request = ReasonRequest {
        act_as: None,
        trace: false,
        observation: ObservationInput::ByText(args.observation),
        depth: args.depth,
        confidence_threshold: args.confidence,
        session_filter,
        max_inferences: args.max_inferences,
        budget_wall_time_ms: args.budget_wall_time_ms,
        request_id: Some(brain_db_sdk::new_id()),
        txn_id: txn,
    };

    let inferences = client.reason(&request).await?;
    Ok(Box::new(ReasonSteps(inferences)))
}

/// Wrap a shell-side validation failure as a `BrainError` the dispatch
/// loop renders as a client-side error card.
fn client_side(message: String) -> BrainError {
    BrainError::Protocol(message)
}
