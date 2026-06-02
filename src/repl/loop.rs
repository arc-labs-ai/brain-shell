//! REPL event loop.

use std::time::{Duration, Instant};

use brain_db_sdk::error::BrainError;
use brain_db_sdk::BrainClient;
use clap::Parser;
use rustyline::error::ReadlineError;

use crate::cli::agent::AgentIdSource;
use crate::cli::config::Config;
use crate::commands;
use crate::commands::render_ctx;
use crate::commands::Dispatched;
use crate::connection;
use crate::core::AgentId;
use crate::parser::tokenize::tokenize_line;
use crate::parser::{format_txn_id, parse_server, Cli, Command, OutputFormatArg};
use crate::repl::editor;
use crate::repl::help;
use crate::session::Session;

/// Run the REPL until the user exits. Returns `Ok(())` on clean
/// shutdown.
pub async fn run(
    mut session: Session,
    mut client: BrainClient,
    agent_id: AgentId,
    agent_source: AgentIdSource,
) -> anyhow::Result<()> {
    let (mut ed, history_path) = editor::build(session.recent_ids.clone())?;

    // Welcome card. Single dispatch through the renderer so the banner
    // picks up the same TermPolicy / Theme the rest of the shell uses.
    let banner = build_welcome_banner(&session, agent_id, &agent_source);
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
            if handle_meta(action, &mut session, &mut client, agent_id, &agent_source).await {
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
                        let card =
                            commands::info::collect(&client, &session, agent_id, &agent_source);
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
                    Some(Command::Config(_)) | Some(Command::Agent(_)) => {
                        // `\config …` / `\agent …` are handled by parse_meta;
                        // the clap path only fires if someone typed the bare
                        // verbs inside the REPL. Point them at the meta form.
                        eprintln!(
                            "use `\\config <subcommand>` or `\\agent <subcommand>` inside the REPL"
                        );
                    }
                    Some(cmd) => {
                        run_one(&client, &mut session, cmd, &cli.global).await;
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
    client: &BrainClient,
    session: &mut Session,
    cmd: Command,
    globals: &crate::parser::GlobalOpts,
) {
    let started = Instant::now();
    let inherited_active_txn = inherits_active_txn(&cmd);
    match commands::dispatch_command(client, session, cmd).await {
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
                }
            }
        }
        Dispatched::Deferred(verb) => {
            eprintln!("{verb} is not yet available in this build");
        }
        Dispatched::Handled => {
            // config / agent / generate-completion / shell are filtered
            // before this point; reaching here means a future variant
            // wasn't routed. Surface it rather than silently dropping.
            eprintln!("(this command is handled outside the verb dispatcher)");
        }
    }
}

/// Render a successful verb result through the same per-line override
/// discipline the one-shot path uses: per-line clap flags win over the
/// session-resolved default, and the timing footer only renders for
/// human-readable formats.
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
        | Command::Agent(_)
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
    Agent,
    AgentSub(AgentSub),
    ConfigSub(ConfigSub),
    Info,
    Caps,
    Unknown(String),
}

#[derive(Debug)]
enum AgentSub {
    List,
    Show(Option<String>),
    Use(String),
    Create { name: String, note: Option<String> },
    SetDefault(String),
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

    // `\agent` and `\agent <subcommand …>`
    if lower == "\\agent" {
        return Some(Meta::Agent);
    }
    if let Some(rest) = lower.strip_prefix("\\agent ") {
        return Some(parse_agent_sub(rest));
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

fn parse_agent_sub(rest: &str) -> Meta {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        ["list"] => Meta::AgentSub(AgentSub::List),
        ["show"] => Meta::AgentSub(AgentSub::Show(None)),
        ["show", name] => Meta::AgentSub(AgentSub::Show(Some((*name).to_string()))),
        ["use", name] => Meta::AgentSub(AgentSub::Use((*name).to_string())),
        ["create", name] => Meta::AgentSub(AgentSub::Create {
            name: (*name).to_string(),
            note: None,
        }),
        ["create", name, "--note", note @ ..] => Meta::AgentSub(AgentSub::Create {
            name: (*name).to_string(),
            note: Some(note.join(" ")),
        }),
        ["set-default", name] => Meta::AgentSub(AgentSub::SetDefault((*name).to_string())),
        ["set-default"] => Meta::Unknown("\\agent set-default needs <name>".into()),
        _ => Meta::Unknown(format!("\\agent {rest}")),
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
    agent_id: AgentId,
    agent_source: &AgentIdSource,
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
                    // sticky_agent (set via `\agent use`) takes precedence over
                    // the process-bound agent_id so the rebind actually changes
                    // the wire identity on reconnect.
                    let effective_agent = session.sticky_agent.unwrap_or(agent_id);
                    match connection::connect(
                        addr,
                        effective_agent.to_bytes(),
                        Duration::from_secs(30),
                    )
                    .await
                    {
                        Ok(new_client) => {
                            *client = new_client;
                            session.server = addr;
                            println!("connected to {addr} as {}", effective_agent.0);
                        }
                        Err(e) => eprintln!("connect failed: {e}"),
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
            false
        }
        Meta::Agent => {
            print_agent_info(&session.output, agent_id, agent_source);
            false
        }
        Meta::AgentSub(sub) => {
            handle_agent_sub(sub, session, agent_source);
            false
        }
        Meta::ConfigSub(sub) => {
            handle_config_sub(sub, session);
            false
        }
        Meta::Info => {
            let card = commands::info::collect(client, session, agent_id, agent_source);
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
            print_caps(client);
            false
        }
        Meta::Unknown(s) => {
            eprintln!("unknown meta command: {s}");
            false
        }
    }
}

/// `\caps` — capabilities from the handshake snapshot. The SDK has no
/// live GET_CAPABILITIES verb yet, so we render the permissions +
/// server features the server granted at connect time and label the
/// source so the operator knows it's cached, not freshly queried.
fn print_caps(client: &BrainClient) {
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
    println!(
        "  idle_timeout_seconds = {}",
        features.idle_timeout_seconds
    );
}

fn handle_agent_sub(sub: AgentSub, session: &mut Session, agent_source: &AgentIdSource) {
    let bound_name = source_name(agent_source);
    let (mut config, _note) = match Config::load_or_default() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };
    match sub {
        AgentSub::List => {
            if config.agents().is_empty() {
                println!("(no named agents — `\\agent create <name>` to add one)");
                return;
            }
            println!("{:<2} {:<16} {:<36} {:<20} NOTE", "", "NAME", "ID", "CREATED");
            for (name, entry) in config.agents() {
                let marker = if Some(name.as_str()) == bound_name.as_deref() {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{marker:<2} {:<16} {:<36} {:<20} {}",
                    name, entry.id, entry.created_at, entry.note,
                );
            }
        }
        AgentSub::Show(name) => {
            let resolved = name.or(bound_name);
            match resolved {
                Some(n) => match config.get_agent(&n) {
                    Ok(e) => {
                        println!("name       = {n}");
                        println!("id         = {}", e.id);
                        println!("created_at = {}", e.created_at);
                        if !e.note.is_empty() {
                            println!("note       = {}", e.note);
                        }
                    }
                    Err(e) => eprintln!("error: {e}"),
                },
                None => {
                    println!("(no named agent — this invocation is ephemeral)");
                }
            }
        }
        AgentSub::Use(name) => {
            // Sticky-agent binding. Looks the name up in config, parses
            // the UUID, stashes it on the session. The reconnect is the
            // owner of the open Client and lives in the caller — here we
            // refuse if there's an open transaction (its handle is tied
            // to the current agent's connection), and otherwise stash
            // the new id so a follow-up `\connect` (or process restart)
            // picks it up.
            if session.active_txn.is_some() {
                eprintln!(
                    "error: active transaction prevents agent rebind — \
                     commit or abort first"
                );
                return;
            }
            let entry = match config.get_agent(&name) {
                Ok(e) => e.clone(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return;
                }
            };
            match uuid::Uuid::parse_str(&entry.id) {
                Ok(uuid) => {
                    session.sticky_agent = Some(AgentId(uuid));
                    // Persist the switch so the next bare `brain`
                    // session picks the same agent. Best-effort: if
                    // the file write fails we keep the in-memory
                    // sticky_agent so the live session still uses the
                    // new id; the user sees a note explaining the
                    // session-vs-disk divergence.
                    if let Err(e) = config.set_active(&name) {
                        eprintln!(
                            "note: could not persist active flag to config: {e}\n\
                             the switch is effective for this session only."
                        );
                    }
                    println!(
                        "sticky agent set to '{name}' ({uuid}); reconnect via \
                         `\\connect <host:port>` to bind the new id on the wire."
                    );
                }
                Err(e) => eprintln!("error: agent '{name}' has malformed uuid: {e}"),
            }
        }
        AgentSub::SetDefault(name) => match config.set_default(&name) {
            Ok(()) => println!("default agent → {name}"),
            Err(e) => eprintln!("error: {e}"),
        },
        AgentSub::Create { name, note } => {
            // Mirror the CLI's first-create-promotes invariant so the
            // in-shell create path also keeps the file valid.
            let promote = if config.agents().is_empty() {
                crate::cli::config::AgentPromotion::DefaultAndActive
            } else {
                crate::cli::config::AgentPromotion::None
            };
            match config.create_agent(&name, note.as_deref().unwrap_or(""), promote) {
                Ok(e) => {
                    let id = e.id.clone();
                    if let Err(err) = config.save() {
                        eprintln!("error: {err}");
                        return;
                    }
                    println!("created agent '{name}' ({id})");
                }
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }
}

fn handle_config_sub(sub: ConfigSub, session: &mut Session) {
    let (mut config, _note) = match Config::load_or_default() {
        Ok(p) => p,
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
        _ => {} // `server` and unknown keys: no live-session mirror.
    }
}

// ─── source-aware banner + \agent helpers ───────────────────────

/// Build the welcome banner from the resolved agent identity and
/// session metadata. Renders via the shared `WelcomeBanner` so the
/// visual matches the encode / info cards.
fn build_welcome_banner(
    session: &Session,
    agent_id: AgentId,
    agent_source: &AgentIdSource,
) -> crate::render::WelcomeBanner {
    use crate::render::BannerAgentSource as BAS;
    let agent_name = match agent_source {
        AgentIdSource::NamedFlag { name, .. }
        | AgentIdSource::NamedEnv { name, .. }
        | AgentIdSource::ActiveFromConfig { name, .. }
        | AgentIdSource::DefaultFromConfig { name, .. }
        | AgentIdSource::AutoMinted { name, .. } => Some(name.clone()),
        AgentIdSource::IdFlag | AgentIdSource::IdEnv | AgentIdSource::Ephemeral => None,
    };
    let source = match agent_source {
        AgentIdSource::NamedFlag { name, .. } => BAS::NamedFlag(name.clone()),
        AgentIdSource::IdFlag => BAS::IdFlag,
        AgentIdSource::NamedEnv { name, .. } => BAS::NamedEnv(name.clone()),
        AgentIdSource::IdEnv => BAS::IdEnv,
        AgentIdSource::ActiveFromConfig { name, file } => BAS::ActiveFromConfig {
            name: name.clone(),
            file_display: file.display().to_string(),
        },
        AgentIdSource::DefaultFromConfig { name, file } => BAS::DefaultFromConfig {
            name: name.clone(),
            file_display: file.display().to_string(),
        },
        AgentIdSource::AutoMinted { name, file } => BAS::AutoMinted {
            name: name.clone(),
            file_display: file.display().to_string(),
        },
        AgentIdSource::Ephemeral => BAS::Ephemeral,
    };
    crate::render::WelcomeBanner {
        product_name: "brain-shell".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        server_addr: session.server.to_string(),
        agent_name,
        agent_id: agent_id.to_bytes(),
        agent_source: source,
    }
}

fn source_label(s: &AgentIdSource) -> String {
    match s {
        AgentIdSource::NamedFlag { name, file } => {
            format!("flag --agent {name} ({})", file.display())
        }
        AgentIdSource::IdFlag => "flag --agent-id".into(),
        AgentIdSource::NamedEnv { name, file } => {
            format!("env BRAIN_AGENT={name} ({})", file.display())
        }
        AgentIdSource::IdEnv => "env BRAIN_AGENT_ID".into(),
        AgentIdSource::ActiveFromConfig { name, file } => {
            format!("config active = {name} ({})", file.display())
        }
        AgentIdSource::DefaultFromConfig { name, file } => {
            format!("config default = {name} ({})", file.display())
        }
        AgentIdSource::AutoMinted { name, file } => {
            format!("auto-minted {name} ({})", file.display())
        }
        AgentIdSource::Ephemeral => "ephemeral (no config file path available)".into(),
    }
}

/// Name (if any) of the agent this session is currently bound to.
/// Used for the `*` marker in `\agent list` and for `\agent show`
/// fallback.
fn source_name(s: &AgentIdSource) -> Option<String> {
    match s {
        AgentIdSource::NamedFlag { name, .. }
        | AgentIdSource::NamedEnv { name, .. }
        | AgentIdSource::ActiveFromConfig { name, .. }
        | AgentIdSource::DefaultFromConfig { name, .. }
        | AgentIdSource::AutoMinted { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Render `\agent` (bare) — current binding summary.
fn print_agent_info(output: &OutputFormatArg, agent_id: AgentId, source: &AgentIdSource) {
    let human = matches!(
        output,
        OutputFormatArg::Auto | OutputFormatArg::Table | OutputFormatArg::Wide
    );
    if human {
        println!("agent_id = {}", agent_id.0);
        println!("source   = {}", source_label(source));
        if let Some(name) = source_name(source) {
            println!("name     = {name}");
        }
        return;
    }
    let kind = match source {
        AgentIdSource::NamedFlag { .. } => "named-flag",
        AgentIdSource::IdFlag => "id-flag",
        AgentIdSource::NamedEnv { .. } => "named-env",
        AgentIdSource::IdEnv => "id-env",
        AgentIdSource::ActiveFromConfig { .. } => "active",
        AgentIdSource::DefaultFromConfig { .. } => "default",
        AgentIdSource::AutoMinted { .. } => "auto-minted",
        AgentIdSource::Ephemeral => "ephemeral",
    };
    let name = source_name(source);
    let body = serde_json::json!({
        "op": "agent",
        "result": {
            "agent_id": agent_id.0.to_string(),
            "source": kind,
            "name": name,
        },
    });
    println!("{body}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ----- parse_meta dispatch -----------------------------------

    #[test]
    fn parse_meta_recognises_backslash_agent_bare() {
        assert!(matches!(parse_meta("\\agent"), Some(Meta::Agent)));
        assert!(matches!(parse_meta("  \\agent  "), Some(Meta::Agent)));
    }

    #[test]
    fn parse_meta_recognises_caps() {
        assert!(matches!(parse_meta("\\caps"), Some(Meta::Caps)));
    }

    #[test]
    fn parse_meta_recognises_agent_subcommands() {
        assert!(matches!(
            parse_meta("\\agent list"),
            Some(Meta::AgentSub(AgentSub::List))
        ));
        match parse_meta("\\agent show foo") {
            Some(Meta::AgentSub(AgentSub::Show(Some(n)))) => assert_eq!(n, "foo"),
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\agent show") {
            Some(Meta::AgentSub(AgentSub::Show(None))) => {}
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\agent use work") {
            Some(Meta::AgentSub(AgentSub::Use(n))) => assert_eq!(n, "work"),
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\agent create work") {
            Some(Meta::AgentSub(AgentSub::Create { name, note })) => {
                assert_eq!(name, "work");
                assert!(note.is_none());
            }
            other => panic!("got {other:?}"),
        }
        match parse_meta("\\agent create work --note prod notebook") {
            Some(Meta::AgentSub(AgentSub::Create { name, note })) => {
                assert_eq!(name, "work");
                assert_eq!(note.as_deref(), Some("prod notebook"));
            }
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

    // ----- source helpers ----------------------------------------

    #[test]
    fn source_name_returns_named_variants_only() {
        let p = PathBuf::from("/x/y");
        assert_eq!(
            source_name(&AgentIdSource::NamedFlag {
                name: "work".into(),
                file: p.clone(),
            }),
            Some("work".to_string())
        );
        assert_eq!(
            source_name(&AgentIdSource::NamedEnv {
                name: "demo".into(),
                file: p,
            }),
            Some("demo".to_string())
        );
        assert!(source_name(&AgentIdSource::IdFlag).is_none());
        assert!(source_name(&AgentIdSource::IdEnv).is_none());
        assert!(source_name(&AgentIdSource::Ephemeral).is_none());
    }
}
