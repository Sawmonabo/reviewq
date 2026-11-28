# Findings Ledger Implementation Plan

**Goal:** Replace the single report hand-over with the findings ledger: agents file records through six tools, ReviewQ composes and publishes every report version, and no code of the earlier path remains.

**Architecture:** Each ledger call is one short SQLite write behind the existing native tool paths. Publishing reads the records, composes the fixed layout outside SQLite and commits one conditional transaction. ReviewQ decides what a turn owes from stored state, states one rule at launch, repeats it after compaction and backs it with a stop gate.

**Tech stack:** Rust 1.100 (Edition 2024), rusqlite, schemars and jsonschema for the tool contract, pulldown-cmark for field checks, sha2, the Claude plugin's JavaScript module, React and TypeScript for the reader. No new package.

**Spec:** [Review workflow](../../specs/review-workflow.md#findings-ledger-and-report-versions), [persistence](../../specs/persistence.md), [sessions](../../specs/agent-sessions.md). The [REPORT brief](reports.md) lists the deliverables and the evidence. Read the spec sections before a task. This plan fixes ownership, order and the names tasks share.

## Global constraints

- This is a fresh-installation product. Write no migration, no schema version bump, no compatibility field and no fallback to the earlier path. `FORMAT_ID` stays as it is.
- Delete what a change makes unused in the same task. No `#[allow(dead_code)]`. A relocated crate-private item that waits one stage for its caller carries `#[expect(dead_code, reason = "…")]`, which the compiler rejects once the caller exists. A public item of a library needs nothing and must not carry it.
- Limits come from the spec and are named constants with units: title 1–200 characters, category 1–48, extra-note title 1–120, reasons 1–1,024, path 1–512, at most 8 locations, 10 IDs per read, 10 discussion IDs, 10 assessments, `issue`, `consequence`, evidence text and `proposedComment` 8 KiB each, `details` 16 KiB, note body 32 KiB, at most 12 extra notes, body line 8 KiB, 200 active findings per run, tool input 128 KiB, composed text 4 MiB, `read_draft` page 64 KiB, read-back 8,000 bytes, 50 field errors plus a count, 2 gate blocks per session and turn, 3 recompose attempts, chunks of 64 KiB.
- A ledger call reads no transcript, resumes no thread and makes no provider call. Run nothing but SQLite work inside a write transaction.
- Every code file stays at or below 900 lines. Split by responsibility at 800.
- The protocol's `validate_ledger_input` is the one owner of field shapes and sizes, in characters and in bytes. The app owns the Markdown rules and the per-run limits. The store owns revisions, duplicates and replay. No layer repeats another's check.
- Tests stay few: only the cases this plan names. Each guards a regression that no existing check catches. Delete the tests of deleted code.
- The browser keeps its look. Reader changes remove code that only served authored reports or fix a named bug.
- Follow the [Rust rules](../../../crates/AGENTS.md), the [web rules](../../../web/AGENTS.md) and the [Rust conventions](../../rust-conventions.md): `name.rs` plus `name/child.rs`, private by default, one owner for a shared rule.
- Reuse before writing: the writer and reader connection split, `WriteTx`, `tx.emit`, `insert_notification`, native-call replay, the chunk readers, the display index trigger, the batch completion and the jsonschema field-error collector already exist.
- State an expected performance benefit as expected. Record a measured one with its command and numbers.

## Review focus

These are the conditions most likely to hurt the user and least likely to be seen in a quick run. Each has an owner.

1. An agent writes `Vec<u8>` or a placeholder such as `<runner>` outside code. The text stays visible. Owner: W1 (reader shows raw HTML as text) and K1 (the expected composed file contains such a line).
2. A field leaves a code fence open, at the top level or inside a list item. Only the first can swallow the next card and only it is refused, with its line. Owner: F1 test `stand_alone_fields`.
3. The lead retries `save_finding` as a new call after a lost reply. The same ID and revision come back and no second finding exists. Owner: K2 test `ledger_writes_are_idempotent`.
4. A subagent files after the lead went idle, or the lead's turn was interrupted after a change. The run still settles: an update is published, or the review shows as unfinished once. Owner: F1 test `settle_and_gate_decisions` and V1 flow step 5.
5. The user saves a disposition while the lead closes. The closing call recomposes and the user's choice is on the card. Owner: K2 test `publish_is_conditional`.

## Task graph

Stages run in order. Tasks inside a stage run at the same time in the primary checkout on branch `findings-ledger`, each on its own files.

| Stage | Task | Owns | Needs |
|---|---|---|---|
| 1 | **C1** Remove the hand-over path and settle the read contract | Everything listed under C1 | — |
| 2 | **F1** Domain model, app ports and field checks | `crates/reviewq-domain/src/ledger.rs`, `crates/reviewq-app/src/ledger.rs`, `ledger/markdown.rs` and the other, empty children | C1 |
| 2 | **F2** Tool contract | `crates/reviewq-protocol/src/ledger.rs`, the schema export, `contracts/`, `fixtures/contracts/`, the generated plugin schema, Makefile contract lines | C1 |
| 2 | **W1** Reader cleanup | `web/src/features/review/report/**`, `web/src/components/markdown/**`, generated web types | C1 |
| — | **Review A** | Stages 1 and 2 | |
| 3 | **K1** Composer, publishing and settling | `crates/reviewq-app/src/ledger/{compose,publish}.rs`, the expected composed file | F1 |
| 3 | **K2** Ledger storage | `crates/reviewq-store/src/ledger.rs` and `ledger/`, `schema/ledger.sql`, the store edits named in K2 | F1 |
| 3 | **K3** Record tools, passes, gate, rule and read-back | `crates/reviewq-app/src/ledger/{record,passes,readback}.rs`, `launch.rs`, `followups/work.rs` | F1 |
| 4 | **E1** Routes and MCP tools | `crates/reviewq-transport/src/entry/features/ledger.rs`, `entry/mcp/ledger.rs`, their registrations, the shared bridge guard, the Claude hook route | F2, K1–K3 |
| 4 | **E2** Binary: bridge, hooks, installer, wiring | `crates/reviewq/src/native_client/**`, `install/**`, `cli.rs`, `runtime/**` | K1–K3 |
| 4 | **E3** Claude plugin | `hosts/claude/plugin/hooks/{ledger.js,register.jsx,workflow.js,hooks.json,bridge.js}` | F2 |
| 5 | **X1** Fixtures and the populated view | `tools/dev/**`, `fixtures/reviews/**`, `docs/plans/01-foundation/fixtures.md` | E1–E3 |
| — | **Review B** | Stages 3 and 4 | |
| 6 | **V1** End-to-end evidence, measurements, README and briefs | `README.md`, `docs/**` | All |
| — | **Final review** | The whole branch against `main` | |

### Dispatch rules

- Implementers run on Opus 5.5 at high effort and reviewers on Opus 5.5 at xhigh. A reviewer gets the diff and the spec, never the implementer's summary.
- An implementer edits only the paths its task owns. A needed change elsewhere is reported to the lead with the reason, and the lead assigns it.
- C1 and the review fixes left `#[expect(dead_code)]` on items that wait for a caller, some at module level in `ledger/layout.rs` and `ledger/markdown.rs`. A task removes the attribute from an item it starts to call when the file is its own. The lead removes the shared ones at the stage check.
- Tasks of one stage share one tree, so a package check can fail on a sibling's half-written file. An implementer that sees an error outside its own files waits and checks again, never repairs it, and reports it if it persists. The lead runs the stage's full check once every task of the stage has reported.
- Implementers run no Git command that writes. The lead checks each task's paths, runs the stage checks and commits one task per commit. The branch is pushed after each accepted stage and is never merged by this plan.
- An implementer never signals a process it did not start and starts no service on the default profile. Checks use `cargo check --locked -p <package>` while editing and the Make targets named in the task before reporting.
- Before each stage the lead searches the tree for an existing implementation of anything a task is about to add. After each stage the lead compares the new files side by side for repeated queries, validation or text, and gives a repeat one owner before the next stage starts.
- A stage is accepted when its checks pass, its review findings are fixed and `rg` finds none of that stage's removed names.

## Shared names

Tasks use these names exactly. F1 and F2 give them their Rust types. Later tasks read those files rather than this table.

| Thing | Name |
|---|---|
| Tools | `save_finding`, `set_finding_outcome`, `save_section`, `read_draft`, `submit_review`, `wait_for_user` |
| Routes under `/api/v1/native/ledger/` | `save-finding`, `set-finding-outcome`, `save-section`, `read-draft`, `submit-review`, `wait-for-user`, `gate`, `readback` |
| Tool route body and replies | Claude's bridge only: `{binding:{sessionKey,callId,turnId?,agentId?},input:{…}}`. Codex reaches the same operations through the MCP server, which builds the app's `HostSession::Codex` from the request metadata and its actor. Success is the tool's result. A refused input is the existing error reply of `entry/error.rs` with `fieldErrors:[{path,message}]`; no second error shape is added. The binding never travels inside `input`. |
| Gate and read-back bodies | Codex hooks only. Gate: `{threadId,turnId?,stopHookActive}` returning `{block:false}` or `{block:true,reason}`. Read-back: `{threadId}` returning `{text}`. Claude's hook route returns the same two replies. |
| Claude bridge action | `reviewq internal claude-bridge ledger <id>`, with the tool, the call's identity and the input as one JSON object on stdin, and the route's reply on stdout |
| Hook helpers | `reviewq internal claude-hook` (existing, now also answers `Stop` and `SessionStart`), `reviewq internal codex-hook` (new, replaces `codex-report-witness`) |
| Hook replies | Stop hold, on stdout: Codex `{"decision":"block","reason":"…"}`; Claude `{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"…"}}`, which Claude Code shows as hook feedback and not as a hook error. Read-back: the text on stdout. Nothing on stdout means no effect. |
| Generated contract files | `contracts/ledger-tools.schema.json`, `fixtures/contracts/ledger-tools.json`, `hosts/claude/plugin/hooks/ledger-schema.js` |
| Schema export flags | `--ledger-tools-schema`, `--ledger-tools-example`, `--claude-ledger-tools` |
| Tables | `ledger_findings`, `ledger_notes`, `ledger_changes`, `review_passes`, `ledger_calls`, `report_delivery_failures`, `codex_stop_hook_reported` |
| Failure kinds | `unfinished_review`, `closing_refused`, `update_failed` |
| Delivery states | absent, `saved`, `failed` |
| Version kinds | `full_review`, `selected_threads`, `update` |
| App modules | `ledger::record`, `ledger::passes`, `ledger::markdown`, `ledger::compose`, `ledger::publish`, `ledger::readback` |
| Store port | `LedgerStore` in `crates/reviewq-app/src/ledger.rs` |
| Turn completion | Claude's existing turn signal and the stored session states. There is no turn-end route. Claude's `Stop` goes through its existing hook route, which answers with the gate reply. Codex's `Stop` hook posts to `gate`. |

---

### C1: Remove the hand-over path and settle the read contract

One writer. Work top-down so each step compiles: callers first, then what they called.

**Files**

- Delete: `crates/reviewq-transport/src/entry/features/reports/{handoff,witness}.rs`, `entry/mcp/report.rs`; `crates/reviewq/src/cli/report.rs`, `native_client/codex.rs`; `crates/reviewq-providers/src/report_file.rs`, `history/report.rs`, `history/codex_report.rs` and its tests; `crates/reviewq-app/src/reports/{capture,ledger,index,structured}.rs`; `crates/reviewq-store/src/reports/capture.rs`, `reports/capture/**`, `reports/address.rs`; `crates/reviewq-protocol/src/reports/handoff.rs`, `reports/handoff/**`; `contracts/report-handoff.schema.json`, `fixtures/contracts/report-handoff.json`, `hosts/claude/plugin/hooks/report-handoff-schema.js`.
- Modify: every caller of the above. The code maps name them: the route and tool registries, `error.rs`, `entry.rs`, `entry/hosts/claude/delivery.rs`, `mcp.rs`, `mcp/operations.rs`, `features/reports.rs`, `cli.rs`, `cli/workflow.rs`, `native_client.rs`, `native_client/claude.rs`, `install.rs`, `install/bundle.rs`, `install/codex_hook.rs`, `runtime.rs`, the two `Cargo.toml` files that lose a dependency, the provider `workflow` and `history` modules, app `reports.rs`, `workflow.rs`, `access.rs`, `launch.rs`, `followups/work.rs`, `hosts/claude.rs`, store `reports.rs`, `reports/context.rs`, `connection.rs`, `connection/database.rs`, `launches.rs`, `reports/mutations.rs`, `followups/work.rs`, `followups/work/idle.rs`, `followups/poll.rs`, `notifications/gitlab.rs`, `connection/display_cache.rs`, `hosts/recovery/dispatch.rs`, `hosts/claude/workflow.rs`, `queue/archive.rs`, `schema/{reports,projection_evidence,queue,launches,claude}.sql`, protocol `reports.rs`, `scalar.rs`, `mcp.rs`, `bin/export-schema.rs` (and delete `bin/export-schema/report.rs`), domain `review_stage.rs` and `identity.rs`, the `Makefile` contract lines and `hosts/claude/plugin/hooks/{register.jsx,workflow.js}`.

**Keep and relocate**

| Item today | New home | Why |
|---|---|---|
| `capture/batch.rs`: the assessment checks, `complete`, the frozen-manifest loader it takes, and the `conclusion_errors` test | `crates/reviewq-store/src/followups/completion.rs` | FOLLOW's batch completion. K2 calls it inside the publish transaction. Rename its error paths from `/handoff/assessments/…` to `/assessments/…` and reword "This handoff". |
| `capture/failures.rs`: `note` and `resolve` | `crates/reviewq-store/src/reports/failures.rs` | K2 keys them by pass and kind. |
| `capture/authority.rs`: project and grant hash, `report_lead` | `crates/reviewq-store/src/reports/authority.rs`, with `report_lead` renamed `run_lead` | Actions use it today. K2 uses it for the caller binding. |
| `capture/reconciliation.rs`: the reconciled-observations statement | beside `completion.rs` | Used by `complete`. |
| The chunk-append statement | `crates/reviewq-store/src/reports/artifact.rs` | K2 stores the composed text with it, in the publish transaction. |
| App `capture.rs`: `CHUNK_BYTES`, `LineStats`, `Assessment`, `RevisionEvidence`, `ReportAuthority` | `crates/reviewq-app/src/reports.rs` | Used by reads and by K1. |
| App `HandoffFieldError::bounded` | `crates/reviewq-app/src/access.rs` as `FieldError::bounded`; `AppError::InvalidHandoff` becomes `AppError::InvalidFields` | One field-error type for every tool. |
| Protocol `validate.rs`: the jsonschema error collector and its cap | `crates/reviewq-protocol/src/field_errors.rs` | F2 validates six inputs with it. |
| Protocol `limits.rs`: `MAX_DISCUSSION_ID_BYTES` | `crates/reviewq-protocol/src/scalar.rs` | Imported there already. |
| Protocol `ReportRevision`, `Assessment`, `FindingLocation`, `FindingBasis` and its kind | `crates/reviewq-protocol/src/reports.rs`, beside `Verdict` and `FindingConclusion` | `FindingDetails` uses the last two today. F2 reuses all of them. |
| Store `cache_report_display` and what it calls | stays in `connection/display_cache.rs` | Its one caller is deleted here. K2 calls it after a publish commits. |
| Provider `codex_report.rs`: `latest_failure` and `event()` | `crates/reviewq-providers/src/history/file.rs` or its recovery owner | Recovery reads them. |

**The read contract after C1**

- `ReportSummary` loses `originalSha256`, `nativeProvenance`, `semanticIndex`, `unanchoredFindingCount` and `complete`. `capturedAt` becomes `publishedAt`. `findingCount` and `changeCount` are required numbers.
- `DeliveryState` is `saved` or `failed`. The queue's `report_saving` tag, the stage input `pending_completion` and the browser's saving notice go with it. Keep `pending_report`: it marks an unhandled new version.
- `Descriptor::Finding` has `severity` as `P0`–`P3`, and a required `blocking`. `struck`, `carriedFrom` and `evidenceSha256` go. `FindingDetails` carries what the dispositions panel shows: `locations`, `consequence` and `evidence`. Its store query returns no details until K2 reads them from `ledger_changes`.
- `CandidateReport`, `ReportRead::Candidate`, the candidates route, `DisplayFallback::IncompleteSource` and `ArtifactStats.complete` go.
- `report_versions` drops `handoff_receipt_id` and `semantic_index`, renames `captured_at_ms` to `published_at_ms`, and gains `kind` and `ledger_sequence`. `report_findings` gains `record_revision`, and its offsets become required. `artifacts` drops `original_sha256`, `complete` and the staging index. `report_handoffs` and `report_correction_requests` go. `report_delivery_failures` gains `pass_id` and the three kinds, and loses the rejection kind. `claude_turns` loses `response_id` and `response_sha256`, and the plugin's turn report stops sending the response.
- A finding ID is `F-` and a number from 1 to 999,999,999, written with at least two digits and no further leading zero: `^F-(0[1-9]|[1-9][0-9]{1,8})$`. The domain's `FindingId` and the protocol's scalar accept exactly this.

**Steps**

- [ ] Remove the entry layers: the four `native/reports` routes, the MCP report tool and its Claude tool-list filter, the CLI `report` commands and `CodexReportWitness`, the witness store, the capture worker and its claims in `runtime.rs`, and the plugin's hand-over handler. Leave the plugin registering only the action tool. Remove the installer's `PreToolUse` entry, its messages and its test assertions; the JSON editing and ownership helpers that E2 reuses stay, under `#[expect(dead_code)]`. `cargo check --locked --workspace` passes.
- [ ] Remove the proof code: the report methods of the native workflow proof port together with both provider implementations, the transcript readers that only they used, and `WorkflowTool::Report` with its types. Keep `verify_active_call`, `ended_and_idle` and readiness. The check passes.
- [ ] Remove the recovery rule that reads `report_handoffs` in `hosts/recovery/dispatch.rs` and reword its prompt. K2 restores the rule from `ledger_calls`.
- [ ] Remove the app and store capture code, relocating the items in the table first. Remove the 2-second recheck, the 60-second claim lease, the 30-second transcript flush wait, the parse semaphore and the report wake-up channel if nothing else listens. The check passes.
- [ ] Reshape the schema, the app types and the protocol DTOs to the read contract above. Regenerate with `make contracts`. Remove the hand-over lines and variables from the `Makefile`, keeping the unexpected-schema guard.
- [ ] Remove the launch guidance text, its fixture include and the follow-up prompt sentence about the hand-over. K3 adds the rule in their place.
- [ ] Delete the tests of deleted code: the two protocol validator cases, the four store capture cases, the two app ledger cases, and the provider and binary cases for the witness and the report proof.
- [ ] Run `make crate-deps`, `make rust-check` and `make contracts-check`. Run `rg -n "report_handoff|handoff|witness|candidate|semantic_index|amends|pending_completion|correction|captured_at|a\.complete|original_sha256|handoff_receipt_id|response_sha256" crates hosts contracts fixtures Makefile` and account for every hit that remains. A dropped column that survives in a SQL string compiles and then fails at run time, so open a fresh database and run each changed read once.

**Done when** the Rust workspace is green, no hand-over name remains outside the docs, and a fresh database opens with the reshaped schema. `make web-check` fails until W1 lands. That is expected and stated in the report.

### F1: Domain model, app ports and field checks

**Files**

- Create: `crates/reviewq-domain/src/ledger.rs`, `crates/reviewq-app/src/ledger.rs`, `crates/reviewq-app/src/ledger/markdown.rs`, and `crates/reviewq-app/src/ledger/{record,passes,compose,publish,readback}.rs` holding only their module documentation.
- Modify: the two `lib.rs` files, one `pub mod ledger;` line each. App `access.rs` and transport `entry/error.rs`, so the field-error cap has one owner.

**Produces**

- Domain types with checked constructors, built with the identity macros where they fit and labelled the way `RunStage` is: `FindingId` (`F-01`, never reused, ordered by number), `RecordRevision`, `PassKind`, `PassState` (`Open`, `Waiting`, `Closed`), `VersionKind`, `VersionState` (`New`, `Updated`, `Retired`, `Carried`), `FailureKind`, `Tool`, `CallerRole` (`Lead`, `Child`).
- The app's `reports.rs` already owns `Verdict`, `Severity`, `FindingKind`, `FindingBasisKind` and `FindingConclusion` with `retires_finding`. The ledger uses them as its severity, finding kind, evidence kind and outcome. Define none of them again. Add `NoteSection` beside the app's ledger types with the `labels!` macro.
- Domain rules as pure functions over small input structs:
  - `may_call(Tool, CallerRole) -> bool`. Only the lead may call `submit_review` and `wait_for_user`.
  - `record_opens_pass(has_version, pass_open) -> bool` for the first record of a run with no version. A launch start and a delivered batch name their pass kind directly.
  - `gate(GateFacts) -> GateDecision`. Block only a lead's stop with an open pass that is not waiting, fewer than 2 blocks in this stop sequence, and no pending blocking question or permission. A stop with `stopHookActive` false starts a new sequence.
  - `settle(SettleFacts) -> Settlement` returning `Nothing`, `PublishUpdate` or `Unfinished`, from: lead idle, every known child idle, recovery pending, open pass and its state, unpublished records.
  - `closing_kind(ClosingFacts) -> Result<VersionKind, ClosingError>` covering a first version, an open pass of each kind, `fullReview`, and assessments without a selected-thread pass.
  - `untouched_findings(parent_active, touched) -> Vec<FindingId>` for the full-review check.
  - `version_state(parent_revision, current_revision, retired) -> VersionState`.
- App field checks in `ledger/markdown.rs`, on `satteri_pulldown_cmark` and its `into_offset_iter` source ranges, as the display cutter uses them (`reports/display/cutter.rs`):
  - `check_field(kind, text) -> Result<(), FieldProblem>` with the line where a problem starts. It applies the 8 KiB line limit, the stand-alone rule, the rule that no paragraph of a finding's field begins with one of the card's bold labels, the begins-with-a-paragraph rule for `issue`, `consequence`, evidence text and `proposedComment`, and the rules for `summary` (at most four plain paragraphs) and `rationale` (one): no other block kind, and no paragraph that starts with bold text or with a label the header uses.
  - `shift_headings(text) -> Cow<str>`: ATX and setext headings become ATX with the shallowest at level four and a floor of six. Every other line is returned unchanged.
- App types in `ledger.rs`: `Caller` (run, role, provider, native session, call ID, optional turn and agent), the request and receipt types of the six operations, `LedgerSnapshot` (what publishing reads), `PublishCommit`, and `PublishOutcome` with `Published`, `Unchanged`, `Stale` and `Invalid(Vec<FieldError>)`.
- The `LedgerStore` port, one method per transaction or read: resolve a caller, apply a record write, read a page, hold a pass, answer and count a gate request, read a snapshot, commit a publish, index a published version for display, list the runs to settle with their facts, record a failure, read the read-back data, and note and read a hook report.

**Steps**

- [ ] Write the domain types and rules. The domain crate uses only the standard library.
- [ ] Write the stand-alone check: parse the field followed by a blank line and a sentinel heading, and require the last block to be that heading. Find the offending line from the block that swallowed it. Do not hand-scan fences.
- [ ] Write the app types and the `LedgerStore` port against the existing `Actor`, `AppFuture`, `Snapshot`, `AppError`, `FieldError` and ID types. Give every public item a one-line description of its outcome and failures.
- [ ] Remove the app's `FieldError::bounded` and `MAX_FIELD_ERRORS`. In the transport's `entry/error.rs`, pass the converted app errors through the protocol's `bounded_field_errors`, so every refusal is capped once, where the reply is written.
- [ ] Add test `settle_and_gate_decisions` in the domain: one table over the settle facts (a child still working, a waiting pass, a pending recovery, unpublished records with no pass) and the gate sequence (two blocks, then release; a fresh stop starts again; a waiting pass is never blocked).
- [ ] Add test `stand_alone_fields` in the app: an open top-level fence is refused with its line, an open fence inside a list item is accepted, an unclosed HTML comment is refused, a setext heading is shifted, and a summary paragraph that starts with `Scope:` is refused.
- [ ] Run `make crate-deps`, `cargo check --locked --workspace`, then `make test-package PACKAGE=reviewq-domain` and `make test-package PACKAGE=reviewq-app`.

**Done when** K1, K2 and K3 can each be written against these files without asking for a type.

### F2: Tool contract

**Files**

- Create: `crates/reviewq-protocol/src/ledger.rs`, `contracts/ledger-tools.schema.json`, `fixtures/contracts/ledger-tools.json`, `hosts/claude/plugin/hooks/ledger-schema.js` (the last three generated).
- Create also: `crates/reviewq-protocol/src/bin/export-schema/ledger.rs`.
- Modify: `crates/reviewq-protocol/src/lib.rs`, `src/bin/export-schema.rs`, the `Makefile` contract variables, guard and diffs.

**Produces**

- One input struct per tool with `deny_unknown_fields`, limits as schemars attributes from named constants, and enums in the spec's spelling. The root of every schema is one object with no `oneOf`. `revision` is `{sha}` or `{sha:null,reason}` as one object with an optional `reason`.
- Result DTOs: `{id,revision,unpublished}`, `{section,title?,revision,unpublished}`, the ledger page with its `cursor`, the closing acknowledgement, and the held reply.
- The route bodies from the shared names table: the `{binding,input}` envelope, the `fieldErrors` reply, and the gate and read-back bodies. The plugin, the bridge, the hook helpers and the routes all use these and define none of their own.
- `LEDGER_TOOLS`: name, description and schema for each tool, in the order of the table above. A description says in two or three sentences when to call the tool, what comes back and what a field error means. It repeats no limit the schema already states.
- `validate_ledger_input(tool, &serde_json::Value)` returning the typed input or every field error in field order, built on `field_errors.rs`. The transport caps the list once. The schema carries character maxima. This function also enforces the byte limits, the one-line fields and the title rule, so no other layer checks a field's size.
- The filled example file: one valid input per tool, taken from the spec's worked calls.

**Steps**

- [ ] Write the DTOs and descriptions. Reuse `FindingIdDto`, `DiscussionIdDto`, `GitShaDto`, `Verdict`, `FindingConclusion`, `ReportRevision`, `Assessment`, `NextAction`, `FindingLocation` and `FindingBasis`.
- [ ] Add the three export modes and the Make wiring so `make contracts` writes the three files and `make contracts-check` diffs them. Keep the surrogate-range strip the plugin schema needs.
- [ ] Run `make contracts`, `make contracts-check` and `make contracts-verify`. Confirm by reading the schema that no tool root has alternative shapes and every string has a maximum.

**Done when** the three generated files are checked in and reproduce byte for byte.

### W1: Reader cleanup

**Files**

- Delete: `web/src/features/review/report/useUnanchoredFindings.ts`.
- Modify: `report-presentation.ts`, `report-structure.ts`, `report-reader.ts`, `RichReport.tsx`, `ReportDocument.tsx`, `ReportTable.tsx`, `ReportHistory.tsx`, `ReportDetails.tsx`, `Findings.tsx`, `report.css`, `web/src/components/markdown/{parse-markdown,settled,table-page}.ts`, and the queue tag table that names `report_saving`. The generated types under `web/src/api/generated/` are F2's: C1 regenerated them, and only `make contracts` writes them.

**Steps**

- [ ] Remove what composed versions never reach: cards drawn from context rows and their texts, struck and retired heading handling, the retired card, the source-only state, the `complete` check, the candidate types, `reportHandoffReason` (show the stored reason as it is), the `preparing` and `saving` notice, the `Severity:` and `Blocking:` line variants and the alternative previous-threads section names. Keep the `Evidence` field label, the identity line style and the verdict and counts handling: the composed layout uses them.
- [ ] Remove the outcome badge from the previous-threads table: the composed outcome cell already starts with the outcome in bold. Keep the toned `Status` cell of a table written in the `threads` note. Keep cards and the counts line as published. A user's later blocking choice shows in the dispositions panel and on the next version's card line, as it does today.
- [ ] Parse report text with raw HTML shown as text at every place report text is parsed: `report-reader.ts`, `ReportTable.tsx`, and the report callers of `settled.ts` and `table-page.ts`. Other callers of those two modules keep their mode. In `literalHtml`, turn a lone `<br>`, `<br/>` or `<br />` into a line break and every other HTML node into text.
- [ ] Fix `stripPrefix` so a card whose title starts with code, emphasis or a link still loses its `F-01 ·` prefix.
- [ ] Rename `capturedAt` reads to `publishedAt`. Run `make web-check` and `make web-build`.

**Done when** `make web-check` passes and no removed name remains in `web/src`. X1 compares the populated view once composed versions exist.

### K1: Composer, publishing and settling

**Files**

- Fill: `crates/reviewq-app/src/ledger/{compose,publish}.rs`.
- Create: `fixtures/reviews/ledger/worked.md`, the expected composed text.

**Consumes** F1's types, port and `markdown::shift_headings`. `LineStats`, `CHUNK_BYTES` and `sha2` from the existing reports module.

**Produces**

- `compose::compose(&LedgerSnapshot) -> Composed` with the text, its SHA-256, the line statistics, and one span (scalar offset and length) per card and section.
- `publish::submit_review(store, caller, input) -> Result<SubmitReceipt, AppError>`: the closing-call checks, compose, commit, at most 3 recomposes on a stale snapshot, then the display index call after commit. `Invalid` from the commit returns its field errors to the caller.
- `publish::settle_due(store)`: apply the domain's `settle` decision to every run that is due. Whether a Claude companion is live, gone or unknown is the store's one companion watch: silence counts once ReviewQ has watched for 35 seconds without a break, a lead whose companion is unknown is not idle, and a runtime tick of its own, run at startup and every 10 seconds, reads the watch and stores each companion that went silent, or whose trusted host process `ClaudeHost::is_dead` proves dead, as ended. The heartbeat helper names its parent process; the store keeps it only when it is on the service's boot and pid namespace, and trusts it after two reports of one generation 5 seconds apart. Every report first asks whether the stored generation's trusted host is dead: if so the companion is ended, a report of that generation is refused, and another generation takes over at once. The watch tick runs in a task of its own. Publish an update, or record `unfinished_review` once. An update that cannot publish records `update_failed`.

**Steps**

- [ ] Write the composer as one pass that appends to a `String` reserved from the record sizes, recording spans as it writes. Escape `|` in table cells. Write locations as code spans with a fence longer than any backtick run in the path. Place the details after the evidence and `Proposed comment` last. Take every fixed word from `ledger/layout.rs`. Escape an extra note's title so it reads as written in a heading, including a trailing `#`. Write a gap under `Evidence`, beginning `Not confirmed.` Leave out the verdict line for `not_specified`. Parse a field only to shift its headings below the layout's own: fields were checked when saved.
- [ ] Write `submit_review` and `settle` over the port. They hold no SQLite transaction while composing.
- [ ] In `reports/display/cutter.rs`, treat an HTML block and inline HTML as ordinary content instead of falling back to the whole document. The reader now shows report HTML as text, so it needs no document context, and a large report that mentions `Vec<u8>` outside code keeps the windowed reader.
- [ ] Add test `composed_worked_set_matches`: build the worked snapshot in the test, compose it, compare with `worked.md` byte for byte, and check that each recorded span slices exactly its card or section. The snapshot includes a subagent's finding, a gap, a retired finding, a user disposition, an extra note, a title that starts with a code span, a `|` in a reason, an authored level-two heading, and a line with `Vec<u8>` and `<runner>` outside code.
- [ ] Run `cargo check --locked -p reviewq-app` and `make test-package PACKAGE=reviewq-app`.

### K2: Ledger storage

**Files**

- Create: `crates/reviewq-store/src/ledger.rs`, `ledger/{records,passes,publish,settle}.rs`, `schema/ledger.sql`.
- Modify: `lib.rs`, `connection/database.rs` (add the schema file), `reports.rs` (`read_delivery`), `reports/context.rs` (finding details from `ledger_changes`), `reports/failures.rs` and the `report_delivery_failures` table in `schema/projection_evidence.sql` (an integer pass, an optional turn, the ledger sequence the failure was recorded at, and one unresolved row per run, pass and kind), the checked time of a version in `schema/reports.sql` and its reads, `followups/completion.rs`, `launches.rs` and `hosts/delivery/workflow.rs` (open a pass inside the existing confirmation transactions), the batch-creation code and `queue/archive.rs` (end a pass), `hosts/recovery/dispatch.rs` (recovery eligibility), `schema/queue.sql` (the report state of `review_activity`).

**Consumes** F1's port and types. `WriteTx`, `tx.emit`, `insert_notification`, `reports/artifact.rs`, `reports/authority.rs`, `followups/completion.rs`, `cache_report_display`, `recovery::complete_turn`. C1 left each of these under `#[expect(dead_code)]`; remove the attribute as each gains its caller. The native-call replay in `actions/commands.rs` is tied to the actions table: follow its pattern, do not call it.

**Produces** the `LedgerStore` implementation on `SqliteStore`:

- **Record write**, one immediate transaction: resolve the caller, replay a known call from `ledger_calls`, apply the three duplicate rules, check the expected revision (a stale one returns the current revision and the changed field names), enforce the per-run limits, write the row in `ledger_findings` or `ledger_notes`, append a full snapshot to `ledger_changes`, advance the run's ledger sequence, open a full-review pass when the run has no version and no open pass, store the call result. A user's disposition is never read or changed here.
- **Caller binding.** Claude: the session's run, the role from `agentId`. Codex: the lead by its registered thread. A child by the request's parent thread when that is the lead or an already bound child of the run, otherwise by the discovered-children rows. An unattributable caller is refused.
- **Pass rows**: one open pass per run, opened inside the launch confirmation and the batch-executing transactions, held by `wait_for_user`, reopened when the lead's latest input is newer than the hold, closed by publish, ended by a successor batch or archive. The gate request counts the blocks of the current stop sequence on the pass row.
- **Snapshot read**: the records at their revisions, the ledger sequence, the disposition revision, the parent version's finding rows, the open pass, the frozen batch, and the facts for the first line and the cover lines.
- **Publish commit**, one immediate transaction: recheck the ledger sequence, disposition revision, current receipt, run lead and frozen batch. Seal the chunks, append the version with its kind, write the finding and section rows, move the head, bind the disposition revisions, close the pass, call the batch completion with the assessments (`final`, or `unchanged` with no version when no record changed and the verdict is the parent's), store the closing call, including one answered with the current version and no pass, resolve earlier failures and dismiss their notifications, insert the notification and set the desktop wake flag, emit the change events. Assessment errors from the completion come back as `Invalid`, and nothing is written.
- **Display index** for a published version, through `cache_report_display`.
- **Failures**: one row per run, pass and kind. A repeat updates the reason and raises nothing.
- **Settle read**: the runs with an open pass or unpublished records, joined to the stored session states of the lead and its children and to pending recovery, from one indexed query. No provider is asked.
- **Recovery eligibility**: a closing call accepted in the failed turn, read from `ledger_calls`, ends it.
- **Hook report**: one row in `codex_stop_hook_reported`, written the first time Codex's stop hook reports.
- **Delivery read** and the queue's report state from the head and the unresolved failure rows.

**Steps**

- [ ] Write `schema/ledger.sql` with the keys and partial unique indexes that enforce one open pass per run, one row per failure key and one stored result per call.
- [ ] Write the transactions with cached prepared statements. No statement scans a run's change history on the write path.
- [ ] Add test `ledger_writes_are_idempotent`: the same call replays its result, an equal create under a new call returns the same ID and revision, a stale revision is refused with the current revision and field names, and a reopened database keeps both records.
- [ ] Add test `publish_is_conditional`: a record written after the snapshot, then a disposition saved after the snapshot, each make the commit report a stale snapshot and change nothing. A commit on a current snapshot stores the version, closes the pass, resolves the failure and takes its notification out of the inbox. A selected-thread close that changed nothing completes the batch as `unchanged` and adds no version. Read the stored version, its history row and a finding's context back through the production reads, so each changed query runs on real rows.
- [ ] Keep the relocated `conclusion_errors` test passing.
- [ ] Run `cargo check --locked -p reviewq-store` and `make test-package PACKAGE=reviewq-store`. Open a fresh database and reopen it.

### K3: Record tools, passes, gate, rule and read-back

**Files**

- Fill: `crates/reviewq-app/src/ledger/{record,passes,readback}.rs`.
- Modify: `crates/reviewq-app/src/launch.rs`, `followups/work.rs`, and `ledger/markdown.rs` for the footnote check only.

**Consumes** F1's types, port and `markdown::check_field`.

**Produces**

- `record::{save_finding, set_finding_outcome, save_section, read_draft}`: check the Markdown fields and the per-run limits, return every field error, then one port call. The transport caps the list. Field sizes were checked by the protocol and are not checked again.
- `passes::wait_for_user(store, caller, question)` and `passes::gate(store, request) -> GateDecision`. The block reason names what is owed and how many findings are saved.
- `readback::RULE`: the rule text, written once. `readback::read_back(store, session) -> String` within 8,000 bytes of UTF-8: the rule, the open pass and what it owes, then whole lines, one per active finding, newest change first, and a last line that counts those left out and names `read_draft` for the rest.
- The launch input and the selected-thread prompt append `RULE` after the user's own text.

**Steps**

- [ ] Write the rule as five short imperative sentences from the spec's Rule paragraph. Name the tools. Say that only what is saved is shown and that the lead closes. Do not mention the gate.
- [ ] Write the four record operations and the two pass operations over the port. Check an extra note's title with `layout::repeats_fixed_heading`.
- [ ] In `check_field`, refuse a footnote definition or reference the way a link reference definition is refused, and add one row to the `stand_alone_fields` table.
- [ ] Write the read-back with a hard character cap and a deterministic order.
- [ ] Run `cargo check --locked -p reviewq-app`.

### E1: Routes and MCP tools

**Files**

- Create: `crates/reviewq-transport/src/entry/features/ledger.rs`, `entry/mcp/ledger.rs`.
- Modify: `entry/features.rs`, `entry/mcp.rs`, `entry/error.rs`, `entry/hosts.rs`, `entry/hosts/claude/delivery.rs`, `entry/features/actions/native.rs`.

**Steps**

- [ ] Put the bridge-credential and content-type check in one function and use it from the ledger routes, the action routes and the host routes. Remove the two copies that exist.
- [ ] Add the six tool routes. Each reads `{binding,input}`, validates `input` with `validate_ledger_input`, maps the DTO to the app request field by field, and returns the result or `fieldErrors`.
- [ ] Add `gate` and `readback`. `gate` notes the stop hook's report and returns the app's gate decision.
- [ ] In the Claude hook route, answer `stopping` with the same gate reply, taking `stopHookActive` from the hook input, and `compacted` with the read-back. The turn signal only records the turn; the attention loop settles the run.
- [ ] Return the stop hook's report state with the link reply in `entry/hosts.rs`.
- [ ] Add the six MCP tools from `LEDGER_TOOLS`. Bind the caller from `_meta`: `threadId`, `callId`, and `turn_id` and `parent_thread_id` from `x-codex-turn-metadata` when present. Build the same `{binding,input}` and call the same handler as the routes, so the binding and the error mapping exist once. A Claude client's tool list omits the six tools, because the plugin registers them. Give `submit_review` a 55-second deadline and the others the default.
- [ ] Return a field error to the model as a tool error whose text lists each field and message, so the agent can correct it in the same turn.
- [ ] Run `cargo check --locked -p reviewq-transport` and `make rust-check`.

### E2: Binary: bridge, hooks, installer, wiring

**Files**

- Create: `crates/reviewq/src/native_client/codex_hook.rs`.
- Modify: `native_client/claude.rs`, `native_client.rs`, `cli.rs`, `install/codex_hook.rs`, `install/codex_hook/json.rs`, `install.rs`, `install/bundle.rs`, `runtime/workflow.rs`.

**Steps**

- [ ] Add the bridge action `ledger <id>`: read `{tool,callId,turnId?,agentId?,input}` from stdin, verify the service with the signed status request, make one POST to the tool's route and print the reply. Every path that sends the bridge key verifies the service first, through one function. Make no transcript check on this path.
- [ ] Make `claude-hook` print what the hook route answers: the block reply for `Stop`, and the read-back text for a `SessionStart` whose source is `compact`. Send the hook input's `stop_hook_active`. Keep ignoring subagent hooks.
- [ ] Add `codex-hook`: read the hook JSON from stdin, send `Stop` to `gate` and a `compact` `SessionStart` to `readback`, print the replies in the same form through the same printing function `claude-hook` uses, and exit 0 on every failure so a session is never broken.
- [ ] Rewrite the installer to own two entries in `$CODEX_HOME/hooks.json`, `Stop` and `SessionStart` with matcher `^compact$`, each recorded with its event, with no output redirect. Retarget `install/codex_hook/json.rs` from `PreToolUse` to the two events and restore the `reconcile` calls on install and on both uninstall paths, which C1 removed. Then trust ReviewQ's own two entries through a private `codex app-server` with `hooks/list` and `config/batchWrite` on `hooks.state`, and confirm them trusted; touch no other hook's trust. The installer says which config file Codex wrote. The service checks again, awaited, at startup and whenever a Codex session becomes a lead, before `thread/start` on a launch, and starts a child only when `hooks.json`, `config.toml` or the Codex executable changed since its last completed check. Uninstall removes those two trust entries the same way, each key written as `null`, before it removes the hooks. After a write that changed trust, either way, ask the user's running Codex daemon once, within 2 seconds, to reload hook trust into its open conversations (`config/batchWrite` with no edits and `reloadUserConfig`); start no daemon, and let no failure fail the caller. The installer and `reviewq link` say the hooks still need the user only when Codex could not trust them. When they are trusted, `reviewq link` reads the linked conversation's own hooks from the daemon (`hooks/list` with its `threadId`); when the daemon holds it without them, it reloads once and lists again a few times, about 1.3 seconds in all, before concluding that they do not run. The first such check on a daemon session also lists, beside it, the hooks of a random thread, and the session keeps the answer: only Codex's "thread not found" refusal (`-32600`, a message starting "thread not found") shows that the daemon scopes the list by conversation, as an older one ignores the field; connecting asks nothing. The whole check takes at most 3 seconds; when a slow connection leaves too little of it for every list, the last list after the reload decides. `reviewq link` says the conversation may not run the hooks yet only when the daemon still holds it without them, or when the daemon cannot show it, no ReviewQ hook has reported from it since the service started (kept in memory), and Codex created it before ReviewQ last wrote that trust. The creation time comes from the thread ID when it is a UUID version 7; the trust time is `trusted_at` in `codex-hooks.json`, written by each approval that changed trust, the installer's or the service's. When either is unknown it says nothing. Rewrite the one installer test for the two entries, and remove the `#[expect(dead_code)]` C1 left.
- [ ] Wire settling in `runtime/workflow.rs`: call `publish::settle_due` in the existing 10-second loop, before the branch that skips a tick when sources are unavailable, and once at startup. Add no new timer. Keep the watch in the loop: it starts with the loop and starts again when the wall clock and elapsed time move more than 5 seconds apart between two settle reads. The Claude turn signal and a ledger write settle their own run through E1 and K1.
- [ ] Return the rule from the terminal link as text the model reads. Add `ledger.js` and `ledger-schema.js` to the plugin file list in `install/bundle.rs`.
- [ ] Run `make rust-check`.

### E3: Claude plugin

**Files**

- Create: `hosts/claude/plugin/hooks/ledger.js`.
- Modify: `register.jsx`, `workflow.js`, `hooks.json`, `bridge.js`.

**Steps**

- [ ] Register the six tools from `ledger-schema.js`.
- [ ] Handle a call with one bridge spawn that sends the tool, the tool-use ID, the active turn, the subagent's ID when the event has one, and the input. Send no heartbeat for a ledger call. Accept subagent calls for the four record tools and refuse the two closing tools from a subagent with a message that says the lead closes.
- [ ] Return a result as the tool result and field errors as an error the model can read. The plugin refuses nothing itself; the service words every refusal.
- [ ] Return the link's rule as text the model reads, not as a display-only notice.
- [ ] Add the `SessionStart` entry to `hooks.json` with the `compact` matcher, and end every hook command with a guard so a wrong binary can never block a turn. The plugin outlasts the bridge on every ledger call and on `track_posting`: the bridge allows 55 seconds for `submit_review` and `track_posting` and 30 for every other ledger tool, and the plugin waits 30 seconds longer, a wait the deadline owner exports with the tool contract. The plugin answers every refusal of its own tools with `{ deny }`, which the model reads as the call's error.
- [ ] Keep `register.jsx` at or below its current size by moving the ledger handlers into `ledger.js`. Run `node --check` on `ledger.js`, `workflow.js` and `bridge.js`.

### X1: Fixtures and the populated view

No code path stores a report version from fixture files, and none is added. Fixture versions come from the product.

**Steps**

- [ ] Seed the worked set by calling the production ledger routes against a dev service in a dedicated profile, from the fixture generator's own records. Write no second composer and no direct version insert.
- [ ] Build the size-limit specimens the same way, from records sized to cross each reader limit. Regenerate the pinned sizes, offsets and digests in the fixture manifest and in the [fixture brief](../01-foundation/fixtures.md).
- [ ] Load the `worked` preset and compare Findings, History and the dispositions panel with the same content on the current `main` build: identity line, title, verdict card, counts, cards, previous-threads table. Report every visible difference. The expected ones are the single layout and raw HTML shown as text.

### V1: End-to-end evidence, measurements, README and briefs

**Steps**

- [ ] Run `make web-install` and `make check`.
- [ ] Exercise the six reproductions in the [REPORT evidence table](reports.md#evidence) through the production routes in a disposable profile. Step 5 of the flow is a subagent record after the lead went idle and an interrupted chat turn.
- [ ] Measure on the worked set and record with the commands: median and 99th-percentile time of a record write, time to compose and commit a version, peak resident memory of the service during a publish of the largest specimen, the response size of the short ledger list at 200 findings, and the display index build for a version with 200 findings and for one with inline HTML, which takes the whole-document path.
- [ ] Run one linked review on each host with `claude-sonnet-5-5` at high effort and `gpt-6.2-sol` at high effort, in disposable host profiles that leave the user's settings files untouched. Confirm: a subagent's call is attributed to its lead, the stop gate holds and releases, the read-back arrives after compaction, and the Codex request carries the thread and turn metadata the binding reads. Record what could not be exercised as a proof limit.
- [ ] Update `README.md` for the two Codex hooks, the lines of `docs/dependency-decisions.md` about heading indexing, the REPORT brief's items that closed, and the roadmap row.
- [ ] Remove the task's profiles, builds and scratch output, and read back the cleanup ledger.

## Reviews

- **Review A** covers C1, F1, F2 and W1: nothing of the earlier path remains, the relocated items have one owner each, the domain rules match the spec's tables row by row, and the tool schemas match the spec's fields and limits. It also compares every SELECT that C1 changed with its row decoder column by column, because those reads have not run on stored rows yet, and judges two behaviour changes C1 made: `failures::resolve` resolves every open failure of the run, and the archive time reads versions of kind `full_review`.
- **Review B** covers K1–K3 and E1–E3: each transaction is short and conditional, no transcript or provider read sits on a ledger call, the composer's output matches the spec's layout line for line, the gate and settle paths cannot loop or double-record, and nothing is duplicated between the Claude and Codex paths.
- **Final review** reads the whole branch against `main` for dead code, repeated logic, files over the size limit and tests that do not guard a regression.
- Each review reports findings with a file, a line and a failing scenario. The lead fixes confirmed findings before the next stage.
