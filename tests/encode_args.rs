//! `encode` flag parsing: `--request-id`, `--from-file`, `--from-stdin`,
//! `--wait`. Verifies parse, plus the mutual-exclusivity matrix clap
//! enforces. Memory kind, salience, edges, and dedup are server-internal
//! now — those flags are gone and must be rejected.

use brain_shell::parser::{Cli, Command};
use clap::Parser;

fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
    let mut full = vec!["brain".to_string()];
    full.extend(argv.iter().map(|s| (*s).to_string()));
    Cli::try_parse_from(full)
}

fn parse_ok(argv: &[&str]) -> Cli {
    parse(argv).expect("parse should succeed")
}

#[test]
fn encode_edge_flag_is_removed() {
    // Edges are derived server-side; the encode-time `--edge` flag is gone.
    // Use the `link` verb for explicit edges.
    let err = parse(&["encode", "hello", "--edge", "similar_to:s2/m17/v1"]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unexpected") || msg.contains("unrecognized") || msg.contains("--edge"),
        "--edge must be unknown to clap, got: {msg}"
    );
}

#[test]
fn encode_kind_and_salience_flags_are_removed() {
    // Memory kind and salience are decided server-side now.
    assert!(parse(&["encode", "hello", "--kind", "semantic"]).is_err());
    assert!(parse(&["encode", "hello", "--salience", "0.8"]).is_err());
}

#[test]
fn encode_request_id_accepted_as_uuid() {
    let cli = parse_ok(&[
        "encode",
        "hello",
        "--request-id",
        "019e3b00-0000-7000-8000-000000000001",
    ]);
    let Some(Command::Encode(a)) = cli.subcommand else {
        panic!("expected Encode")
    };
    assert!(a.request_id.is_some());
}

#[test]
fn encode_from_file_excludes_positional_text() {
    let err = parse(&["encode", "hello", "--from-file", "/tmp/x"]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot be used with") || msg.contains("not allowed"),
        "expected clap mutual-exclusivity error, got: {msg}"
    );
}

#[test]
fn encode_from_stdin_alone() {
    let cli = parse_ok(&["encode", "--from-stdin"]);
    let Some(Command::Encode(a)) = cli.subcommand else {
        panic!("expected Encode")
    };
    assert!(a.from_stdin);
    assert!(a.text.is_none());
}

#[test]
fn encode_rejects_unknown_flag_cleanly() {
    // Unknown flags must surface as a clap error (no panic, no
    // backtrace). The shell relies on clap's `try_parse_from` for
    // this; the regression is that nothing in the call chain
    // unwraps or panics on bad argv.
    let err = parse(&["encode", "hello", "--bogus-flag", "x"]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unexpected") || msg.contains("unrecognized") || msg.contains("--bogus-flag"),
        "expected clap unknown-arg error, got: {msg}"
    );
}

#[test]
fn encode_vector_flag_is_removed() {
    // The flag is gone — clap must reject it rather than silently
    // accepting it (which would mean a leftover declaration).
    let err = parse(&["encode", "hello", "--vector", "0.1,0.2,0.3"]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unexpected") || msg.contains("unrecognized") || msg.contains("--vector"),
        "--vector must be unknown to clap, got: {msg}"
    );
}

#[test]
fn encode_wait_parses_and_covers_all_stages() {
    let cli = parse_ok(&["encode", "hello", "--wait"]);
    let Some(Command::Encode(a)) = cli.subcommand else {
        panic!("expected Encode")
    };
    assert!(a.wait);
}

#[test]
fn encode_legacy_wait_for_extraction_flag_rejected() {
    // No back-compat alias: the old --wait-for-extraction is gone.
    // Brain is pre-release, so renaming wins over a synonym layer.
    assert!(parse(&["encode", "hello", "--wait-for-extraction"]).is_err());
}

#[test]
fn encode_dedup_flags_are_removed() {
    // Near-duplicate consolidation is automatic and server-internal; the
    // client has no dedup knob. All former dedup flags must be rejected.
    assert!(parse(&["encode", "hello", "--allow-duplicate"]).is_err());
    assert!(parse(&["encode", "hello", "--deduplicate"]).is_err());
    assert!(parse(&["encode", "hello", "--no-dedup"]).is_err());
}

#[test]
fn short_o_flag_aliases_output() {
    let cli = parse_ok(&["-o", "ndjson", "encode", "hi"]);
    use brain_shell::parser::OutputFormatArg;
    assert_eq!(cli.global.output, Some(OutputFormatArg::Ndjson));
}

#[test]
fn output_jsonpath_carries_expr() {
    let cli = parse_ok(&["-o", "jsonpath=.memory_id", "recall", "x"]);
    use brain_shell::parser::OutputFormatArg;
    match cli.global.output {
        Some(OutputFormatArg::JsonPath(expr)) => assert_eq!(expr, ".memory_id"),
        other => panic!("expected JsonPath, got {other:?}"),
    }
}
