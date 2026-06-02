# brain-shell — Revival Build Plan

A production-grade interactive client (`psql` / `redis-cli` / `mongosh`) for the
Brain memory database, rebuilt by **reviving the proven backup implementation
on top of `brain-db-sdk`**.

This supersedes the earlier scaffold plan (a clean-room, independent-codec
shell). That direction is dropped: the SDK now exists and already *is* the
independent third-party-client proof, so the shell stands on it instead of
re-implementing the wire a second time.

> **Status: PLAN — build only after review.** No code is written from this plan
> until the phases below are approved.

---

## 1. Goal & chosen architecture

**Revive the backup shell** (`~/Desktop/backup/brain/crates/brain-shell`,
53 files, ~9,950 lines: parser, REPL, completion, help, agent identity, and a
command per verb) into this standalone repo (`brain-db-io/brain-shell`), layered
on the **`brain-db-sdk` Rust crate**.

### Why this architecture (decision: revive-on-SDK)

- **Keeps the working code.** The parser/REPL/command/help/agent business logic
  — the bulk of the value — survives the port. Only the dependency *seams*
  change.
- **The SDK is the client contract.** Brain ships no client; `brain-db-sdk` is
  the hand-written, conformance-guarded implementation of the wire protocol. The
  shell should be a *consumer* of that contract, not a second codec. Two
  independent wire implementations in one org would drift and double the
  maintenance.
- **The "third-party proof" is already satisfied** by the SDK itself (it
  re-implements the protocol from the spec + conformance corpus). The shell does
  not need to re-prove it.
- **Mux gets us a better client for free.** `BrainClient` verbs take `&self` and
  multiplex concurrent streams over one `MuxConnection` — eliminating the
  backup's 4-connection pool workaround.

### Layering

```
                       ┌─────────────────────────────────────────┐
  brain-shell (repo)   │  bin: brain                              │
                       │  ├─ parser   clap Cli + tokenizer        │
                       │  ├─ cli      one-shot dispatch           │
                       │  ├─ repl     rustyline loop + completion │
                       │  ├─ session  REPL state (txn, ctx, ids)  │
                       │  ├─ commands one module per verb         │
                       │  ├─ agent    identity (name+id, mint)    │
                       │  ├─ connection  thin BrainClient wrapper │
                       │  └─ render   ← vendored brain-explore    │
                       └───────────────────┬─────────────────────┘
                                           │ path dep
                       ┌───────────────────▼─────────────────────┐
  brain-db-sdk (repo)  │ BrainClient · MuxConnection · verbs ·    │
                       │ wire::{frame,cbor,opcode,types} · error  │
                       └───────────────────┬─────────────────────┘
                                           │ TCP / BRN0 frames
                                  ┌────────▼────────┐
                                  │   brain-server  │
                                  └─────────────────┘
```

### Decisions locked for this plan

1. **Architecture:** revive backup on `brain-db-sdk` (above).
2. **Rendering:** **vendor `brain-explore` as an internal `render` module** of
   brain-shell (the shell is now its only consumer — brain-cli is gone). Its 25
   server-internal type imports are re-pointed to `brain_db_sdk::wire::types`.
3. **SDK dependency:** **path dep** `brain-db-sdk = { path = "../brain-sdk/rust" }`.
   - Caveat to handle: isolated CI of this repo won't have the sibling checked
     out. Phase 0 documents the assumption (sibling repo present) and CI checks
     out both; a future move to a git/crates.io dep is a one-line swap.
4. **Runtime:** `tokio` (SDK is tokio-based). **REPL:** `rustyline`. **Args:**
   `clap` derive. **Output:** human table default, `--json` for scripts.

---

## 2. The SDK coverage gap (the central work item)

The backup shell drives more verbs than the SDK currently exposes. Closing this
gap is the spine of the plan. **The SDK is the right home for every verb** — the
shell stays a thin consumer — so missing verbs are added to `brain-db-sdk`
first, then consumed.

| Shell command | Wire op(s) | In `brain-db-sdk` today | Action |
|---|---|---|---|
| `encode` | ENCODE | ✅ `encode` | consume |
| `recall` | RECALL (streamed) | ✅ `recall` / `recall_frames` | consume |
| `forget` | FORGET | ✅ `forget` | consume |
| `entity create` | ENTITY_CREATE | ✅ `create_entity` | consume |
| `statement create` | STATEMENT_CREATE | ✅ `create_statement` | consume |
| `relation create` | RELATION_CREATE | ✅ `create_relation` | consume |
| `schema upload` | SCHEMA_UPLOAD | ✅ `upload_schema` | consume |
| `query` | QUERY | ✅ `query` | consume |
| (procedural) | MATERIALIZE_PROCEDURAL | ✅ `materialize_procedural` | consume |
| `info` / `\caps` | GET_CAPABILITIES | ⚠️ partial (`session()` snapshot) | add live `capabilities()` to SDK |
| `entity show/neighbors/list` | ENTITY_GET / NEIGHBORS / LIST | ❌ | **add to SDK** |
| `statement show/list` | STATEMENT_GET / LIST | ❌ | **add to SDK** |
| `relation list` | RELATION_LIST | ❌ | **add to SDK** |
| `mention list` | MENTION_LIST | ❌ | **add to SDK** |
| `schema get/list/validate` | SCHEMA_GET / LIST / VALIDATE | ❌ | **add to SDK** |
| `link` / `unlink` | edge create / remove | ❌ | **add to SDK** |
| `plan` | PLAN | ❌ | **add to SDK** |
| `reason` | REASON | ❌ | **add to SDK** |
| `subscribe` | SUBSCRIBE (event stream + replay) | ❌ | **add to SDK** (streaming) |
| `txn begin/commit/abort` | TXN_* | ❌ | **add to SDK** |

> Escape hatch (discouraged): the shell *could* send a raw frame via the SDK's
> `mux`/`wire` layers for a verb before the SDK gains it. We use this only to
> unblock a demo, and always backfill the SDK method — the wire must not leak
> into shell command code as the steady state.

Each SDK addition is a small, uniform unit: a wire `types` struct pair (+ opcode
if missing), a `BrainClient` method, and a conformance-corpus round-trip test —
mirroring the existing verbs. The exact opcodes/payload shapes are pinned from
`~/Desktop/brain/spec/04_wire_protocol/03_opcodes.md` and the protocol source at
build time.

---

## 3. Porting seams (what actually changes from the backup)

The backup compiled inside the Brain workspace; three seams must be rewritten
for the standalone repo. Everything else is largely mechanical.

1. **SDK API shape.** Backup used `brain_sdk_rust::{Client, ClientConfig,
   ClientError, PoolConfig}` with `Client::connect_with(addr, agent_id, config)`
   and an explicit pool. New API: `brain_db_sdk::{BrainClient, ClientConfig,
   Auth, BrainError}`; `BrainClient::connect_with(addr, config)` (agent flows
   through `Auth`/`ClientConfig`); verbs take `&self` and multiplex.
   → `connection.rs` shrinks: no pool, one `BrainClient`, agent in config.
2. **Server-internal types.** Backup imported `brain_core::AgentId` (14×) and
   `brain_protocol::…` (9×) directly. These don't exist standalone. Re-point to
   `brain_db_sdk::wire::types` (`WireMemoryId`, `MemoryKindWire`, `ForgetMode`,
   `EdgeRequest`, response structs…) and the SDK's id helpers (`new_id`,
   `agent_id() -> [u8;16]`). Where the SDK lacks a public type the shell needs,
   export it from the SDK (small additions).
3. **Rendering (vendored explore).** Copy `brain-explore/src` into
   `brain-shell/src/render/`, drop the placeholder TUI binary + `tui` feature
   (not needed), and re-point its 25 `brain_protocol`/`brain_core` import sites
   to `brain_db_sdk::wire::types`. Carries its own deps (`comfy-table`,
   `owo-colors`, `termtree`, `supports-hyperlinks`, `terminal_size`,
   `unicode-segmentation`, `serde_yaml`, `serde_json_path`, `time`).

---

## 4. Phases (each ends green: `cargo fmt && clippy -D warnings && test`)

### Phase 0 — Repo wiring & ground truth
- Replace the scaffold `Cargo.toml`/`src` with the revival skeleton; add the
  `../brain-sdk/rust` path dep; document the sibling-checkout assumption.
- Pin verb/opcode/payload facts for the gap list from the Brain spec + SDK
  source (read-only). Confirm the SDK's public type surface the shell needs.
- **Done when:** an SDK-linked skeleton `cargo build`s; the gap table is
  confirmed against real opcodes.

### Phase 1 — Vendor the renderer (foundation + SDK-covered renderers)
Rendering is vendored **incrementally, aligned with the verb phases** — the
gap-verb renderers (link/plan/reason/subscribe/txn) import types the SDK does
not have yet, so they land with their verbs in Phases 6–7, not here.
- Copy `brain-explore` → `src/render/`; **exclude** `tui/` (no TUI) and the
  gap-verb card modules. Keep the foundation (`theme`, `term`, `table`,
  `format`, `util`) + SDK-covered renderers (encode, memory, forget, entity /
  statement / relation cards, info, banner, help, graph_tree, error).
- Remount internal paths (`crate::` → `crate::render::`) and re-point
  `brain_protocol` / `brain_core` imports to `brain_db_sdk::wire::types`. The SDK
  structs are field-identical to the protocol's, so this is import-repointing,
  not field adaptation. `brain_core::MemoryId` → `WireMemoryId` (u128).
- **Done when:** the kept `render` subtree compiles against
  `brain_db_sdk::wire::types` and its render unit tests pass; `tui` + gap-verb
  renderers are recorded as deferred.

### Phase 2 — Connection + handshake + core verbs (encode/recall/forget)
- Port `connection.rs` onto `BrainClient`; port `parser`, `cli`, `session`,
  `agent` identity, and the encode/recall/forget command modules + renderers.
- **Done when:** `brain encode/recall/forget` work one-shot against a live
  `brain-server`; idempotency ids auto-minted; recall stream consumed to EOS.

### Phase 3 — REPL UX
- Port the rustyline loop, prompt (agent + endpoint), persistent history,
  completion, and the help system (`\help`, `\agent`, `\caps`, `\quit`/BYE).
- **Done when:** an interactive session encodes/recalls/forgets without restart;
  history + completion persist; help cards render.

### Phase 4 — Schema upload (the only typed-graph verb the SDK already covers) — DONE
Correction to the original plan: the backup shell has **no create or standalone
query commands**. Its typed-graph surface is entirely **read-side** (entity /
statement / relation / mention `list` / `show` / `neighbors`) plus **schema**
(upload / get / list / validate). Of those, only `schema upload` maps to an
existing SDK verb (`upload_schema`); everything else needs SDK-gap verbs and
moves to Phases 5–7.
- Ported `schema upload` (`SchemaUploadRequest` → `client.upload_schema`,
  AdHoc table of validation errors or the merged namespace/version). Wired into
  the shared dispatch; `schema get/list/validate` stay deferred (and the
  pre-connect guard knows `schema upload` is live).
- **Done when:** `schema upload` builds + dispatches; ✅ (live round-trip needs a
  server). 354 tests green, clippy `-D warnings` clean.

### Phase 5 — SDK gap I: read-side typed-graph (SDK + shell) — DONE
- **brain-db-sdk:** added `get_entity`, `list_entities`, `get_statement`,
  `list_statements`, `list_relations_from/to`, `get_schema`, `list_schemas`,
  `validate_schema`, live `capabilities()` (opcodes 0x0121–0x0155, 0x0032).
  LIST ops are streamed (`*_frames` + flattened pair, mirroring recall). Verified
  by round-trip + mock-server tests; the conformance corpus has no golden bytes
  for read ops (create-side only), so those are round-trip-verified, corpus intact.
- **brain-shell:** entity list/show/neighbors, statement list/show, relation
  list, schema get/list/validate — rewritten onto the flat wire verbs (the
  backup's typed-model `EntityHandle<Person>` API is gone) and un-deferred.
- **Capability gaps (surfaced as clean errors, not faked):**
  - `entity show <name>` → no ENTITY_RESOLVE op; by-id only.
  - `entity list --type <qname>` → wire filters on numeric `entity_type_id`; numeric only.
  - `statement list --object`, `relation list --from+--to` → no server-side
    filter; applied client-side over returned rows.
  - `mention` → no wire op exists; stays deferred.
  - `*View` carries ids not canonical names for far endpoints / evidence text →
    cards show ids where names aren't on the wire.
- **Done when:** builds + dispatches; ✅ 363 tests, clippy `-D warnings` clean
  (live round-trips need a server).

### Phase 6 — SDK gap II: edges + cognitive ops — DONE
- **SDK:** `link`/`unlink` (unary, 0x0025/0x0026), `plan`/`reason` (streamed,
  0x0022/0x0023; `_frames` + flattened pairs). Mixed enum serde matched to the
  protocol exactly (verified: `PlanStatus`/`PlanStrategy`/`ReasonStatus` are
  `serde_repr`; `TransitionKind`/`InferenceKind`/`PlanState`/`ObservationInput`
  are plain-serde strings).
- **Shell:** vendored the Phase-1-deferred link/plan/reason renderers (re-pointed
  to SDK types, re-exported) + ported the four commands + un-deferred them.
- **Done when:** builds + dispatches; ✅ 371 tests, clippy `-D warnings` clean.
  `PlanBudget.max_branches_explored` / `ReasonRequest.budget_wall_time_ms` have
  no clap arg → named-const defaults (256 / 5000ms), matching the backup.

### Phase 7 — SDK gap III: streaming + transactions — DONE
- **SDK:** TXN_* (begin/commit/abort, unary, 0x0040–0x0042) + SUBSCRIBE
  (0x0030). SUBSCRIBE needed a new mux primitive: the request path collects to
  EOS, but a subscription pushes events indefinitely — so the mux was refactored
  (shared state behind `Arc`) to add `subscribe() -> Subscription` with
  `next()`/`unsubscribe()`/Drop-cleanup. Verified: existing request path +
  conformance corpus intact, and a mock-server test proves drain → unsubscribe →
  EOS with no deadlock and no route leak.
- **Shell:** vendored the deferred txn + subscribe renderers; `txn` threads
  `session.active_txn`; `subscribe` streams events in a `select!` loop racing
  `ctrl_c()` (cross-platform) → `unsubscribe()`, with a `--collect N` mode.
- **`encode --wait`:** left as the graceful no-op + note (clean re-enable needs
  pre-encode subscription race/timeout handling not verifiable offline) — Phase 8.
- **Done when:** builds + dispatches; ✅ 378 tests, clippy `-D warnings` clean.
  Only `mention` (no wire op) + one-shot `info` (REPL meta) remain deferred.

### Phase 8 — Hardening & release — DONE
- Audited: `run()` → `dispatch_argv` (no stub); exit-code taxonomy consistent
  (0 success / 1 op-or-connect failure / 2 usage-or-validation); every
  `unwrap`/`expect`/`panic` is in test code or a documented-invariant `expect`
  (no unwrap-in-prod); `--json`/ndjson/yaml/jsonpath available for every verb via
  the render dispatch (each card has json tests); TTY color + truncation honor
  `NO_COLOR`/`CLICOLOR`; help-drift green.
- Rewrote `README.md` as a production README (build, full verb set with
  examples, output formats, agent identity, REPL meta-commands, status).
- **Done when:** ✅ production checklist passes; 378 shell tests + 50 SDK tests
  green; clippy `-D warnings` clean on both repos; zero deps on Brain internals.
- Deferred (documented): `mention` (no wire op), one-shot `brain info` (REPL
  `\info` covers it), `encode --wait` (graceful no-op pending event integration).

---

## 5. Testing strategy

- **Port the backup's tests.** `tests/{parser,session,encode_args,sticky_agent,
  help_drift}.rs` move over and are re-pointed to SDK types — they cover the
  parser, agent stickiness, arg handling, and help-card drift offline (no
  server).
- **SDK additions are conformance-guarded.** Every new SDK verb gets a
  corpus/round-trip test in the SDK repo (the SDK's existing discipline), so the
  shell consumes only verified codecs.
- **Integration tests** against a locally-spawned `brain-server` (dev config,
  `AuthMethod::None`), `#[ignore]`-by-default so unit/parser CI runs without a
  server.
- **Render snapshot tests** for table/graph/JSON output (vendored from explore).
- **No unwrap outside tests; clippy `-D warnings` in CI.**

---

## 6. Risks & open questions

1. **SDK type exposure.** Some types the shell needs may be private in the SDK
   (e.g. response sub-structs, enums). Mitigation: export them from the SDK as
   part of each gap phase; treat the SDK's public surface as a deliverable.
2. **Streaming/txn are the hardest SDK additions.** SUBSCRIBE (long-lived,
   server-push) and TXN (stateful) are more than request/response. They are
   deliberately last (Phase 7) and may each warrant their own sub-plan in the
   SDK repo before shell work starts.
3. **Path-dep + CI.** Isolated CI must check out the sibling SDK repo. If that
   becomes painful, switch to a git dep (one-line change) — noted, not blocking.
4. **Spec drift.** Opcodes/payloads are pinned from the Brain spec at build
   time; the SDK's conformance corpus is the drift guard. Re-confirm at the
   start of each gap phase.
5. **Scope.** Phases 0–4 deliver a genuinely useful shell (all core + create
   verbs) on the SDK as-is. Phases 5–7 are SDK-coupled expansions and can be
   prioritized/deferred independently.

---

## 7. Constraints (separate-repo hygiene)

- brain-shell and brain-db-sdk are **client-side** repos under `brain-db-io`.
  Neither adds CI, `just` targets, or path deps into the **Brain server repo**
  (`~/Desktop/brain`). The Brain spec/corpus is read-only ground truth only.
- The shell depends only on the SDK (+ vendored renderer + leaf crates) — never
  on Brain's internal crates (`brain-core`, `brain-protocol`, `brain-ops`, …).
- Public verbs are domain verbs (`encode`, `recall`, `query`), not engine names.
- No `Co-Authored-By` trailers on commits.
