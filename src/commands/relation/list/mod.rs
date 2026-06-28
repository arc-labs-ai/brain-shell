//! `relation list [--from E] [--to E] [--type T]` — table.
//!
//! Dispatches to RELATION_LIST_FROM or RELATION_LIST_TO per the args.
//! `--from` alone lists outgoing relations; `--to` alone lists incoming;
//! both runs the `--from` query and filters the result to the `--to`
//! endpoint client-side (there is no cross-filter wire op). At least one of
//! `--from` / `--to` is required.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    RelationListFromRequest, RelationListToRequest, RelationView, WireUuid,
};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::{id_to_string, parse_wire_id};
use crate::commands::Rendered;
use crate::parser::RelationListArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: RelationListArgs,
) -> Result<Rendered, BrainError> {
    let from = match &args.from {
        Some(s) => Some(parse_wire_id(s, "from entity id")?),
        None => None,
    };
    let to = match &args.to {
        Some(s) => Some(parse_wire_id(s, "to entity id")?),
        None => None,
    };
    let type_filter = args.type_qname.clone().unwrap_or_default();

    let items: Vec<RelationView> = match (from, to) {
        (Some(from_entity), None) => {
            client
                .list_relations_from(&from_request(from_entity, type_filter, args.limit))
                .await?
        }
        (None, Some(to_entity)) => {
            client
                .list_relations_to(&to_request(to_entity, type_filter, args.limit))
                .await?
        }
        (Some(from_entity), Some(to_entity)) => {
            let all = client
                .list_relations_from(&from_request(from_entity, type_filter, args.limit))
                .await?;
            all.into_iter()
                .filter(|r| r.to_entity == to_entity)
                .collect()
        }
        (None, None) => {
            return Err(BrainError::Protocol(
                "relation list requires --from or --to (or both)".into(),
            ));
        }
    };

    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|r| {
            vec![
                id_to_string(&r.relation_id),
                r.relation_type.clone(),
                id_to_string(&r.from_entity),
                id_to_string(&r.to_entity),
                format!("{:.2}", r.confidence),
            ]
        })
        .collect();

    Ok(Box::new(AdHocTable {
        headers: vec![
            "id".into(),
            "type".into(),
            "from".into(),
            "to".into(),
            "conf".into(),
        ],
        rows,
    }))
}

fn from_request(from_entity: WireUuid, type_filter: String, limit: u32) -> RelationListFromRequest {
    RelationListFromRequest {
        from_entity,
        relation_type_filter: type_filter,
        time_range_start_unix_nanos: 0,
        time_range_end_unix_nanos: 0,
        include_superseded: false,
        include_tombstoned: false,
        limit,
        cursor: Vec::new(),
    }
}

fn to_request(to_entity: WireUuid, type_filter: String, limit: u32) -> RelationListToRequest {
    RelationListToRequest {
        to_entity,
        relation_type_filter: type_filter,
        time_range_start_unix_nanos: 0,
        time_range_end_unix_nanos: 0,
        include_superseded: false,
        include_tombstoned: false,
        limit,
        cursor: Vec::new(),
    }
}
