//! `statement show <id>` — single-card view.
//!
//! Maps the wire `StatementView` onto the render card. The view carries no
//! canonical names, so the subject shows as its id and an entity-reference
//! object uses its id for both id and name. A tombstoned row surfaces as
//! record-time invalidation so the card prints the "stopped believing"
//! line.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{StatementGetRequest, StatementObjectWire};
use brain_db_sdk::BrainClient;

use crate::commands::typed_graph::{id_to_string, kind_label, object_label, parse_wire_id};
use crate::commands::Rendered;
use crate::parser::StatementShowArgs;
use crate::render::{ObjectRef, StatementCard};
use crate::session::Session;

pub async fn run(
    client: &BrainClient,
    _session: &mut Session,
    args: StatementShowArgs,
) -> Result<Rendered, BrainError> {
    let statement_id = parse_wire_id(&args.id, "statement id")?;
    let resp = client
        .get_statement(&StatementGetRequest {
            act_as: None,
            statement_id,
            follow_supersession: false,
        })
        .await?;
    let s = resp.statement;

    let object = match &s.object {
        StatementObjectWire::EntityRef(id) => {
            let id_str = id_to_string(id);
            ObjectRef::Entity {
                id: id_str.clone(),
                name: id_str,
            }
        }
        other => ObjectRef::Literal(object_label(other)),
    };

    let record_invalidated_at_unix_nanos = if s.tombstoned {
        Some(s.tombstoned_at_unix_nanos)
    } else {
        None
    };

    let card = StatementCard {
        id: id_to_string(&s.statement_id),
        kind: kind_label(s.kind).to_string(),
        subject_canonical: id_to_string(&s.subject),
        predicate_qname: s.predicate.clone(),
        object,
        confidence: s.confidence,
        evidence_memories: Vec::new(),
        record_invalidated_at_unix_nanos,
    };
    Ok(Box::new(card))
}
