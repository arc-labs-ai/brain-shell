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
