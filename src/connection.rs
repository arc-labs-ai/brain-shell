//! Thin wrapper around [`brain_db_sdk::BrainClient`] applying the shell's
//! defaults (agent identity, per-request timeout).
//!
//! The mux client multiplexes concurrent streams over one socket, so the shell
//! needs only a single connection — a held SUBSCRIBE stream no longer starves
//! concurrent ENCODE / RECALL calls the way it did with the old serial client.

use std::net::SocketAddr;
use std::time::Duration;

use brain_db_sdk::error::BrainError;
use brain_db_sdk::retry::{RetryPolicy, RetryStep};
use brain_db_sdk::{Auth, BrainClient, ClientConfig};

/// Connect to `addr` as `agent_id`, with a per-request `timeout`.
///
/// When `token` is a non-empty string the shell authenticates with
/// `Token` auth; otherwise it connects anonymously (the server decides
/// whether to allow that). A wrong/absent token makes
/// `BrainClient::connect_with` return an auth error rather than silently
/// falling back to anonymous.
pub async fn connect(
    addr: SocketAddr,
    agent_id: [u8; 16],
    timeout: Duration,
    token: Option<&str>,
) -> Result<BrainClient, BrainError> {
    let auth = match token {
        Some(t) if !t.is_empty() => Auth::Token(t.as_bytes().to_vec()),
        _ => Auth::Anonymous,
    };
    let config = ClientConfig {
        agent_id,
        auth,
        request_timeout: Some(timeout),
        ..ClientConfig::default()
    };
    BrainClient::connect_with(addr, config).await
}

/// Connect with bounded retry on transient transport failures.
///
/// A REPL/tool dying because the server was a second late to accept (just
/// restarted, briefly overloaded) is the #1 papercut, so the initial connect
/// retries on retryable errors (I/O drop, timeout, server `Unavailable` /
/// `ResourceExhausted`) per `policy`. Non-retryable failures — a bad token
/// (auth) or a version mismatch — fail fast, since retrying can't fix them. A
/// one-line notice goes to stderr before each wait so an interactive user sees
/// the tool is waiting, not hung.
pub async fn connect_with_retry(
    addr: SocketAddr,
    agent_id: [u8; 16],
    timeout: Duration,
    token: Option<&str>,
    policy: &RetryPolicy,
) -> Result<BrainClient, BrainError> {
    let mut attempt: u32 = 1;
    loop {
        match connect(addr, agent_id, timeout, token).await {
            Ok(client) => return Ok(client),
            Err(err) => match policy.step(attempt, &err) {
                RetryStep::Retry(delay) => {
                    match delay {
                        Some(d) => {
                            eprintln!(
                                "connect to {addr} failed ({err}); retrying in {:.1}s …",
                                d.as_secs_f32()
                            );
                            tokio::time::sleep(d).await;
                        }
                        None => {
                            eprintln!("connect to {addr} failed ({err}); retrying …");
                        }
                    }
                    attempt += 1;
                }
                RetryStep::GiveUp => return Err(err),
            },
        }
    }
}
