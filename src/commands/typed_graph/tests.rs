use super::*;

#[test]
fn parse_wire_id_accepts_hyphenated_and_bare() {
    let hyphenated = "0191b6f0-1234-7890-abcd-ef0123456789";
    let bare = "0191b6f012347890abcdef0123456789";
    let a = parse_wire_id(hyphenated, "entity id").unwrap();
    let b = parse_wire_id(bare, "entity id").unwrap();
    assert_eq!(a, b);
}

#[test]
fn parse_wire_id_rejects_garbage() {
    let err = parse_wire_id("not-an-id", "entity id").unwrap_err();
    assert!(matches!(err, BrainError::Protocol(_)));
}

#[test]
fn id_round_trips_through_string() {
    let id = parse_wire_id("0191b6f0-1234-7890-abcd-ef0123456789", "x").unwrap();
    let s = id_to_string(&id);
    let back = parse_wire_id(&s, "x").unwrap();
    assert_eq!(id, back);
}

#[test]
fn zero_id_detected() {
    assert!(is_zero_id(&[0u8; 16]));
    let nonzero = parse_wire_id("0191b6f0-1234-7890-abcd-ef0123456789", "x").unwrap();
    assert!(!is_zero_id(&nonzero));
}

#[test]
fn object_label_handles_each_variant() {
    let id = [1u8; 16];
    assert_eq!(
        object_label(&StatementObjectWire::EntityRef(id)),
        id_to_string(&id)
    );
    assert_eq!(
        object_label(&StatementObjectWire::Value(StatementValueWire::Text(
            "hi".into()
        ))),
        "hi"
    );
    assert_eq!(
        object_label(&StatementObjectWire::Value(StatementValueWire::Integer(7))),
        "7"
    );
    assert!(object_label(&StatementObjectWire::MemoryRef(id)).starts_with("memory "));
    assert!(object_label(&StatementObjectWire::StatementRef(id)).starts_with("statement "));
}

#[test]
fn object_entity_id_only_for_entity_refs() {
    let id = [2u8; 16];
    assert!(object_entity_id(&StatementObjectWire::EntityRef(id)).is_some());
    assert!(
        object_entity_id(&StatementObjectWire::Value(StatementValueWire::Bool(true))).is_none()
    );
    assert!(object_is_entity(&StatementObjectWire::EntityRef(id)));
    assert!(!object_is_entity(&StatementObjectWire::Value(
        StatementValueWire::Bool(true)
    )));
}
