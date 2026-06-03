//! `entity resolve <name> --type <id>` — resolve a candidate name to an
//! entity id via ENTITY_RESOLVE.
//!
//! The server requires a non-zero type hint and resolves by exact canonical
//! name (tier 1). The outcome is rendered as a small table: the verdict and
//! tier/confidence, plus the resolved id when one was found, or the ranked
//! candidate ids when the resolve was ambiguous.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    EntityResolveRequest, EntityResolveResponse, ResolutionOutcomeWire,
};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::id_to_string;
use crate::commands::Rendered;
use crate::parser::EntityResolveArgs;
use crate::render::AdHocTable;
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: EntityResolveArgs,
) -> Result<Rendered, BrainError> {
    let resp = resolve(client, &args).await?;
    Ok(Box::new(render(&resp)))
}

/// Issue the ENTITY_RESOLVE request from parsed args.
pub(crate) async fn resolve(
    client: &BrainClient,
    args: &EntityResolveArgs,
) -> Result<EntityResolveResponse, BrainError> {
    client
        .resolve_entity(&EntityResolveRequest {
            candidate_name: args.name.clone(),
            context: args.context.clone().unwrap_or_default(),
            entity_type_hint: args.type_id,
            allow_create: args.allow_create,
            request_id: brain_db_sdk::new_id(),
        })
        .await
}

/// Short label for a resolution outcome.
fn outcome_label(outcome: ResolutionOutcomeWire) -> &'static str {
    match outcome {
        ResolutionOutcomeWire::Resolved => "resolved",
        ResolutionOutcomeWire::Created => "created",
        ResolutionOutcomeWire::Ambiguous => "ambiguous",
        ResolutionOutcomeWire::NotFound => "not_found",
    }
}

/// Build the outcome table. Resolved/Created surface the single id; Ambiguous
/// lists the ranked candidate ids; NotFound has neither.
fn render(resp: &EntityResolveResponse) -> AdHocTable {
    let mut rows = vec![
        vec!["outcome".into(), outcome_label(resp.outcome).to_string()],
        vec!["tier".into(), resp.tier.to_string()],
        vec!["confidence".into(), format!("{:.3}", resp.confidence)],
    ];

    match resp.outcome {
        ResolutionOutcomeWire::Resolved | ResolutionOutcomeWire::Created => {
            rows.push(vec![
                "entity_id".into(),
                id_to_string(&resp.resolved_entity),
            ]);
        }
        ResolutionOutcomeWire::Ambiguous => {
            for (i, id) in resp.candidate_ids.iter().enumerate() {
                rows.push(vec![format!("candidate[{i}]"), id_to_string(id)]);
            }
        }
        ResolutionOutcomeWire::NotFound => {}
    }

    AdHocTable {
        headers: vec!["field".into(), "value".into()],
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(
        outcome: ResolutionOutcomeWire,
        resolved: [u8; 16],
        candidates: Vec<[u8; 16]>,
    ) -> EntityResolveResponse {
        EntityResolveResponse {
            outcome,
            tier: 1,
            confidence: 1.0,
            resolved_entity: resolved,
            candidate_ids: candidates,
            audit_id: [0; 16],
        }
    }

    #[test]
    fn resolved_shows_single_id() {
        let table = render(&resp(
            ResolutionOutcomeWire::Resolved,
            [7u8; 16],
            Vec::new(),
        ));
        assert!(table.rows.iter().any(|r| r[0] == "entity_id"));
        assert!(table
            .rows
            .iter()
            .any(|r| r[0] == "outcome" && r[1] == "resolved"));
    }

    #[test]
    fn ambiguous_lists_candidates() {
        let table = render(&resp(
            ResolutionOutcomeWire::Ambiguous,
            [0; 16],
            vec![[1u8; 16], [2u8; 16]],
        ));
        assert_eq!(
            table
                .rows
                .iter()
                .filter(|r| r[0].starts_with("candidate"))
                .count(),
            2
        );
    }

    #[test]
    fn not_found_has_no_id_rows() {
        let table = render(&resp(ResolutionOutcomeWire::NotFound, [0; 16], Vec::new()));
        assert!(!table.rows.iter().any(|r| r[0] == "entity_id"));
        assert!(!table.rows.iter().any(|r| r[0].starts_with("candidate")));
    }
}
