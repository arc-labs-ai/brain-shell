//! One module per verb. Each `run` borrows the SDK [`BrainClient`],
//! mutates the [`Session`](crate::session::Session) where appropriate,
//! and returns a boxed [`Render`] for the dispatch loop to format.

pub mod dispatch;
pub mod encode;
pub mod entity;
pub mod forget;
pub mod info;
pub mod link;
pub mod plan;
pub mod reason;
pub mod recall;
pub mod relation;
pub mod schema;
pub mod statement;
pub mod subscribe;
pub mod txn;
pub mod typed_graph;
pub mod unlink;

pub use dispatch::{deferred_verb_name, dispatch_command, Dispatched};

use brain_db_sdk::error::BrainError;
use brain_db_sdk::wire::types::ErrorCodeWire;

use crate::parser::{ColorMode, HyperlinkMode, OutputFormatArg};
use crate::render::term::{ColorMode as RenderColorMode, HyperlinkMode as RenderHyperlinkMode};
use crate::render::{OutputFormat, Render, RenderCtx, RenderableError, TermPolicy, Theme};

/// Boxed `Render` return for every verb — the dispatch loop owns the
/// buffer + format choice.
pub type Rendered = Box<dyn Render>;

/// Build the `RenderCtx` the renderer's dispatcher consumes.
///
/// Centralised so the one-shot dispatcher (and, later, the REPL loop)
/// don't drift on how they translate the shell's clap-flag enums into
/// the render layer's types. `color` / `hyperlinks` flow through
/// `TermPolicy::detect`, which reconciles them with NO_COLOR / CLICOLOR
/// / isatty.
#[must_use]
pub fn render_ctx(output: OutputFormatArg, color: ColorMode, hyperlinks: HyperlinkMode) -> RenderCtx {
    let render_color: RenderColorMode = color.into();
    let render_hyperlinks: RenderHyperlinkMode = hyperlinks.into();
    RenderCtx {
        policy: TermPolicy::detect(render_color, render_hyperlinks),
        theme: Theme::default(),
        format: OutputFormat::from(output),
    }
}

/// `true` when the server's response says the transaction we used is no
/// longer usable from this session — either the id was never created
/// (`TxnNotFound`) or it exists but is no longer Active
/// (`TransactionTimeout`). Both mean: discard the tracked active_txn and
/// warn the user.
///
/// Returns `false` for every other error, including
/// `IdempotencyConflict` (which is unrelated to txn lifetime).
#[must_use]
pub fn is_txn_terminal(err: &BrainError) -> bool {
    matches!(
        err,
        BrainError::Server { code, .. }
            if matches!(code, ErrorCodeWire::TxnNotFound | ErrorCodeWire::TransactionTimeout)
    )
}

/// Translate a [`BrainError`] into the renderer's card type. A
/// server-returned ERROR drives the heading off the wire `code`;
/// every client-side failure (I/O, frame/CBOR codec, protocol-sequence,
/// version mismatch, peer close, timeout) collapses to the client-side
/// card with code `0`.
#[must_use]
pub fn brain_error_to_renderable(err: &BrainError) -> RenderableError {
    match err {
        BrainError::Server { code, message, .. } => {
            RenderableError::from_server(*code as u16, message.clone())
        }
        _ => RenderableError::client_side(err.to_string()),
    }
}
