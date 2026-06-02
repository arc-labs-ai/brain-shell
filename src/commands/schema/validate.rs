//! `schema validate --from-file <PATH>` — parse + validate a `.brain` DSL
//! file without persisting it. A clean validation reports the namespace and
//! the version it would become; otherwise every diagnostic is surfaced as a
//! row so the operator can fix the source.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::SchemaValidateRequest;
use brain_db_sdk::BrainClient;

use crate::commands::Rendered;
use crate::parser::SchemaValidateArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: SchemaValidateArgs,
) -> Result<Rendered, BrainError> {
    let source = super::read_source(&args.from_file)?;
    let resp = client
        .validate_schema(&SchemaValidateRequest {
            schema_document: source,
        })
        .await?;

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
        headers: vec![
            "namespace".into(),
            "would_be_version".into(),
            "status".into(),
        ],
        rows: vec![vec![
            resp.namespace,
            resp.would_be_version.to_string(),
            "valid".into(),
        ]],
    }))
}
