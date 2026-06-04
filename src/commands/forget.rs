//! `forget` verb.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::ForgetRequest;
use brain_db_sdk::BrainClient;

use crate::parser::{parse_txn_id, ForgetArgs};
use crate::session::Session;

use super::Rendered;

/// Send a `FORGET`. Uses `--txn` when given, else the session's active
/// txn. The response renders directly (`ForgetResponse` implements
/// `Render`).
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: ForgetArgs,
) -> Result<Rendered, BrainError> {
    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(client_side)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);

    let request = ForgetRequest {
        memory_id: args.id.0.raw(),
        mode: args.mode.into_wire(),
        request_id: brain_db_sdk::new_id(),
        txn_id: txn,
    };
    let resp = client.forget(&request).await?;
    Ok(Box::new(resp))
}

/// Wrap a shell-side validation failure as a `BrainError` the dispatch
/// loop renders as a client-side error card.
fn client_side(message: String) -> BrainError {
    BrainError::Protocol(message)
}
