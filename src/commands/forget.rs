//! `forget` verb.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::ForgetRequest;
use brain_db_sdk::BrainClient;

use crate::parser::ForgetArgs;
use crate::session::Session;

use super::Rendered;

/// Send a `FORGET`. Inherits the session's active txn. The response
/// renders directly (`ForgetResponse` implements `Render`).
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: ForgetArgs,
) -> Result<Rendered, BrainError> {
    let request = ForgetRequest {
        memory_id: args.id.0.raw(),
        mode: args.mode.into_wire(),
        request_id: brain_db_sdk::new_id(),
        txn_id: session.active_txn,
    };
    let resp = client.forget(&request).await?;
    Ok(Box::new(resp))
}
