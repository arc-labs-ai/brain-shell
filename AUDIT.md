# brain-shell — production-readiness audit (2026-06-26)

Verdict: functionally broad and well-built (commands, REPL, rendering, agent UX,
config), **not yet production-ready** — held back by one critical resilience gap
(no reconnect), coarse exit codes, no end-to-end tests, and a crates.io publish
blocker.

## BUILT (production-solid)
- 25 verbs fully wired end-to-end; one parser+dispatcher → one-shot ↔ REPL parity; no `todo!()`s.
- REPL: rustyline editing, persistent history (10k), tab-completion (verbs/flags/recent ids),
  state-aware prompt, full `\meta` command set.
- Agent-first-class UX: banner agent block, `\agent` list/show/use/create/set-default,
  named-agent switching, source annotation, first-run auto-mint.
- Rendering: 7 modes (table/wide/json/ndjson/yaml/jsonpath), TTY/color/hyperlink detect,
  pager, comfy-table, graph trees; polished renderers for encode/recall/entity/error/banner/help.
- Config: atomic writes, chmod 0600, TOML validation, agent registry, legacy migration.
- Help: complete per-verb cards + drift tests. Hygiene: forbid(unsafe), CI (fmt+clippy -D+test+doctest),
  full crates.io metadata, no unwraps on user/server input.

## PARTIAL (left to build)
- **Connection resilience — CRITICAL**: no connect retry/backoff, no auto-reconnect; REPL breaks
  after server restart; one-shot dies on transient error; dropped SUBSCRIBE can hang.
- **Exit codes — High**: only 0/1/2; conn/auth/not-found/server all → exit(1).
- **E2E tests — High**: 378 tests but zero integration vs a real/mock BrainClient; most run() paths +
  connection/error layer untested beyond parse/render units.
- **Signals — Medium**: Ctrl-C only in subscribe; no one-shot/dispatch handler, no SIGTERM, no in-flight abort.
- **Token redaction — Medium**: token safe (env/flag, not persisted) but SDK errors rendered raw.
- **UX polish — Low**: wide-mode only encode/error; no flag-arg completion; no true multi-line input.
- **Docs — Low**: 6 rustdoc warnings; no CHANGELOG/CONTRIBUTING; no checked-in completion scripts.

## NOT BUILT / DEFERRED
- `\caps`/GET_CAPABILITIES parsed but not wired.
- `encode --wait*` flags parse but are silent no-ops.
- `mention` deferred (no server op).
- Typed-graph WRITES (entity/statement/relation create), `query`, `materialize_procedural`
  intentionally not surfaced — decide if v1 needs explicit typed-graph writes.
- In-flight uncommitted refactor (parser/help/encode/recall, −229/+78) needs finishing + landing.

## PUBLISH BLOCKER
`[patch.crates-io] brain-db-sdk = { path = "../brain-sdk/rust" }` — cannot publish to crates.io
until the SDK is published (0.1.x) and the patch removed. Local/side-by-side builds fine.

## Roadmap
P0: (1) connection resilience [retry+backoff one-shot, auto-reconnect REPL, subscribe drop detect];
    (2) granular exit codes; (3) resolve SDK patch dep.
P1: (4) E2E tests vs mock server (all run()/error/connection paths); (5) signal handling +
    in-flight abort; (6) token redaction + wire \caps + make encode --wait* real-or-rejected;
    (7) land the in-flight refactor.
P2: (8) wide-mode recall/entity/statement, flag-arg completion, multi-line input;
    (9) fix rustdoc warnings, CHANGELOG/CONTRIBUTING, checked-in completions,
    doc-warnings + `cargo package` CI gates, help-drift tests for browse verbs.
