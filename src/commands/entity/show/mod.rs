//! `entity show <id|name>` — stacked-card view of one entity.
//!
//! Fetches the entity by id, then fans out to relation-list (from/to) and
//! statement-list (by subject) to fill the card's sections. The wire views
//! carry only ids on the far side of a relation, so the "other name"
//! columns fall back to the id string. `mentioned_in` stays empty — there
//! is no mention-list wire op. A non-id positional is resolved first via
//! ENTITY_RESOLVE when `--type <id>` is supplied; without a type, the name
//! case is rejected cleanly because the server's resolver needs a type hint.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    RelationListFromRequest, RelationListToRequest, RelationView, ResolutionOutcomeWire,
    StatementListRequest,
};
use brain_db_sdk::BrainClient;

use crate::commands::entity::resolve;
use crate::commands::typed_graph::{id_to_string, kind_label, object_label, parse_wire_id};
use crate::commands::Rendered;
use crate::parser::{EntityResolveArgs, EntityShowArgs};
use crate::render::{EntityCard, RelationSummary, StatementSummary};
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: EntityShowArgs,
) -> Result<Rendered, BrainError> {
    let entity_id = resolve_id(client, &args).await?;

    let resp = client
        .get_entity(&brain_db_sdk::wire::types::EntityGetRequest {
            entity_id,
            act_as: None,
        })
        .await?;
    let entity = resp.entity;

    let relations_out = client
        .list_relations_from(&RelationListFromRequest {
            act_as: None,
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
            act_as: None,
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
            act_as: None,
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

/// Turn the positional into an entity id. A valid UUID is used directly; a
/// non-id name is resolved via ENTITY_RESOLVE when `--type <id>` is given.
/// Without a type, the name case is a clean error (the resolver needs a hint).
async fn resolve_id(client: &BrainClient, args: &EntityShowArgs) -> Result<[u8; 16], BrainError> {
    if let Ok(id) = parse_wire_id(&args.id_or_name, "entity id") {
        return Ok(id);
    }

    let type_id = args.type_id.ok_or_else(|| {
        BrainError::Protocol(
            "entity show by name needs --type <id> to resolve; or pass the entity id".into(),
        )
    })?;

    let resp = resolve::resolve(
        client,
        &EntityResolveArgs {
            name: args.id_or_name.clone(),
            type_id,
            session: None,
            allow_create: false,
        },
    )
    .await?;

    match resp.outcome {
        ResolutionOutcomeWire::Resolved | ResolutionOutcomeWire::Created => {
            Ok(resp.resolved_entity)
        }
        ResolutionOutcomeWire::Ambiguous => Err(BrainError::Protocol(format!(
            "`{}` is ambiguous ({} candidates); show by id instead",
            args.id_or_name,
            resp.candidate_ids.len()
        ))),
        ResolutionOutcomeWire::NotFound => Err(BrainError::Protocol(format!(
            "no entity named `{}` for type {type_id}",
            args.id_or_name
        ))),
    }
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
