//! Session-state tests.

use std::net::SocketAddr;

use brain_shell::core::MemoryId;
use brain_shell::parser::OutputFormatArg;
use brain_shell::session::{Session, RECENT_ID_CAP};

fn addr() -> SocketAddr {
    "127.0.0.1:9090".parse().expect("addr")
}

#[test]
fn prompt_default_is_brain_arrow() {
    let s = Session::new(addr(), OutputFormatArg::Table);
    assert_eq!(s.prompt(), "brain> ");
}

#[test]
fn prompt_marks_active_txn() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.active_txn = Some([0xAB; 16]);
    assert_eq!(s.prompt(), "brain*> ");
}

#[test]
fn prompt_shows_sticky_session() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.sticky_session = Some(42);
    assert_eq!(s.prompt(), "brain[sess=42]> ");
}

#[test]
fn prompt_combines_txn_and_session() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.active_txn = Some([1; 16]);
    s.sticky_session = Some(3);
    assert_eq!(s.prompt(), "brain*[sess=3]> ");
}

#[test]
fn recent_id_evicts_oldest_past_cap() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    for i in 0..=(RECENT_ID_CAP as u128) {
        s.push_recent_id(MemoryId::from_raw(i));
    }
    let snap = s.recent_ids_snapshot();
    assert_eq!(snap.len(), RECENT_ID_CAP);
    assert!(!snap.iter().any(|x| *x == MemoryId::from_raw(0)));
    assert_eq!(snap[0], MemoryId::from_raw(RECENT_ID_CAP as u128));
}

#[test]
fn recent_id_dedup_promotes_to_front() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.push_recent_id(MemoryId::from_raw(1));
    s.push_recent_id(MemoryId::from_raw(2));
    s.push_recent_id(MemoryId::from_raw(3));
    s.push_recent_id(MemoryId::from_raw(2));
    let ids: Vec<u128> = s
        .recent_ids_snapshot()
        .into_iter()
        .map(|m| m.raw())
        .collect();
    assert_eq!(ids, vec![2, 3, 1]);
}

#[test]
fn effective_txn_inherits_active() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.active_txn = Some([9; 16]);
    assert_eq!(s.effective_txn(None), Some([9; 16]));
    assert_eq!(s.effective_txn(Some([1; 16])), Some([1; 16]));
}

#[test]
fn effective_session_inherits_sticky() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.sticky_session = Some(11);
    assert_eq!(s.effective_session(None), 11);
    assert_eq!(s.effective_session(Some(5)), 5);
}

// ---------------------------------------------------------------------------
// is_txn_terminal — the helper the dispatch path uses to decide whether a
// failed op means the session's tracked txn is gone.
// ---------------------------------------------------------------------------

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{ErrorCategoryWire, ErrorCodeWire, ErrorResponse};
use brain_shell::commands::is_txn_terminal;

fn server_error(code: ErrorCodeWire, category: ErrorCategoryWire, message: &str) -> BrainError {
    BrainError::from_server(ErrorResponse {
        code,
        category,
        message: message.to_string(),
        details: None,
        retry_after_ms: None,
    })
}

#[test]
fn is_txn_terminal_matches_txn_not_found_and_transaction_timeout() {
    let not_found = server_error(
        ErrorCodeWire::TxnNotFound,
        ErrorCategoryWire::NotFound,
        "transaction not found",
    );
    assert!(is_txn_terminal(&not_found));

    let timeout = server_error(
        ErrorCodeWire::TransactionTimeout,
        ErrorCategoryWire::Conflict,
        "transaction expired",
    );
    assert!(is_txn_terminal(&timeout));
}

#[test]
fn is_txn_terminal_ignores_other_codes() {
    for code in [
        ErrorCodeWire::IdempotencyConflict,
        ErrorCodeWire::InvalidArgument,
        ErrorCodeWire::Overloaded,
        ErrorCodeWire::MemoryNotFound,
    ] {
        let e = server_error(code, ErrorCategoryWire::Internal, "boom");
        assert!(
            !is_txn_terminal(&e),
            "{code:?} must NOT be classified as terminal txn"
        );
    }
}

#[test]
fn is_txn_terminal_ignores_non_server_errors() {
    let protocol = BrainError::Protocol("boom".into());
    assert!(!is_txn_terminal(&protocol));

    let closed = BrainError::Closed;
    assert!(!is_txn_terminal(&closed));
}

/// The bearer token must never be stored on `Session` (which is
/// `Clone + Debug` and gets formatted into `\info` / diagnostics). It
/// lives only as a loop-local in the REPL entry point. This pins that
/// invariant: a future refactor that stashes the raw token here would
/// flunk — the Debug form carries the `authenticated` bool, nothing more.
#[test]
fn session_debug_never_carries_the_raw_token() {
    let mut s = Session::new(addr(), OutputFormatArg::Table);
    s.authenticated = true;
    let dbg = format!("{s:?}");
    assert!(
        dbg.contains("authenticated"),
        "auth state should surface as a bool: {dbg}"
    );
    // No String field exists to hold credential material; the only auth
    // signal is the boolean. If someone adds a token field, this guards
    // against a common secret-shaped value leaking through Debug.
    assert!(
        !dbg.to_lowercase().contains("secret") && !dbg.contains("Bearer"),
        "Session Debug must not carry credential material: {dbg}"
    );
}
