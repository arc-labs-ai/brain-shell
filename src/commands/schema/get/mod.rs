//! `schema get <namespace> [--version N]` — show a namespace's active (or
//! a specific) schema version. `--version 0` (the default) resolves to the
//! active version.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::SchemaGetRequest;
use brain_db_sdk::BrainClient;

use crate::commands::Rendered;
use crate::parser::SchemaGetArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: SchemaGetArgs,
) -> Result<Rendered, BrainError> {
    let resp = client
        .get_schema(&SchemaGetRequest {
            namespace: args.namespace,
            version: args.version,
        })
        .await?;

    let has_source = !resp.schema_document.is_empty();
    Ok(Box::new(AdHocTable {
        headers: vec![
            "namespace".into(),
            "version".into(),
            "validator".into(),
            "has_source".into(),
        ],
        rows: vec![vec![
            resp.namespace,
            resp.schema_version.to_string(),
            resp.validator_version.to_string(),
            if has_source { "yes" } else { "no" }.into(),
        ]],
    }))
}
