//! `unlink` verb.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::UnlinkRequest;
use brain_db_sdk::BrainClient;

use crate::parser::{parse_txn_id, UnlinkArgs};
use crate::render::UnlinkRendered;
use crate::session::Session;

use super::Rendered;

/// Remove the edge identified by `(source, kind, target)`. Idempotent:
/// removing a non-existent edge succeeds with `removed = false`.
/// Inherits the session's active txn unless `--txn` overrides it.
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: UnlinkArgs,
) -> Result<Rendered, BrainError> {
    let explicit_txn = match args.txn.as_deref() {
        Some(s) => Some(parse_txn_id(s).map_err(BrainError::Protocol)?),
        None => None,
    };
    let txn = session.effective_txn(explicit_txn);

    let request = UnlinkRequest {
        source: args.src.0.raw(),
        target: args.tgt.0.raw(),
        kind: args.kind.into_wire(),
        request_id: brain_db_sdk::new_id(),
        txn_id: txn,
    };
    let resp = client.unlink(&request).await?;
    Ok(Box::new(UnlinkRendered(resp)))
}
