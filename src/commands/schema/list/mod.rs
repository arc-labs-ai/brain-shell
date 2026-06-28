//! `schema list [namespace]` — list the versions stored for a namespace,
//! newest first.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::SchemaListRequest;
use brain_db_sdk::BrainClient;

use crate::commands::Rendered;
use crate::parser::SchemaListArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: SchemaListArgs,
) -> Result<Rendered, BrainError> {
    let namespace = args.namespace.clone();
    let items = client
        .list_schemas(&SchemaListRequest {
            namespace: namespace.clone(),
            limit: 0,
            cursor: Vec::new(),
        })
        .await?;

    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|e| {
            vec![
                namespace.clone(),
                e.schema_version.to_string(),
                e.validator_version.to_string(),
                if e.has_source_text { "yes" } else { "no" }.into(),
            ]
        })
        .collect();

    Ok(Box::new(AdHocTable {
        headers: vec![
            "namespace".into(),
            "version".into(),
            "validator".into(),
            "has_source".into(),
        ],
        rows,
    }))
}
