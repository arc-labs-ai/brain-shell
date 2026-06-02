//! `entity list [--type N] [--prefix P] [--limit N]` — paginated table.
//!
//! The wire op filters by a numeric `entity_type_id`, not a type qname:
//! the shell has no schema-side qname→id map, so `--type` here is the
//! numeric type id. A non-numeric value is rejected with a clear note
//! rather than silently ignored.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::EntityListRequest;
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::id_to_string;
use crate::commands::Rendered;
use crate::parser::EntityListArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: EntityListArgs,
) -> Result<Rendered, BrainError> {
    let request = build_request(&args)?;
    let items = client.list_entities(&request).await?;

    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            let e = &item.entity;
            vec![
                id_to_string(&e.entity_id),
                e.canonical_name.clone(),
                e.entity_type_id.to_string(),
                e.mention_count.to_string(),
            ]
        })
        .collect();

    Ok(Box::new(AdHocTable {
        headers: vec![
            "id".into(),
            "name".into(),
            "type_id".into(),
            "mentions".into(),
        ],
        rows,
    }))
}

/// Build the wire request from parsed args. `--type` must be a numeric
/// entity type id; `0` (the wire's "no filter" sentinel) is used when the
/// flag is absent.
fn build_request(args: &EntityListArgs) -> Result<EntityListRequest, BrainError> {
    let entity_type_id = match &args.type_qname {
        None => 0,
        Some(s) => s.trim().parse::<u32>().map_err(|_| {
            BrainError::Protocol(format!(
                "entity list --type expects a numeric entity type id (no qname→id map in this build); got `{s}`"
            ))
        })?,
    };
    Ok(EntityListRequest {
        entity_type_id,
        name_prefix: args.prefix.clone().unwrap_or_default(),
        mention_count_min: 0,
        include_tombstoned: false,
        include_merged: false,
        limit: args.limit,
        cursor: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_maps_filters() {
        let args = EntityListArgs {
            type_qname: Some("3".into()),
            limit: 25,
            prefix: Some("Pri".into()),
        };
        let req = build_request(&args).unwrap();
        assert_eq!(req.entity_type_id, 3);
        assert_eq!(req.name_prefix, "Pri");
        assert_eq!(req.limit, 25);
    }

    #[test]
    fn build_request_no_type_is_zero() {
        let args = EntityListArgs {
            type_qname: None,
            limit: 50,
            prefix: None,
        };
        let req = build_request(&args).unwrap();
        assert_eq!(req.entity_type_id, 0);
        assert!(req.name_prefix.is_empty());
    }

    #[test]
    fn build_request_rejects_non_numeric_type() {
        let args = EntityListArgs {
            type_qname: Some("Person".into()),
            limit: 50,
            prefix: None,
        };
        assert!(matches!(build_request(&args), Err(BrainError::Protocol(_))));
    }
}
