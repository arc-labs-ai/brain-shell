//! `brain` — interactive shell + one-shot CLI for the Brain memory database.
//! The `psql` / `redis-cli` / `mongosh` equivalent.
//!
//! brain-shell is a *consumer* of [`brain_db_sdk`]: the SDK owns the wire
//! protocol, transport, and verbs; this crate owns the parser, the REPL, the
//! per-verb command logic, credential profiles, and rendering. It never
//! depends on Brain's internal crates.
//!
//! # Module map (filled in across the revival phases)
//!
//! - `connection` — wrap [`brain_db_sdk::BrainClient`] with shell defaults.
//! - `parser`     — clap `Cli` + tokenizer (one tree for argv and REPL lines).
//! - `commands`   — one module per verb, returning a uniform renderer.
//! - `session`    — REPL state across lines (active txn, sticky context, ids).
//! - `repl`       — rustyline editor, completion, event loop.
//! - `cli`        — one-shot dispatch.
//! - `render`     — vendored terminal rendering (theme, table, graph, json).

#![forbid(unsafe_code)]

use std::process::ExitCode;

pub mod cli;
pub mod commands;
pub mod connection;
pub mod core;
pub mod exit;
pub mod parser;
pub mod render;
pub mod repl;
pub mod session;

/// Top-level entry — parses argv and dispatches to the one-shot path.
/// Returns a process exit code (0 success, 1 op failure, 2 usage error).
pub async fn run() -> ExitCode {
    cli::dispatch_argv(std::env::args().collect()).await
}
