# TRANSFER · Move an existing installation into a fresh store

**Goal.** A user can inspect the supported installed format, prepare a separate candidate and verify what will move before selecting it during installation. An incompatible file, lost disposition, changed receipt, missing report byte or active writer produces an actionable error. The user's original installation stays available for recovery. [Operations](../../specs/operations.md)

**Needs.** PREFS and FOLLOW. Their accepted inputs include HISTORY and STORE's supported schema. Use the agreed ENTRY parsing and PACK activation contracts. TRANSFER can qualify the explicit conversion before installation integration. Supply a reproducible synthetic source independently of either installed account. A normal fresh installation starts directly with STORE's `0001_initial.sql` schema (application ID `0x52565132`, first user version 1). The target is `$XDG_DATA_HOME/reviewq/reviewq.sqlite3`, distinct from the earlier workflow and settings. Activate only into a fresh target or replay an already recorded identical cutover. An existing unrelated or later target is a conflict.

## Files and public contract

| File | Responsibility |
|---|---|
| `crates/store/src/transfer.rs` (source reader) | Recognize and stream the supported input schema into typed transfer facts, read-only. This code runs only for an explicit transfer command. Keep SQLite ownership inside `reviewq-store`. |
| `crates/store/src/transfer.rs` (candidate and comparison) | `inspect(source: &SourcePaths) -> Result<Inventory>`, `prepare(source: &SourcePaths, destination: &Path) -> Result<Comparison>`, `verify(directory: &Path) -> Result<Comparison>`. Own isolated import, normalization, coverage accounting and independent readback through target repositories. |
| `crates/reviewq/src/transfer.rs` | CLI argument/path handling, owner-private scratch directory, safe summary/exit code and destination receipt passed to PACK. ENTRY owns clap registration. No service IPC, host activation or provider calls. |
| `fixtures/transfer/thread-followup-v1.sql`, `fixtures/transfer/README.md` | Reproducible schema, independent nonsecret source rows, synthetic setup/runtime inputs, expected IDs, hashes and relationships. Include the source fingerprint and precise generation commands. Keep real data and comparison output private. |

Take absolute paths. Here `$tool` is the candidate bundle's `bin/reviewq` and the three inputs point to a single quiesced source profile. Prepare creates a previously absent `$candidate`. Verify reopens it and the source paths recorded in its manifest.

```sh
"$tool" transfer inspect --workflow "$workflow" --settings "$settings" --config "$config"
"$tool" transfer prepare --workflow "$workflow" --settings "$settings" --config "$config" --out "$candidate"
"$tool" transfer verify --candidate "$candidate"
"$tool" install --from "$bundle" --transfer "$candidate"
```

`inspect` shows recognized schema, source IDs/versions, family counts and any blockers, without payloads. `prepare` emits `prepared`, the private candidate path and comparison ID. `verify` emits `verified`, the same ID, source/target family counts, mismatch count and presence of items held for reconciliation. Exit zero requires every required family accounted for with zero unexplained differences. Incompatible input, stale comparison, conflicting path, unreadable source, unsupported field or byte mismatch exits nonzero with a family/reason and the first offending nonsecret record identity. Reserve `activated` for a successful PACK install.

## Identify and capture the source

- [ ] Recognize the installed `thread-followup-v1` workflow database by SQLite application ID `0x52565131`, `user_version=1` and ordered schema fingerprint `cc40e901c677583d4bcddf2868ca1f50fa464ce2fbee9b4f754170065f29ec6d`. Hash the UTF-8, compact JSON array of objects with fields `type,name,tbl_name,sql` in that order from `sqlite_schema`, excluding names matching `sqlite_%`, ordered by type/name. Match the corresponding settings schema, version 1 with the singleton `settings(id,version,payload)` and optional `setup(id,draft,hints)` row, plus the supplied nonsecret runtime config. Validate the values/constraints used by the translator. Earlier split queue/agent databases and other unrecognized schemas get an unsupported-source diagnostic identifying the recognized format and source file. A stale split-store backup alongside a recognized workflow is outside this import.
- [ ] Reject overlapping source/destination, symlinks/aliases to installed data, non-private output, previously existing output and concurrent prepare at the same name. Keep a permanent lock inode in the candidate's parent namespace and stage in a unique sibling. An interrupted temporary is identified by its operation ID. A repeat with an existing completed candidate reports the path for `verify`; a failed candidate can be regenerated in a new directory.
- [ ] For final capture, stop the source user service and other writers using the operations procedure. Require an explicit offline boundary for the workflow/settings/config set. Resolve paths without following untrusted links, fingerprint the original main files, config and any present WAL/journal before and after capture, including absence. Treat a hot rollback journal as requiring recovery by the source application. Under quiescence copy the source file families into a new private snapshot directory with the corresponding names, including WAL bytes when present. Use SQLite on those private copies to produce standalone coherent backups and run integrity and FK checks. Original WAL/SHM bookkeeping is never deleted or modified. A lone filesystem copy of a live main database can omit committed WAL frames. A prepared preview made while the original service was in use cannot authorize activation. [SQLite backup](https://www.sqlite.org/backup.html) · [SQLite WAL](https://www.sqlite.org/wal.html)
- [ ] Use STORE's selected SQLite binding with its backup support and bounded iteration. Use SQLite's own backup/integrity APIs and existing SHA-256 dependency. These avoid an extra service and preserve pages across WAL-backed snapshots. Include progress and bytes for large datasets. [rusqlite backup](https://docs.rs/rusqlite/latest/rusqlite/backup/)

## Translate meaning and compare

Read the supported source using fixed ordered queries and validated JSON field decoders. Maintain the relationship keys below through the destination model. Import into an unpublished store with foreign keys enabled. Write related objects in one transaction or use bounded transactions entirely inside that private candidate. A failed import is never eligible for activation.

| Source | Required target evidence |
|---|---|
| `merge_requests`, `review_state`, `workspace_cache` | Preserve origin/project/IID, tickets, board/sprint memberships and archive scope, last successful synchronization, checkpoint/acknowledgment time, full-review SHA and local review disposition. Explicitly list any disposable query cache rebuilt from these facts. |
| `review_runs`, `agent_sessions`, `reader_checkpoints` | Preserve run/lead/child ancestry, provider-native IDs and source path/identity, checkout, actual turn, requested SHA, previous/pending handoff, correction state and action/request bindings. Preserve recoverable read positions where the reader supports them, otherwise reindex by the recorded source identity. A missing native file remains an unavailable registered source. |
| `final_reports`, `report_state` | Stream the exact original UTF-8 Markdown through SHA-256. Derive byteLength, totalCodePoints, lineCount and maxLineBytes with the [report artifact rules](../../specs/review-workflow.md). LF and CRLF each end one line, maxLineBytes excludes the delimiter, there is no phantom line after terminal LF and empty input has zero lines. Seal metadata and bounded immutable chunks so large imports open first as summaries with paged plain source and streamed download. Keep immutable receipt/parent chain, byte length, hash, timestamp, provenance, SHA or revision reason, completeness, coverage, findings/change IDs, user dispositions and current accepted pointer. Distinguish incomplete/clipped versions, evidence candidates and pending handoffs from accepted versions. |
| `message_outbox`, `notifications` | Retain submission/run/session/target/quote, report pointer/hash, purpose, native acceptance/rejection/uncertainty, event source + ID, read/dismiss/notified state and occurrence identity. Hold any in-flight or replay-sensitive imported delivery at reconciliation until verified through the bound native session. Preserve pending user-visible work and never issue a second external effect on import. |
| `followup_monitors`, `followup_threads`, `followup_batches` | Keep assigned lead, user policy, selected discussion/original note IDs, exclusions, baselines/member versions, generation, held/reserved/executing batch and outcomes. A completed scoped check retains its own checked SHA and the full-review checkpoint. |
| Runtime config and settings row/draft | Write port, polling, desktop options, host executable paths and WSL distribution to the candidate runtime.json according to [runtime options](../../specs/operations.md#configure-the-local-runtime). Import auto-resume to the PREFS SQLite preference and concurrency/policy/overrides to FOLLOW rows. Map origins, account identity, boards, projects/checkouts, default skill, draft and relevant revision/scope. Insert bound provider credentials into STORE's private credential table in the same transaction as versioned settings. Rotate installation-specific bridge, browser sessions and repository grant tokens through PACK/ENTRY. Verify connection presence/binding privately. Never print raw settings or derive a public token fingerprint. |

Validate source FK/JSON consistency, counts, IDs and exact report bytes before writing. Compare destination *reads* with independently derived source facts per stable identity. Canonicalize ordered tuples using typed fields and lengths, preserve null/empty, numeric precision and list order where meaningful, and sort only unordered sets. Count every source family. Keep omission/rebuild/rotation reasons on explicit allowlists with counts. Unknown values or semantics fail preparation, even at equal row counts. Confirm current report hashes via the same target read path used by the app, including a pending newer candidate. Native transcripts stay in the original hosts at their recorded references. Verify missing sources show unavailable without inventing content or prompting an agent.

Close and checkpoint the candidate. Write `comparison.json` privately with format, converter/build ID, input paths/versions and stable source-file hashes, candidate-relative path and closed file digest(s), per-family source/target counts and semantic digests, receipt identities, allowed transformations, mismatches and completion status. Include private ancillary output identities without printing individual secret hashes. Sync files and parent directory before publishing `$candidate`. `verify` rechecks the snapshot, original source generation, destination digests and semantic readback read-only. PACK verifies again at the actual installed placement before releasing the service. A filesystem rename or altered JSON manifest alone cannot authorize a different source, profile or executable. A candidate already activated in this profile replays its recorded result; a later database cannot be replaced by resubmitting the same candidate.

## Demonstrate recovery

Use two same-IID projects, a two-version accepted report chain, pending candidate, reviewed SHA distinct from a scoped follow-up SHA, a child with missing history, one acknowledged event, one pending message and an uncertain submission. Build the source fixture with a WAL commit and capture its expected keys and Markdown hashes independently. Run the built CLI and use the target repositories against the candidate independent of PACK. At RELEASE, repeat the public flags on the packaged CLI and open the installed product in the isolated QA account. Check both heads, dispositions, origin and receipt states and zero spontaneous delivery/publication. Test removing one report byte, changing a source after prepare, unsupported split input, repeated verify/activation and interruption before publication. Each conflict must preserve the source, existing installation and last accepted output. Keep the coherent old backup and matching release for a separately verified rollback. [Release check](release-check.md)

## Input DDL and fixture construction

The following standalone SQL defines the accepted workflow source. Keep the SQL text inside each statement unchanged when generating fixtures: the preflight hashes SQLite's stored DDL including its internal whitespace. Extract this block into the planned fixture file. Execute it with standard SQLite in a new database and then insert the test rows in a transaction with foreign keys enabled.

```sql
PRAGMA foreign_keys=ON;
PRAGMA application_id=0x52565131;
PRAGMA user_version=1;
CREATE TABLE merge_requests (
  mr_key TEXT PRIMARY KEY NOT NULL, origin TEXT NOT NULL, project_id INTEGER NOT NULL, iid INTEGER NOT NULL,
  snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json)), ownership_json TEXT NOT NULL CHECK(json_valid(ownership_json)),
  scope_history_json TEXT NOT NULL CHECK(json_valid(scope_history_json)), synced_at TEXT NOT NULL,
  UNIQUE(origin,project_id,iid)
);
CREATE TABLE review_state (
  mr_key TEXT PRIMARY KEY NOT NULL REFERENCES merge_requests(mr_key), reviewed_sha TEXT,
  disposition TEXT CHECK(disposition IN ('reviewed','waiting_author','needs_review')), reviewed_at TEXT,
  checkpoint_json TEXT NOT NULL CHECK(json_valid(checkpoint_json))
);
CREATE TABLE review_runs (
  run_id TEXT PRIMARY KEY NOT NULL, mr_key TEXT NOT NULL REFERENCES merge_requests(mr_key), lead_session_key TEXT NOT NULL UNIQUE,
  started_at TEXT NOT NULL, requested_sha TEXT, review_boundary_json TEXT NOT NULL CHECK(json_valid(review_boundary_json)),
  source_json TEXT NOT NULL CHECK(json_valid(source_json)), pending_handoff_json TEXT NOT NULL CHECK(json_valid(pending_handoff_json)),
  last_handoff_json TEXT NOT NULL CHECK(json_valid(last_handoff_json)), correction_json TEXT NOT NULL CHECK(json_valid(correction_json)),
  FOREIGN KEY(run_id,lead_session_key) REFERENCES agent_sessions(run_id,session_key) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE agent_sessions (
  session_key TEXT PRIMARY KEY NOT NULL, provider TEXT NOT NULL CHECK(provider IN ('claude','codex')), native_id TEXT NOT NULL,
  run_id TEXT NOT NULL REFERENCES review_runs(run_id), parent_session_key TEXT, cwd TEXT NOT NULL,
  metadata_json TEXT NOT NULL CHECK(json_valid(metadata_json)), UNIQUE(provider,native_id), UNIQUE(run_id,session_key),
  FOREIGN KEY(run_id,parent_session_key) REFERENCES agent_sessions(run_id,session_key) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE final_reports (
  run_id TEXT NOT NULL REFERENCES review_runs(run_id), receipt_id TEXT PRIMARY KEY NOT NULL, markdown TEXT NOT NULL,
  sha256 TEXT NOT NULL, source_json TEXT NOT NULL CHECK(json_valid(source_json)), reviewed_sha TEXT, revision_reason TEXT,
  verdict TEXT NOT NULL, captured_at TEXT NOT NULL, completeness TEXT NOT NULL CHECK(completeness IN ('complete','previously_clipped')),
  parent_receipt_id TEXT REFERENCES final_reports(receipt_id), coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
  findings_json TEXT NOT NULL CHECK(json_valid(findings_json)), changes_json TEXT NOT NULL CHECK(json_valid(changes_json)),
  disposition_version TEXT NOT NULL, declaration_json TEXT NOT NULL CHECK(json_valid(declaration_json))
);
CREATE TABLE report_state (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES review_runs(run_id), current_receipt_id TEXT REFERENCES final_reports(receipt_id),
  user_disposition_version TEXT NOT NULL, dispositions_json TEXT NOT NULL CHECK(json_valid(dispositions_json))
);
CREATE TABLE notifications (
  source TEXT NOT NULL CHECK(source IN ('gitlab','agent')), id INTEGER NOT NULL CHECK(id>0), occurrence_key TEXT NOT NULL UNIQUE,
  mr_key TEXT REFERENCES merge_requests(mr_key), run_id TEXT REFERENCES review_runs(run_id), session_key TEXT REFERENCES agent_sessions(session_key),
  event_ref_json TEXT NOT NULL CHECK(json_valid(event_ref_json)), kind TEXT NOT NULL, description TEXT NOT NULL, url TEXT NOT NULL,
  created_at TEXT NOT NULL, read_at TEXT, dismissed_at TEXT, notified_at TEXT, PRIMARY KEY(source,id)
);
CREATE TABLE message_outbox (
  submission_id TEXT PRIMARY KEY NOT NULL, run_id TEXT NOT NULL REFERENCES review_runs(run_id), session_key TEXT NOT NULL REFERENCES agent_sessions(session_key),
  user_text TEXT NOT NULL, quote TEXT NOT NULL, report_hash TEXT, report_ref_json TEXT NOT NULL CHECK(json_valid(report_ref_json)),
  state TEXT NOT NULL CHECK(state IN ('queued','dispatching','started','rejected','uncertain')), detail TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  purpose TEXT NOT NULL CHECK(purpose IN ('user','codex_recovery','thread_followup')), followup_batch_id TEXT,
  CHECK((purpose='thread_followup' AND followup_batch_id IS NOT NULL) OR (purpose!='thread_followup' AND followup_batch_id IS NULL))
);
CREATE TABLE reader_checkpoints (
  session_key TEXT NOT NULL REFERENCES agent_sessions(session_key), purpose TEXT NOT NULL, source_identity TEXT NOT NULL,
  generation TEXT NOT NULL, position_json TEXT NOT NULL CHECK(json_valid(position_json)), PRIMARY KEY(session_key,purpose)
);
CREATE TABLE workspace_cache (
  cache_key TEXT PRIMARY KEY NOT NULL CHECK(cache_key IN ('tickets','scope','sprints','reviewerIdentities','sync')),
  value_json TEXT NOT NULL CHECK(json_valid(value_json))
);
CREATE TABLE followup_monitors (
  mr_key TEXT PRIMARY KEY NOT NULL REFERENCES merge_requests(mr_key), run_id TEXT NOT NULL REFERENCES review_runs(run_id),
  generation INTEGER NOT NULL CHECK(generation>0), state TEXT NOT NULL CHECK(state IN ('enabled','paused','archived')),
  reviewer_ids_json TEXT NOT NULL CHECK(json_valid(reviewer_ids_json)), updated_at TEXT NOT NULL
);
CREATE TABLE followup_threads (
  mr_key TEXT NOT NULL REFERENCES followup_monitors(mr_key), discussion_id TEXT NOT NULL,
  selected INTEGER NOT NULL CHECK(selected IN (0,1)), selection_json TEXT NOT NULL CHECK(json_valid(selection_json)),
  baseline_json TEXT NOT NULL CHECK(json_valid(baseline_json)), version INTEGER NOT NULL CHECK(version>=0),
  available INTEGER NOT NULL CHECK(available IN (0,1)), PRIMARY KEY(mr_key,discussion_id)
);
CREATE TABLE followup_batches (
  batch_id TEXT PRIMARY KEY NOT NULL, mr_key TEXT NOT NULL REFERENCES followup_monitors(mr_key),
  run_id TEXT NOT NULL REFERENCES review_runs(run_id), generation INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','executing','completed','cancelled')),
  members_json TEXT NOT NULL CHECK(json_valid(members_json)), first_observed_at TEXT NOT NULL,
  last_observed_at TEXT NOT NULL, due_at TEXT NOT NULL, dispatch_json TEXT NOT NULL CHECK(json_valid(dispatch_json)),
  hold_reason TEXT, completed_at TEXT, result_json TEXT NOT NULL CHECK(json_valid(result_json))
);
CREATE UNIQUE INDEX followup_one_pending ON followup_batches(mr_key) WHERE state='pending' AND dispatch_json='null';
CREATE UNIQUE INDEX followup_one_reserved ON followup_batches(mr_key) WHERE state='executing' OR (state='pending' AND dispatch_json!='null');
CREATE INDEX followup_due ON followup_batches(state,due_at);
CREATE INDEX reports_run_order ON final_reports(run_id,captured_at DESC,receipt_id);
CREATE INDEX runs_mr_started ON review_runs(mr_key,started_at);
CREATE INDEX sessions_run_parent ON agent_sessions(run_id,parent_session_key);
CREATE INDEX notifications_source_order ON notifications(source,created_at DESC,id DESC);
CREATE INDEX notifications_mr_source ON notifications(mr_key,source,id);
CREATE INDEX outbox_session_state ON message_outbox(session_key,state,created_at);
```

The settings source is a separate SQLite file. For a first-run fixture use this complete input. A populated connection variant increments both the singleton version and payload version together, and exercises the private fields in the mapping below.

```sql
CREATE TABLE settings(id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL, payload TEXT NOT NULL);
CREATE TABLE setup(id INTEGER PRIMARY KEY CHECK(id=1), draft TEXT NOT NULL, hints TEXT NOT NULL);
PRAGMA user_version=1;
INSERT INTO settings VALUES(1,0,'{"version":0,"mode":"setup","config":null,"secrets":{"gitlab":null,"jira":null},"identity":null}');
```

To bootstrap a fixture directly from these two blocks on a fresh checkout, run this Python 3 recipe from the repository root. It writes only beneath a new private temporary directory. Its independent source fingerprint and accepted body hashes are available before any converter exists.

```python
import hashlib
import json
import os
import re
import sqlite3
import tempfile
from pathlib import Path

os.umask(0o077)
root = Path(tempfile.mkdtemp(prefix="reviewq-transfer-"))
doc = Path("docs/plans/06-release/transfer.md").read_text()
workflow_ddl, settings_ddl = re.findall(r"^```sql\n(.*?)\n```$", doc, re.M | re.S)
def compact(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()

with sqlite3.connect(root / "settings.sqlite3") as settings:
    settings.executescript(settings_ddl)
(root / "config.json").write_text(compact({"port":8766, "pollSeconds":300, "desktopNotifications":True}))
workflow = sqlite3.connect(root / "review.sqlite3")
workflow.row_factory = sqlite3.Row
workflow.executescript(workflow_ddl)
schema = [dict(row) for row in workflow.execute(
    "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")]
assert digest(compact(schema)) == "cc40e901c677583d4bcddf2868ca1f50fa464ce2fbee9b4f754170065f29ec6d"
origin, stamp, sha = "https://gitlab.example.test", "2025-01-01T12:00:00.000Z", "a" * 40
run, native = "00000000-0000-4000-8000-000000000041", "lead-a"
lead, key = "codex:" + native, origin + "#11!41"
body_by_receipt = {}
with workflow:
    for project_id, project in [(11,"alpha"), (22,"beta")]:
        mr_key = origin + f"#{project_id}!41"
        snapshot = {"key":mr_key, "projectId":project_id, "iid":41, "project":project,
                    "url":origin + f"/forge/{project}/-/merge_requests/41",
                    "title":"Review " + project, "ticketKeys":[f"RQ-{project_id}"], "authorId":7,
                    "author":"Sam", "sha":sha, "state":"opened", "draft":False,
                    "updatedAt":stamp, "inSprint":True, "discussions":[]}
        workflow.execute("INSERT INTO merge_requests VALUES (?,?,?,?,?,?,?,?)",
                         (mr_key,origin,project_id,41,compact(snapshot),"{}","{}",stamp))
    workflow.execute("INSERT INTO review_state VALUES (?,?,?,?,?)",
                     (key,sha,"waiting_author",stamp,'{"eventsThrough":0,"sessions":{}}'))
    workflow.execute("INSERT INTO review_runs VALUES (?,?,?,?,?,?,?,?,?,?)",
                     (run,key,lead,stamp,sha,compact({"requestedSha":sha}),
                      compact({"provider":"codex","nativeId":native}),"null","null","null"))
    metadata = {"name":"QA lead","kind":"saved","jobId":None,"status":"idle",
                "lastActivity":stamp,"lastSeen":stamp,"startedAt":stamp,"managed":False,
                "modSeen":None,"availability":{"state":"unavailable"},"historyRef":None,
                "terminalResponseRef":None}
    workflow.execute("INSERT INTO agent_sessions VALUES (?,?,?,?,?,?,?)",
                     (lead,"codex",native,run,None,str(root),compact(metadata)))
    parent = None
    for num, markdown in enumerate(["# Alpha\nPreviously reviewed.\n", "# Alpha\nRevised café.\n"], 1):
        receipt = digest("synthetic receipt " + str(num))
        body_by_receipt[receipt] = digest(markdown)
        workflow.execute("INSERT INTO final_reports VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                         (run,receipt,markdown,digest(markdown),'{"kind":"response"}',sha,None,
                          "approve",stamp,"complete",parent,
                          compact({"kind":"complete_mr","sourceSha":sha,"targetSha":None}),
                          "[]","[]","0","null"))
        parent = receipt
    workflow.execute("INSERT INTO report_state VALUES (?,?,?,?)", (run,parent,"0","{}"))
    workflow.execute("INSERT INTO message_outbox VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                     ("00000000-0000-4000-8000-000000000042",run,lead,"QA-LATE","",None,
                      "null","uncertain","provider reply lost",stamp,stamp,"user",None))
assert workflow.execute("PRAGMA foreign_key_check").fetchall() == []
assert workflow.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
workflow.close()
print(root, compact({"sourceReports":body_by_receipt,"acceptedReceipt":parent}))
```

The basic result is two scoped MRs, one run/lead in alpha, zero runs in beta, two immutable report versions, the second receipt selected, one uncertain message, one local waiting-on-author decision and no external operations. The body digest includes the UTF-8 accent and final LF. Use the transfer CLI above with the generated files. Verification of this minimal fixture is one part of the populated-case exercise.

### Decode the populated fields

Use the relational keys first, then translate these JSON fields into the corresponding target repositories. Keep the source key beside the new ID while resolving references. The converter must reject a required relationship that resolves to a different run, MR or origin.

| Source value | Decode and qualification variant |
|---|---|
| `mr_key`, `snapshot_json`, `ownership_json`, `scope_history_json` | MR key syntax is `<origin>#<positive projectId>!<positive iid>`. Match it against all three columns and snapshot identity. Snapshot fields include branch, ticket keys, author, SHA/state, timestamps, sprint membership, discussions with original note IDs/positions and personal approval when known. Scope history keys are settings revisions. `review_state.checkpoint_json` is null or `{eventsThrough,sessions:{sessionKey:semanticReceipt}}`. Archive and decision projections must still observe the same boundary. |
| `agent_sessions.metadata_json` | Scalar identity/ancestry comes from columns. Metadata supplies name, kind, status, last seen/activity, turn, model/effort, managed flag and source availability. `historyRef`, `terminalResponseRef`, `planRef`, `assignmentRef`, `skillRef` and launch args/context references use provider, nativeSessionId and optional path/turn/item. Add an unavailable child using the same run and parent key. `codexRecovery`, `codexFailure`, `codexInputs` and `reviewInput` retain original purpose and native identity; an expired interactive handle needs inspection. |
| `agent_sessions.metadata_json.reviewAction` | Move action ID, intent, reviewedSha, request entry/turn/call/submission IDs, comment digests and their individual states/note IDs, personal approval state, attempts/checks, queued and executed retry IDs, next time and timestamps to the action repository. Include a `cancelled` publication and one `checking` with unknown approval in separate cases. The assignment and reviewed revision must match its lead. |
| `review_runs` handoff columns | `pending_handoff_json`, `last_handoff_json` and `correction_json` retain their own state and identity. Retained conflict/candidate artifacts carry Markdown, SHA-256, reason, capturedAt and provenance. Check each byte hash without promoting it. A source reference can be `{kind:"response",provenance?}` or `{kind:"file",path,sha256,provenance?}`; it records what backed the preserved report. The accepted pointer lives only in `report_state`. |
| `final_reports`, `report_state` | Preserve `complete` versus `previously_clipped`, per-version parent, coverage (`complete_mr` or `thread_followup`), declared findings with stable ID/title/SHA, changes, disposition version and declaration. `report_state` carries the current authoritatively accepted receipt plus the independent user disposition version/map. Include a last scoped check whose checked SHA differs from the latest complete-MR SHA. Preserve both. |
| `notifications`, `message_outbox` | A notification identity includes source as well as numeric ID. Include agent:1 and gitlab:1, one acknowledged/dismissed and one unread. Preserve native occurrence and already-notified time. Outbox purpose is user, Codex recovery or thread follow-up; preserve subtype identity (including non-UUID automated IDs), quote/report ref, created/updated/stage and batch membership. Include a queued user request and the uncertain receipt from the basic fixture. Neither may execute during conversion. |
| `followup_threads`, `followup_batches` | Selection contains `discussionId, originalNoteIds[], findingIds[]` and a separate selected bit, source baseline, version and availability. A batch member includes observation ID, discussion/version/fingerprint, triggers, note/actor/finding/original-note IDs and timestamp. A frozen dispatch stores submissionId, source/target SHA, base report receipt/hash, userDispositionVersion, prompt, frozenAt and nativeTurnIds. Include an excluded thread and an executing batch. Preserve monitor generation, deduplication and reservations even if the source agent is unavailable. |
| `settings.payload`, `setup`, `config.json` | Payload includes version, mode, config, identity and secrets. Config includes GitLab origin, Jira site/email/tokenType, boards (boardId, type, enabled, projectIds, reviewStatusIds, jiraProjectKeys, teamFilterId, storyPointsField), repositories (projectId, path, checkout, boardSelection, defaultSkills) and additionalReviewerIds. Bind GitLab/Jira credentials to source identity and origin. `secrets.chat` grants are installation-specific and rotate. Preserve setup draft and hints separately. Runtime config carries port/pollSeconds, desktopNotifications, autoResumeCodexReviews, autoReviewConcurrency, followups policy/overrides, executable paths and WSL distribution. Generate synthetic strings for all secret fields. |
