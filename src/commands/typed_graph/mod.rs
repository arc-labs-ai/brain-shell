//! Shared helpers for the typed-graph read commands (entity / statement /
//! relation / schema).
//!
//! The new SDK is wire-level: it returns flat `*View` structs keyed by raw
//! 16-byte ids, with no canonical-name backfill and no entity-type qname
//! lookup. These helpers do the small, repeated translations every read
//! command needs — parse a hex/UUID id from user input, render a 16-byte id
//! as a UUID string, format a statement object/value for display, and map
//! the all-zero sentinel ids the wire uses for "absent" to `None`.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{
    StatementKindWire, StatementObjectWire, StatementValueWire, WireUuid,
};
use uuid::Uuid;

/// Parse a 16-byte wire id from user input. Accepts the hyphenated UUID
/// form (`0191b6f0-1234-7890-abcd-ef0123456789`) and the bare 32-hex form
/// (`0191b6f012347890abcdef0123456789`).
pub fn parse_wire_id(s: &str, what: &str) -> Result<WireUuid, BrainError> {
    let trimmed = s.trim();
    Uuid::parse_str(trimmed)
        .map(|u| *u.as_bytes())
        .map_err(|e| BrainError::Protocol(format!("invalid {what} `{trimmed}`: {e}")))
}

/// Render a 16-byte wire id as a hyphenated UUID string.
#[must_use]
pub fn id_to_string(id: &WireUuid) -> String {
    Uuid::from_bytes(*id).to_string()
}

/// `true` when a wire id is the all-zero sentinel the server uses for an
/// absent value (no supersession, no merge target, …).
#[must_use]
pub fn is_zero_id(id: &WireUuid) -> bool {
    id.iter().all(|b| *b == 0)
}

/// Render a statement kind as the short label the cards use.
#[must_use]
pub fn kind_label(kind: StatementKindWire) -> &'static str {
    match kind {
        StatementKindWire::Fact => "Fact",
        StatementKindWire::Preference => "Preference",
        StatementKindWire::Event => "Event",
        StatementKindWire::Attribute => "Attribute",
        StatementKindWire::Relation => "Relation",
        StatementKindWire::Directive => "Directive",
        StatementKindWire::Custom(_) => "Custom",
    }
}

/// Render a statement object value (the right-hand side of a triple) as a
/// human-facing label. Entity references show as their id (the wire view
/// carries no canonical name to backfill).
#[must_use]
pub fn object_label(object: &StatementObjectWire) -> String {
    match object {
        StatementObjectWire::EntityRef(id) => id_to_string(id),
        StatementObjectWire::Value(v) => value_label(v),
        StatementObjectWire::MemoryRef(id) => format!("memory {}", id_to_string(id)),
        StatementObjectWire::StatementRef(id) => format!("statement {}", id_to_string(id)),
    }
}

/// `true` when a statement object is an entity reference.
#[must_use]
pub fn object_is_entity(object: &StatementObjectWire) -> bool {
    matches!(object, StatementObjectWire::EntityRef(_))
}

/// The entity id behind an entity-reference object, if any.
#[must_use]
pub fn object_entity_id(object: &StatementObjectWire) -> Option<String> {
    match object {
        StatementObjectWire::EntityRef(id) => Some(id_to_string(id)),
        _ => None,
    }
}

fn value_label(value: &StatementValueWire) -> String {
    match value {
        StatementValueWire::Text(s) => s.clone(),
        StatementValueWire::Integer(i) => i.to_string(),
        StatementValueWire::Float(f) => f.to_string(),
        StatementValueWire::Bool(b) => b.to_string(),
        StatementValueWire::UnixNanos(n) => n.to_string(),
        StatementValueWire::Blob(b) => format!("<{} bytes>", b.len()),
    }
}

#[cfg(test)]
mod tests;
