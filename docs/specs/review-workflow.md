# Review decisions, findings and selected-thread checks

A reviewer can privately investigate an MR, have the lead file exact findings, ask the registered lead to act, and verify what reached GitLab. A local decision, a published report version, a native delivery, a confirmed GitLab action and an MR merge each have their own evidence. All examples use the selected installation and its configured GitLab origin, project ID and MR IID. The implementation owners are [ACTION](../plans/04-workflow/actions.md), [REPORT](../plans/04-workflow/reports.md) and [FOLLOW](../plans/04-workflow/followups.md).


## What the reviewer sees

| Operation | Observable outcome |
|---|---|
| Track, register and prepare | Tracking one MR adds it to discovery exceptions. Registering an actual native lead adds a run under that MR. Preview shows target checkout, skill or custom prompt, model, effort and revision. Only Start invokes the native host. A registration or accepted native send alone leaves reviewed revision unknown. [Sessions](agent-sessions.md) |
| Prepare several reviews | Queue and Runs share Select MRs. Select only open MRs in scope. Drop selections outside the effective scope. Before submitting, show the exact targets. Intersect available catalog/model/effort choices and expand `{ticket}` in each target's repository context. Launch each through HOST's normal single-target preview/start. Retain one submission ID and visible result per MR. On rejection or lost confirmation continue independently, retain the outcome and permit inspection. Closing the panel leaves started reviews running. No durable job exists for targets the operator never submitted. |
| Save a local decision | Review complete, Waiting on author or Clear local review record goes through QUEUE's SHA-checked decision command. Follow-up handled covers the displayed GitLab events, report and session attention, and exact completed follow-up batches. Later arrivals stay pending. Notification read/dismiss touches only named occurrence receipts. Selecting a report, recording a nonblocking finding or clearing a decision does not publish anything. [Queue and Settings](queue-and-settings.md) |
| Declare comments, approval or both | The registered lead binds the current actual user request, exact open MR revision and intended comment hashes. After native execution, ReviewQ independently reads GitLab. A comments-only confirmation hands work to the author even for an approve verdict. For explicit approval, read back the authenticated user's own approval. |
| Deliver findings or ask a question | The lead and its subagents file each finding and review note through ReviewQ's ledger tools as they confirm them, and the lead closes the review with one call. Findings presents the published version with its reviewed SHA, content hash and receipt. While a review is in progress, keep the previous version readable. A question can require input without publishing a version. A review that ends without its closing call shows its own reason. [Findings ledger](#findings-ledger-and-report-versions) |
| Reply to a report | Feedback captures the displayed immutable version and exact optional source quote. The dispatch receipt names its lead and report even if another version arrives or the drawer changes. Preserve the draft on error. |
| Monitor or check selected threads | Manage monitored threads selects an assigned lead and original published reviewer notes, including an explicit empty selection. Enable, pause, resume and Review selected updates now act on that MR. A private check can draft a reply. Only a later explicit user request can authorize publication. |

The resulting stage, focus session and reason drive Queue, Runs, browser totals and MCP from one read snapshot. One card represents each MR with at least one registered lead. Pick the newest registered lead by start time and a stable key, follow real parent links for its current family, and retain all earlier runs in the drawer. QUEUE owns `classify_review(&WorkspaceEvidence) -> ReviewStageView` in `crates/reviewq-domain/src/review_stage.rs`. Inputs include MR state/head and handed-off time, the observed checkpoint, all registered session statuses and native input/attention times, report delivery/semantic receipt, current action, recovery and monitor evidence. Return stage, reason kind, target lead and focus, `hasAgentQuestions`, coverage and independently observed approval. Bare timestamps and UI filters cannot manufacture execution.

Apply the following precedence, first match wins.

1. Closed or merged goes to Archived. Missing MR evidence for an existing registered run needs input. Keep unavailable distinct from closed.
2. Relevant blocking questions or permissions need input immediately. Check actual native question state from the newest review start or a later active personal approval. A newer question in an explicitly resumed older session can qualify. An optional unanswered question remains visible as `hasAgentQuestions` while the current lead works. A working current lead shows Working. A current unhandled family failure or unavailable state needs input unless superseded by a later lead report or handled recovery. Otherwise another working registered member makes the MR Working. Any remaining current question needs input.
3. Active action verification is Working. With idle native work, an unfinished review or a refused closing call needs input. A review pass still being filed is Working only while its lead or a child works. A pending action retry waits and an uncertain continuation needs input. For a qualifying Codex quota, model-capacity or access interruption, eligible automatic recovery waits and disabled/cancelled recovery needs input. A closing call accepted in that exact interrupted turn ends its recovery eligibility. Keep the last observed state and show freshness separately when a provider read is delayed.
4. Withdrawn personal approval can need a decision. For an unapproved MR, an accumulating or reserved automatic check waits. A selected reply, edit, reopen or unresolved handling of an actual resolution after the checkpoint needs input. Code-only changes remain visible in coverage/notifications. A completed focused result with a new requested decision needs input until handed off or handled. Track outstanding actions across batches even if a later batch says waiting on author. Queued work takes precedence over an older unread result.
5. A current verified comments-only action waits on author. A current confirmed approval waits on merge. Completed action evidence is scoped to revision and semantic report identity. An unhandled new report or newly started review can need input. Otherwise use the latest valid local/GitLab handoff for Waiting on author or Awaiting merge. An open, finished lead lacking a handoff needs a decision. An active configured-reviewer approval suppresses routine reply/commit prompts while preserving their inbox events. The MCP filter `needs_input` selects the dashboard's Awaiting input stage. `agent_questions` selects the independent question flag.

## Commands, reads and authority

Paths below are under `/api/v1`. The shared envelope wraps each data shape in `{data,cursor}`. Lists use `items,nextCursor` and stable opaque scope-bound page cursors. IDs are opaque strings, revisions decimal strings, Git SHAs full hashes, timestamps UTC. `submissionId` on a user mutation is a stable caller-generated ID scoped to operation, actor and exact target. Identical replay returns the stored result before evaluating a now-stale precondition. A different payload for that identity conflicts. Native tool calls use their proven provider/session/turn/call identity for the same purpose. HTTP adapters verify browser/native grants, method, scope, strict shapes and body caps. ENTRY delegates its CLI and MCP commands to the same app services. The browser cannot assert a native tool identity or accepted GitLab result.

| Route | Input and successful data |
|---|---|
| POST `native/actions/declare`, `native/actions/reconcile`, `native/actions/input` | Native lead proof as below. Declare takes `{sessionKey,callId,turnId?,requestEntryId?,declaration:{intent,reviewedSha,comments}}` and returns Action. Reconcile takes `{sessionKey,callId}` and returns Action or null. Input is an authenticated Claude prompt callback bound to that exact native session/occurrence. It supersedes the previous authority once. |
| GET `runs/{runId}/action` | Current Action or null. Action includes actionId, runId, reviewer request entry/turn/call, reviewedSha, intent, typed targets with evidence, approval, state, attempts, pending submission, last executed submission/turn, next check and reason. All reads expose only the selected run. |
| POST `native/ledger/{record-finding,assess-finding,record-note,list,finish,needs-input}`, `native/ledger/{gate,readback}` | Each tool route takes the caller binding `{sessionKey,callId,turnId?,agentId?}` and the tool's input from [Ledger tools](#ledger-tools). Return the tool's result or its field errors. A repeat of the same call identity returns the stored result. Each tool route's body is `{binding,input}`, and a refused input returns the common error reply with `fieldErrors:[{path,message}]`. Gate takes `{sessionKey,turnId?,stopHookActive}` from a host's stop hook and returns `{block:false}` or `{block:true,reason}` under the [gate](#rule-read-back-and-gate) rules. Readback takes `{sessionKey}` and returns the rule and the short ledger list. |
| GET `runs/{runId}/report`, `…/history`, `…/versions/{receiptId}` | Current `{report,delivery}`, newest-first summary page (limit 1–50, currentReceiptId and delivery), or exact immutable version. Default `view=summary` on current/version returns metadata only. `view=markdown` is explicit and applies the rich-view limits below. Unknown run/version is missing, known run without a published version returns report null. Delivery is absent, `saved` or `failed`, with its receipt and reason. The summary includes receiptId, attentionReceiptId, sha256, reviewedSha or revisionReason, coverage, fullMrReviewedSha, parentReceiptId, verdict, publishedAt, and counts of active findings and recorded outcomes. Summary also carries byteLength, lineCount, maxLineBytes and totalCodePoints. Only an eligible selected report explicitly requested as Markdown includes the report text. |
| GET `runs/{runId}/report/context`, `…/section` | Context returns baseReport (receipt/hash or null), userDispositionVersion, a bounded page of the version's finding records, outcomes, dispositions or section titles/hashes via the `collection` query parameter. Default to dispositions. Pin the receipt on all pages and the disposition revision for mutable dispositions. Return items and nextCursor, at most 50 descriptors and 64 KiB of serialized page data. Bound display previews and link to source offsets for the full text. Report history uses compact summaries with a 64 KiB page budget and continuation. Sections accept optional receiptId, offset ≥0, characters 1–12,000 (default 6,000). Return text, receipt/hash, offset, nextOffset or null, totalCodePoints, offsetUnit `unicode_codepoints`. Pin receiptId after first page. The browser reads published versions only. Unpublished records are read through `review_ledger`. |
| GET `runs/{runId}/report/versions/{receiptId}/source?download=1` | Authenticate the same-origin request and immutable version. Stream the version's stored UTF-8 bytes as an attachment with a safe service-generated filename, `nosniff`, known content length and digest/receipt identity. Use browser-native download facilities or a guarded streaming sink. |
| PUT `runs/{runId}/report/dispositions` | `{submissionId,expectedVersion,receiptId,findingId,disposition:{blocking?,publication?,reason}}`. Require current receipt and known current finding, nonempty reason, at least one choice. Publication is withheld, authorized or published as recorded by the user. Apply only stated fields, advance the disposition version and return the edited finding and new revision. Refresh affected pages through the context read. A local label cannot execute or verify a GitLab action. |
| POST `runs/{runId}/report/feedback`, `…/viewed` | Feedback `{submissionId,receiptId,reportHash,quote,quoteRef?,text}`, quote may be empty, text required, each ≤12,000 Unicode scalar values. `quoteRef={offset,length,unit:'unicode_codepoints'}` identifies the exact source slice. Require it for a nonempty quote. An empty quote with no source range applies to the whole report. Return HOST DeliveryReceipt including exact reportRef. Viewed `{submissionId,receiptId,renderedBatchId?}` returns selected receipt, previousReceiptId and assessed changes through it. Advance monotonically only for actual displayed content. An unrendered assessment page keeps its own unread state. |
| GET/PUT `followups/preferences` | GET `{automatic,autoReviewConcurrency,revision}`. PUT supplies submissionId, expectedRevision and one or both changed fields. Fresh installation defaults off and 2 concurrent checks, valid 1–8. Persist with PREFS, apply immediately, let active work finish on a reduction. |
| GET `reviews/{reviewId}/followups`, `…/results`, `…/assessments` | Enrollment returns policy/default/override, monitor or null, eligible candidates, recent batch summaries, latest completed result and unseen assessment page or continuation. Summaries carry IDs, state, member count, checked SHA, hold reason and timings. Exclude the frozen prompt. Results page 1–20 completed batch outcomes: `final` when the check published a version and `unchanged` when it did not. Assessments page 1–500 with distinct discussion assessment, batchId, receiptId and fixed throughBatchId for exact viewing acknowledgement. |
| POST `reviews/{reviewId}/followups/{configure,pause,resume,review-now}` | `{submissionId,expectedGeneration}` (null for first enrollment). Configure adds exact runId, state enabled/paused and selections `{discussionId,originalNoteIds:[…],findingIds:[…]}`. Validate against fresh MR/lead/identity. Return current enrollment and, for review-now, the admitted batch ID. Replays reuse that admission. A changed lead, generation or unavailable selected discussion requires a fresh choice. |

Use the [shared error contract](data-contracts.md). Malformed declarations return invalid_request. Wrong grants map to unauthorized/forbidden, inaccessible or absent targets to not_found, altered replay to idempotency_conflict and obsolete expected state to stale_revision. Use conflict for an applicable workflow whose proven binding or evidence no longer permits the transition. Use precondition_failed for an immutable content If-Match mismatch. Use payload_too_large for a metadata/rich-view byte budget, range_not_satisfiable for an invalid range, and unavailable for temporarily unreadable proof. Use the common structured error envelope and stable codes. Persist operational failure, retry and unknown delivery states even if a request disconnects. A read does not consume review work. HTTP status communicates admission or retrieval. Durable receipts supply evidence of later transitions.

Use typed application failures so callers choose a recovery action from the cause. `TargetMismatch` carries which authority boundary failed (review, lead, turn or call). `PreconditionChanged` carries the changed field and currently observed revision (head, disposition, monitor, report identity or native position). `IdempotencyConflict` identifies a reused ID with changed content. `EvidenceUnavailable` names the source and whether a fresh read is possible. `InvalidEvidence` indicates source hash, quote, completeness or scoped finding validation. `DeliveryUncertain` carries the existing submission ID for status lookup. The transport maps these to the common codes and sanitized messages. Unknown delivery after admission is a durable receipt the user can inspect. Compare-and-set failures keep the draft and return enough scoped identity for a fresh read.

## Verified actions and dispatch

A native declaration needs a currently working registered lead, matching provider native ID, run, review, turn, active call and **latest complete actual user input**. Match a known user submission to its persisted purpose. Reject an automatic/child prompt, hidden instruction, older entry, pending newer manual send or mismatched Claude companion entry. Re-read the native entry and current open MR after any async evidence read. For a cancel, verify the superseding user input. `comments` and `both` need 1–50 distinct targets keyed by (exact UTF-8 body SHA-256, discussionId or top-level). Identical reply text is valid in two different discussions. Each target needs its own matching GitLab note ID. `approval` and `cancel` carry zero comments. The native lead performs only the authorized remote work. A scoped automatic prompt may never declare new authority.

Drive `working → checking → completed | working | retry_wait | needs_input | cancelled`. Compare each note against the authenticated GitLab user's ID, requested digest, optional discussion and positioned head SHA. Store confirmed note IDs/time. Read approval for that exact authenticated user, independently of other configured reviewer identities or aggregate approval count. Conclusive absence requires complete current evidence. Keep unknown on partial reads, preserve prior confirmations, and show remaining targets. Keep original request identity, currently queued retry and last executed retry distinct. On superseding input, stop, closure or changed head, cancel only this authority and its known-unstarted continuation. An already sent operation can require readback. Another user's approval cannot complete ours.

Use the single HOST dispatcher. In one store transaction, save the action/retry and its exact outbox row, then wake HOST after commit. At claim and immediately before native submission, recheck current input/turn, permission, original SHA, live approval/notes, owner, capability, known completion and pending user work. A fresh read that already confirms the targets completes the action without sending. Continue only genuinely absent targets. At most three automatic sends for the same explicit request, a 10 s base wait and capped doubling for unknown evidence through 80 s. Hold and re-read unknown GitLab outcomes. An uncertain native send enters needs_input and is reconciled by exact submission ID. Inspect halted, uncancelled actions about once a minute so later confirmed success can close them. No automatic action continuation may steer a working turn. The native provider's final cross-client race remains visible through native receipts.

## Findings ledger and report versions

ReviewQ owns the findings of a run. The lead and its subagents file each finding and review note through ReviewQ's tools while they review. ReviewQ stores each one as a record, composes the report text from the records and publishes it as an immutable version. The repository's review instructions own the review method. ReviewQ never reads, parses or requires a report file or the agent's last message, and nothing an agent writes outside a tool call changes a finding. An agent may still write whatever report its instructions ask for. The ledger uses the existing SQLite store and the existing native tool paths, and adds no package.

### Ledger tools

Both providers expose the same six tools from one generated set of input schemas: Claude through the plugin's registered tools and Codex through the ReviewQ MCP server. Every input is one JSON object whose root has no alternative shapes.

| Tool | Caller | Input and effect |
|---|---|---|
| `record_finding` | Lead or subagent | Files or changes one finding. Without `id`, ReviewQ assigns the next unused ID and the input needs `title`, `severity`, `blocking`, `section`, `issue`, `consequence` and `evidence`. With `id` and `expectedRevision`, only the fields given change. Changing a retired finding reopens it. Returns `{id,revision,unpublished}`. |
| `assess_finding` | Lead or subagent | `{id,expectedRevision,outcome,reason,discussionIds?,checkedSha?}` records what became of a finding already on the run. `fixed`, `clarified_withdrawn` and `superseded` retire it. `partly_addressed`, `still_present` and `unverified` keep it active. Reason is one line of 1–1,024 characters and at most 10 discussion IDs apply. Returns `{id,revision,unpublished}`. |
| `record_note` | Lead or subagent | `{section,title?,body,expectedRevision?}` replaces one review note. An `extra` note is identified by its `title`. An empty body removes the note. `expectedRevision` is required once the note exists. Returns `{section,title?,revision,unpublished}`. |
| `review_ledger` | Lead or subagent | Read only. With no input it returns the short list: the current version's receipt, the open pass and what it owes, one line per active finding (ID, revision, severity, blocking, title and author) and one per note (section, revision and length). `{ids}` with at most 10 IDs, or `{sections}`, returns those records in full, and `extra` returns every extra note. A result holds whole records up to 64 KiB, which always fits one, and a `cursor` to pass back for the rest. |
| `finish_review` | Lead | `{verdict?,revision?,fullReview?,assessments?}` closes the review and publishes. See [Publishing a version](#publishing-a-version). |
| `needs_input` | Lead | `{reason}` of 1–1,024 characters records a question the reviewer must answer before the review can finish. It holds the open pass and its scope, completes no assessment and lets the turn end. The pass waits on the reviewer until the lead's next turn starts. From then it is open again and owes its closing call. Without an open pass the call is refused. |

| Finding field | Rule |
|---|---|
| `title` | One line of 1–200 characters. It does not begin with a severity label such as `P1 –`, because severity has its own field. |
| `severity` | `P0`, `P1`, `P2` or `P3`, most severe first. |
| `blocking` | Boolean. Whether the lead holds the merge on this finding. |
| `section` | `functional` or `quality`. Security and correctness concerns are functional. |
| `category` | Optional one-line label of 1–48 characters without `·`, such as Security or Tests. Defaults to the section's name. |
| `locations` | Optional, at most 8 of `{path,line?}`. A path has 1–512 characters. |
| `issue`, `consequence` | Markdown up to 8 KiB each: what is wrong, and why it matters. |
| `evidence` | `{kind,text}`. `observed` states what the agent confirmed. `gap` states what it could not verify. Text is Markdown up to 8 KiB. |
| `proposedComment` | Optional Markdown up to 8 KiB: the comment the lead would post. |
| `details` | Optional Markdown up to 16 KiB for diagrams, tables and longer reasoning. It is shown after the evidence. |

A note's `section` is `title` (one line of 1–200 characters), `summary`, `rationale`, `flow`, `changes`, `threads`, `questions`, `verification`, `publication` or `extra`. `summary` says what the MR does in at most four plain paragraphs, and `rationale` gives the reason for the verdict in one. Neither holds a heading, list, table, quote or code block, and their paragraphs start with plain words: not with bold text, and not with a label the header uses, such as `Scope:`. An `extra` note needs its own one-line `title` of 1–120 characters. The title identifies the note and cannot repeat the heading of a fixed section. A run holds at most 12 extra notes. A note body is Markdown up to 32 KiB.

IDs are `F-01`, `F-02` and so on. ReviewQ assigns them per run and never reuses one, and the same concern keeps its ID across versions. Every record has a revision that starts at 1 and advances with each accepted change. A change names the revision the caller last read. A stale revision is refused with the current revision and the names of the fields that changed, so the caller reads the record again and retries. A user's disposition is not a record change. It advances no revision and refuses no call.

Three rules stop a repeated call from making a duplicate. A repeat of the same native call returns its stored result. A write whose content equals the stored record succeeds without a new revision, whatever revision it names. A new finding matches an active one when their titles and first locations are equal, or their titles are equal and neither has a location. When the rest of its content is equal too, the call returns that finding's ID and revision. Otherwise it is refused with that ID, so the caller changes the finding instead.

Schema and value errors return while the call is open and name each field, capped at 50 errors plus one entry that counts the rest. A refused call changes nothing. No line of a body may exceed 8 KiB, a run holds at most 200 active findings, and one tool input fits 128 KiB. Markdown is stored as written. Each Markdown field must stand alone, which means a heading written after it still reads as a heading. A code fence left open fails this, and so does an HTML block that never ends, such as an unclosed comment or `<pre>`. ReviewQ tests it with the Markdown parser it already uses, never with its own scanner. `issue`, `consequence`, the evidence text and `proposedComment` each begin with a paragraph, so their first words follow the card's label. No paragraph in a finding's field begins with one of the card's bold labels, such as `**Proposed comment:**`, so a reader never takes authored text for a field. A field that breaks a rule is refused with the line where the problem starts. A finding leaves the active set only through a retiring outcome. A finding withdrawn before any version published it is left out of the report and kept in the ledger's history. An outcome's checked revision is its `checkedSha` when given, otherwise the reviewed SHA of the closing call that publishes it, otherwise unknown.

ReviewQ binds every call to its provider, native session, turn, call and calling agent from what the call itself carries. Claude's plugin supplies the session, the active turn, the tool-use ID and the subagent's ID when a subagent calls. Codex's MCP request names the calling thread, call and item, and carries the turn in its turn metadata. A Codex subagent calls from its own thread, which must descend from the registered lead in the same run. ReviewQ verifies that from the parent thread the request names, or from the children it has already discovered. A ledger call reads no transcript and resumes no thread. A lead-only tool is accepted only from the registered lead itself: a Claude call without a subagent ID, or a Codex call from the lead's own thread. A call that cannot be attributed to the lead or a verified child is refused. A write is durable when its transaction commits and the tool result says so. ReviewQ reads nothing back from the provider afterwards. Terminal output, ordinary replies and text that resembles a receipt carry no authority.

### When a closing call is owed

ReviewQ decides when a closing call is owed. The agent never labels a turn. A pass is review work on a run that ReviewQ is waiting to have closed. A run has at most one open pass.

1. Starting a review from ReviewQ opens a pass. Start opens a full-review pass, and a delivered selected-thread batch opens a selected-thread pass.
2. Filing a record on a run that has no published version opens a full-review pass, because a first version needs a verdict and a reviewed revision.
3. Nothing else opens a pass. This covers a message the reviewer types or sends from the dashboard, report feedback and a publication action.

An open pass owes one closing call from the lead: `finish_review`, or `needs_input` to hold it. It owes that call at the end of every lead turn until it is closed, whoever started the turn. While a pass is open, only its closing call publishes.

| Lead turn | Owed | State when the turn ends |
|---|---|---|
| Any turn while a pass is open: a review ReviewQ started, a first review the reviewer typed, or a later chat turn before the pass is closed | A closing call | Closed: the version or batch result is published. Not closed: the [gate](#rule-read-back-and-gate) asks, then the review shows as unfinished. Its records stay unpublished until the pass closes. |
| No open pass, and the turn changes a record on a run with a version | Nothing | ReviewQ publishes an update that carries the parent's verdict, reviewed revision and coverage. `finish_review` in the turn can state a new verdict or revision, and with `fullReview:true` makes it a full review. |
| No open pass, and the turn changes no record | Nothing | No version, no failure and no notice. On a run with no version, the finished lead needs a decision. |

ReviewQ settles a run from its state, never from a single event. It checks when the lead or a child becomes idle, when a record is written while they are idle, when a recovery ends and at startup. A run is settled when its lead and every known child are idle and no recovery is pending. A settled run with unpublished records and no open pass gets an update. A settled run with an open pass that is not waiting on the reviewer is unfinished. The stage classifier and this check read the same idle evidence.

An update has no open call to return an error to. One that cannot be published, because its text would exceed the size limit or the records kept changing, leaves every record in place, shows `failed` with its reason and is tried again the next time the run settles.

An unfinished review's delivery is `failed`, with a reason that says the lead ended without finishing and how many findings are on file. The previous version stays readable. A first review shows the explicit empty state, which is distinct from a published review with zero findings. The filed records stay in the ledger and the next closing call publishes them. A turn that ends early through an interrupt or a provider failure leaves the pass open and follows the session recovery rules first. A pass ends when it is closed, when the successor of an ended selected-thread attempt takes over its records, or when the run is archived. Any later turn of the same lead can close it.

ReviewQ records a failure once. It writes one row and one notification when an unfinished review, a refused closing call or a failed update is first seen for its pass or run. Seeing the same failure again updates its reason in place and raises nothing.

Delivery describes the result of publishing. It is absent before any version or failure, `saved` when the current version is stored and nothing has failed since, and `failed` for an unfinished review, a refused closing call or an update that could not be published. Filing records does not change delivery, so Findings shows the previous version without a notice while a review is in progress.

### Rule, read-back and gate

**Rule.** ReviewQ states one short rule to the model. File each finding with `record_finding` when it is confirmed. Record what became of an earlier finding with `assess_finding`. Put review context in notes. Subagents may file, and the lead closes with `finish_review` or `needs_input`. ReviewQ shows only what is filed. Every launch path appends the rule after the original invocation and user context. A terminal link returns it as text the model reads, never as a display-only notice. A selected-thread prompt repeats it with the frozen discussions. The rule confers no external-write authority.

**Read-back.** When the host compacts the lead's conversation, a `SessionStart` hook for the `compact` source returns the rule and the short list from `review_ledger`. The text holds at most 4,000 characters: the rule, the open pass and what it owes, then one line per active finding with its ID, revision, severity, blocking value and a title cut to 80 characters, newest change first, and a count of any lines left out. It reaches the lead only. A subagent reads the ledger with `review_ledger`.

**Gate.** When any lead turn is about to end normally with an open pass that is not waiting on the reviewer, the host's stop hook asks ReviewQ. ReviewQ answers with a block and a reason that names what is owed and what is on file. ReviewQ counts the blocks of one stop sequence and allows two. A stop request that follows no block starts a new count, and the third request of a sequence lets the turn end. It never blocks a subagent's turn, a turn that owes nothing, a turn in which `needs_input` was accepted, or a turn with a pending blocking question or permission. Detecting an unfinished review does not depend on the gate.

**Host hooks.** Claude uses the plugin's tool handlers for the six tools, its turn-complete event as the turn-end signal, its `Stop` hook as the gate and a `SessionStart` hook for read-back. Codex uses the MCP server for the tools and two user hooks, `Stop` and `SessionStart`. A Codex turn end is also read from its rollout. Codex runs a hook only after the user reviews and trusts it, and skips an untrusted hook without notice. ReviewQ never writes that trust state. The stop hook reports to ReviewQ when it runs. The installer and `reviewq link` say so while it has never reported on this installation, which usually means it still awaits the user's trust. Without the Codex hooks a review still files, closes and shows as unfinished when it should. Only the gate and the read-back are missing.

### Publishing a version

The lead's `finish_review` publishes a version, and so does ReviewQ for an update that owes nothing.

`finish_review` takes a verdict (approve, request_changes, blocked or not_specified) and a revision, which is an exact full SHA or `{sha:null,reason}`. Both are required for a first version and for a full review. On a run with no version the call is a full review whether or not a pass is open, so a review with no findings can be published. In a selected-thread pass ReviewQ binds the frozen batch's checked SHA itself, and the verdict defaults to the current version's. With no open pass on a run with a version, each defaults to the current version's value, and the call publishes an update, or a full review when `fullReview` is true. `fullReview` is refused in a selected-thread pass. `assessments` belongs to a selected-thread pass, holds at most 10 entries, and each supplies discussionId, conclusion, reason, findingIds and nextAction `review` or `waiting_author`. These checks run while the call is open and return field errors the lead corrects in the same turn:

- A full review leaves an active finding of the last version neither changed nor assessed in this pass. The error names each ID and title, and the lead answers with `assess_finding`.
- A selected-thread pass lacks exactly one assessment per selected discussion covering that discussion's finding IDs, states a revision other than the frozen batch's checked SHA, or links an outcome to a discussion outside the batch.
- Assessments arrive without a selected-thread pass.
- The composed text would exceed 4 MiB. The error names the largest records.

The call is refused, with every record kept, only when the workspace, repository grant, settings authority or follow-up binding changed, or the run's registered lead changed. A repeated `finish_review` with nothing changed returns the acknowledgement of the lead's last closing call. This includes a repeat of the assessments that closed a selected-thread pass. The acknowledgement is `{receiptId,sha256,result,new,updated,retired,carried,bySubagents,emptySections}`: the IDs in each state, the count of carried findings, the IDs that subagents filed or changed, and which of the `summary`, `rationale`, `flow`, `changes` and `verification` notes are missing.

Publishing reads every record at its current revision together with the run's ledger sequence, composes the text outside SQLite and commits in one conditional transaction. The transaction checks the ledger sequence, the disposition revision, the current receipt, run membership and the frozen batch if present. It appends the version, seals the text in chunks of at most 64 KiB with its SHA-256 and size statistics, stores one row per finding and section with its record revision and scalar offsets, and advances the current pointer by compare-and-set. In the same commit it binds the disposition revisions, closes the pass, saves the batch completion and covered observations, resolves earlier failures and inserts the due notification. A changed ledger sequence or disposition revision recomposes, at most three times, then returns a retryable error. Invalidations and the dispatch wake follow the commit. The text is composed once and stored. An earlier version is never composed again.

| State | Rule |
|---|---|
| New | An active finding that the parent version lacks. |
| Updated | A parent finding whose revision advanced, with or without a keeping outcome. |
| Retired | A parent finding with a retiring outcome. It leaves the active set, counts and blocking, and its outcome stays on the version. |
| Carried | A parent finding at the same revision. It stays active with its fields, details and dispositions. |

| Version | Continuity |
|---|---|
| Full review | Establishes a new complete-MR baseline at the stated reviewed SHA, or keeps the unknown reason. It always publishes a version. Registration's requested SHA, the current head, the verdict and GitLab approval are independent facts. |
| Selected threads | Binds FOLLOW's exact batch, frozen submission, monitor generation, registered lead, baseline receipt and hash, checked source and target, every observation ID and version, and lineage. When the pass retires every finding a discussion tracks, that discussion's assessment must be retiring. When it retires only some, the assessment must be retiring or `partly_addressed`. Untouched findings are carried. When no record changed in the pass and the verdict equals the current version's, ReviewQ records the batch result as `unchanged` against that version and adds no version. Otherwise the result is `final`, with the batch's checked SHA as its reviewed revision. |
| Update | Carries the parent's verdict, reviewed revision and coverage unless `finish_review` states new ones. Untouched findings and notes are carried. It completes no batch. |

Only a full review advances fullMrReviewedSha. A full review and a selected-thread result raise their notification. An update takes a new attention identity and raises the ready notification only when its active findings, blocking values or verdict differ from its parent's. Otherwise it keeps the parent's attentionReceiptId. Publishing binds the configured project, checkout and GitLab identity. An unrelated board or automation edit can proceed while a pass is open.

### Composed report layout

Every version has the same layout, which ReviewQ writes from the records. The reader, History, quotes and downloads work on this stored text.

```markdown
[{project path} · !{MR IID}]({MR URL})

# {title}

{summary}

**{APPROVE | REQUEST CHANGES | BLOCKED}**

{rationale}

P0: {n} · P1: {n} · P2: {n} · P3: {n} · Blocking: {n} · Nonblocking: {n}

**Scope:** {Full review | Selected threads | Update}

**Reviewed source:** {SHA, or unknown with its reason}

**Reviewed target:** {SHA or unknown}

**Checked at:** {UTC time}

## Principal code / data flow
## What changes across the MR
## Our previous threads

| Finding | Outcome and verification | Thread or evidence |
| --- | --- | --- |
| F-03 · {title} | **Fixed.** {reason} | {discussions and checked revision} |

## Functional findings

### F-01 · {title}

**P1 · {category} · Blocking**

**Location:** `{path:line}`, …

**Issue:** {issue}

**Why it matters:** {consequence}

**Confirmed:** {evidence}

{details}

**Proposed comment:** {proposedComment}

## Quality findings
## Open questions and decisions
## Verification and limits
## {each extra note's title}
## Publication and dispositions
```

- The first line names the project and MR and links to the MR. The title is the `title` note, or the MR title when there is none. `summary` and `rationale` appear only when recorded.
- The verdict line states the version's verdict in capitals and is left out for `not_specified`. The counts line counts the active findings by severity and by effective blocking value. The four cover lines come from ReviewQ's own facts for the version.
- The eight fixed sections always appear in this order. `flow`, `changes`, `questions` and `verification` fill their sections. An empty section says `Not recorded.`, `No earlier threads.`, `No functional findings.`, `No quality findings.`, `No open questions.` or `No publication or disposition changes recorded.` as fits.
- Cards appear in ID order within their section. The card line states the severity, the category, and `Blocking` or `Nonblocking` from the effective blocking value at publish: the user's disposition when one exists, otherwise the lead's. Locations are written as code. `Location`, the details and `Proposed comment` appear only when given, and `Proposed comment` is always the last field.
- `Confirmed` holds observed evidence. A gap is written under `Evidence` instead and begins `Not confirmed.`
- `Our previous threads` holds the table above with one row per outcome recorded on this version, then the `threads` note. The Finding cell starts with the ID. The outcome cell starts with the outcome in bold (`Fixed`, `Clarified and withdrawn`, `Superseded`, `Partly addressed`, `Still present` or `Unverified`), followed by its reason. The last cell lists the linked discussions and the checked revision. ReviewQ escapes `|` in a cell. A retired finding has a row and no card. Its last card stays readable in the earlier version.
- Extra notes follow `Verification and limits` as level-two sections in the order first filed.
- `Publication and dispositions` holds one line per active finding that has a user disposition or a monitored discussion link at the version's starting disposition revision, then the `publication` note.
- Headings inside an authored field, ATX or setext, are written as ATX headings shifted so that the shallowest is level four, with deeper levels stopping at six. Together with the stand-alone rule, no authored text can open or swallow a section or a card.
- Raw HTML is shown as text. Only `<br>` keeps its meaning, as a line break.
- Apart from the heading shift, ReviewQ writes authored lines unchanged and puts a blank line between blocks. It records each card's and section's scalar offset and length as it writes them.

### Worked ledger calls

On a run with no version, the lead files one finding. The call needs no ID.

```json
{"title":"Retry writes the same invoice twice","severity":"P1","blocking":true,"section":"functional","locations":[{"path":"src/invoices.rs","line":74}],"issue":"The retry builds a fresh idempotency key after the first insert commits.","consequence":"A lost acknowledgement creates a duplicate invoice.","evidence":{"kind":"observed","text":"Replaying the request after a dropped reply inserted two rows."}}
```

ReviewQ assigns the ID and opens a full-review pass.

```json
{"id":"F-01","revision":1,"unpublished":1}
```

The lead then closes the review.

```json
{"verdict":"request_changes","revision":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}
```

With synthetic receipt ID `receipt-rq41`, ReviewQ publishes the first version and answers with the digest of the composed text.

```json
{"receiptId":"receipt-rq41","sha256":"<SHA-256 of the composed text>","result":"final","new":["F-01"],"updated":[],"retired":[],"carried":0,"bySubagents":[],"emptySections":["summary","rationale","flow","changes","verification"]}
```

The functional section of that version reads as follows.

```markdown
## Functional findings

### F-01 · Retry writes the same invoice twice

**P1 · Functional · Blocking**

**Location:** `src/invoices.rs:74`

**Issue:** The retry builds a fresh idempotency key after the first insert commits.

**Why it matters:** A lost acknowledgement creates a duplicate invoice.

**Confirmed:** Replaying the request after a dropped reply inserted two rows.
```

A later chat turn in which the lead calls `assess_finding` with `{"id":"F-01","expectedRevision":1,"outcome":"fixed","reason":"The retry now reuses the stored key."}` owes nothing. When that turn ends, ReviewQ publishes an update in which F-01 is retired and listed under `Our previous threads`.

## Versions, dispositions and feedback

The user's latest disposition overlays the lead's blocking value on every version and always wins. For new versions record immutable per-finding disposition changes by run and revision. Bind a version to its starting disposition revision and freeze its final visible disposition revision when superseded, in the same commit as the new head. Current reads use the live revision. Historical reads reconstruct the last view of that receipt. Feedback locates the exact selected bytes/quote, including an older version and Unicode. Both the rich Findings reader and the bounded source reader can supply quotes. Match the submitted quote exactly at the specified scalar offset and length against the named immutable version. A repeated literal quote requires its reference to identify a particular occurrence. For rendered text spanning markup or decoded entities, offer the exact underlying source for confirmation or reselection, preserving the draft and pinned version. Whole-report feedback remains available. Never manufacture a source range from synthetic headings, decorated labels or rendered HTML. The report viewing checkpoint, decision checkpoint, batch assessment viewing checkpoint and notification receipts advance independently.

Findings continues to show the current version when a later review is unfinished or its closing call is refused. Its History control identifies the failed update and opens the reason alongside the latest version's date and checked revision. An unfinished first review leaves an explicit empty state. A failure never advances the current version. Closing the pass, or publishing any later version or batch result on the run, resolves every earlier failed update on the run, so the History control and the run card stop reporting a failure. A failure recorded after the pass it names was closed is resolved at once. The failure's record and its notification are kept; History shows only an unresolved failure. A failure from a later pass stays unresolved, and a question resolves nothing.

### Report size and indexing

ReviewQ writes the text, so it knows where every card and section starts. Publishing needs no parse of the whole document, and every version has a complete finding and section index. Counts and card-level operations come from the version's finding rows. The record limits keep an ordinary review inside the rich view. The reader's display index is built from the stored text and stays optional, as [Indexed Markdown for Findings](../plans/05-browser/indexed-findings.md) describes. The expected cost is one short write transaction per ledger call and one composition per version. Measure both on the worked review set during REPORT.

### Reading large reports

Compute byteLength in UTF-8, totalCodePoints in Unicode scalars, and physical line statistics when composing a version, then persist them with its receipt. Count LF boundaries (consume CRLF as one delimiter), exclude line terminators from maxLineBytes and do not count a trailing empty line after terminal LF. Return metadata first. Serve `view=markdown` only when byteLength ≤1 MiB, lineCount ≤10,000 and maxLineBytes ≤16 KiB. An over-budget explicit rich request returns payload_too_large with zero Markdown bytes. The summary already provides the metadata and immutable identity needed for section and download routes. A published version keeps every composed byte.

For larger versions, or on rich parser/AST-budget exhaustion, open a paged plain Markdown source reader pinned to the version. Use at most 12,000 code points per page, choose `max(0,totalCodePoints-12000)` for Last, and provide previous, next and explicit offset navigation. Quote feedback can refer to global source offsets spanning visible source text. STORE supplies immutable chunks up to 64 KiB and indexed source offsets. End each short read snapshot before waiting on the network. Offer the complete text as a streamed native download without buffering the entire artifact into a JavaScript Blob or retaining it in browser cache. Keep summaries and notification invalidations free of report bodies.

## Selected-thread scheduling

Global default off, per-MR enabled/disabled overrides win. Monitor states are enabled, paused and archived. A complete authorized current MR read and stable reviewer identities are prerequisites. Choose the lead of the latest published version, or the sole registered lead when no version exists. Multiple possible leads without reports require a selection. Preserve assignment across navigation and new reports. Initial enrollment uses actual published notes by the configured reviewer identities. Exclusions follow original note IDs if GitLab promotes an individual comment into a discussion. Require a unique identity match. A matching filename, similar body, resolved badge or private finding cannot establish a thread.

| Event or decision | Recorded work |
|---|---|
| Initial full snapshot or new enrollment | Store baseline of note IDs/actors/body hashes and resolution. Adopt only eligible outstanding QUEUE events on selected discussions after the handled boundary, recording adoption with the observation in the same transaction. An already resolved thread starts resolved. If a new review note and subsequent external reply first appear together in a later complete sync, enroll the note and capture its reply once. |
| Reply, edited external body, resolution or reopen | Compare exact selected discussion and known original IDs on a complete snapshot. Record observation ID, discussion version/fingerprint, action kinds, actors where known, note and finding IDs. Exclude own identified actions, system notes and timestamp-only changes. Incomplete reads or ambiguous/missing originals cannot supply negative evidence. Commit-only changes, unrelated feedback and old read/dismiss history never schedule by themselves. Polling cannot reconstruct an event wholly between two snapshots. |
| Configure, pause, resume, review-now | Observe retained selections before editing. Advance generation for a meaningful edit. Carry remaining unstarted observations into the new generation, retain exclusions, and validate every selected reviewer note. Pause holds known pending work. Resume requires fresh MR and approval state. Manual Check Now admits all selected available concerns at their observed versions immediately, even with no new reply. An unavailable selected thread or empty selection returns a named reason. Bulk configure/pause uses a per-target preview and exact per-target commands, reporting skipped/failed targets separately. |
| Approval, closure or loss of identity | A confirmed approval by a configured monitored reviewer pauses automatic work. Unrelated approvals have no effect. Unknown approval holds. Preserve pending/uncertain delivery for exact reconciliation, cancel proven-unstarted ineligible work. Already executing private work may deliver its frozen result. Merge/close archives the monitor, reopening requires explicit resume. A changed reviewer identity requires revalidation. |
| Due work | Set due time to min(last observation + 60 s, first + 180 s). Run a coalesced due pass at startup, after relevant commits and about every 10 s. One active/reserved attempt per MR. Respect the 1–8 global active cap, default 2. Distinguish active native execution/reservation from a confirmed idle question-held or ended run. Retain exact binding for a later answer even when freeing its global slot. Unknown provider state and uncertain submission still reserve capacity. An unfinished selected-thread pass keeps its binding and frees its slot. |
| Freeze and deliver | Fetch fresh MR and recheck saved Settings revision, open/unapproved state, monitor, recipient, actual thread availability and HOST eligibility. Freeze bounded groups (start with 10 observations, maximum 12,000-character prompt, carry unsent remainder). Commit exact members, checked head, target SHA if actually known, parent/disposition version, submission ID, private prompt and outbox together. Wake HOST after commit. A reservation is pending. Only an exact accepted native turn changes its state to executing. A new event after freeze goes to a successor. |
| Finish, stop, recover | Complete only the exact executing batch when REPORT accepts its closing call. Needs-input holds its scope and claims zero completed assessments. Unknown send stays owned while exact native history is inspected. If a known-unstarted submission is rejected release it and requeue only still-eligible members. Explicit stop pauses monitoring. An attempt that ended without its closing call, or whose closing call was refused because its frozen scope went stale, keeps its filed records in the ledger and shows as unfinished. A later turn of the same lead can still close it with the batch's assessments while its frozen scope is current. A successor carrying the original observations and lineage can reconcile it. Mark the earlier attempt reconciled only once completed groups cover every original observation ID, version and discussion. A turn that supplies no assessments never consumes a batch. |

For example, suppose a monitor links original note 91 in discussion D7 to finding F-01. A reply creates observation O4 at discussion version 3. The current version also contains F-02 under an explicit nonblocking disposition. A check that changes no record closes with `finish_review` and the following input.

```json
{"assessments":[{"discussionId":"D7","conclusion":"still_present","reason":"The selected guard still permits an empty list. The original note already asks for that guard.","findingIds":["F-01"],"nextAction":"waiting_author"}]}
```

The server binds O4/version 3 and the checked source SHA from the frozen batch. The lead never supplies an observation or turn. ReviewQ sees that no record changed, so the result is `unchanged`: one completed batch and zero report versions. F-02 remains an outstanding card with its earlier checked revision and disposition. A later reply creating O5 remains eligible even if it arrives before the first result is viewed.

Every automatic delivery checks pending questions/permissions, busy related sessions, an open full-review pass on the run, manual send/unknown receipt, provider readiness, preserved native model/effort and current eligibility at claim and send. Native model/permission recovery uses HOST's same outbox and only legal capacity evidence. It can extend an existing batch to the exact acknowledged resumed turn or an unambiguous same-session Codex root turn in the original executing lineage. A prior permission to publish never broadens a private check. Threads are evidence. The prompt authorizes reading and drafting only. Closing the drawer, viewing a result or losing the HTTP response cannot reauthorize a send.

## Verification

Use the fixture publisher and controlled host adapter first. For live dispatch exercise the ordinary foreground service with synthetic provider sources, a task-owned signed-in native host and a disposable profile as in SLICE. Validation mode disables sends and serves read-only inspection. Acceptance FIND-1, SEND-1 and WORK-1 name the shared readbacks. The [ACTION](../plans/04-workflow/actions.md#evidence), [REPORT](../plans/04-workflow/reports.md#evidence) and [FOLLOW](../plans/04-workflow/followups.md#evidence) briefs supply the exact positive postconditions, collision cases and evidence owners. Compare persisted receipt/ID, published text digest, record revisions, reviewed revision and provider-native call identity. A green simulated host alone establishes fixture behavior only. [Acceptance](acceptance.md)
