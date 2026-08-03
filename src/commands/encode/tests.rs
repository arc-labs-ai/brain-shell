use super::*;
use brain_db_sdk::wire::types::{
    EdgeEventPayload, MemoryKindWire, StageAutoEdgePayload, StageExtractorPayload,
};

fn base_event(event_type: EventType, memory_id: u128) -> SubscriptionEvent {
    SubscriptionEvent {
        event_type,
        memory_id,
        session_id: 0,
        text: String::new(),
        kind: MemoryKindWire::Episodic,
        salience: 0.0,
        timestamp_unix_nanos: 0,
        lsn: 1,
        graph_payload: None,
        edge_payload: None,
        stage_kind: None,
        stage_outcome: None,
        stage_payload: None,
    }
}

#[test]
fn auto_edge_stage_summary_uses_similar_to_phrasing() {
    let mut ev = base_event(EventType::StageCompleted, 7);
    ev.stage_kind = Some(StageKind::AutoEdge);
    ev.stage_outcome = Some(StageOutcome::Ok);
    ev.stage_payload = Some(StagePayload::AutoEdge(StageAutoEdgePayload {
        edges_written: 2,
    }));
    let res = stage_result_from_event(&ev).expect("stage result");
    assert_eq!(res.kind, StageKindLabel::AutoEdge);
    assert_eq!(res.outcome, StageOutcomeLabel::Ok);
    assert_eq!(res.summary, "2 SimilarTo edges written");
}

#[test]
fn extractor_summary_matches_render_count_format() {
    let mut ev = base_event(EventType::StageCompleted, 7);
    ev.stage_kind = Some(StageKind::Extractor);
    ev.stage_outcome = Some(StageOutcome::Ok);
    ev.stage_payload = Some(StagePayload::Extractor(StageExtractorPayload {
        entity_count: 3,
        statement_count: 5,
        relation_count: 0,
        audit_status: StageAuditStatus::Succeeded,
        error_message: String::new(),
    }));
    let res = stage_result_from_event(&ev).expect("stage result");
    // The render layer's augment_extractor_summary keys off this exact
    // "N statements, N relations" phrasing — keep them in lockstep.
    assert_eq!(
        res.summary,
        "3 entities, 5 statements, 0 relations · succeeded"
    );
}

#[test]
fn failed_extractor_summary_carries_the_error() {
    let mut ev = base_event(EventType::StageCompleted, 7);
    ev.stage_kind = Some(StageKind::Extractor);
    ev.stage_outcome = Some(StageOutcome::Failed);
    ev.stage_payload = Some(StagePayload::Extractor(StageExtractorPayload {
        entity_count: 0,
        statement_count: 0,
        relation_count: 0,
        audit_status: StageAuditStatus::Failed,
        error_message: "llm timeout".into(),
    }));
    let res = stage_result_from_event(&ev).expect("stage result");
    assert_eq!(res.outcome, StageOutcomeLabel::Failed);
    assert!(res.summary.contains("llm timeout"), "got: {}", res.summary);
}

#[test]
fn non_stage_event_yields_no_stage_result() {
    let ev = base_event(EventType::Encoded, 7);
    assert!(stage_result_from_event(&ev).is_none());
}

#[test]
fn auto_derived_edge_is_collected_explicit_is_not() {
    let mut ev = base_event(EventType::EdgeAdded, 7);
    // 42 as a 16-byte big-endian id → u128 42 after conversion.
    let mut to_id = [0u8; 16];
    to_id[15] = 42;
    ev.edge_payload = Some(EdgeEventPayload {
        from_kind: 0,
        from_id: [0u8; 16],
        to_kind: 0,
        to_id,
        edge_kind_tag: 0,
        edge_kind_byte: 3, // SimilarTo
        relation_type_id: None,
        weight: 0.87,
        relation_id: None,
        superseded_relation_id: None,
        origin: 1, // AUTO_DERIVED
    });
    let edge = auto_edge_from_event(&ev).expect("auto edge");
    assert_eq!(edge.target, 42);
    assert_eq!(edge.kind, "SimilarTo");
    assert!((edge.weight - 0.87).abs() < 1e-6);

    // Same event, but explicit origin → not collected.
    if let Some(p) = ev.edge_payload.as_mut() {
        p.origin = 0;
    }
    assert!(auto_edge_from_event(&ev).is_none());
}

#[test]
fn unknown_edge_kind_byte_falls_back_to_raw() {
    assert_eq!(edge_kind_name(3), "SimilarTo");
    assert_eq!(edge_kind_name(99), "kind#99");
}
