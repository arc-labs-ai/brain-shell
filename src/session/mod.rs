//! REPL state that survives across lines.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::core::MemoryId;
use crate::parser::OutputFormatArg;

/// Default cap on remembered memory ids for `$N` aliases + completion.
pub const RECENT_ID_CAP: usize = 100;

/// Shared id ring. `Arc<Mutex>` because the REPL loop pushes ids onto
/// it as commands run, while the rustyline completer (owned by the
/// editor for its lifetime) reads it on every Tab.
pub type RecentIds = Arc<Mutex<VecDeque<MemoryId>>>;

/// Stateful REPL session.
#[derive(Debug, Clone)]
pub struct Session {
    /// Active transaction id (set by `txn begin`, cleared by
    /// `txn commit` / `txn abort` / `\unset txn`).
    pub active_txn: Option<[u8; 16]>,
    /// Sticky default for `--session` set via `\set session N`.
    pub sticky_session: Option<u64>,
    /// Recently-returned memory ids (newest first, oldest evicted at
    /// [`RECENT_ID_CAP`]). Shared with the rustyline completer.
    pub recent_ids: RecentIds,
    /// Output renderer choice.
    pub output: OutputFormatArg,
    /// Show per-op elapsed wall time.
    pub timing: bool,
    /// Server endpoint (may change via `\connect` / `\profile use`).
    pub server: SocketAddr,
    /// Whether the connection authenticated (always true in practice —
    /// the shell requires an API-key credential). Surfaced by `\info`;
    /// the token value itself is deliberately *not* stored here (this
    /// struct is `Clone + Debug`), it lives only as a loop-local in the
    /// REPL entry point.
    pub authenticated: bool,
    /// Tenant namespace the server bound this connection to. Server-derived
    /// from auth (the client never sends a namespace); captured once after
    /// connect and refreshed on reconnect. Empty == the reserved `brain`
    /// system namespace (no tenant). Surfaced in the prompt + `\info`.
    pub namespace: String,
}

impl Session {
    /// Build a fresh session pointing at `server`.
    #[must_use]
    pub fn new(server: SocketAddr, output: OutputFormatArg) -> Self {
        Self {
            active_txn: None,
            sticky_session: None,
            recent_ids: Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_ID_CAP))),
            output,
            timing: false,
            server,
            authenticated: false,
            namespace: String::new(),
        }
    }

    /// Build a session seeded from persisted `Settings`. The
    /// caller has already merged `--output` overrides into
    /// `output`; we apply `sticky_session` and `timing` here so the
    /// REPL inherits the user's saved preferences. The `server`
    /// field on `settings` is consumed earlier (when picking the
    /// connect address) and is not duplicated into the session.
    #[must_use]
    pub fn from_settings(
        server: SocketAddr,
        output: OutputFormatArg,
        settings: &crate::cli::config::Settings,
    ) -> Self {
        Self {
            active_txn: None,
            sticky_session: settings.sticky_session,
            recent_ids: Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_ID_CAP))),
            output,
            timing: settings.timing.unwrap_or(false),
            server,
            authenticated: false,
            namespace: String::new(),
        }
    }

    /// Push a memory id onto the recent-ids list. Deduplicates: if
    /// the id is already present it's promoted to the front. Evicts
    /// the oldest entry once the cap is exceeded.
    pub fn push_recent_id(&mut self, id: MemoryId) {
        let mut ring = self.recent_ids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = ring.iter().position(|x| *x == id) {
            ring.remove(pos);
        }
        ring.push_front(id);
        while ring.len() > RECENT_ID_CAP {
            ring.pop_back();
        }
    }

    /// Render the REPL prompt for the current state.
    #[must_use]
    pub fn prompt(&self) -> String {
        let txn_marker = if self.active_txn.is_some() { "*" } else { "" };
        // `[ns=…]` only renders when a tenant is bound — the system-namespace
        // (empty) case is the common default and would just clutter the prompt.
        let ns_seg = if self.namespace.is_empty() {
            String::new()
        } else {
            format!("[ns={}]", self.namespace)
        };
        let session_seg = match self.sticky_session {
            Some(sess) => format!("[sess={sess}]"),
            None => String::new(),
        };
        format!("brain{txn_marker}{ns_seg}{session_seg}> ")
    }

    /// Helper used by op commands: if the caller didn't supply a
    /// `--txn`, inherit the active session one.
    #[must_use]
    pub fn effective_txn(&self, explicit: Option<[u8; 16]>) -> Option<[u8; 16]> {
        explicit.or(self.active_txn)
    }

    /// Helper used by `encode`: if the caller didn't supply a
    /// `--session`, inherit the sticky one (else 0).
    #[must_use]
    pub fn effective_session(&self, explicit: Option<u64>) -> u64 {
        explicit.or(self.sticky_session).unwrap_or(0)
    }

    /// Read-only snapshot of the recent-id ring. Used by tests and
    /// formatter code that wants a `Vec` without holding the mutex.
    #[must_use]
    pub fn recent_ids_snapshot(&self) -> Vec<MemoryId> {
        self.recent_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:9090".parse().expect("parse")
    }

    #[test]
    fn prompt_no_state() {
        let s = Session::new(addr(), OutputFormatArg::Table);
        assert_eq!(s.prompt(), "brain> ");
    }

    #[test]
    fn prompt_with_txn() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.active_txn = Some([0u8; 16]);
        assert_eq!(s.prompt(), "brain*> ");
    }

    #[test]
    fn prompt_with_sticky_session() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.sticky_session = Some(7);
        assert_eq!(s.prompt(), "brain[sess=7]> ");
    }

    #[test]
    fn prompt_with_both() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.active_txn = Some([0u8; 16]);
        s.sticky_session = Some(7);
        assert_eq!(s.prompt(), "brain*[sess=7]> ");
    }

    #[test]
    fn prompt_with_namespace() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.namespace = "acme".to_string();
        assert_eq!(s.prompt(), "brain[ns=acme]> ");
    }

    #[test]
    fn prompt_with_namespace_and_session() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.namespace = "acme".to_string();
        s.sticky_session = Some(7);
        assert_eq!(s.prompt(), "brain[ns=acme][sess=7]> ");
    }

    #[test]
    fn prompt_empty_namespace_is_omitted() {
        let s = Session::new(addr(), OutputFormatArg::Table);
        assert!(!s.prompt().contains("ns="));
    }

    #[test]
    fn recent_id_cap_evicts() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        for i in 0..(RECENT_ID_CAP + 1) {
            s.push_recent_id(MemoryId::from_raw(i as u128));
        }
        let snap = s.recent_ids_snapshot();
        assert_eq!(snap.len(), RECENT_ID_CAP);
        assert!(!snap.iter().any(|x| *x == MemoryId::from_raw(0)));
        assert_eq!(snap[0], MemoryId::from_raw(RECENT_ID_CAP as u128));
    }

    #[test]
    fn recent_id_dedups_to_front() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.push_recent_id(MemoryId::from_raw(1));
        s.push_recent_id(MemoryId::from_raw(2));
        s.push_recent_id(MemoryId::from_raw(1));
        let snap = s.recent_ids_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0], MemoryId::from_raw(1));
    }

    #[test]
    fn effective_txn_inherits_active() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.active_txn = Some([7u8; 16]);
        assert_eq!(s.effective_txn(None), Some([7u8; 16]));
        assert_eq!(s.effective_txn(Some([1u8; 16])), Some([1u8; 16]));
    }

    #[test]
    fn effective_session_inherits_sticky() {
        let mut s = Session::new(addr(), OutputFormatArg::Table);
        s.sticky_session = Some(5);
        assert_eq!(s.effective_session(None), 5);
        assert_eq!(s.effective_session(Some(9)), 9);
        s.sticky_session = None;
        assert_eq!(s.effective_session(None), 0);
    }
}
