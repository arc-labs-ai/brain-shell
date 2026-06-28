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
