//! Top-level argv dispatch — picks between one-shot exec, launching the
//! interactive REPL, and the connectionless helper subcommands (config /
//! profile / generate-completion).
//!
//! The three core verbs (encode / recall / forget) run end to end; every
//! other verb parses but reports that it is not yet wired in this build. A
//! bare `brain` (or `brain shell`) connects and launches the REPL. One-shot
//! and REPL share `commands::dispatch_command`, so their wired-verb set and
//! deferral list can't drift.
//!
//! Authentication is mandatory and fail-closed: the shell resolves an
//! API-key credential before connecting (see [`crate::cli::credential`]).
//! With no profile, no `--token`, and no `BRAIN_TOKEN`, startup aborts with
//! a non-zero exit rather than attempting an anonymous connect.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use brain_db_sdk::error::BrainError;
use clap::{CommandFactory, Parser};

use crate::cli::config::Config;
use crate::cli::credential::{self, CredentialInputs};
use crate::commands;
use crate::commands::render_ctx;
use crate::connection;
use crate::parser::{
    parse_server, Cli, Command, ConfigCommand, GlobalOpts, OutputFormatArg, ProfileCommand,
};
use crate::session::Session;

/// Entry point used by both `main` and integration tests.
pub async fn dispatch_argv(argv: Vec<String>) -> ExitCode {
    // ── unified help interception (pre-clap) ──────────────────────
    // clap validates required positional args before surfacing global
    // flags, so `brain recall --help` would otherwise fail "missing
    // <QUERY>". The pre-scan respects `--` so `brain encode -- --help`
    // (encode the literal text) still works. The rich per-verb help
    // cards live in the REPL help module; the CLI routes every help
    // request to clap's generated top-level help.
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
    if let Some(Command::Profile(p)) = cli.subcommand.clone() {
        return run_profile(p, &cli.global);
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

    // Load settings (defaults if file missing).
    let settings_file = match Config::load_or_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let settings = settings_file.settings().clone();

    // ── credential resolution — FAIL-CLOSED ────────────────────────
    // Resolve (server, token) from --profile / --token / active_profile.
    // No credential means no connection; abort before dialing.
    let token = cli.global.token.clone().filter(|t| !t.is_empty());
    let server_override = if cli.global.server != crate::parser::command::DEFAULT_SERVER {
        Some(cli.global.server.clone())
    } else {
        None
    };
    let resolved = match credential::resolve(
        &settings_file,
        &CredentialInputs {
            profile_flag: cli.global.profile.as_deref(),
            token: token.as_deref(),
            server_override: server_override.as_deref(),
        },
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let profile_name = resolved.source.profile_name().map(str::to_owned);

    let addr = match parse_server(&resolved.server) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let timeout = Duration::from_secs(cli.global.timeout);

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
    let client =
        match connection::connect_with_retry(addr, timeout, &resolved.token, &connect_policy).await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("connect failed: {e}");
                return ExitCode::from(crate::exit::for_error(&e));
            }
        };

    let mut session = Session::from_settings(addr, output_format, &settings);
    // A connection only exists with a token, so the session is always
    // authenticated past this point.
    session.authenticated = true;
    // The server derives the tenant namespace from auth and reports it in the
    // handshake; capture it for the prompt + `\info`. The client never sends it.
    session.namespace = client.namespace().to_string();

    // ── bare `brain` / `brain shell` → launch the interactive REPL ─
    if matches!(cli.subcommand, None | Some(Command::Shell)) {
        return match crate::repl::run_loop(session, client, resolved.token, profile_name).await {
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
    // needs the resolved identity the dispatcher doesn't carry — the same
    // `info::collect` the REPL's `\info` meta uses. No wire I/O.
    let res: Result<Box<dyn crate::render::Render>, BrainError> = if matches!(cmd, Command::Info) {
        Ok(Box::new(commands::info::collect(
            &client,
            &session,
            profile_name.as_deref(),
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
                unreachable!("config / profile / info / generate-completion handled above")
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
    let mut config = match Config::load_or_default() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
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
// brain profile <subcommand>
// ---------------------------------------------------------------------------

/// Connectionless credential-profile management. `add` reuses the global
/// `--server` / `--token` flags for the new profile's endpoint + key, so
/// there is one canonical pair of flags in the tree. Tokens are never
/// printed by any subcommand.
fn run_profile(cmd: ProfileCommand, globals: &GlobalOpts) -> ExitCode {
    let mut config = match Config::load_or_default() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    match cmd {
        ProfileCommand::List => {
            if config.profiles().is_empty() {
                println!(
                    "(no profiles — `brain profile add <name> --server <addr> --token <key>`)"
                );
                return ExitCode::SUCCESS;
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
            ExitCode::SUCCESS
        }
        ProfileCommand::Show { name } => {
            let resolved = name.or_else(|| config.active_profile().map(str::to_owned));
            match resolved {
                Some(n) => match config.get_profile(&n) {
                    Ok(e) => {
                        println!("name   = {n}");
                        println!("server = {}", e.server);
                        // Never print the token; just confirm one is set.
                        println!("token  = {}", redact(&e.token));
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(2)
                    }
                },
                None => {
                    println!("(no active profile — `brain profile use <name>` to select one)");
                    ExitCode::SUCCESS
                }
            }
        }
        ProfileCommand::Use { name } => match config.set_active_profile(&name) {
            Ok(()) => match config.save() {
                Ok(()) => {
                    println!("active profile → {name}");
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
        ProfileCommand::Add { name } => {
            let token = match globals.token.as_deref().filter(|t| !t.is_empty()) {
                Some(t) => t,
                None => {
                    eprintln!(
                        "error: `profile add` requires --token (or BRAIN_TOKEN) for the API key"
                    );
                    return ExitCode::from(2);
                }
            };
            match config.add_profile(&name, &globals.server, token) {
                Ok(()) => match config.save() {
                    Ok(()) => {
                        println!("added profile '{name}' → {}", globals.server);
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
        ProfileCommand::Remove { name } => match config.remove_profile(&name) {
            Ok(_) => match config.save() {
                Ok(()) => {
                    println!("removed profile '{name}'");
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
    }
}

/// Mask a token for display. Reports presence + a short tail so an
/// operator can tell two profiles apart without exposing the key.
fn redact(token: &str) -> String {
    if token.is_empty() {
        "(unset)".to_string()
    } else {
        "set (••••)".to_string()
    }
}
