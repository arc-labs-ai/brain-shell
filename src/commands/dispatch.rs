//! Shared verb dispatch used by BOTH the one-shot CLI path
//! (`cli::args`) and the interactive REPL loop (`repl::repl_loop`).
//!
//! Keeping the wired-verb set and the deferral set in one place means
//! the two surfaces can never drift: if a verb graduates from "deferred"
//! to "wired", it does so once, here.

use brain_db_sdk::error::BrainError;
use brain_db_sdk::BrainClient;

use crate::commands::{self, Rendered};
use crate::parser::{Command, EntityCommand, RelationCommand, SchemaCommand, StatementCommand};
use crate::session::Session;

/// Outcome of attempting to dispatch a parsed [`Command`].
pub enum Dispatched {
    /// A wired verb ran end to end; carries the operation name (for
    /// timing footers / logging) and the renderer result.
    Ran {
        op: &'static str,
        result: Result<Rendered, BrainError>,
    },
    /// The verb parses but isn't wired in this build. The caller
    /// surfaces the standard "<verb> is not yet available" notice.
    Deferred(&'static str),
    /// The command is handled out-of-band by the caller (config / space
    /// / generate-completion / shell). Never returned for verbs that
    /// touch the wire.
    Handled,
}

/// Run a parsed [`Command`] against the live `client`, threading session
/// state. Only `encode` / `recall` / `forget` touch the wire; every
/// other verb returns [`Dispatched::Deferred`] with the same human name
/// both surfaces print. `config` / `profile` / `generate-completion` /
/// `shell` are caller-owned and come back as [`Dispatched::Handled`].
pub async fn dispatch_command(
    client: &BrainClient,
    session: &mut Session,
    cmd: Command,
) -> Dispatched {
    match cmd {
        Command::Encode(a) => Dispatched::Ran {
            op: "encode",
            result: commands::encode::run(client, session, a).await,
        },
        Command::Recall(a) => Dispatched::Ran {
            op: "recall",
            result: commands::recall::run(client, session, a).await,
        },
        Command::Forget(a) => Dispatched::Ran {
            op: "forget",
            result: commands::forget::run(client, session, a).await,
        },
        Command::Schema(SchemaCommand::Upload(a)) => Dispatched::Ran {
            op: "schema_upload",
            result: commands::schema::upload::run(client, session, a).await,
        },
        Command::Schema(SchemaCommand::Get(a)) => Dispatched::Ran {
            op: "schema_get",
            result: commands::schema::get::run(client, session, a).await,
        },
        Command::Schema(SchemaCommand::List(a)) => Dispatched::Ran {
            op: "schema_list",
            result: commands::schema::list::run(client, session, a).await,
        },
        Command::Schema(SchemaCommand::Validate(a)) => Dispatched::Ran {
            op: "schema_validate",
            result: commands::schema::validate::run(client, session, a).await,
        },
        Command::Entity(EntityCommand::List(a)) => Dispatched::Ran {
            op: "entity_list",
            result: commands::entity::list::run(client, session, a).await,
        },
        Command::Entity(EntityCommand::Show(a)) => Dispatched::Ran {
            op: "entity_show",
            result: commands::entity::show::run(client, session, a).await,
        },
        Command::Entity(EntityCommand::Resolve(a)) => Dispatched::Ran {
            op: "entity_resolve",
            result: commands::entity::resolve::run(client, session, a).await,
        },
        Command::Entity(EntityCommand::Neighbors(a)) => Dispatched::Ran {
            op: "entity_neighbors",
            result: commands::entity::neighbors::run(client, session, a).await,
        },
        Command::Statement(StatementCommand::List(a)) => Dispatched::Ran {
            op: "statement_list",
            result: commands::statement::list::run(client, session, a).await,
        },
        Command::Statement(StatementCommand::Show(a)) => Dispatched::Ran {
            op: "statement_show",
            result: commands::statement::show::run(client, session, a).await,
        },
        Command::Relation(RelationCommand::List(a)) => Dispatched::Ran {
            op: "relation_list",
            result: commands::relation::list::run(client, session, a).await,
        },
        Command::Link(a) => Dispatched::Ran {
            op: "link",
            result: commands::link::run(client, session, a).await,
        },
        Command::Unlink(a) => Dispatched::Ran {
            op: "unlink",
            result: commands::unlink::run(client, session, a).await,
        },
        Command::Plan(a) => Dispatched::Ran {
            op: "plan",
            result: commands::plan::run(client, session, a).await,
        },
        Command::Reason(a) => Dispatched::Ran {
            op: "reason",
            result: commands::reason::run(client, session, a).await,
        },
        Command::Txn(a) => Dispatched::Ran {
            op: "txn",
            result: commands::txn::run(client, session, a).await,
        },
        Command::Subscribe(a) => Dispatched::Ran {
            op: "subscribe",
            result: commands::subscribe::run(client, session, a).await,
        },
        Command::Config(_)
        | Command::Profile(_)
        | Command::GenerateCompletion(_)
        // `info` is built from cached state + the resolved identity, which
        // the dispatcher doesn't carry — both the CLI and the REPL assemble
        // it at the call site, so it's caller-owned like config / profile /
        // shell.
        | Command::Info
        | Command::Shell => Dispatched::Handled,
        other => Dispatched::Deferred(
            deferred_verb_name(&other).expect("invariant: non-wired, non-handled verb has a name"),
        ),
    }
}

/// Human-facing verb name for a parser variant that isn't wired in this
/// build. Returns `None` for the wired verbs (encode / recall / forget)
/// and the caller-owned subcommands (config / profile / generate-completion
/// / shell).
#[must_use]
pub fn deferred_verb_name(cmd: &Command) -> Option<&'static str> {
    match cmd {
        Command::Encode(_) | Command::Recall(_) | Command::Forget(_) => None,
        Command::Config(_)
        | Command::Profile(_)
        | Command::GenerateCompletion(_)
        | Command::Shell => None,
        // link / unlink / plan / reason / txn / subscribe are wired.
        Command::Plan(_) | Command::Reason(_) | Command::Link(_) | Command::Unlink(_) => None,
        Command::Txn(_) | Command::Subscribe(_) => None,
        // entity / statement / relation read verbs are wired.
        Command::Entity(
            EntityCommand::List(_)
            | EntityCommand::Show(_)
            | EntityCommand::Resolve(_)
            | EntityCommand::Neighbors(_),
        ) => None,
        Command::Statement(StatementCommand::List(_) | StatementCommand::Show(_)) => None,
        Command::Relation(RelationCommand::List(_)) => None,
        // `mention` has no wire op yet.
        Command::Mention(_) => Some("mention"),
        // `schema upload / get / list / validate` are all wired.
        Command::Schema(
            SchemaCommand::Upload(_)
            | SchemaCommand::Get(_)
            | SchemaCommand::List(_)
            | SchemaCommand::Validate(_),
        ) => None,
        // `info` is wired (assembled at the call site, not deferred).
        Command::Info => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_is_wired_not_deferred() {
        // `info` is assembled at the call site (CLI `args.rs` + REPL
        // `\info`), so it must not be in the deferred set — otherwise
        // `brain info` would print "not yet available in this build".
        assert!(deferred_verb_name(&Command::Info).is_none());
    }
}
