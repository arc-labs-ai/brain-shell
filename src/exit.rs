//! Process exit codes by failure class.
//!
//! One-shot `brain <verb>` is meant for scripts, so the exit code has to say
//! *what kind* of failure happened — not just "nonzero". Callers branch on it:
//! `3` (unavailable) is worth retrying, `4` (auth) and `5` (not-found) are not,
//! `6` (server rejected/failed) is a request problem. The mapping is a stable
//! contract documented in the README.
//!
//! Scheme (kept small and POSIX-friendly — all ≤ 125):
//! - `0` success
//! - `1` generic / unexpected (protocol desync, version mismatch, render error)
//! - `2` usage (bad CLI args / unknown-or-deferred verb — clap's own convention)
//! - `3` unavailable: transport down, timeout, or server `ResourceExhausted` /
//!   `Unavailable` — transient, safe to retry
//! - `4` auth: `Authentication` / `Authorization`
//! - `5` not found
//! - `6` server: the server rejected or failed the request (`Validation` /
//!   `Conflict` / `Internal` / `Protocol`) — don't blind-retry

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::ErrorCategoryWire;

pub const SUCCESS: u8 = 0;
pub const GENERIC: u8 = 1;
pub const USAGE: u8 = 2;
pub const UNAVAILABLE: u8 = 3;
pub const AUTH: u8 = 4;
pub const NOT_FOUND: u8 = 5;
pub const SERVER: u8 = 6;

/// Map an SDK error to the shell's class-based exit code.
#[must_use]
pub fn for_error(err: &BrainError) -> u8 {
    match err {
        // Local transport failed mid-flight, the peer hung up, or we ran out of
        // time — all transient, a retry could succeed.
        BrainError::Io(_) | BrainError::Closed | BrainError::Timeout(_) => UNAVAILABLE,

        // The byte stream desynchronized or the peer speaks a version we don't —
        // not retryable, and not the *server's* structured verdict; surface as
        // "something unexpected went wrong".
        BrainError::Frame(_)
        | BrainError::Cbor(_)
        | BrainError::Protocol(_)
        | BrainError::VersionMismatch { .. } => GENERIC,

        // A structured ERROR from the server — branch on its stable category.
        BrainError::Server { category, .. } => match category {
            ErrorCategoryWire::Authentication | ErrorCategoryWire::Authorization => AUTH,
            ErrorCategoryWire::NotFound => NOT_FOUND,
            ErrorCategoryWire::ResourceExhausted | ErrorCategoryWire::Unavailable => UNAVAILABLE,
            // Validation | Conflict | Internal | Protocol → the request itself
            // failed at the server; retrying the same request won't fix it.
            _ => SERVER,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_db_sdk::wire::types::{ErrorCodeWire, ErrorResponse};
    use std::time::Duration;

    fn server(category: ErrorCategoryWire, code: ErrorCodeWire) -> BrainError {
        BrainError::Server {
            code,
            category,
            message: "x".into(),
            retry_after_ms: None,
            response: Box::new(ErrorResponse {
                code,
                category,
                message: "x".into(),
                details: None,
                retry_after_ms: None,
            }),
        }
    }

    #[test]
    fn transport_errors_are_unavailable() {
        assert_eq!(
            for_error(&BrainError::Io(std::io::Error::other("boom"))),
            UNAVAILABLE
        );
        assert_eq!(for_error(&BrainError::Closed), UNAVAILABLE);
        assert_eq!(
            for_error(&BrainError::Timeout(Duration::from_secs(1))),
            UNAVAILABLE
        );
    }

    #[test]
    fn protocol_family_is_generic() {
        assert_eq!(for_error(&BrainError::Protocol("desync".into())), GENERIC);
        assert_eq!(
            for_error(&BrainError::VersionMismatch {
                chosen: 9,
                supported: vec![1]
            }),
            GENERIC
        );
    }

    #[test]
    fn auth_maps_to_4() {
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::Authentication,
                ErrorCodeWire::Unauthenticated
            )),
            AUTH
        );
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::Authorization,
                ErrorCodeWire::PermissionDenied
            )),
            AUTH
        );
    }

    #[test]
    fn not_found_maps_to_5() {
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::NotFound,
                ErrorCodeWire::MemoryNotFound
            )),
            NOT_FOUND
        );
    }

    #[test]
    fn exhausted_and_unavailable_are_retryable_3() {
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::ResourceExhausted,
                ErrorCodeWire::InvalidArgument
            )),
            UNAVAILABLE
        );
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::Unavailable,
                ErrorCodeWire::InvalidArgument
            )),
            UNAVAILABLE
        );
    }

    #[test]
    fn validation_and_internal_are_server_6() {
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::Validation,
                ErrorCodeWire::InvalidArgument
            )),
            SERVER
        );
        assert_eq!(
            for_error(&server(
                ErrorCategoryWire::Internal,
                ErrorCodeWire::InvalidArgument
            )),
            SERVER
        );
    }
}
