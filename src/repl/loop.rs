//! REPL event loop.

use std::time::{Duration, Instant};

use brain_db_sdk::error::BrainError;
use brain_db_sdk::BrainClient;
use clap::Parser;
use rustyline::error::ReadlineError;

use crate::cli::config::Config;
use crate::commands;
use crate::commands::render_ctx;
use crate::commands::Dispatched;
use crate::connection;
use crate::parser::tokenize::tokenize_line;
use crate::parser::{format_txn_id, parse_server, Cli, Command, OutputFormatArg};
use crate::repl::editor;
use crate::repl::help;
use crate::session::Session;

/// Run the REPL until the user exits. Returns `Ok(())` on clean
/// shutdown.
///
/// `token` is the API-key credential the initial connection used; it is
/// kept as a mutable loop-local (never on `Session`) so the secret stays
/// out of a `Clone + Debug` struct, and is reused / swapped on reconnect
/// (`\connect` and `\profile use`). `profile` is the active credential
/// profile name, if any.
pub async fn run(
    mut session: Session,
    mut client: BrainClient,
    mut token: String,
    mut profile: Option<String>,
) -> anyhow::Result<()> {
    let (mut ed, history_path) = editor::build(session.recent_ids.clone())?;

    // Welcome card. Single dispatch through the renderer so the banner
    // picks up the same TermPolicy / Theme the rest of the shell uses.
    let banner = build_welcome_banner(&session, &client, profile.as_deref());
    let banner_ctx = render_ctx(
        OutputFormatArg::Auto,
        crate::parser::ColorMode::Auto,
        crate::parser::HyperlinkMode::Auto,
    );
    let mut stdout = std::io::stdout();
    if let Err(e) = crate::render::dispatch(&banner, &banner_ctx, &mut stdout) {
        eprintln!("banner render error: {e}");
    }

    loop {
        let prompt = session.prompt();
        let line = match ed.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(action) = parse_meta(trimmed) {
            if handle_meta(action, &mut session, &mut client, &mut token, &mut profile).await {
                break;
            }
            continue;
        }

        let tokens = match tokenize_line(trimmed) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };
        if tokens.is_empty() {
            continue;
        }
        let argv: Vec<String> = std::iter::once("brain".to_string()).chain(tokens).collect();

        // Unified help interception (pre-clap). Same reasoning as in
        // cli/args.rs — clap rejects `recall --help` for missing
        // <QUERY> otherwise. The pre-scan respects `--` so encoding
        // the literal text `--help` via `encode -- --help` works.
        if let Some(intent) = crate::parser::detect_help_intent(&argv) {
            let ctx = render_ctx(
                session.output.clone(),
                crate::parser::ColorMode::Auto,
                crate::parser::HyperlinkMode::Auto,
            );
            let mut stdout = std::io::stdout();
            if let Err(e) = help::render(intent.verb.as_deref(), &ctx, &mut stdout) {
                eprintln!("output error: {e}");
            }
            continue;
        }

        match Cli::try_parse_from(&argv) {
            Ok(cli) => {
                match cli.subcommand {
                    None | Some(Command::Shell) => {
                        eprintln!("(already in the shell)");
                    }
                    Some(Command::GenerateCompletion(_)) => {
                        eprintln!("generate-completion is only available as a one-shot subcommand");
                    }
                    Some(Command::Info) => {
                        // Mirror the `\info` meta — the bare verb is here so
                        // users who type `info` (no backslash) get the same
                        // diagnostic instead of a deferral notice.
                        let card = commands::info::collect(&client, &session, profile.as_deref());
                        let ctx = render_ctx(
                            cli.global
                                .output
                                .clone()
                                .unwrap_or_else(|| session.output.clone()),
                            cli.global.color,
                            cli.global.hyperlinks,
                        );
                        let mut stdout = std::io::stdout();
                        if let Err(e) = crate::render::dispatch(&card, &ctx, &mut stdout) {
                            eprintln!("output error: {e}");
                        }
                    }
                    Some(Command::Config(_)) | Some(Command::Profile(_)) => {
                        // `\config …` / `\profile …` are handled by parse_meta;
                        // the clap path only fires if someone typed the bare
                        // verbs inside the REPL. Point them at the meta form.
                        eprintln!(
                            "use `\\config <subcommand>` or `\\profile <subcommand>` inside the REPL"
                        );
                    }
                    Some(cmd) => {
                        run_one(&mut client, &mut session, cmd, &cli.global, &token).await;
                    }
                }
            }
            Err(e) => {
                eprint!("{e}");
            }
        }
    }

    editor::save(&mut ed, &history_path);
    Ok(())
}

async fn run_one(
    client: &mut BrainClient,
    session: &mut Session,
    cmd: Command,
    globals: &crate::parser::GlobalOpts,
    token: &str,
) {
    let started = Instant::now();
    let inherited_active_txn = inherits_active_txn(&cmd);
    // Ctrl-C while a command is in flight cancels just that command and drops
    // us back at the prompt — it must not kill the whole REPL. Dropping the
    // dispatch future cancels its mux stream; the connection stays usable for
    // the next command. `subscribe` is excluded: its streaming loop owns Ctrl-C.
    let dispatched = if matches!(cmd, Command::Subscribe(_)) {
        commands::dispatch_command(client, session, cmd).await
    } else {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                eprintln!("^C (cancelled)");
                return;
            }
            d = commands::dispatch_command(client, session, cmd) => d,
        }
    };
    match dispatched {
        Dispatched::Ran { result, .. } => {
            let elapsed = started.elapsed();
            let elapsed_ms = if session.timing {
                Some(elapsed.as_millis())
            } else {
                None
            };
            match result {
                Ok(body) => render_success(body.as_ref(), globals, session, elapsed_ms),
                Err(e) => {
                    if inherited_active_txn
                        && session.active_txn.is_some()
                        && commands::is_txn_terminal(&e)
                    {
                        let stale = session.active_txn.take();
                        if let Some(bytes) = stale {
                            eprintln!(
                                "note: server reported the active transaction is no longer \
                                 usable; session no longer attached to txn {}",
                                format_txn_id(&bytes),
                            );
                        }
                    }
                    render_error(&e, globals, session);
                    // A transport drop (server restart / network blip) leaves the
                    // held connection dead — every later command would fail until
                    // it's rebuilt. Re-establish it now so the REPL stays usable.
                    // Deliberately NOT an auto-retry: a write may have landed
                    // before the drop, so the user re-runs on purpose.
                    if connection::is_dead(&e) {
                        reconnect(client, session, token).await;
                    }
                }
            }
        }
        Dispatched::Deferred(verb) => {
            eprintln!("{verb} is not yet available in this build");
        }
        Dispatched::Handled => {
            // config / profile / generate-completion / shell are filtered
            // before this point; reaching here means a future variant
            // wasn't routed. Surface it rather than silently dropping.
            eprintln!("(this command is handled outside the verb dispatcher)");
        }
    }
}

/// Re-establish the connection in place after a drop, reusing the current
/// credential, with bounded retry/backoff. On failure the user can still
/// reconnect manually via `\connect`.
async fn reconnect(client: &mut BrainClient, session: &mut Session, token: &str) {
    eprintln!("connection to {} lost; reconnecting…", session.server);
    let policy = brain_db_sdk::retry::RetryPolicy::default();
    match connection::connect_with_retry(session.server, Duration::from_secs(30), token, &policy)
        .await
    {
        Ok(new_client) => {
            *client = new_client;
            // A reconnect is a fresh connection; the server re-derives the
            // tenant namespace, so refresh the cached value for the prompt.
            session.namespace = client.namespace().to_string();
            eprintln!("reconnected to {} — re-run your command.", session.server);
        }
        Err(e) => {
            eprintln!(
                "reconnect failed: {e} — fix the server and run `\\connect {}`",
                session.server
            );
        }
    }
}

/// Render a successful verb result through the same per-line override
/// discipline the one-shot path uses.
fn render_success(
    body: &dyn crate::render::Render,
    globals: &crate::parser::GlobalOpts,
    session: &Session,
    elapsed_ms: Option<u128>,
) {
    let mut stdout = std::io::stdout();
    let output = globals
        .output
        .clone()
        .unwrap_or_else(|| session.output.clone());
    let ctx = render_ctx(output.clone(), globals.color, globals.hyperlinks);
    if let Err(e) = crate::render::dispatch(body, &ctx, &mut stdout) {
        eprintln!("output error: {e}");
    }
    if let Some(ms) = elapsed_ms {
        if matches!(
            output,
            OutputFormatArg::Auto | OutputFormatArg::Table | OutputFormatArg::Wide
        ) {
            use std::io::Write as _;
            let _ = writeln!(stdout, "({ms} ms)");
        }
    }
}

/// Render a `BrainError` as a card via the renderer, matching the
/// success-path formatting. Errors land on stderr so JSON-piped success
/// output isn't corrupted by an error frame.
fn render_error(err: &BrainError, globals: &crate::parser::GlobalOpts, session: &Session) {
    let output = globals
        .output
        .clone()
        .unwrap_or_else(|| session.output.clone());
    let ctx = render_ctx(output, globals.color, globals.hyperlinks);
    let rendered = commands::brain_error_to_renderable(err);
    let mut stderr = std::io::stderr();
    if let Err(io_err) = crate::render::dispatch(&rendered, &ctx, &mut stderr) {
        // Fall back to the raw Display if the renderer itself errors;
        // never swallow the original diagnostic.
        eprintln!("error: {err}");
        eprintln!("(renderer failed: {io_err})");
    }
}

fn inherits_active_txn(cmd: &Command) -> bool {
    match cmd {
        Command::Encode(a) => a.txn.is_none(),
        Command::Recall(a) => a.txn.is_none(),
        Command::Link(a) => a.txn.is_none(),
        Command::Unlink(a) => a.txn.is_none(),
        Command::Forget(_) | Command::Plan(_) | Command::Reason(_) => true,
        // Browse + extract surfaces are read-only or admin-flavored;
        // none of them implicitly bind to the active txn the way encode
        // / link / unlink do.
        Command::Entity(_)
        | Command::Statement(_)
        | Command::Relation(_)
        | Command::Mention(_)
        | Command::Schema(_)
        | Command::Subscribe(_)
        | Command::Txn(_)
        | Command::Config(_)
        | Command::Profile(_)
        | Command::Shell
        | Command::Info
        | Command::GenerateCompletion(_) => false,
    }
}

// ─── meta commands ──────────────────────────────────────────────

#[derive(Debug)]
enum Meta {
    Quit,
    Help(Option<String>),
    SetOutput(OutputFormatArg),
    SetContext(u64),
    UnsetTxn,
    Timing(bool),
    Connect(String),
    Whoami,
    Profile(ProfileMeta),
    Namespace,
    ConfigSub(ConfigSub),
    Info,
    Caps,
    Unknown(String),
}

#[derive(Debug)]
enum ProfileMeta {
    List,
    Show(Option<String>),
    Use(String),
}

#[derive(Debug)]
enum ConfigSub {
    List,
    Get(String),
    Set { key: String, value: String },
    Path,
    Edit,
}

fn parse_meta(line: &str) -> Option<Meta> {
    let lower = line.trim();
    if lower == "quit" || lower == "exit" || lower == "\\q" || lower == "\\quit" {
        return Some(Meta::Quit);
    }
    if lower == "?" || lower == "\\?" || lower == "help" {
        return Some(Meta::Help(None));
    }
    if let Some(rest) = lower
        .strip_prefix("help ")
        .or_else(|| lower.strip_prefix("? "))
        .or_else(|| lower.strip_prefix("\\? "))
    {
        return Some(Meta::Help(Some(rest.trim().to_string())));
    }

    if let Some(rest) = lower.strip_prefix("\\set ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() == 2 && parts[0] == "output" {
            let v = parts[1].to_ascii_lowercase();
            return match v.as_str() {
                "auto" => Some(Meta::SetOutput(OutputFormatArg::Auto)),
                "json" => Some(Meta::SetOutput(OutputFormatArg::Json)),
                "table" => Some(Meta::SetOutput(OutputFormatArg::Table)),
                "wide" => Some(Meta::SetOutput(OutputFormatArg::Wide)),
                "ndjson" => Some(Meta::SetOutput(OutputFormatArg::Ndjson)),
                "yaml" => Some(Meta::SetOutput(OutputFormatArg::Yaml)),
                _ => Some(Meta::Unknown(v)),
            };
        }
        if parts.len() == 2 && parts[0] == "context" {
            if let Ok(n) = parts[1].parse::<u64>() {
                return Some(Meta::SetContext(n));
            }
        }
        return Some(Meta::Unknown(rest.to_string()));
    }

    if lower == "\\unset txn" {
        return Some(Meta::UnsetTxn);
    }

    if lower == "\\info" {
        return Some(Meta::Info);
    }

    if lower == "\\caps" {
        return Some(Meta::Caps);
    }

    if lower == "\\whoami" {
        return Some(Meta::Whoami);
    }

    // `\namespace` (alias `\ns`) — read-only: show the server-derived tenant.
    if lower == "\\namespace" || lower == "\\ns" {
        return Some(Meta::Namespace);
    }

    // `\profile` and `\profile <subcommand …>`
    if lower == "\\profile" {
        return Some(Meta::Profile(ProfileMeta::List));
    }
    if let Some(rest) = lower.strip_prefix("\\profile ") {
        return Some(parse_profile_sub(rest));
    }

    // `\config <subcommand …>`
    if let Some(rest) = lower.strip_prefix("\\config ") {
        return Some(parse_config_sub(rest));
    }
    if lower == "\\config" {
        return Some(Meta::Unknown("\\config requires a subcommand".into()));
    }

    if let Some(rest) = lower.strip_prefix("\\timing ") {
        return match rest.trim() {
            "on" => Some(Meta::Timing(true)),
            "off" => Some(Meta::Timing(false)),
            other => Some(Meta::Unknown(other.to_string())),
        };
    }

    if let Some(rest) = lower.strip_prefix("\\connect ") {
        return Some(Meta::Connect(rest.trim().to_string()));
    }

    if lower.starts_with('\\') {
        return Some(Meta::Unknown(lower.to_string()));
    }

    None
}

fn parse_profile_sub(rest: &str) -> Meta {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        ["list"] => Meta::Profile(ProfileMeta::List),
        ["show"] => Meta::Profile(ProfileMeta::Show(None)),
        ["show", name] => Meta::Profile(ProfileMeta::Show(Some((*name).to_string()))),
        ["use", name] => Meta::Profile(ProfileMeta::Use((*name).to_string())),
        ["use"] => Meta::Unknown("\\profile use needs <name>".into()),
        _ => Meta::Unknown(format!("\\profile {rest}")),
    }
}

fn parse_config_sub(rest: &str) -> Meta {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        ["list"] => Meta::ConfigSub(ConfigSub::List),
        ["path"] => Meta::ConfigSub(ConfigSub::Path),
        ["edit"] => Meta::ConfigSub(ConfigSub::Edit),
        ["get", key] => Meta::ConfigSub(ConfigSub::Get((*key).to_string())),
        ["set", key, value] => Meta::ConfigSub(ConfigSub::Set {
            key: (*key).to_string(),
            value: (*value).to_string(),
        }),
        _ => Meta::Unknown(format!("\\config {rest}")),
    }
}

/// Returns `true` when the loop should exit.
async fn handle_meta(
    meta: Meta,
    session: &mut Session,
    client: &mut BrainClient,
    token: &mut String,
    profile: &mut Option<String>,
) -> bool {
    match meta {
        Meta::Quit => true,
        Meta::Help(v) => {
            let payload = help::lookup(v.as_deref());
            let ctx = render_ctx(
                OutputFormatArg::Auto,
                crate::parser::ColorMode::Auto,
                crate::parser::HyperlinkMode::Auto,
            );
            let mut stdout = std::io::stdout();
            if let Err(e) = crate::render::dispatch(payload.as_ref(), &ctx, &mut stdout) {
                eprintln!("help render error: {e}");
            }
            false
        }
        Meta::SetOutput(o) => {
            let label = o.short_name();
            session.output = o;
            println!("output = {label}");
            false
        }
        Meta::SetContext(n) => {
            session.sticky_context = Some(n);
            println!("sticky context = {n}");
            false
        }
        Meta::UnsetTxn => {
            session.active_txn = None;
            println!("active txn cleared");
            false
        }
        Meta::Timing(on) => {
            session.timing = on;
            println!("timing = {on}");
            false
        }
        Meta::Connect(addr_str) => {
            match parse_server(&addr_str) {
                Ok(addr) => {
                    match connection::connect(addr, Duration::from_secs(30), token).await {
                        Ok(new_client) => {
                            *client = new_client;
                            session.server = addr;
                            // Re-derived by the server from auth on the new
                            // connection; refresh so the prompt stays accurate.
                            session.namespace = client.namespace().to_string();
                            println!("connected to {addr}");
                        }
                        Err(e) => eprintln!("connect failed: {e}"),
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
            false
        }
        Meta::Whoami => {
            print_whoami(client, profile.as_deref());
            false
        }
        Meta::Profile(sub) => {
            handle_profile_sub(sub, session, client, token, profile).await;
            false
        }
        Meta::Namespace => {
            // Read-only: the namespace is server-derived from auth; the shell
            // only reports what the connection resolved to. Empty == the
            // reserved `brain` system namespace (no tenant).
            let ns = client.namespace();
            if ns.is_empty() {
                println!("system namespace (no tenant)");
            } else {
                println!("namespace = {ns}");
            }
            false
        }
        Meta::ConfigSub(sub) => {
            handle_config_sub(sub, session);
            false
        }
        Meta::Info => {
            let card = commands::info::collect(client, session, profile.as_deref());
            let ctx = render_ctx(
                session.output.clone(),
                crate::parser::ColorMode::Auto,
                crate::parser::HyperlinkMode::Auto,
            );
            let mut stdout = std::io::stdout();
            if let Err(e) = crate::render::dispatch(&card, &ctx, &mut stdout) {
                eprintln!("output error: {e}");
            }
            false
        }
        Meta::Caps => {
            print_caps(client).await;
            false
        }
        Meta::Unknown(s) => {
            eprintln!("unknown meta command: {s}");
            false
        }
    }
}

/// `\whoami` — the authoritative, server-resolved identity.
fn print_whoami(client: &BrainClient, profile: Option<&str>) {
    let session = client.session();
    let perms = &session.permissions;
    let agent_hex: String = client
        .agent_id()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("agent_id    = {agent_hex}");
    let ns = client.namespace();
    if ns.is_empty() {
        println!("namespace   = (system)");
    } else {
        println!("namespace   = {ns}");
    }
    println!("profile     = {}", profile.unwrap_or("(token)"));
    println!("permissions:");
    println!("  can_encode = {}", perms.can_encode);
    println!("  can_recall = {}", perms.can_recall);
    println!("  can_plan   = {}", perms.can_plan);
    println!("  can_reason = {}", perms.can_reason);
    println!("  can_forget = {}", perms.can_forget);
    println!("  can_admin  = {}", perms.can_admin);
}

/// `\caps` — two views the operator cares about.
///
/// The first block is the per-agent grant + transport limits captured at
/// connect time (the handshake snapshot, always available). The second block
/// is the shard's *live* capability state — reranker, extractor tiers, active
/// schema namespaces, embedding dim — fetched fresh via GET_CAPABILITIES.
async fn print_caps(client: &BrainClient) {
    let session = client.session();
    let perms = &session.permissions;
    let features = &session.server_features;
    println!("capabilities (from handshake)");
    println!("  server_id            = {}", session.server_id);
    println!("  wire_version         = {}", session.chosen_version);
    println!("  bound_shard          = {}", session.bound_shard_id);
    println!("  can_encode           = {}", perms.can_encode);
    println!("  can_recall           = {}", perms.can_recall);
    println!("  can_plan             = {}", perms.can_plan);
    println!("  can_reason           = {}", perms.can_reason);
    println!("  can_forget           = {}", perms.can_forget);
    println!("  can_admin            = {}", perms.can_admin);
    println!("  max_payload_size     = {}", features.max_payload_size);
    println!(
        "  max_concurrent_streams = {}",
        features.max_concurrent_streams
    );
    println!("  idle_timeout_seconds = {}", features.idle_timeout_seconds);

    match client
        .capabilities(&brain_db_sdk::wire::types::GetCapabilitiesRequest {})
        .await
    {
        Ok(resp) => {
            let c = resp.capabilities;
            let namespaces = if c.schema_namespaces.is_empty() {
                "(none — brain system namespace only)".to_string()
            } else {
                c.schema_namespaces.join(", ")
            };
            println!("capabilities (live)");
            println!("  rerank               = {}", c.rerank);
            println!("  pattern_extractor    = {}", c.pattern_extractor);
            println!("  classifier_extractor = {}", c.classifier_extractor);
            println!("  llm_extractor        = {}", c.llm_extractor);
            println!("  vector_dim           = {}", c.vector_dim);
            println!("  schema_namespaces    = {namespaces}");
        }
        Err(e) => {
            eprintln!("capabilities (live): query failed: {e}");
        }
    }
}

/// Handle `\profile` subcommands. `use` switches the live credential and
/// reconnects; `list` / `show` are read-only and never print a token.
async fn handle_profile_sub(
    sub: ProfileMeta,
    session: &mut Session,
    client: &mut BrainClient,
    token: &mut String,
    profile: &mut Option<String>,
) {
    let mut config = match Config::load_or_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };
    match sub {
        ProfileMeta::List => {
            if config.profiles().is_empty() {
                println!(
                    "(no profiles — `brain profile add <name> --server <addr> --token <key>`)"
                );
                return;
            }
            let active = config.active_profile().map(str::to_owned);
            println!("{:<2} {:<20} SERVER", "", "NAME");
            for (name, entry) in config.profiles() {
                let marker = if active.as_deref() == Some(name.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!("{marker:<2} {:<20} {}", name, entry.server);
            }
        }
        ProfileMeta::Show(name) => {
            let resolved = name.or_else(|| config.active_profile().map(str::to_owned));
            match resolved {
                Some(n) => match config.get_profile(&n) {
                    Ok(e) => {
                        println!("name   = {n}");
                        println!("server = {}", e.server);
                        // Never print the token; only confirm one is set.
                        println!("token  = set (••••)");
                    }
                    Err(e) => eprintln!("error: {e}"),
                },
                None => {
                    println!("(no active profile — `\\profile use <name>` to select one)");
                }
            }
        }
        ProfileMeta::Use(name) => {
            // Switching credential reconnects, which drops any server-side
            // transaction handle tied to the current connection. Refuse while
            // a txn is open so the user makes the call explicitly.
            if session.active_txn.is_some() {
                eprintln!(
                    "error: active transaction prevents a profile switch — \
                     commit or abort first"
                );
                return;
            }
            let entry = match config.get_profile(&name) {
                Ok(e) => e.clone(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return;
                }
            };
            let addr = match parse_server(&entry.server) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("error: profile '{name}' has an invalid server: {e}");
                    return;
                }
            };
            match connection::connect(addr, Duration::from_secs(30), &entry.token).await {
                Ok(new_client) => {
                    *client = new_client;
                    session.server = addr;
                    session.namespace = client.namespace().to_string();
                    *token = entry.token;
                    *profile = Some(name.clone());
                    // Persist the switch so the next bare `brain` resumes here.
                    if let Err(e) = config
                        .set_active_profile(&name)
                        .and_then(|()| config.save())
                    {
                        eprintln!(
                            "note: could not persist active profile to config: {e}\n\
                             the switch is effective for this session only."
                        );
                    }
                    println!("switched to profile '{name}' — connected to {addr}");
                }
                Err(e) => eprintln!("connect failed for profile '{name}': {e}"),
            }
        }
    }
}

fn handle_config_sub(sub: ConfigSub, session: &mut Session) {
    let mut config = match Config::load_or_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };
    match sub {
        ConfigSub::List => {
            for (k, v) in config.list() {
                println!("{k:<15} {v}");
            }
        }
        ConfigSub::Get(key) => match config.get(&key) {
            Ok(v) => println!("{v}"),
            Err(e) => eprintln!("error: {e}"),
        },
        ConfigSub::Set { key, value } => match config.set(&key, &value) {
            Ok(()) => match config.save() {
                Ok(()) => {
                    // Mirror into the live session so the change takes
                    // effect immediately. `server` is the exception —
                    // `\connect` is the live verb for that, so we just
                    // persist without disturbing the open connection.
                    apply_setting_to_session(&key, &value, session);
                    println!("{key} = {value}");
                }
                Err(e) => eprintln!("error: {e}"),
            },
            Err(e) => eprintln!("error: {e}"),
        },
        ConfigSub::Path => println!("{}", config.path.display()),
        ConfigSub::Edit => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            if !config.path.exists() {
                if let Err(e) = config.save() {
                    eprintln!("error: {e}");
                    return;
                }
            }
            let status = std::process::Command::new(editor)
                .arg(&config.path)
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => eprintln!("editor exited with status {}", s.code().unwrap_or(-1)),
                Err(e) => eprintln!("error launching editor: {e}"),
            }
        }
    }
}

fn apply_setting_to_session(key: &str, value: &str, session: &mut Session) {
    match key {
        "output" => {
            session.output = match value {
                "auto" => OutputFormatArg::Auto,
                "json" => OutputFormatArg::Json,
                "wide" => OutputFormatArg::Wide,
                "ndjson" => OutputFormatArg::Ndjson,
                "yaml" => OutputFormatArg::Yaml,
                _ => OutputFormatArg::Table,
            };
        }
        "timing" => {
            session.timing = matches!(value, "true" | "on" | "1");
        }
        "sticky_context" => {
            if let Ok(n) = value.parse::<u64>() {
                session.sticky_context = Some(n);
            }
        }
        _ => {} // `server` / `active_profile` / unknown: no live-session mirror.
    }
}

// ─── banner ──────────────────────────────────────────────────────

/// Build the welcome banner from the server-assigned identity and session
/// metadata. Renders via the shared `WelcomeBanner` so the visual matches
/// the encode / info cards.
fn build_welcome_banner(
    session: &Session,
    client: &BrainClient,
    profile: Option<&str>,
) -> crate::render::WelcomeBanner {
    crate::render::WelcomeBanner {
        product_name: "brain-shell".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        server_addr: session.server.to_string(),
        agent_id: client.agent_id(),
        namespace: client.namespace().to_string(),
        profile: profile.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_meta_recognises_whoami() {
        assert!(matches!(parse_meta("\\whoami"), Some(Meta::Whoami)));
        assert!(matches!(parse_meta("  \\whoami  "), Some(Meta::Whoami)));
    }

    #[test]
    fn parse_meta_recognises_caps() {
        assert!(matches!(parse_meta("\\caps"), Some(Meta::Caps)));
    }

    #[test]
    fn parse_meta_recognises_namespace_and_alias() {
        assert!(matches!(parse_meta("\\namespace"), Some(Meta::Namespace)));
        assert!(matches!(parse_meta("\\ns"), Some(Meta::Namespace)));
        assert!(matches!(parse_meta("  \\ns  "), Some(Meta::Namespace)));
    }

    #[test]
    fn parse_meta_recognises_profile_bare_and_subcommands() {
        assert!(matches!(
            parse_meta("\\profile"),
            Some(Meta::Profile(ProfileMeta::List))
        ));
        assert!(matches!(
            parse_meta("\\profile list"),
            Some(Meta::Profile(ProfileMeta::List))
        ));
        match parse_meta("\\profile show work") {
            Some(Meta::Profile(ProfileMeta::Show(Some(n)))) => assert_eq!(n, "work"),
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\profile show") {
            Some(Meta::Profile(ProfileMeta::Show(None))) => {}
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\profile use prod") {
            Some(Meta::Profile(ProfileMeta::Use(n))) => assert_eq!(n, "prod"),
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\profile use") {
            Some(Meta::Unknown(s)) => assert!(s.contains("needs <name>")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_meta_recognises_config_subcommands() {
        assert!(matches!(
            parse_meta("\\config list"),
            Some(Meta::ConfigSub(ConfigSub::List))
        ));
        assert!(matches!(
            parse_meta("\\config path"),
            Some(Meta::ConfigSub(ConfigSub::Path))
        ));
        match parse_meta("\\config get output") {
            Some(Meta::ConfigSub(ConfigSub::Get(k))) => assert_eq!(k, "output"),
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\config set output json") {
            Some(Meta::ConfigSub(ConfigSub::Set { key, value })) => {
                assert_eq!(key, "output");
                assert_eq!(value, "json");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_meta_bare_config_is_unknown() {
        match parse_meta("\\config") {
            Some(Meta::Unknown(s)) => assert!(s.contains("subcommand")),
            other => panic!("got {other:?}"),
        }
    }
}
