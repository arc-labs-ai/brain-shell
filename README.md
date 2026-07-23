# brain-shell

`brain-shell` is an interactive client — a `psql` / `redis-cli` / `mongosh` for
the [Brain](https://github.com/arc-labs-ai/brain-db) memory database. The user-facing
command is **`brain`**: run it bare for an interactive REPL, or with a verb for
one-shot scripting. Both paths share one parser and one dispatch, so they never
drift.

It is a thin UI layered on [`brain-db-sdk`](../brain-sdk): the SDK owns Brain's
`BRN0` wire protocol, transport, multiplexed connection, and verbs (verified
byte-for-byte against Brain's conformance corpus); the shell owns the parser,
the REPL, per-verb command logic, agent identity, and terminal rendering. It
depends **only** on the SDK — never on Brain's internal crates.

## Build & run

```bash
cargo build --release            # produces the `brain` binary
./target/release/brain           # interactive REPL against 127.0.0.1:8080
./target/release/brain --server host:8080 recall "what did I learn about X"
```

The SDK is a path dependency on the sibling `../brain-sdk/rust` checkout; have
both repos checked out side by side.

## Verbs

Memory:

```bash
brain encode "Priya leads the platform team" --salience 0.8
brain recall "who leads platform" --top-k 5 --min-confidence 0.6
brain forget <memory-id> [--hard]
brain link <src> <tgt> caused        # create an edge
brain unlink <src> <tgt> caused
```

Typed graph (read-side) + schema + cognition:

```bash
brain entity list --type 1
brain entity show <entity-id>
brain entity neighbors <entity-id>
brain statement list --subject <id>
brain relation list --from <id>
brain schema upload --from-file schema.brain
brain schema get <namespace>
brain schema list
brain schema validate --from-file schema.brain
brain plan --from "<state>" --to "<goal>"
brain reason "<observation>"
```

Transactions & live events:

```bash
brain txn begin            # subsequent writes can pass --txn <id>
brain txn commit | abort
brain subscribe                       # stream events until Ctrl-C
brain subscribe --collect 20          # collect N events then stop
```

## Output formats

Every verb renders a human table by default and a structured form on demand:

```bash
brain recall "x" --output json        # or: ndjson, yaml, jsonpath='$[0].text'
```

Color and hyperlinks auto-detect the terminal and honor `NO_COLOR` / `CLICOLOR`.

## Agent identity

A connection acts as an agent; memories are isolated per agent. Identity
resolves by precedence: `--agent-id` / `--agent <name>` flags → `BRAIN_AGENT*`
env → the active agent in the config → a freshly-minted one (persisted on first
run). Manage named agents with `brain agent …` (or `\agent` in the REPL).

## Interactive REPL

`brain` with no subcommand opens the REPL: line editing, persistent history, and
completion. Meta-commands are `\`-prefixed: `\help [verb]`, `\agent`, `\config`,
`\set` / `\unset`, `\info`, `\caps`, `\timing`, `\connect`, `\quit`.

## Status

All of the backup shell's verbs are wired end-to-end over both surfaces.
Not yet available: `mention` (no wire op exists server-side). `encode --wait`
parses but is a no-op pending event-stream integration.

## License

Apache-2.0.
