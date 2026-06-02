//! `entity show <id>` — stacked-card view of one entity.
//!
//! Fetches the entity by id, then fans out to relation-list (from/to) and
//! statement-list (by subject) to fill the card's sections. The wire views
//! carry only ids on the far side of a relation, so the "other name"
//! columns fall back to the id string. `mentioned_in` stays empty — there
//! is no mention-list wire op. `entity show <name>` is rejected cleanly:
//! there is no ENTITY_RESOLVE verb to map a name to an id.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    RelationListFromRequest, RelationListToRequest, RelationView, StatementListRequest,
};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::{id_to_string, kind_label, object_label, parse_wire_id};
use crate::commands::Rendered;
use crate::parser::EntityShowArgs;
use crate::render::{EntityCard, RelationSummary, StatementSummary};
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: EntityShowArgs,
) -> Result<Rendered, BrainError> {
    // `entity show` accepts only an id: there is no ENTITY_RESOLVE verb in
    // the SDK to turn a canonical name into an id, so a non-id argument is
    // a clean capability gap rather than a silent miss.
    let entity_id = parse_wire_id(&args.id_or_name, "entity id").map_err(|_| {
        BrainError::Protocol(
            "entity show by name needs the resolve op, not yet available; use the entity id".into(),
        )
    })?;

    let resp = client
        .get_entity(&brain_db_sdk::wire::types::EntityGetRequest { entity_id })
        .await?;
    let entity = resp.entity;

    let relations_out = client
        .list_relations_from(&RelationListFromRequest {
            from_entity: entity_id,
            relation_type_filter: String::new(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: false,
            include_tombstoned: false,
            limit: 100,
            cursor: Vec::new(),
        })
        .await?;
    let relations_in = client
        .list_relations_to(&RelationListToRequest {
            to_entity: entity_id,
            relation_type_filter: String::new(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: false,
            include_tombstoned: false,
            limit: 100,
            cursor: Vec::new(),
        })
        .await?;

    let statements = client
        .list_statements(&StatementListRequest {
            subject: entity_id,
            predicate: String::new(),
            kind: 0,
            min_confidence: 0.0,
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            only_current: false,
            include_tombstoned: false,
            limit: 100,
            cursor: Vec::new(),
        })
        .await?;

    let card = EntityCard {
        id: id_to_string(&entity.entity_id),
        canonical_name: entity.canonical_name,
        type_qname: entity.entity_type_id.to_string(),
        aliases: entity.aliases,
        statements: statements
            .iter()
            .map(|s| StatementSummary {
                id: id_to_string(&s.statement_id),
                kind: kind_label(s.kind).to_string(),
                predicate: s.predicate.clone(),
                object: object_label(&s.object),
                confidence: s.confidence,
            })
            .collect(),
        mentioned_in: Vec::new(),
        relations_out: relations_out
            .iter()
            .map(|r| relation_summary(r, r.to_entity))
            .collect(),
        relations_in: relations_in
            .iter()
            .map(|r| relation_summary(r, r.from_entity))
            .collect(),
    };
    Ok(Box::new(card))
}

/// Build a relation summary where `other` is the endpoint on the far side
/// from the entity being shown. The wire view has no canonical name for the
/// other endpoint, so the id stands in for the name.
fn relation_summary(r: &RelationView, other: [u8; 16]) -> RelationSummary {
    let other_id = id_to_string(&other);
    RelationSummary {
        other_id: other_id.clone(),
        other_name: other_id,
        predicate: r.relation_type.clone(),
    }
}
