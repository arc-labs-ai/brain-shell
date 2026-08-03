//! `link` verb.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::LinkRequest;
use brain_db_sdk::BrainClient;

use crate::parser::{parse_txn_id, LinkArgs};
use crate::render::LinkRendered;
use crate::session::Session;

use super::Rendered;

/// Create or reweight an edge between two memories. Inherits the
/// session's active txn unless `--txn` overrides it.
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: LinkArgs,
) -> Result<Rendered, BrainError> {
    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(BrainError::Protocol)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);

    let request = LinkRequest {
        act_as: None,
        source: args.src.0.raw(),
        target: args.tgt.0.raw(),
        kind: args.kind.into_wire(),
        weight: args.weight,
        request_id: brain_db_sdk::new_id(),
        txn_id: txn,
    };
    let resp = client.link(&request).await?;
    Ok(Box::new(LinkRendered(resp)))
}
