//! `subscribe` verb — two modes.
//!
//! - `subscribe --collect <N>` — batch wait. Returns after N events
//!   have arrived (or the stream ends first), unsubscribes cleanly,
//!   and renders the collected list via [`SubscriptionEventList`].
//!   Useful in tests + scripts.
//! - `subscribe` (no `--collect`) — real-time stream. Events render
//!   one at a time as they arrive; the loop exits on Ctrl-C or when
//!   the server closes the stream, sending `UNSUBSCRIBE` first so the
//!   server's registry entry doesn't leak.
//!
//! `--start-lsn N` makes the server replay any historical events with
//! `lsn >= N` from the WAL before joining the live tail. The cutover
//! is invisible to the client (no gap, no dupes). The server rejects
//! values below the oldest available LSN with `SubscriptionLsnTooOld`.

use std::io::{self, Write};

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::{SubscribeRequest, SubscriptionEvent, SubscriptionFilter};
use brain_db_sdk::BrainClient;
use serde_json::Value;

use crate::commands::render_ctx;
use crate::parser::{OutputFormatArg, SubscribeArgs};
use crate::render::{
    dispatch, Render, RenderCtx, SubscriptionEventList, SubscriptionEventRendered,
};
use crate::session::Session;

use super::Rendered;

/// Treat anything that isn't a JSON / yaml / jsonpath family as
/// human-table output for the purposes of streaming banners.
#[must_use]
fn is_human_output(o: &OutputFormatArg) -> bool {
    matches!(
        o,
        OutputFormatArg::Auto | OutputFormatArg::Table | OutputFormatArg::Wide
    )
}

/// Build the `SubscribeRequest` from the parsed args. `max_inflight`
/// of 0 lets the server pick its default window.
fn build_request(args: &SubscribeArgs) -> SubscribeRequest {
    let contexts = if args.context.is_empty() {
        None
    } else {
        Some(args.context.clone())
    };
    let kinds = if args.kind.is_empty() {
        None
    } else {
        Some(args.kind.iter().map(|k| k.into_wire()).collect())
    };
    SubscribeRequest {
        filter: SubscriptionFilter {
            contexts,
            kinds,
            similar_to: None,
            agents: None,
        },
        include_history: args.start_lsn.is_some(),
        from_lsn: args.start_lsn,
        max_inflight: 0,
    }
}

pub async fn run(
    client: &BrainClient,
    session: &mut Session,
    args: SubscribeArgs,
) -> Result<Rendered, BrainError> {
    let request = build_request(&args);

    match args.collect {
        Some(n) => collect_n(client, &request, n).await,
        None => stream_until_signal(client, &request, session).await,
    }
}

/// Batch path: drain up to `n` events (or until the stream ends),
/// then unsubscribe cleanly and render the collected list.
async fn collect_n(
    client: &BrainClient,
    request: &SubscribeRequest,
    n: usize,
) -> Result<Rendered, BrainError> {
    let mut subscription = client.subscribe(request).await?;
    let mut events: Vec<SubscriptionEvent> = Vec::with_capacity(n);
    while events.len() < n {
        match subscription.next().await {
            Some(Ok(event)) => events.push(event),
            Some(Err(e)) => return Err(e),
            None => break,
        }
    }
    // Best-effort clean teardown; ignore the outcome — the events
    // we collected are the result regardless of whether the server
    // acks the unsubscribe.
    let _ = subscription.unsubscribe().await;
    Ok(Box::new(SubscriptionEventList(events)))
}

/// Real-time loop: render events as they arrive; clean up on Ctrl-C.
/// Returns a summary renderer once the stream ends or is interrupted.
async fn stream_until_signal(
    client: &BrainClient,
    request: &SubscribeRequest,
    session: &Session,
) -> Result<Rendered, BrainError> {
    let mut subscription = client.subscribe(request).await?;

    let output = session.output.clone();
    if is_human_output(&output) {
        let _ = writeln!(io::stderr(), "subscribed — Ctrl-C to stop");
        let _ = io::stderr().flush();
    }

    // Build the render context once and reuse it for every event so
    // color / OSC 8 / width handling stays consistent. Pretty JSON
    // and YAML buffer poorly across event boundaries, so force ndjson
    // for any structured stream.
    let stream_format = match output.clone() {
        OutputFormatArg::Json
        | OutputFormatArg::Ndjson
        | OutputFormatArg::Yaml
        | OutputFormatArg::JsonPath(_) => OutputFormatArg::Ndjson,
        other => other,
    };
    let ctx = render_ctx(
        stream_format,
        crate::parser::ColorMode::Auto,
        crate::parser::HyperlinkMode::Auto,
    );

    let mut count: u64 = 0;
    let mut stream_err: Option<BrainError> = None;
    let mut server_closed = false;
    let mut signalled = false;

    loop {
        tokio::select! {
            biased;
            sig = tokio::signal::ctrl_c() => {
                // A handler-install failure is unusual; treat it as a
                // signal so we don't spin without a way to break out.
                let _ = sig;
                signalled = true;
                break;
            }
            ev = subscription.next() => match ev {
                Some(Ok(event)) => {
                    if let Err(e) = render_stream_event(&event, &ctx) {
                        // stdout closed (broken pipe): exit quietly.
                        if e.kind() == io::ErrorKind::BrokenPipe {
                            break;
                        }
                        return Err(BrainError::Protocol(format!("render: {e}")));
                    }
                    count += 1;
                }
                Some(Err(e)) => {
                    stream_err = Some(e);
                    break;
                }
                None => {
                    server_closed = true;
                    break;
                }
            },
        }
    }

    if signalled && is_human_output(&output) {
        let _ = writeln!(io::stderr(), "\nclosing stream…");
        let _ = io::stderr().flush();
    }

    // Best-effort unsubscribe so the server doesn't keep the registry
    // entry alive. Skip when the stream already errored or the server
    // closed — the registry has cleaned up in either case.
    if !server_closed && stream_err.is_none() {
        let _ = subscription.unsubscribe().await;
    }
    // Drop the subscription explicitly so its route is deregistered
    // before we report the summary.
    drop(subscription);

    if let Some(e) = stream_err {
        return Err(e);
    }

    let footer = if server_closed {
        format!("(stream closed by server; {count} events)")
    } else {
        format!("(unsubscribed; {count} events)")
    };
    Ok(Box::new(StreamSummary { footer }))
}

/// One event → one line on stdout, flushed.
///
/// Goes through the render layer's `dispatch` so streamed events get
/// the same color / OSC 8 / format matrix as one-shot output. The
/// caller has already normalised JSON / YAML into NDJSON before this
/// point, which is the right shape across event boundaries.
fn render_stream_event(ev: &SubscriptionEvent, ctx: &RenderCtx) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let item = SubscriptionEventRendered(ev.clone());
    dispatch(&item, ctx, &mut out)?;
    out.flush()
}

/// Final summary of a streaming subscription. The events were printed
/// live; this renders the one-line footer the dispatch loop emits.
struct StreamSummary {
    footer: String,
}

impl Render for StreamSummary {
    fn render_table(&self, _ctx: &RenderCtx, w: &mut dyn Write) -> io::Result<()> {
        writeln!(w, "{}", self.footer)
    }

    fn render_json(&self, _ctx: &RenderCtx) -> Value {
        Value::Null
    }
}
