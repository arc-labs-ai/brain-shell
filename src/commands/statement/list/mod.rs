//! `statement list [--subject E] [--predicate P] [--object E]` — table.
//!
//! The wire op filters by subject + predicate; there is no server-side
//! object filter, so `--object` is applied client-side over the returned
//! rows (matching only entity-reference objects).

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{StatementListRequest, StatementView};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::{
    id_to_string, kind_label, object_entity_id, object_label, parse_wire_id,
};
use crate::commands::Rendered;
use crate::parser::StatementListArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: StatementListArgs,
) -> Result<Rendered, BrainError> {
    let subject = match &args.subject {
        Some(s) => parse_wire_id(s, "subject id")?,
        None => [0u8; 16],
    };
    let object_filter = match &args.object {
        Some(s) => Some(parse_wire_id(s, "object id")?),
        None => None,
    };

    let request = StatementListRequest {
        act_as: None,
        subject,
        predicate: args.predicate.clone().unwrap_or_default(),
        kind: 0,
        min_confidence: 0.0,
        time_range_start_unix_nanos: 0,
        time_range_end_unix_nanos: 0,
        only_current: false,
        include_tombstoned: false,
        limit: args.limit,
        cursor: Vec::new(),
    };
    let mut items = client.list_statements(&request).await?;

    if let Some(obj) = object_filter {
        let obj_str = id_to_string(&obj);
        items.retain(|s| object_entity_id(&s.object).as_deref() == Some(obj_str.as_str()));
    }

    let rows: Vec<Vec<String>> = items.iter().map(format_row).collect();
    Ok(Box::new(AdHocTable {
        headers: vec![
            "id".into(),
            "kind".into(),
            "subject".into(),
            "predicate".into(),
            "object".into(),
            "conf".into(),
        ],
        rows,
    }))
}

fn format_row(s: &StatementView) -> Vec<String> {
    vec![
        id_to_string(&s.statement_id),
        kind_label(s.kind).to_string(),
        id_to_string(&s.subject),
        s.predicate.clone(),
        object_label(&s.object),
        format!("{:.2}", s.confidence),
    ]
}
