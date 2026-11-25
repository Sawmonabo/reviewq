# Findings Ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single report hand-over with the findings ledger: agents file records through six tools, ReviewQ composes and publishes every report version, and no code of the earlier path remains.

**Architecture:** Each ledger call is one short SQLite write behind the existing native tool paths. Publishing reads the records, composes the fixed layout outside SQLite and commits one conditional transaction. ReviewQ decides what a turn owes from stored state, states one rule at launch, repeats it after compaction and backs it with a stop gate.

**Tech stack:** Rust 1.100 (Edition 2024), rusqlite, schemars and jsonschema for the tool contract, pulldown-cmark for field checks, sha2, the Claude plugin's JavaScript module, React and TypeScript for the reader. No new package.

**Spec:** [Review workflow](../../specs/review-workflow.md#findings-ledger-and-report-versions), [persistence](../../specs/persistence.md), [sessions](../../specs/agent-sessions.md). The [REPORT brief](reports.md) lists the deliverables and the evidence. Read the spec sections before a task. This plan fixes ownership, order and the names tasks share.

## Global constraints

- This is a fresh-installation product. Write no migration, no schema version bump, no compatibility field and no fallback to the earlier path. `FORMAT_ID` stays as it is.
- Delete what a change makes unused in the same task. No `#[allow(dead_code)]`. A relocated item that waits one stage for its caller carries `#[expect(dead_code, reason = "…")]`, which the compiler rejects once the caller exists.
- Limits come from the spec and are named constants with units: title 1–200 characters, category 1–48, path 1–512, at most 8 locations, `issue`, `consequence`, evidence text and `proposedComment` 8 KiB each, `details` 16 KiB, note body 32 KiB, at most 12 extra notes, body line 8 KiB, 200 active findings per run, tool input 128 KiB, composed text 4 MiB, `review_ledger` page 64 KiB, read-back 4,000 characters, 50 field errors plus a count, 2 gate blocks per session and turn, 3 recompose attempts, chunks of 64 KiB.
- A ledger call reads no transcript, resumes no thread and makes no provider call. Run nothing but SQLite work inside a write transaction.
- Every code file stays at or below 900 lines. Split by responsibility at 800.
- Tests stay few: only the cases this plan names. Each guards a regression that no existing check catches. Delete the tests of deleted code.
- The browser keeps its look. Reader changes remove code that only served authored reports or fix a named bug.
- Follow the [Rust rules](../../../crates/AGENTS.md), the [web rules](../../../web/AGENTS.md) and the [Rust conventions](../../rust-conventions.md): `name.rs` plus `name/child.rs`, private by default, one owner for a shared rule.
- Reuse before writing: the writer and reader connection split, `WriteTx`, `tx.emit`, `insert_notification`, native-call replay, the chunk readers, the display index trigger, the batch completion and the jsonschema field-error collector already exist.
- State an expected performance benefit as expected. Record a measured one with its command and numbers.

## Review focus

These are the conditions most likely to hurt a reviewer and least likely to be seen in a quick run. Each has an owner.

1. An agent writes `Vec<u8>` or a placeholder such as `<runner>` outside code. The text stays visible. Owner: W1 (reader shows raw HTML as text) and K1 (the expected composed file contains such a line).
2. A field leaves a code fence open, at the top level or inside a list item. Only the first can swallow the next card and only it is refused, with its line. Owner: K1 test `stand_alone_fields`.
3. The lead retries `record_finding` as a new call after a lost reply. The same ID and revision come back and no second finding exists. Owner: K2 test `ledger_writes_are_idempotent`.
4. A subagent files after the lead went idle, or the lead's turn was interrupted after a change. The run still settles: an update is published, or the review shows as unfinished once. Owner: F1 test `settle_decisions` and V1 flow step 5.
5. The reviewer saves a disposition while the lead closes. The closing call recomposes and the reviewer's choice is on the card. Owner: K2 test `publish_is_conditional`.

## Task graph

Stages run in order. Tasks inside a stage run at the same time in the primary checkout on branch `findings-ledger`, each on its own files.

| Stage | Task | Owns | Needs |
|---|---|---|---|
| 1 | **C1** Remove the hand-over path and settle the read contract | Everything listed under C1 | — |
| 2 | **F1** Domain model and app ports | `crates/reviewq-domain/src/ledger.rs`, `crates/reviewq-app/src/ledger.rs` and its empty children | C1 |
| 2 | **F2** Tool contract | `crates/reviewq-protocol/src/ledger.rs`, the schema export, `contracts/`, `fixtures/contracts/`, the generated plugin schema, Makefile contract lines | C1 |
| 2 | **W1** Reader cleanup | `web/src/features/review/report/**`, `web/src/components/markdown/parse-markdown.ts`, generated web types | C1 |
| — | **Review A** | Stages 1 and 2 | |
| 3 | **K1** Field checks, composer, publishing and settling | `crates/reviewq-app/src/ledger/{markdown,compose,publish}.rs`, the expected composed file | F1 |
| 3 | **K2** Ledger storage | `crates/reviewq-store/src/ledger.rs` and `ledger/`, `schema/ledger.sql`, the store edits named in K2 | F1 |
| 3 | **K3** Record tools, passes, gate, rule and read-back | `crates/reviewq-app/src/ledger/{record,passes,readback}.rs`, `launch.rs`, `followups/work.rs` | F1 |
| 4 | **E1** Routes and MCP tools | `crates/reviewq-transport/src/entry/features/ledger.rs`, `entry/mcp/ledger.rs`, their registrations | F2, K1–K3 |
| 4 | **E2** Binary: bridge, hooks, installer, wiring | `crates/reviewq/src/native_client/**`, `install/codex_hook*`, `cli.rs`, `runtime*` | K1–K3 |
| 4 | **E3** Claude plugin | `hosts/claude/plugin/hooks/{ledger.js,register.jsx,workflow.js,hooks.json,bridge.js}`, `install/bundle.rs` list entry | F2 |
| 4 | **X1** Fixtures | `tools/dev/review_fixture/**`, `tools/dev/seed-review`, `fixtures/reviews/**` | K1–K3 |
| — | **Review B** | Stages 3 and 4 | |
| 5 | **V1** End-to-end evidence, measurements, README and briefs | `README.md`, `docs/**` | All |
| — | **Final review** | The whole branch against `main` | |

### Dispatch rules

- Implementers run on Opus 5.5 at high effort and reviewers on Opus 5.5 at xhigh. A reviewer gets the diff and the spec, never the implementer's summary.
- An implementer edits only the paths its task owns. A needed change elsewhere is reported to the lead with the reason, and the lead assigns it. A sibling's unfinished code that breaks a build is reported, never repaired.
- Implementers run no Git command that writes. The lead checks each task's paths, runs the stage checks and commits one task per commit. The branch is pushed after each accepted stage and is never merged by this plan.
- An implementer never signals a process it did not start and starts no service on the default profile. Checks use `cargo check --locked -p <package>` while editing and the Make targets named in the task before reporting.
- Before each stage the lead searches the tree for an existing implementation of anything a task is about to add. After each stage the lead compares the new files side by side for repeated queries, validation or text, and gives a repeat one owner before the next stage starts.
- A stage is accepted when its checks pass, its review findings are fixed and `rg` finds none of that stage's removed names.

## Shared names

Tasks use these names exactly. F1 and F2 give them their Rust types. Later tasks read those files rather than this table.

| Thing | Name |
|---|---|
| Tools | `record_finding`, `assess_finding`, `record_note`, `review_ledger`, `finish_review`, `needs_input` |
| Routes under `/api/v1/native/ledger/` | `record-finding`, `assess-finding`, `record-note`, `list`, `finish`, `needs-input`, `turn-end`, `readback` |
| Caller binding sent with every tool route | `{sessionKey,callId,turnId?,agentId?}` |
| Claude bridge action | `reviewq internal claude-bridge ledger <tool> <session>`, input JSON on stdin, one JSON reply on stdout |
| Hook helpers | `reviewq internal claude-hook` (existing, now also answers `Stop` and `SessionStart`), `reviewq internal codex-hook` (new, replaces `codex-report-witness`) |
| Hook replies | Stop block: `{"decision":"block","reason":"…"}` on stdout. Read-back: the text on stdout. Nothing on stdout means no effect. |
| Generated contract files | `contracts/ledger-tools.schema.json`, `fixtures/contracts/ledger-tools.json`, `hosts/claude/plugin/hooks/ledger-schema.js` |
| Schema export flags | `--ledger-tools-schema`, `--ledger-tools-example`, `--claude-ledger-tools` |
| Tables | `ledger_findings`, `ledger_notes`, `ledger_changes`, `review_passes`, `ledger_calls`, `report_delivery_failures` |
| Failure kinds | `unfinished_review`, `closing_refused`, `update_failed` |
| Delivery states | absent, `saved`, `failed` |
| Version kinds | `full_review`, `selected_threads`, `update` |
| App modules | `ledger::record`, `ledger::passes`, `ledger::markdown`, `ledger::compose`, `ledger::publish`, `ledger::readback` |
| Store port | `LedgerStore` in `crates/reviewq-app/src/ledger.rs` |

---

### C1: Remove the hand-over path and settle the read contract

One writer. Work top-down so each step compiles: callers first, then what they called.

**Files**

- Delete: `crates/reviewq-transport/src/entry/features/reports/{handoff,witness}.rs`, `entry/mcp/report.rs`; `crates/reviewq/src/cli/report.rs`, `native_client/codex.rs`; `crates/reviewq-providers/src/report_file.rs`, `history/report.rs`, `history/codex_report.rs` and its tests; `crates/reviewq-app/src/reports/{capture,ledger,index,structured}.rs`; `crates/reviewq-store/src/reports/capture.rs`, `reports/capture/**`, `reports/address.rs`; `crates/reviewq-protocol/src/reports/handoff.rs`, `reports/handoff/**`; `contracts/report-handoff.schema.json`, `fixtures/contracts/report-handoff.json`, `hosts/claude/plugin/hooks/report-handoff-schema.js`.
- Modify: every caller of the above. The code maps name them: the route and tool registries, `error.rs`, `entry.rs`, `mcp.rs`, `mcp/operations.rs`, `features/reports.rs`, `cli.rs`, `cli/workflow.rs`, `native_client.rs`, `native_client/claude.rs`, `install.rs`, `install/bundle.rs`, `runtime.rs`, the provider `workflow` and `history` modules, app `reports.rs`, `workflow.rs`, `access.rs`, `launch.rs`, `followups/work.rs`, `hosts/claude.rs`, store `reports.rs`, `reports/context.rs`, `connection.rs`, `connection/database.rs`, `launches.rs`, `followups/work.rs`, `followups/work/idle.rs`, `hosts/recovery/dispatch.rs`, `queue/archive.rs`, `schema/{reports,projection_evidence,queue,launches}.sql`, protocol `reports.rs`, `scalar.rs`, `mcp.rs`, `bin/export-schema.rs`, domain `review_stage.rs` and `identity.rs`, the `Makefile` contract lines and `hosts/claude/plugin/hooks/{register.jsx,workflow.js}`.

**Keep and relocate**

| Item today | New home | Why |
|---|---|---|
| `capture/batch.rs`: the assessment checks, `complete`, and the `conclusion_errors` test | `crates/reviewq-store/src/followups/completion.rs` | FOLLOW's batch completion. K2 calls it inside the publish transaction. |
| `capture/failures.rs`: `note` and `resolve` | `crates/reviewq-store/src/reports/failures.rs` | K2 keys them by pass and kind. |
| `capture/authority.rs`: project and grant hash, `report_lead` | `crates/reviewq-store/src/reports/authority.rs`, with `report_lead` renamed `run_lead` | Actions use it today. K2 uses it for the caller binding. |
| `capture/reconciliation.rs`: the reconciled-observations statement | beside `completion.rs` | Used by `complete`. |
| The chunk-append statement and `accept::seal` | `crates/reviewq-store/src/reports/artifact.rs` | K2 seals the composed text with them. |
| App `capture.rs`: `CHUNK_BYTES`, `ArtifactChunk`, `LineStats`, `Assessment`, `RevisionEvidence`, `ReportAuthority` | `crates/reviewq-app/src/reports.rs` | Used by reads and by K1. |
| App `HandoffFieldError::bounded` | `crates/reviewq-app/src/access.rs` as `FieldError::bounded`; `AppError::InvalidHandoff` becomes `AppError::InvalidFields` | One field-error type for every tool. |
| Protocol `validate.rs`: the jsonschema error collector and its cap | `crates/reviewq-protocol/src/field_errors.rs` | F2 validates six inputs with it. |
| Protocol `limits.rs`: `MAX_ID_BYTES`, `MAX_DISCUSSION_ID_BYTES` | `crates/reviewq-protocol/src/scalar.rs` | Imported there already. |
| Protocol `Verdict`, `FindingConclusion`, `ReportRevision`, `Assessment`, `NextAction` | `crates/reviewq-protocol/src/reports.rs` | Wire enums that F2 reuses. |
| Provider `codex_report.rs`: `latest_failure` and `event()` | `crates/reviewq-providers/src/history/file.rs` or its recovery owner | Recovery reads them. |

**The read contract after C1**

- `ReportSummary` loses `originalSha256`, `nativeProvenance`, `semanticIndex`, `unanchoredFindingCount` and `complete`. `capturedAt` becomes `publishedAt`. `findingCount` and `changeCount` are required numbers.
- `DeliveryState` is `saved` or `failed`. The queue's `report_saving` tag, the stage inputs `pending_completion` and `pending_report`, and the browser's saving notice go with it.
- `Descriptor::Finding` has `severity` as `P0`–`P3`, a required `blocking`, `section`, `category` and `recordRevision`. `struck`, `carriedFrom` and `evidenceSha256` go. `FindingDetails` mirrors the record: `locations`, `issue`, `consequence`, `evidence`, `proposedComment`, `details`. Its store query returns no details until K2 reads them from `ledger_changes`.
- `CandidateReport`, `ReportRead::Candidate`, the candidates route, `DisplayFallback::IncompleteSource` and `ArtifactStats.complete` go.
- `report_versions` drops `handoff_receipt_id` and `semantic_index`, renames `captured_at_ms` to `published_at_ms`, and gains `kind` and `ledger_sequence`. `report_findings` gains `record_revision`, `section` and `category`, and its offsets become required. `artifacts` drops `original_sha256`, `complete` and the staging index. `report_handoffs` and `report_correction_requests` go. `report_delivery_failures` gains `pass_id` and the three kinds, and loses the rejection kind.
- The finding-ID pattern in `scalar.rs` is `F-` followed by at least two digits.

**Steps**

- [ ] Remove the entry layers: the four `native/reports` routes, the MCP report tool, the CLI `report` commands and `CodexReportWitness`, the witness store, the capture worker and its claims in `runtime.rs`, and the plugin's hand-over handler. Leave the plugin registering only the action tool. `cargo check --locked --workspace` passes.
- [ ] Remove the proof code: the report methods of the native workflow proof port together with both provider implementations, the transcript readers that only they used, and `WorkflowTool::Report` with its types. Keep `verify_active_call`, `ended_and_idle` and readiness. The check passes.
- [ ] Remove the app and store capture code, relocating the items in the table first. Remove the 2-second recheck, the 60-second claim lease, the 30-second transcript flush wait, the parse semaphore and the report wake-up channel if nothing else listens. The check passes.
- [ ] Reshape the schema, the app types and the protocol DTOs to the read contract above. Regenerate with `make contracts`. Remove the hand-over lines and variables from the `Makefile`, keeping the unexpected-schema guard.
- [ ] Remove the launch guidance text, its fixture include and the follow-up prompt sentence about the hand-over. K3 adds the rule in their place.
- [ ] Delete the tests of deleted code: the two protocol validator cases, the four store capture cases, the two app ledger cases, and the provider and binary cases for the witness and the report proof.
- [ ] Run `make crate-deps`, `make rust-check` and `make contracts-check`. Run `rg -n "report_handoff|handoff|witness|candidate|semantic_index|amends|pending_completion|correction" crates hosts contracts fixtures Makefile` and account for every hit that remains.

**Done when** the Rust workspace is green, no hand-over name remains outside the docs, and a fresh database opens with the reshaped schema. `make web-check` fails until W1 lands. That is expected and stated in the report.

### F1: Domain model and app ports

**Files**

- Create: `crates/reviewq-domain/src/ledger.rs`, `crates/reviewq-app/src/ledger.rs`, and `crates/reviewq-app/src/ledger/{record,passes,markdown,compose,publish,readback}.rs` holding only their module documentation.
- Modify: the two `lib.rs` files, one `pub mod ledger;` line each.

**Produces**

- Domain types with checked constructors: `FindingId` (`F-01`, never reused, ordered by number), `RecordRevision`, `Severity`, `FindingSection`, `EvidenceKind`, `Outcome` with `retires()`, `NoteSection`, `PassKind`, `PassState` (`Open`, `Waiting`, `Closed`), `VersionKind`, `VersionState` (`New`, `Updated`, `Retired`, `Carried`), `FailureKind`, `Tool`, `CallerRole` (`Lead`, `Child`).
- Domain rules as pure functions over small input structs:
  - `may_call(Tool, CallerRole) -> bool`. Only the lead may call `finish_review` and `needs_input`.
  - `opens_pass(...) -> Option<PassKind>` for a launch start, a delivered batch and the first record of a run with no version.
  - `gate(GateFacts) -> GateDecision`. Block only a lead turn that ends normally with an open pass that is not waiting, with fewer than 2 blocks for this session and turn, and with no pending blocking question or permission.
  - `settle(SettleFacts) -> Settlement` returning `Nothing`, `PublishUpdate` or `Unfinished`, from: lead idle, every known child idle, recovery pending, open pass and its state, unpublished records.
  - `closing_kind(ClosingFacts) -> Result<VersionKind, ClosingError>` covering a first version, an open pass of each kind, `fullReview`, and assessments without a selected-thread pass.
  - `untouched_findings(parent_active, touched) -> Vec<FindingId>` for the full-review check.
  - `version_state(parent_revision, current_revision, retired) -> VersionState`.
- App types in `ledger.rs`: `Caller` (run, role, provider, native session, call ID, optional turn and agent), the request and receipt types of the six operations, `FieldError` use, `LedgerSnapshot` (what publishing reads), `PublishCommit`, `PublishOutcome`, `SettleFacts` loading, and the `LedgerStore` port with one method per transaction: resolve a caller, apply a record write, read a page, hold a pass, answer and count a gate request, read a snapshot, commit a publish, list runs to settle, record a failure.

**Steps**

- [ ] Write the domain types and rules. The domain crate uses only the standard library.
- [ ] Write the app types and the `LedgerStore` port against the existing `Actor`, `AppFuture`, `Snapshot`, `AppError` and ID types. Give every public item a one-line description of its outcome and failures.
- [ ] Add test `settle_decisions` in the domain: one table over the facts above, including a child still working, a waiting pass, a pending recovery and unpublished records with no pass.
- [ ] Add test `full_review_names_untouched_findings` in the domain: an earlier active finding that the pass neither changed nor assessed is returned with its ID.
- [ ] Run `make crate-deps` and `cargo check --locked -p reviewq-domain -p reviewq-app`, then `make test-package PACKAGE=reviewq-domain`.

**Done when** K1, K2 and K3 can each be written against these two files without asking for a type.

### F2: Tool contract

**Files**

- Create: `crates/reviewq-protocol/src/ledger.rs`, `contracts/ledger-tools.schema.json`, `fixtures/contracts/ledger-tools.json`, `hosts/claude/plugin/hooks/ledger-schema.js` (the last three generated).
- Modify: `crates/reviewq-protocol/src/lib.rs`, `src/bin/export-schema.rs` and `export-schema/report.rs` (renamed `ledger.rs`), the `Makefile` contract variables, guard and diffs.

**Produces**

- One input struct per tool with `deny_unknown_fields`, limits as schemars attributes from named constants, and enums in the spec's spelling. The root of every schema is one object with no `oneOf`. `revision` is `{sha}` or `{sha:null,reason}` as one object with an optional `reason`.
- Result DTOs: `{id,revision,unpublished}`, `{section,title?,revision,unpublished}`, the ledger page with its `cursor`, the closing acknowledgement, and the held reply.
- `LEDGER_TOOLS`: name, description and schema for each tool, in the order of the table above. A description says in two or three sentences when to call the tool, what comes back and what a field error means. It repeats no limit the schema already states.
- `validate_ledger_input(tool, &serde_json::Value)` returning the typed input or the capped field errors, built on `field_errors.rs`.
- The filled example file: one valid input per tool, taken from the spec's worked calls.

**Steps**

- [ ] Write the DTOs and descriptions. Reuse `FindingIdDto`, `DiscussionIdDto`, `GitShaDto`, `Verdict`, `FindingConclusion`, `Assessment` and `NextAction`.
- [ ] Add the three export modes and the Make wiring so `make contracts` writes the three files and `make contracts-check` diffs them. Keep the surrogate-range strip the plugin schema needs.
- [ ] Run `make contracts`, `make contracts-check` and `make contracts-verify`. Confirm by reading the schema that no tool root has alternative shapes and every string has a maximum.

**Done when** the three generated files are checked in and reproduce byte for byte.

### W1: Reader cleanup

**Files**

- Delete: `web/src/features/review/report/useUnanchoredFindings.ts`.
- Modify: `report-presentation.ts`, `report-structure.ts`, `report-reader.ts`, `RichReport.tsx`, `ReportDocument.tsx`, `ReportHistory.tsx`, `ReportDetails.tsx`, `Findings.tsx`, `report.css`, `web/src/components/markdown/parse-markdown.ts`, the queue tag table that names `report_saving`, and the generated types under `web/src/api/generated/`.

**Steps**

- [ ] Remove what composed versions never reach: cards drawn from context rows and their texts, struck and retired heading handling, the retired card, the source-only state, the `complete` check, the candidate types, `reportHandoffReason` (show the stored reason as it is), the `preparing` and `saving` notice, the `Severity:` and `Blocking:` line variants and the alternative previous-threads section names. Keep the `Evidence` field label, the identity line style and the verdict and counts handling: the composed layout uses them.
- [ ] Remove the outcome badge that `reportTable` adds to the previous-threads table and the outcomes read that only fed it. The composed cell already starts with the outcome in bold.
- [ ] Parse report text with raw HTML shown as text. In `literalHtml`, turn a lone `<br>`, `<br/>` or `<br />` into a line break and every other HTML node into text.
- [ ] Fix `stripPrefix` so a card whose title starts with code, emphasis or a link still loses its `F-01 ·` prefix.
- [ ] Rename `capturedAt` reads to `publishedAt`. Run `make web-check` and `make web-build`.
- [ ] Write a composed specimen by hand from the spec's layout into the task's scratch space, load it through the existing fixture service in a dedicated profile, and compare the Findings view with the current build on the same content: header, verdict card, counts, cards, previous-threads table, dispositions panel. Report every visible difference.

**Done when** `make web-check` passes, no removed name remains in `web/src`, and the only visible differences are the ones this task names.

### K1: Field checks, composer, publishing and settling

**Files**

- Fill: `crates/reviewq-app/src/ledger/{markdown,compose,publish}.rs`.
- Create: `fixtures/reviews/ledger/worked.json` (records, notes, outcomes and facts of the worked set) and `fixtures/reviews/ledger/worked.md` (the expected composed text).

**Consumes** F1's types and port. `LineStats`, `CHUNK_BYTES` and `sha2` from the existing reports module.

**Produces**

- `markdown::check_field(kind, text) -> Result<(), FieldProblem>` with the line where a problem starts. It applies the line limit, the stand-alone rule, the begins-with-a-paragraph rule for `issue`, `consequence`, evidence text and `proposedComment`, and the plain-paragraph rule for `summary` (at most four) and `rationale` (one).
- `markdown::shift_headings(text) -> Cow<str>`: ATX and setext headings become ATX with the shallowest at level four and a floor of six. Every other line is returned unchanged.
- `compose::compose(&LedgerSnapshot) -> Composed` with the text, its SHA-256, the line statistics, and one span (scalar offset and length) per card and section.
- `publish::finish_review(store, caller, input) -> Result<FinishReceipt, AppError>`: the closing-call checks, compose, commit, at most 3 recomposes on a changed ledger sequence or disposition revision, the display index trigger after commit.
- `publish::settle(store, run)` and `publish::settle_due(store)`: apply the domain's `settle` decision. Publish an update, or record `unfinished_review` once. An update that cannot publish records `update_failed`.

**Steps**

- [ ] Write the stand-alone test with the parser already in the crate: parse the field followed by a blank line and a sentinel heading, and require the last block to be that heading. Find the offending line from the block that swallowed it. Do not hand-scan fences.
- [ ] Write the composer as one pass that appends to a `String` reserved from the record sizes, recording spans as it writes. Escape `|` in table cells. Write locations as code spans with a fence longer than any backtick run in the path. Place the details after the evidence and `Proposed comment` last. Write a gap under `Evidence`, beginning `Not confirmed.` Leave out the verdict line for `not_specified`.
- [ ] Write `finish_review` and `settle` over the port. They hold no SQLite transaction while composing.
- [ ] Add test `composed_worked_set_matches`: compose `worked.json`, compare with `worked.md` byte for byte, and check that each recorded span slices exactly its card or section. The set includes a subagent's finding, a gap, a retired finding, a user disposition, an extra note, a title with a code span, a `|` in a reason, an authored level-two heading, and a line with `Vec<u8>` and `<runner>` outside code.
- [ ] Add test `stand_alone_fields`: an open top-level fence is refused with its line, an open fence inside a list item is accepted, an unclosed HTML comment is refused, and a setext heading is shifted.
- [ ] Run `cargo check --locked -p reviewq-app` and `make test-package PACKAGE=reviewq-app`.

### K2: Ledger storage

**Files**

- Create: `crates/reviewq-store/src/ledger.rs`, `ledger/{records,passes,publish,settle}.rs`, `schema/ledger.sql`.
- Modify: `connection/database.rs` (add the schema file), `reports.rs` (`read_delivery`), `reports/context.rs` (finding details from `ledger_changes`), `reports/failures.rs`, `followups/completion.rs`, `launches.rs` and `hosts/delivery/workflow.rs` (open a pass inside the existing confirmation transactions), `schema/queue.sql` (the report state of `review_activity`).

**Consumes** F1's port and types. `WriteTx`, `tx.emit`, `insert_notification`, the native-call replay in `actions/commands.rs`, `reports/artifact.rs`, `followups/completion.rs`.

**Produces** the `LedgerStore` implementation on `SqliteStore`:

- **Record write**, one immediate transaction: resolve the caller, replay a known call from `ledger_calls`, apply the three duplicate rules, check the expected revision (a stale one returns the current revision and the changed field names), enforce the per-run limits, write the row in `ledger_findings` or `ledger_notes`, append a full snapshot to `ledger_changes`, advance the run's ledger sequence, open a full-review pass when the run has no version and no open pass, store the call result. A user's disposition is never read or changed here.
- **Caller binding.** Claude: the session's run, the role from `agentId`. Codex: the lead by its registered thread. A child by the request's parent thread when that is the lead or an already bound child of the run, otherwise by the discovered-children rows. An unattributable caller is refused.
- **Pass rows**: one open pass per run, opened inside the launch confirmation and the batch-executing transactions, held by `needs_input`, reopened when the lead's latest input is newer than the hold, closed by publish, ended by a successor batch or archive. The gate request counts blocks per session and turn on the pass row.
- **Snapshot read**: the records at their revisions, the ledger sequence, the disposition revision, the parent version's finding rows, the open pass, the frozen batch, and the facts for the first line and the cover lines.
- **Publish commit**, one immediate transaction: recheck the ledger sequence, disposition revision, current receipt, run lead and frozen batch. Seal the chunks, append the version with its kind, write the finding and section rows, move the head, bind the disposition revisions, close the pass, call the batch completion with the assessments (`final`, or `unchanged` with no version when no record changed and the verdict is the parent's), resolve earlier failures, insert the notification and set the desktop wake flag, emit the change events.
- **Failures**: one row per run, pass and kind. A repeat updates the reason and raises nothing.
- **Delivery read** and the queue's report state from the head and the unresolved failure rows.

**Steps**

- [ ] Write `schema/ledger.sql` with the keys and partial unique indexes that enforce one open pass per run, one row per failure key and one stored result per call.
- [ ] Write the transactions with cached prepared statements. No statement scans a run's change history on the write path.
- [ ] Add test `ledger_writes_are_idempotent`: the same call replays its result, an equal create under a new call returns the same ID and revision, a stale revision is refused with the current revision and field names, and a restart keeps both records.
- [ ] Add test `publish_is_conditional`: a record written after the snapshot, then a disposition saved after the snapshot, each make the commit report a stale snapshot and change nothing. A commit on a current snapshot stores the version, closes the pass and resolves the failure. A selected-thread close that changed nothing completes the batch as `unchanged` and adds no version.
- [ ] Keep the relocated `conclusion_errors` test passing against the new outcome type.
- [ ] Run `cargo check --locked -p reviewq-store` and `make test-package PACKAGE=reviewq-store`. Open a fresh database and reopen it.

### K3: Record tools, passes, gate, rule and read-back

**Files**

- Fill: `crates/reviewq-app/src/ledger/{record,passes,readback}.rs`.
- Modify: `crates/reviewq-app/src/launch.rs`, `followups/work.rs`.

**Produces**

- `record::{record_finding, assess_finding, record_note, review_ledger}`: validate with `markdown::check_field` and the limits, return every field error capped by `FieldError::bounded`, then one port call.
- `passes::needs_input(store, caller, reason)` and `passes::gate(store, request) -> GateDecision`. The block reason names what is owed and how many findings are on file.
- `readback::RULE`: the rule text, written once. `readback::read_back(store, session) -> String` within 4,000 characters: the rule, the open pass and what it owes, then one line per active finding, newest change first, with a count of lines left out.
- The launch input and the selected-thread prompt append `RULE` after the user's own text.

**Steps**

- [ ] Write the rule as five short imperative sentences from the spec's Rule paragraph. Name the tools. Say that only what is filed is shown and that the lead closes. Do not mention the gate.
- [ ] Write the four record operations and the two pass operations over the port.
- [ ] Write the read-back with a hard character cap and a deterministic order.
- [ ] Run `cargo check --locked -p reviewq-app`. `markdown.rs` is K1's file: call it, and report a missing function to the lead.

### E1: Routes and MCP tools

**Files**

- Create: `crates/reviewq-transport/src/entry/features/ledger.rs`, `entry/mcp/ledger.rs`.
- Modify: `entry/features.rs`, `entry/mcp.rs`, `entry/error.rs`.

**Steps**

- [ ] Add the eight routes. Each tool route checks the bridge credential and content type through one shared guard (replace the three copies the code has), validates with `validate_ledger_input`, maps the DTO to the app request field by field, and returns the result or `fieldErrors`.
- [ ] `turn-end` takes `{sessionKey,turnId,normal,stopping}`. With `stopping` it returns the gate decision. Without it, it settles the run.
- [ ] Keep, in the entry state, the last time each hook reported for a session, bounded in size, and return it with the link reply. It is a diagnostic, so it need not survive a restart.
- [ ] Add the six MCP tools from `LEDGER_TOOLS`. Bind the caller from `_meta`: `threadId`, `callId`, `itemId`, and `turn_id` and `parent_thread_id` from `x-codex-turn-metadata` when present. Hide the six tools from a Claude client's tool list, as the action tool is hidden today. Give `finish_review` a 60-second deadline and the others the default.
- [ ] Return a field error to the model as a tool error whose text lists each field and message, so the agent can correct it in the same turn.
- [ ] Run `cargo check --locked -p reviewq-transport` and `make rust-check`.

### E2: Binary: bridge, hooks, installer, wiring

**Files**

- Create: `crates/reviewq/src/native_client/codex_hook.rs`.
- Modify: `native_client/claude.rs`, `native_client.rs`, `cli.rs`, `install/codex_hook.rs`, `install/codex_hook/json.rs`, `install.rs`, `runtime.rs` or its `runtime/` children.

**Steps**

- [ ] Add the bridge action `ledger <tool>`: read the input from stdin, make one POST to the tool's route and print the reply. Make no status request and no transcript check on this path.
- [ ] Make `claude-hook` answer `Stop` (print the block reply when the route blocks) and `SessionStart` with source `compact` (print the read-back). Keep ignoring subagent hooks.
- [ ] Add `codex-hook`: read the hook JSON from stdin, send `Stop` to `turn-end` and a `compact` `SessionStart` to `readback`, print the same replies, and exit 0 on every failure so a session is never broken. Each run reports itself to the service.
- [ ] Rewrite the installer to own two entries in `$CODEX_HOME/hooks.json`, `Stop` and `SessionStart` with matcher `^compact$`, each recorded with its event. Remove the `PreToolUse` entry and the output redirect. Never write trust state. The installer and `reviewq link` name a hook that is installed but has not reported for the linked session. Rewrite the one installer test for the two entries.
- [ ] Wire settling: after a Claude turn-end signal, inside the existing 10-second attention poll for runs that have an open pass or unpublished records, after a ledger write made while the lead is idle, after a recovery ends, and once at startup. Add no new timer.
- [ ] Return the rule from the terminal link as text the model reads.
- [ ] Run `make rust-check`.

### E3: Claude plugin

**Files**

- Create: `hosts/claude/plugin/hooks/ledger.js`.
- Modify: `register.jsx`, `workflow.js`, `hooks.json`, `bridge.js`, and the plugin file list in `crates/reviewq/src/install/bundle.rs`.

**Steps**

- [ ] Register the six tools from `ledger-schema.js`.
- [ ] Handle a call with one bridge spawn: pass the tool-use ID, the active turn, and the subagent's ID when the event has one. Accept subagent calls for the four record tools and refuse the two closing tools from a subagent with a message that says the lead closes.
- [ ] Return a result as the tool result and field errors as an error the model can read. Deny only an unlinked session.
- [ ] Add the `SessionStart` entry to `hooks.json`. Give `finish_review` a longer bridge timeout than the 5 seconds the other calls keep.
- [ ] Keep `register.jsx` at or below its current size by moving the ledger handlers into `ledger.js`. Check the files with the plugin's own lint.

### X1: Fixtures

**Steps**

- [ ] Trace how `seed-review` and the fixture provider put report versions into a profile, and feed the worked set's records through the real ledger operations so the fixture versions are composed by the product. Write no second composer.
- [ ] Store the size-limit specimens as version text through the fixture path, sized to the limits in the fixture brief. They bypass the record limits on purpose, because they test the reader.
- [ ] Update the pinned offsets and digests in the fixture manifest from the regenerated output.
- [ ] Load the `worked` preset in a dedicated profile and confirm that Findings, History and the dispositions panel show the composed versions.

### V1: End-to-end evidence, measurements, README and briefs

**Steps**

- [ ] Run `make web-install` and `make check`.
- [ ] Exercise the six reproductions in the [REPORT evidence table](reports.md#evidence) through the production routes in a disposable profile. Step 5 of the flow is a subagent record after the lead went idle and an interrupted chat turn.
- [ ] Measure on the worked set and record with the commands: median and 99th-percentile time of a record write, time to compose and commit a version, peak resident memory of the service during a publish of the largest specimen, and the response size of the short ledger list at 200 findings.
- [ ] Run one linked review on each host with `claude-sonnet-5-5` at high effort and `gpt-6.2-sol` at high effort, in disposable host profiles that leave the user's settings files untouched. Confirm: a subagent's call is attributed to its lead, the stop gate holds and releases, the read-back arrives after compaction, and the Codex request carries the thread and turn metadata the binding reads. Record what could not be exercised as a proof limit.
- [ ] Update `README.md` for the two Codex hooks, tick the REPORT brief's items that closed, and set the roadmap row.
- [ ] Remove the task's profiles, builds and scratch output, and read back the cleanup ledger.

## Reviews

- **Review A** covers C1, F1, F2 and W1: nothing of the earlier path remains, the relocated items have one owner each, the domain rules match the spec's tables row by row, and the tool schemas match the spec's fields and limits.
- **Review B** covers K1–K3, E1–E3 and X1: each transaction is short and conditional, no transcript or provider read sits on a ledger call, the composer's output matches the spec's layout line for line, the gate and settle paths cannot loop or double-record, and nothing is duplicated between the Claude and Codex paths.
- **Final review** reads the whole branch against `main` for dead code, repeated logic, files over the size limit and tests that do not guard a regression.
- Each review reports findings with a file, a line and a failing scenario. The lead fixes confirmed findings before the next stage.
