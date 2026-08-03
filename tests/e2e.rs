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
    AuthOkPayload, AuthPayload, EncodeResponse, HelloPayload, MemoryKindWire, ServerFeatures,
    SpacePermissions, StageAutoEdgePayload, StageKind, StageOutcome, StagePayload,
    SubscribeRequest, SubscriptionEvent, UnsubscribeResponse, WelcomePayload,
};
use brain_db_sdk::{Auth, BrainClient, RecallBuilder};
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
        connection_id: [0xAB; 16],
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
    let _auth: AuthPayload = from_cbor_bytes(&auth_frame.payload).expect("decode auth");
    let auth_ok = AuthOkPayload {
        // The server assigns the space id; the client no longer sends one.
        space_id: [0x11; 16],
        bound_shard_id: 0,
        permissions: SpacePermissions {
            can_act_as: false,
            can_encode: true,
            can_recall: true,
            can_plan: true,
            can_reason: true,
            can_forget: true,
            can_admin: false,
        },
        namespace: String::new(),
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
        Duration::from_secs(5),
        "test-token",
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
        Duration::from_secs(2),
        "test-token",
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

    let client = BrainClient::connect(addr, Auth::Token(b"test-token".to_vec()))
        .await
        .expect("connect");
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

/// `encode --wait` must round-trip the full post-encode watch: subscribe
/// replayed from the encode's LSN, collect the queued stage's completion
/// event, and amend the rendered card with its outcome. This is the only
/// place the SUBSCRIBE wiring (from_lsn, client-side memory match, early
/// exit once all queued stages report) is exercised end-to-end.
#[tokio::test]
async fn encode_wait_amends_card_with_stage_results() {
    use brain_db_sdk::wire::opcode::Opcode;
    use brain_shell::parser::{ColorMode, Command, HyperlinkMode, OutputFormatArg};
    use clap::Parser;

    const MEMORY_ID: u128 = 0x1234_5678;
    const ENCODE_LSN: u64 = 99;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        handshake(&mut sock).await;
        let mut buf = Vec::new();

        // 1. ENCODE → respond with one queued stage (AutoEdge) + the LSN.
        let enc = read_frame(&mut sock, &mut buf).await.expect("encode req");
        assert_eq!(
            enc.opcode,
            Opcode::EncodeReq.as_u16(),
            "expected ENCODE_REQ"
        );
        let resp = EncodeResponse {
            trace: None,
            memory_id: MEMORY_ID,
            was_deduplicated: false,
            salience: 0.5,
            auto_edges_added: 0,
            lsn: ENCODE_LSN,
            space_id: [0u8; 16],
            session_id: 0,
            kind: MemoryKindWire::Episodic,
            created_at_unix_nanos: 1,
            edges_out_count: 0,
            embedding_model_fp: [0u8; 16],
            pending_stages: vec![StageKind::AutoEdge],
            has_active_schema: false,
        };
        write_msg(&mut sock, Opcode::EncodeResp, enc.stream_id, &resp).await;

        // 2. SUBSCRIBE → must replay from exactly lsn+1 (the race defense).
        let sub = read_frame(&mut sock, &mut buf)
            .await
            .expect("subscribe req");
        assert_eq!(
            sub.opcode,
            Opcode::SubscribeReq.as_u16(),
            "expected SUBSCRIBE_REQ"
        );
        let sub_req: SubscribeRequest = from_cbor_bytes(&sub.payload).expect("decode subscribe");
        assert_eq!(
            sub_req.from_lsn,
            Some(ENCODE_LSN + 1),
            "watch must replay from the position right after the encode"
        );

        // Push the AutoEdge completion for our memory, then EOS the stream.
        let mut event = stage_event(MEMORY_ID);
        event.stage_kind = Some(StageKind::AutoEdge);
        event.stage_outcome = Some(StageOutcome::Ok);
        event.stage_payload = Some(StagePayload::AutoEdge(StageAutoEdgePayload {
            edges_written: 2,
        }));
        write_msg(&mut sock, Opcode::SubscribeEvent, sub.stream_id, &event).await;

        // 3. UNSUBSCRIBE → ack so the watcher's clean teardown completes.
        let unsub = read_frame(&mut sock, &mut buf)
            .await
            .expect("unsubscribe req");
        assert_eq!(
            unsub.opcode,
            Opcode::UnsubscribeReq.as_u16(),
            "expected UNSUBSCRIBE_REQ"
        );
        let ack = UnsubscribeResponse {
            target_stream_id: sub.stream_id,
            final_lsn: ENCODE_LSN + 1,
        };
        write_msg(&mut sock, Opcode::UnsubscribeResp, unsub.stream_id, &ack).await;

        // Hold the socket open until the client closes.
        let _ = read_frame(&mut sock, &mut buf).await;
    });

    let client = BrainClient::connect(addr, Auth::Token(b"test-token".to_vec()))
        .await
        .expect("connect");
    let mut session = brain_shell::session::Session::new(addr, OutputFormatArg::Table);

    let cli = brain_shell::parser::Cli::try_parse_from(["brain", "encode", "hello", "--wait"])
        .expect("parse encode --wait");
    let args = match cli.subcommand {
        Some(Command::Encode(a)) => a,
        other => panic!("expected encode subcommand, got {other:?}"),
    };

    let rendered = match brain_shell::commands::encode::run(&client, &mut session, args).await {
        Ok(r) => r,
        Err(e) => panic!("encode --wait should succeed: {e:?}"),
    };
    // The rendered card owns no client reference; drop the client so the
    // server task's trailing read observes the close and the task ends
    // (otherwise `server.await` below would deadlock on an idle socket).
    drop(client);

    // Render the card and confirm the stage amendment landed.
    let ctx = brain_shell::commands::render_ctx(
        OutputFormatArg::Table,
        ColorMode::Never,
        HyperlinkMode::Never,
    );
    let mut out = Vec::new();
    brain_shell::render::dispatch(&*rendered, &ctx, &mut out).expect("render");
    let text = String::from_utf8(out).expect("utf8");

    assert!(
        text.contains("waited") && text.contains("background stages"),
        "missing wait header: {text}"
    );
    assert!(
        text.contains("auto_edge") && text.contains("2 SimilarTo edges written"),
        "missing the collected stage outcome: {text}"
    );

    let _ = server.await;
}

/// A bare `SUBSCRIBE_EVENT` skeleton for `memory_id`; callers set the
/// stage fields they care about.
fn stage_event(memory_id: u128) -> SubscriptionEvent {
    SubscriptionEvent {
        event_type: brain_db_sdk::wire::types::EventType::StageCompleted,
        memory_id,
        session_id: 0,
        text: String::new(),
        kind: MemoryKindWire::Episodic,
        salience: 0.0,
        timestamp_unix_nanos: 0,
        lsn: 100,
        graph_payload: None,
        edge_payload: None,
        stage_kind: None,
        stage_outcome: None,
        stage_payload: None,
    }
}
