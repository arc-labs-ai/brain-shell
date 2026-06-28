//! `schema upload --from-file <PATH>` — additively expand the active
//! schema with the declarations in a `.brain` DSL file.
//!
//! The upload merges on top of whatever is already active (the system
//! `brain:` namespace plus any earlier user uploads). On a validation
//! failure the server returns the issue list instead of persisting; we
//! render those rows so the operator can fix the source and retry.

use brain_db_sdk::wire::types::SchemaUploadRequest;
use brain_db_sdk::{new_id, BrainClient, BrainError};

use crate::commands::Rendered;
use crate::parser::SchemaUploadArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: SchemaUploadArgs,
) -> Result<Rendered, BrainError> {
    let source = super::read_source(&args.from_file)?;
    let request = SchemaUploadRequest {
        schema_document: source,
        dry_run: false,
        allow_breaking: false,
        request_id: new_id(),
    };
    let resp = client.upload_schema(&request).await?;

    // A non-empty issue list means the server rejected the upload —
    // surface every diagnostic so the operator can fix the source.
    if !resp.validation_errors.is_empty() {
        let rows: Vec<Vec<String>> = resp
            .validation_errors
            .iter()
            .map(|e| {
                vec![
                    e.code.clone(),
                    format!("{}:{}", e.line, e.column),
                    e.message.clone(),
                ]
            })
            .collect();
        return Ok(Box::new(AdHocTable {
            headers: vec!["code".into(), "loc".into(), "message".into()],
            rows,
        }));
    }

    Ok(Box::new(AdHocTable {
        headers: vec!["namespace".into(), "active_version".into(), "status".into()],
        rows: vec![vec![
            resp.namespace,
            resp.schema_version.to_string(),
            "merged".into(),
        ]],
    }))
}
