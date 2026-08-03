//! `entity neighbors <id> [--depth N]` — termtree neighborhood.
//!
//! Builds a one-hop tree rooted at the entity from its outgoing and
//! incoming relations. Deeper traversal would need a per-hop fan-out the
//! wire ops don't batch, so `--depth` is accepted but only the first hop is
//! materialized; the root label notes the requested depth. Far-side labels
//! are entity ids — the relation views carry no canonical name.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{RelationListFromRequest, RelationListToRequest};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::{id_to_string, parse_wire_id};
use crate::commands::Rendered;
use crate::parser::EntityNeighborsArgs;
use crate::render::{GraphNode, GraphTree};
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: EntityNeighborsArgs,
) -> Result<Rendered, BrainError> {
    let root_id = parse_wire_id(&args.id, "entity id")?;

    let out = client
        .list_relations_from(&RelationListFromRequest {
            act_as: None,
            from_entity: root_id,
            relation_type_filter: String::new(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: false,
            include_tombstoned: false,
            limit: 100,
            cursor: Vec::new(),
        })
        .await?;
    let into = client
        .list_relations_to(&RelationListToRequest {
            act_as: None,
            to_entity: root_id,
            relation_type_filter: String::new(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: false,
            include_tombstoned: false,
            limit: 100,
            cursor: Vec::new(),
        })
        .await?;

    let mut children: Vec<GraphNode> = Vec::with_capacity(out.len() + into.len());
    for r in &out {
        children.push(GraphNode {
            label: format!("--[{}]--> {}", r.relation_type, id_to_string(&r.to_entity)),
            children: Vec::new(),
        });
    }
    for r in &into {
        children.push(GraphNode {
            label: format!(
                "{} --[{}]-->",
                id_to_string(&r.from_entity),
                r.relation_type
            ),
            children: Vec::new(),
        });
    }
    if children.is_empty() {
        children.push(GraphNode {
            label: "(no relations)".into(),
            children: Vec::new(),
        });
    }

    let root = GraphNode {
        label: format!("{}  (depth {})", id_to_string(&root_id), args.depth),
        children,
    };
    Ok(Box::new(GraphTree(root)))
}
