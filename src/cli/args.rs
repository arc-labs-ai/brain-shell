//! Top-level argv dispatch — picks between one-shot exec, launching the
//! interactive REPL, and the connectionless helper subcommands (config /
//! agent / generate-completion).
//!
//! The three core verbs (encode / recall / forget) run end to end; every
//! other verb parses but reports that it is not yet wired in this build. A
//! bare `brain` (or `brain shell`) connects and launches the REPL. One-shot
//! and REPL share `commands::dispatch_command`, so their wired-verb set and
//! deferral list can't drift.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use brain_db_sdk::error::BrainError;
use clap::{CommandFactory, Parser};

use crate::cli::agent;
use crate::cli::config::{Config, MigrationNote};
use crate::commands;
use crate::commands::render_ctx;
use crate::connection;
use crate::parser::{parse_server, AgentCommand, Cli, Command, ConfigCommand, OutputFormatArg};
use crate::session::Session;

/// Entry point used by both `main` and integration tests.
pub async fn dispatch_argv(argv: Vec<String>) -> ExitCode {
    // ── unified help interception (pre-clap) ──────────────────────
    // clap validates required positional args before surfacing global
    // flags, so `brain recall --help` would otherwise fail "missing
    // <QUERY>". The pre-scan respects `--` so `brain encode -- --help`
    // (encode the literal text) still works. The rich per-verb help
    // cards live in the REPL help module (later phase); for now we route
    // every help request to clap's generated top-level help.
    if crate::parser::detect_help_intent(&argv).is_some() {
        let mut cmd = Cli::command();
        let _ = cmd.print_help();
        println!();
        return ExitCode::SUCCESS;
    }

    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
    };

    // ── connectionless subcommands ─────────────────────────────────
    if let Some(Command::GenerateCompletion(args)) = &cli.subcommand {
        let mut cmd = Cli::command();
        clap_complete::generate(args.shell, &mut cmd, "brain", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    if let Some(Command::Config(c)) = cli.subcommand.clone() {
        return run_config(c);
    }
    if let Some(Command::Agent(a)) = cli.subcommand.clone() {
        return run_agent(
            a,
            cli.global.agent.as_deref(),
            cli.global.agent_id.as_deref(),
        );
    }

    // ── verbs not yet ported in this build ─────────────────────────
    // Bare `brain` / `brain shell` launch the REPL (below) and must not
    // trip this guard, so they're excluded from the deferral set.
    if !matches!(cli.subcommand, None | Some(Command::Shell)) {
        if let Some(verb) = cli
            .subcommand
            .as_ref()
            .and_then(commands::deferred_verb_name)
        {
            eprintln!("{verb} is not yet available in this build");
            return ExitCode::from(2);
        }
    }

    // ── agent + settings resolution ────────────────────────────────
    let resolved = match agent::resolve(cli.global.agent.as_deref(), cli.global.agent_id.as_deref())
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(note) = &resolved.migration {
        eprint_migration_note(note);
    }
    let typed_agent_id = resolved.agent_id;
    let agent_source = resolved.source;
    let agent_id = typed_agent_id.to_bytes();

    // Load settings (defaults if file missing). Migration is already
    // applied by the resolver above, so we don't surface it again here.
    let settings_file = match Config::load_or_default() {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let settings = settings_file.settings().clone();

    // Effective server: per-invocation flag > persisted setting > default.
    let server_str = settings
        .server
        .clone()
        .unwrap_or_else(|| cli.global.server.clone());
    let effective_server = if cli.global.server != crate::parser::command::DEFAULT_SERVER {
        cli.global.server.clone()
    } else {
        server_str
    };
    let addr = match parse_server(&effective_server) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let timeout = Duration::from_secs(cli.global.timeout);

    // Bearer token: `--token` flag, else `BRAIN_TOKEN` (clap merges the
    // two). An empty value is treated as "no token" so an unset env var
    // expanded into `--token ""` connects anonymously rather than sending
    // empty credentials.
    let token = cli.global.token.clone().filter(|t| !t.is_empty());

    // Output format: per-invocation flag > persisted setting > `Auto`.
    let output_format = cli.global.output.clone().unwrap_or(match settings.output {
        Some(crate::cli::config::OutputPref::Json) => OutputFormatArg::Json,
        Some(crate::cli::config::OutputPref::Table) => OutputFormatArg::Table,
        None => OutputFormatArg::Auto,
    });

    // Bounded retry on the initial connect so a just-restarted / briefly
    // overloaded server doesn't make the tool fail outright; auth / version
    // errors still fail fast. Exit code reflects the failure class.
    let connect_policy = brain_db_sdk::retry::RetryPolicy::default();
    let client = match connection::connect_with_retry(
        addr,
        agent_id,
        timeout,
        token.as_deref(),
        &connect_policy,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {e}");
            return ExitCode::from(crate::exit::for_error(&e));
        }
    };

    let mut session = Session::from_settings(addr, output_format, &settings);
    session.authenticated = token.is_some();

    // ── bare `brain` / `brain shell` → launch the interactive REPL ─
    if matches!(cli.subcommand, None | Some(Command::Shell)) {
        return match crate::repl::run_loop(session, client, typed_agent_id, agent_source, token)
            .await
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        };
    }

    let cmd = cli.subcommand.expect("invariant: bare/shell handled above");

    let started = Instant::now();
    // `info` is assembled here (not in `dispatch_command`) because the card
    // needs the resolved agent identity the dispatcher doesn't carry — the
    // same `info::collect` the REPL's `\info` meta uses. No wire I/O.
    let res: Result<Box<dyn crate::render::Render>, BrainError> = if matches!(cmd, Command::Info) {
        Ok(Box::new(commands::info::collect(
            &client,
            &session,
            typed_agent_id,
            &agent_source,
        )))
    } else {
        // Shared verb dispatch — the same wired-verb set + deferral list the
        // REPL uses, so the two surfaces can't drift. Ctrl-C aborts the in-flight
        // request (dropping the future cancels its stream) and exits 130, the
        // conventional 128+SIGINT code, rather than leaving the op half-run with
        // no signal to the caller. `subscribe` is excluded: its streaming loop
        // owns Ctrl-C (clean unsubscribe), so wrapping it here would pre-empt the
        // graceful teardown.
        let dispatched = if matches!(cmd, Command::Subscribe(_)) {
            commands::dispatch_command(&client, &mut session, cmd).await
        } else {
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("^C (interrupted)");
                    return ExitCode::from(130);
                }
                d = commands::dispatch_command(&client, &mut session, cmd) => d,
            }
        };
        match dispatched {
            commands::Dispatched::Ran { result, .. } => result,
            commands::Dispatched::Deferred(verb) => {
                eprintln!("{verb} is not yet available in this build");
                return ExitCode::from(2);
            }
            commands::Dispatched::Handled => {
                unreachable!("config / agent / info / generate-completion handled above")
            }
        }
    };
    let elapsed = started.elapsed();
    let elapsed_ms = if session.timing {
        Some(elapsed.as_millis())
    } else {
        None
    };

    match res {
        Ok(body) => {
            let mut stdout = std::io::stdout();
            let ctx = render_ctx(
                session.output.clone(),
                cli.global.color,
                cli.global.hyperlinks,
            );
            if let Err(e) = crate::render::dispatch(body.as_ref(), &ctx, &mut stdout) {
                eprintln!("output error: {e}");
                return ExitCode::from(1);
            }
            // Per-op timing footer — only meaningful for human formats;
            // structured outputs would break under a stray trailer line.
            if let Some(ms) = elapsed_ms {
                if matches!(
                    session.output,
                    OutputFormatArg::Auto | OutputFormatArg::Table | OutputFormatArg::Wide
                ) {
                    use std::io::Write as _;
                    let _ = writeln!(stdout, "({ms} ms)");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let ctx = render_ctx(
                session.output.clone(),
                cli.global.color,
                cli.global.hyperlinks,
            );
            let rendered = commands::brain_error_to_renderable(&e);
            let mut stderr = std::io::stderr();
            if crate::render::dispatch(&rendered, &ctx, &mut stderr).is_err() {
                eprintln!("error: {e}");
            }
            ExitCode::from(crate::exit::for_error(&e))
        }
    }
}

// ---------------------------------------------------------------------------
// brain config <subcommand>
// ---------------------------------------------------------------------------

fn run_config(cmd: ConfigCommand) -> ExitCode {
    let (mut config, note) = match Config::load_or_default() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = &note {
        eprint_migration_note(n);
    }
    match cmd {
        ConfigCommand::List => {
            for (k, v) in config.list() {
                println!("{k:<15} {v}");
            }
            ExitCode::SUCCESS
        }
        ConfigCommand::Get { key } => match config.get(&key) {
            Ok(v) => {
                println!("{v}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        ConfigCommand::Set { key, value } => match config.set(&key, &value) {
            Ok(()) => match config.save() {
                Ok(()) => {
                    println!("{key} = {value}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        ConfigCommand::Path => {
            println!("{}", config.path.display());
            ExitCode::SUCCESS
        }
        ConfigCommand::Edit => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            if !config.path.exists() {
                if let Err(e) = config.save() {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
            let status = std::process::Command::new(editor)
                .arg(&config.path)
                .status();
            match status {
                Ok(s) if s.success() => ExitCode::SUCCESS,
                Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
                Err(e) => {
                    eprintln!("error launching editor: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// brain agent <subcommand>
// ---------------------------------------------------------------------------

fn run_agent(cmd: AgentCommand, agent_flag: Option<&str>, agent_id_flag: Option<&str>) -> ExitCode {
    let (mut config, note) = match Config::load_or_default() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = &note {
        eprint_migration_note(n);
    }
    match cmd {
        AgentCommand::List => {
            let bound_name = resolved_bound_name(agent_flag, agent_id_flag);
            if config.agents().is_empty() {
                println!("(no named agents — `brain agent create <name>` to add one)");
                return ExitCode::SUCCESS;
            }
            println!(
                "{:<2} {:<16} {:<36} {:<20} NOTE",
                "", "NAME", "ID", "CREATED"
            );
            for (name, entry) in config.agents() {
                let marker = if Some(name.as_str()) == bound_name.as_deref() {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{marker:<2} {:<16} {:<36} {:<20} {}",
                    name, entry.id, entry.created_at, entry.note
                );
            }
            ExitCode::SUCCESS
        }
        AgentCommand::Show { name } => {
            let resolved_name = name.or_else(|| resolved_bound_name(agent_flag, agent_id_flag));
            match resolved_name {
                Some(n) => match config.get_agent(&n) {
                    Ok(e) => {
                        println!("name       = {n}");
                        println!("id         = {}", e.id);
                        println!("created_at = {}", e.created_at);
                        if !e.note.is_empty() {
                            println!("note       = {}", e.note);
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(2)
                    }
                },
                None => {
                    println!("(no named agent — this invocation would mint an ephemeral one)");
                    ExitCode::SUCCESS
                }
            }
        }
        AgentCommand::Create { name, note } => {
            let promote = if config.agents().is_empty() {
                crate::cli::config::AgentPromotion::DefaultAndActive
            } else {
                crate::cli::config::AgentPromotion::None
            };
            match config.create_agent(&name, note.as_deref().unwrap_or(""), promote) {
                Ok(entry) => {
                    let id = entry.id.clone();
                    match config.save() {
                        Ok(()) => {
                            println!("created agent '{name}' ({id})");
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                            ExitCode::from(1)
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            }
        }
        AgentCommand::Rename { old, new } => match config.rename_agent(&old, &new) {
            Ok(()) => match config.save() {
                Ok(()) => {
                    println!("renamed '{old}' → '{new}'");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        AgentCommand::Delete { name } => {
            let bound_name = resolved_bound_name(agent_flag, agent_id_flag);
            if bound_name.as_deref() == Some(name.as_str()) {
                eprintln!(
                    "error: refusing to delete '{name}' — the current invocation is bound to it"
                );
                return ExitCode::from(2);
            }
            match config.delete_agent(&name) {
                Ok(_) => match config.save() {
                    Ok(()) => {
                        println!("deleted agent '{name}'");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(1)
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            }
        }
        AgentCommand::SetDefault { name } => match config.set_default(&name) {
            Ok(()) => {
                println!("default agent → {name}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        AgentCommand::Import { name, id, note } => {
            let promote = if config.agents().is_empty() {
                crate::cli::config::AgentPromotion::DefaultAndActive
            } else {
                crate::cli::config::AgentPromotion::None
            };
            match config.import_agent(&name, &id, note.as_deref().unwrap_or(""), promote) {
                Ok(_) => match config.save() {
                    Ok(()) => {
                        println!("imported agent '{name}' ({id})");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(1)
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

/// Name of the agent this invocation would bind to — for the `*` marker
/// in `agent list`. Returns `None` when the binding is ephemeral or by
/// raw id (no name to compare against).
fn resolved_bound_name(agent_flag: Option<&str>, agent_id_flag: Option<&str>) -> Option<String> {
    if let Some(n) = agent_flag {
        return Some(n.to_owned());
    }
    if agent_id_flag.is_some() {
        return None;
    }
    if let Ok(s) = std::env::var(agent::ENV_VAR_NAME) {
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn eprint_migration_note(note: &MigrationNote) {
    eprintln!(
        "note: migrated legacy config.toml to named-agent schema (entry '{name}'); backup at {path}",
        name = note.migrated_name,
        path = note.backup_path.display(),
    );
}
