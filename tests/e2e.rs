//! End-to-end connection-behavior tests against an in-process mock server.
//!
//! The mock speaks the BRN0 handshake with the SDK's own *public* wire codec, so
//! it stays correct by construction (no hand-rolled bytes). These cover the
//! shell's connection layer — `connect_with_retry` and dead-connection
//! detection — the parts a unit test can't reach because they only matter
//! against a real socket. Full verb round-trips are already proven by the SDK's
//! own `verbs.rs`; here we care about what happens when the socket misbehaves.

use std::time::Duration;

use brain_db_sdk::error::BrainError;
use brain_db_sdk::retry::RetryPolicy;
use brain_db_sdk::transport::{read_frame, write_frame};
use brain_db_sdk::wire::cbor::{from_cbor_bytes, to_cbor_bytes};
use brain_db_sdk::wire::frame::{Frame, FLAG_EOS};
use brain_db_sdk::wire::opcode::Opcode;
use brain_db_sdk::wire::types::{
    AgentPermissions, AuthOkPayload, AuthPayload, HelloPayload, ServerFeatures, WelcomePayload,
};
use brain_db_sdk::{BrainClient, RecallBuilder};
use tokio::net::{TcpListener, TcpStream};

/// Read HELLO → write WELCOME, read AUTH → write AUTH_OK. Mirrors the handshake
/// the SDK's own mock server performs.
async fn handshake(sock: &mut TcpStream) {
    let mut buf = Vec::new();

    let hello_frame = read_frame(sock, &mut buf).await.expect("hello");
    let hello: HelloPayload = from_cbor_bytes(&hello_frame.payload).expect("decode hello");
    let welcome = WelcomePayload {
        server_id: "mock-brain".into(),
        chosen_version: 1,
        session_id: [0xAB; 16],
        capabilities: hello.capabilities,
        server_features: ServerFeatures {
            max_payload_size: 1 << 20,
            max_concurrent_streams: 64,
            idle_timeout_seconds: 300,
            auth_methods: vec![],
        },
    };
    write_msg(sock, Opcode::Welcome, 0, &welcome).await;

    let auth_frame = read_frame(sock, &mut buf).await.expect("auth");
    let auth: AuthPayload = from_cbor_bytes(&auth_frame.payload).expect("decode auth");
    let auth_ok = AuthOkPayload {
        agent_id: auth.agent_id,
        bound_shard_id: 0,
        permissions: AgentPermissions {
            can_encode: true,
            can_recall: true,
            can_plan: true,
            can_reason: true,
            can_forget: true,
            can_admin: false,
        },
        server_time_unix_nanos: 1,
    };
    write_msg(sock, Opcode::AuthOk, 0, &auth_ok).await;
}

async fn write_msg<T: serde::Serialize>(sock: &mut TcpStream, op: Opcode, sid: u32, payload: &T) {
    let frame = Frame::new(op.as_u16(), FLAG_EOS, sid, to_cbor_bytes(payload));
    write_frame(sock, &frame).await.expect("write frame");
}

/// `connect_with_retry` must treat a pre-handshake drop as transient and retry,
/// succeeding once the server is actually ready — the "server just restarted"
/// papercut the audit flagged.
#[tokio::test]
async fn connect_with_retry_recovers_from_a_transient_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        // Attempt 1: drop the socket before the handshake — a transient failure.
        let (sock1, _) = listener.accept().await.expect("accept 1");
        drop(sock1);
        // Attempt 2: complete the handshake so the retry succeeds.
        let (mut sock2, _) = listener.accept().await.expect("accept 2");
        handshake(&mut sock2).await;
        // Hold open until the client closes, then end the task.
        let mut buf = Vec::new();
        let _ = read_frame(&mut sock2, &mut buf).await;
    });

    let policy = RetryPolicy::new(5, Duration::from_millis(10), Duration::from_millis(100));
    let result = brain_shell::connection::connect_with_retry(
        addr,
        [0x11; 16],
        Duration::from_secs(5),
        None,
        &policy,
    )
    .await;

    // BrainClient isn't Debug, so match rather than unwrap/assert-with-{:?}.
    match result {
        Ok(client) => drop(client),
        Err(e) => panic!("connect_with_retry should recover after the first drop: {e:?}"),
    }
    let _ = server.await;
}

/// A `none()` policy makes a single attempt: a pre-handshake drop is returned as
/// an error rather than retried.
#[tokio::test]
async fn connect_with_no_retry_fails_on_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept");
        drop(sock);
    });

    let err = match brain_shell::connection::connect_with_retry(
        addr,
        [0x22; 16],
        Duration::from_secs(2),
        None,
        &RetryPolicy::none(),
    )
    .await
    {
        Ok(_) => panic!("a single attempt against a dropped socket must fail"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            BrainError::Closed | BrainError::Io(_) | BrainError::Timeout(_)
        ),
        "expected a transport error, got: {err:?}"
    );
    let _ = server.await;
}

/// When the server vanishes mid-request, the pending verb fails with a
/// transport-level (dead-connection) error — exactly what the REPL's
/// auto-reconnect keys off (`BrainError::Io` / `Closed`).
#[tokio::test]
async fn server_drop_surfaces_a_dead_connection_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        handshake(&mut sock).await;
        // Read the recall request, then vanish without responding.
        let mut buf = Vec::new();
        let _ = read_frame(&mut sock, &mut buf).await;
        drop(sock);
    });

    let client = BrainClient::connect(addr).await.expect("connect");
    let err = client
        .recall(&RecallBuilder::new("anything").build())
        .await
        .expect_err("recall must fail when the server drops mid-request");
    // The shell's reconnect predicate must recognize this real drop — the SDK
    // surfaces it as a Protocol-wrapped close, not a bare Closed/Io.
    assert!(
        brain_shell::connection::is_dead(&err),
        "a server drop should be detected as a dead connection, got: {err:?}"
    );

    let _ = server.await;
}
