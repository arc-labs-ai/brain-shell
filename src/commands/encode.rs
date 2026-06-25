//! `encode` verb.
//!
//! Source modes (in precedence order — clap enforces mutual exclusivity
//! at parse time):
//!   * `--from-file <path>` (path `-` = stdin).
//!   * `--from-stdin` (shortcut for `--from-file -`).
//!   * positional `TEXT`.
//!
//! The `--wait*` flags parse for forward compatibility but do nothing in
//! this build: they need a SUBSCRIBE stream to observe background stages,
//! and the streaming path lands with the interactive REPL. The encode
//! itself is unaffected.

use std::io::Read;

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{EncodeRequest, EncodeResponse};
use brain_db_sdk::BrainClient;
use uuid::Uuid;

use crate::core::{MemoryId, RequestId};
use crate::parser::{parse_txn_id, EncodeArgs};
use crate::render::EncodeRendered;
use crate::session::Session;

use super::Rendered;

/// Send an `ENCODE`. Inherits the session's active txn + sticky context
/// when the caller didn't override them. Pushes the resulting memory id
/// onto the recent-id list.
pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: EncodeArgs,
) -> Result<Rendered, BrainError> {
    let text = resolve_source_text(&args)?;

    if args.wait || args.wait_auto_edges_ms > 0 {
        eprintln!(
            "note: --wait / --wait-auto-edges-ms observe background stages over a subscribe \
             stream, which lands with the interactive REPL; the encode proceeds without waiting."
        );
    }

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

    let rendered = EncodeRendered::new(resp).with_source(text);
    Ok(Box::new(rendered))
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
