# FIXTURE · Make a review another developer can open

**Outcome.** One command generates a portable review workspace with known code, conversations and expected results. The first runnable review and release qualification use the same inputs. A reviewer can derive the expected text and routing independently of the browser or database. FIXTURE is ready independently of CONTRACT. The lead schedules it within the checkout limit.

**Ownership.** One `gpt-6.2-sol` worker at `xhigh` owns the synthetic inputs and fixture driver. A separate `gpt-6.2-astra` reviewer at `xhigh` regenerates and inspects the expected bytes. FIXTURE can close independently. SLICE checks compatibility with the integrated service contracts.

## Source material

Keep the input manifest and small source examples under `fixtures/reviews/`. Use synthetic text, fixed UTC timestamps, LF and UTF-8. Each preset writes a versioned manifest with relative paths, byte lengths and SHA-256 digests. Generate long bodies locally and keep the timeline of actions separate from the initial data. The generated inputs can travel to a second host without access to the original checkout.

Run `tools/dev/seed-review --preset worked --out <new-directory>` with Python 3.8 or later. The parent must exist. The generator creates the destination and refuses any existing path. It prints the output path and manifest SHA-256.

The version-1 source inputs live in [`fixtures/reviews/worked/`](../../../fixtures/reviews/worked/). The generator copies `reviews.json`, `sessions.json`, `discussions.json` and `timeline.json`, generates `quiet.patch`, `active.patch`, `large.patch` and `history.json`, then writes `manifest.json`. The worked manifest digest is `7aff00aad90e93bbb4abc2297c1305077696786fce28c2f3122f3fb45e92b313`. It indexes eight inputs containing 12 review rows, five session aliases and 101 initial conversation observations. All paths are relative to the export. The independently indexed `provider/` directory contains GitLab and Jira source records, native samples, raw documents and expected identities. Its own version-1 manifest covers all companion bytes. The worked baseline remains nine root files. The patch hashes cover the raw UTF-8 patch bodies. GitLab discussion wrappers, response headers and encoded transfer size have separate measurements.

### Worked review set

| Review | Inputs | Independent expectations |
| --- | --- | --- |
| Active, `forge/alpha!41`, ticket `RQ-41` | Working Codex lead, a child and an earlier completed run. The current file is `src/example.ts`, 1,200 additions. Start with current inline discussions at new lines 8 and 600. | New line 820 reads `export const example819 = 819;`. During reading, publish a same-revision update with identical data, then insert a thread at 1060, then one at 12. The target stays at its reading offset. Native Find reaches `example1199 = 1199`. |
| Quiet, `forge/beta!41`, ticket `RQ-42` | Different project, same displayed IID and similar MR title. Completed run, 20 additions, short history, two published report versions, no pending activity or live output. | Exactly 20 added lines. The last line contains `example19 = 19`. The run, report and draft differ from Active. Reading this review never starts a turn. |
| Large, `forge/alpha!43`, ticket `RQ-43` | 12,000 additions in `src/example.ts`, discussions at 8 and 600, a separate mixed/wrapped file and a long read-only history. | One selected patch with 12,000 code lines plus the hunk header. Find reaches `example11999 = 11999` before visiting the last screen. A switch to the mixed file transfers only that file's patch. |

| Patch | Raw bytes | SHA-256 | Zero-based byte ranges for added lines, excluding LF |
| --- | --- | --- | --- |
| `quiet.patch` | 596 | `d2beb4febf98c5a723cb01ce717537855da75c23ce883e4a3ff0f68f318782f5` | Line 20 `[567, 596)` |
| `active.patch` | 38,598 | `860e21b1c8ecc9c3fc758c0bd9ebbaf0dd4ca7a2af18af71e0871e5beec5d8d9` | Line 820 `[26007, 26038)`, line 1200 `[38565, 38598)` |
| `large.patch` | 409,799 | `28301b2f3985845ed768817514257a5334d3af9c1f2cfba98a00450c4b9fc0f3` | Line 12000 `[409764, 409799)` |

The ranges start at the patch's `+` prefix. Each patch has one hunk header and ends at the final line's semicolon. The worked review aliases `review-0`, `review-1` and `review-2` use project ID/IID pairs `(11, 41)`, `(22, 41)` and `(11, 43)`. Row index `i` uses `10 + i` for head, `1000 + i` for base and `2000 + i` for start, each zero-padded to 40 hexadecimal digits. Active's initial markers run from `Update 0` through `Update 99` with corresponding `active-item-0`…`active-item-99` and `active-turn-0`…`active-turn-99`. Quiet's sole observation is `quiet-item-0` in `quiet-turn-0`, with text `Quiet complete`. The timeline adds `active-item-100` (`Live message 1`), closes the stream, then adds `active-item-101` (`Live message 2`). The initial outbox is empty.

### Provider replay and release profiles

The worked export supplies the source observations and cues. The provider driver maps them to real revision-bound thread positions and provider-native shapes. It also supplies Large's mixed file and long history. Report versions come from the product's ledger, as [Publish the reports](#publish-the-reports) describes. To trigger an interruption, use controls on the driver's listener and the actual browser reconnect path.

Bind each worked review to a distinct GitLab instance/project ID/IID, revision triple, path, ReviewQ run, native root/session, turn/item and report receipt. Deliver both additional items to Active. For the real native exercise, register actual host IDs, verify delivery from native evidence and record their mapping to the synthetic review aliases in private output.

The **measurement profile** projects these inputs into 12 queue MRs, exactly one working registered lead with 100 entries, empty outbox and one selected file with discussions at 8 and 600. For each fresh launch pick either the byte-exact 1,200- or 12,000-line patch for that file. The production shell, Files/All conversations controls, inline open/close, working Agent, live append/reconnect, draft, send-once and return must all operate. Use the populated three-review profile for switching and physical desktop scenarios. Supply both profiles without a resident service or connection per review.

| Preset | Additional data and exercise |
| --- | --- |
| `empty` / `worked` | Fresh profile, then the 12-row set above. Supply fixture provider reads to the actual application adapters and a small scrub-free native source example per supported host. Preserve source bytes independently of decoded receipts. |
| `busy` | 67 MRs, 506 session registrations across projects and 200 outbox rows. Include two different MRs per ticket, two runs on one MR, an eligible ticket `RQ-641` with no open MR and two closed MRs with neutral titles/descriptions and source branches `feature/RQ-641-fix` and `feature/RQ-6410-fix` (only the first matches), a pending input, malformed or missing optional data, and duplicate event delivery. Exercise scoped queue/detail reads. A secondary 300-MR variation uses 15 shared-prefix titles and 200 messages of 12,000 ASCII characters each to catch an aggregate transfer during typing/filtering. |
| `reader` | Build upon the three worked reviews. Add 10,000 ordered native items with deterministic user prompts every 25 items, paired tool starts/results, long output of 320 KiB, two equal prompts with distinct native IDs, and independently identifiable first/middle/last messages. Include a child and a second lead. Publish a known composed report from fixture ledger records with a quote, diagram and receipt, and a changed version with a new digest. |
| `rich` | Alongside the byte-exact additions, supply context, insert/delete hunks, renamed file, no-final-newline marker, empty/binary/provider-limited files, long hard-wrapping Unicode, bidi and tabs. Supply old/new side anchors, an earlier-revision comment, a general thread, 200 discussions spread over the mixed file, a long expanded reply with a diagram, and a changed head. Include a Jira v2 description and comment with a wiki heading, code, table, list, labeled link and unknown macro. Include a report selection spanning `**one** &amp; two 🧪` with its raw source coordinates and different visible text. Include expected clipboard bytes and native accessible reading order for a short known selection. |

For rich row windows and explicit source readers add one patch or report crossing each independent [full-document limit](../../specs/browser-experience.md#full-document-and-bounded-source-modes), including a CRLF-terminated line of exactly 16 KiB of content (eligible on the line-length criterion), a single very long Unicode line, an oversized report/diagram and a long body whose final range contains a unique marker. Write the expected final bytes/digest in the source manifest. Supply a transcript where selection/focus pins the requested six pages or byte budget and an incoming seventh page can be triggered explicitly.

Expose the fixture through a loopback-only provider driver, using the same Rust read paths and SQL writes as ordinary operation. Support ordered release of the selected response, a forced stream disconnect, observer counts, an external-authority error and a terminal submission whose response is lost after acceptance. Keep controls in the harness. The application must have a single read/send path in a fixture session. Provide visual specimens from these data for wide, narrow, wrapped and reduced-motion review.

## Run the provider

Run `tools/dev/fixture-provider --inputs "$new_directory"` in the foreground. It verifies both byte manifests, binds an available `127.0.0.1` port and prints one JSON readiness line with `gitlab`, `jira` and `controls` URLs. Set `site` and `controls` to the printed GitLab and controls values. `--port` selects a specific loopback port and Ctrl+C stops the listener. The driver uses Python's standard library and needs only the generated export.

Use `--profile populated` for the three-review workflow. `--profile measurement-active` and `--profile measurement-large` are available with `worked`. Both expose the same 12 MRs, Active lead, 100 initial entries, empty outbox, one selected file and discussions at 8 and 600. They select the 38,598- or 409,799-byte patch for Active.

Point the production GitLab adapter at `gitlab` and Jira at `jira`. The GitLab read endpoints are `/api/v4/user`, `/api/v4/projects`, `/api/v4/merge_requests` and `/api/v4/projects/{id}/merge_requests/{iid}` with `diffs`, `discussions`, `versions`, `approvals` and `approval_state`. GitLab lists honor `page` and `per_page` and return its pagination headers. For example `/api/v4/projects/11/merge_requests/43/diffs?unidiff=true&per_page=1&page=2` returns the mixed file alone. Jira exposes v2 issue/issue comment/search and Agile board/sprint reads. Its fixture search supports the preset's project, sprint and exact issue-key scopes. [GitLab MR API](https://docs.gitlab.com/api/merge_requests/) · [GitLab discussions](https://docs.gitlab.com/api/discussions/) · [Jira REST v2](https://developer.atlassian.com/cloud/jira/platform/rest/v2/api-group-issues/)

The harness exposes registration intents at `GET /fixture/registrations` and queued intents at `GET /fixture/outbox`. The isolated launcher applies those intents through normal application registration, receipt and persistence paths. Native replay reads `GET /fixture/native/{alias}` for original NDJSON followed by new observations. `GET /fixture/native/{alias}/stream` streams that lead's live records. `GET /fixture/events` streams all harness changes. Both accept `after` and `Last-Event-ID`. Harness events describe which provider data changed. Use product routes and native host evidence for the terminal-to-browser qualification. [SLICE](first-review.md) · [Codex App Server](https://developers.openai.com/codex/app-server/) · [Claude transcript hooks](https://code.claude.com/docs/en/hooks)

### Operator controls

Read `"$controls/status"` for the timeline position, upstream request counts, held responses, active observers and the number of accepted synthetic submissions. Send controls as JSON with `Content-Type: application/json` to `"$controls/control"`. These requests use the printed loopback Host and a local tool such as curl.

| Control JSON | Effect |
| --- | --- |
| `{"action":"step"}` | Advances one cue from `GET /fixture/timeline`. Worked publishes an unchanged revision, adds discussions at 1060 and 12, appends item 100, disconnects the harness streams and appends item 101. Busy adds duplicate event delivery. Reader and Rich append 100 further entries at the seventh-page cue. Rich then changes the head and adds a new revision anchor while retaining the raw patch bytes. |
| `{"action":"hold","id":"quiet","path":"/api/v4/projects/22/merge_requests/41/diffs"}` | Holds the next matching completed response up to 20 seconds. Use a full path and query to match one exact page. The status reports `waiting: true` after capture. |
| `{"action":"release","id":"quiet"}` | Releases the captured response. Capture Active and Quiet responses, release Active first, then Quiet to exercise late delivery. |
| `{"action":"fail-next","path":"/api/v4/projects/11/merge_requests/41/approval_state","status":403}` | Returns one external authority error on the requested route. The supported statuses are 401, 403, 429 and 503. |
| `{"action":"disconnect"}` | Ends live harness streams. Exercise browser disconnect with its actual event connection and ordinary reconnect in SLICE. |
| `{"action":"drop-next-ack","lead":"active"}` | Accepts the next synthetic Codex input and closes that request before sending its response. Read the receipt and exact native item, then replay the same intent to verify one acceptance. |

For the acknowledgement exercise, obtain Active's `native_id` from registrations. POST to `/fixture/native/active/messages` with `threadId`, an unused UUID `clientUserMessageId` and `input: [{"type":"text","text":"Fixture message"}]`. Text accepts up to 12,000 Unicode scalar values, including JSON-escaped text. The connection ends without a response after acceptance. Read `GET /fixture/native/active/messages/{submissionId}` to compare `threadId`, `turnId`, `itemId` and `submissionId` against the matching native `item/completed` event. Its user-message correlation field is `clientId`. A replay of the same request returns the same receipt. Changed text with that UUID returns 409.

## Publish the reports

The export holds no report text. `provider/ledger.json` lists, per Claude lead, the ledger calls that file its records and close each version, and the dispositions saved between versions. Run `tools/dev/publish-reviews --inputs "$new_directory" --root <new-directory>` from the checkout. It builds `reviewq`, starts the provider and `reviewq serve --validation` in `<root>/profile` with a synthetic Claude profile in `<root>/claude`, links each lead with the plugin's link helper, and sends every call through `reviewq internal claude-bridge ledger`, the plugin's client of the native ledger routes. A disposition goes through the browser's disposition route. The product composes and stores every version. The tool reads each one back through the summary, source download and section routes, checks the summary and ETag against the stored bytes, and compares the sizes and marker offsets with the values pinned in `tools/dev/review_fixture/ledger.py`. It prints the profile, runs and receipts as JSON and exits 1 on any difference. `--serve` keeps the service until Ctrl+C. Afterwards `pnpm --dir web run dev --profile <root>/profile --validation` opens the populated profile.

Five-digit provider ports keep the MR link, and so the byte offsets, fixed. Receipts, SHA-256 digests and the checked time differ on every run, so the manifest pins measurements, not digests. Every preset except `empty` publishes Quiet's two versions as full reviews. After a disposition on F-02 the second changes F-01, assesses F-01, F-02 and F-03, settles F-03 and adds F-04, because a full review must assess every finding it does not change. Together they file the records of the worked ledger example in `crates/reviewq-app/src/ledger/compose/tests.rs`, and the second version matches [`fixtures/reviews/ledger/worked.md`](../../../fixtures/reviews/ledger/worked.md) apart from the GitLab address in every link, revisions and time and five differences. `Reviewed target` is `unknown`, because ReviewQ knows a target commit only for a selected-thread check. A validation-mode service stores no Jira site, linked ticket or monitored discussion link, so the first line ends after the MR link, F-01's row under `Our previous threads` shows `D9` as code instead of a `Thread` link, F-01's proposed comment opens `Proposed comment — new comment at invoices.rs:74:` instead of a reply in the thread, and `Publication and dispositions` has no `Discussions:` line. `reader` adds the `reader-report` lead and `rich` adds it and the specimen leads, all on Large.

## Handoff

Run the worked command twice into separate directories and compare the manifest and all input bytes. To verify an export on any host with Python 3, set `new_directory` to the exported directory and run this checker. It reads only the export.

```sh
python3 -I - "$new_directory" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
manifest = (root / "manifest.json").read_bytes()
assert hashlib.sha256(manifest).hexdigest() == "7aff00aad90e93bbb4abc2297c1305077696786fce28c2f3122f3fb45e92b313"
files = json.loads(manifest)["files"]
assert set(p.name for p in root.iterdir()) == {"manifest.json", "provider", *files}
for name, expected in files.items():
    data = (root / name).read_bytes()
    assert len(data) == expected["bytes"], name
    assert hashlib.sha256(data).hexdigest() == expected["sha256"], name
provider = root / "provider"
provider_manifest = (provider / "manifest.json").read_bytes()
assert hashlib.sha256(provider_manifest).hexdigest() == "64b7a24292327cda087b88ffd85dc3e6a9c08398bbbdeec9ba3fc896bd5e188e"
companion = json.loads(provider_manifest)
assert {p.relative_to(provider).as_posix() for p in provider.rglob("*") if p.is_file()} == {"manifest.json", *companion["files"]}
for name, expected in companion["files"].items():
    data = (provider / name).read_bytes()
    assert len(data) == expected["bytes"], name
    assert hashlib.sha256(data).hexdigest() == expected["sha256"], name
print("Worked baseline and provider input bytes match")
PY
```

Generate `empty`, `busy`, `busy-filter`, `rich` and `reader` with the same command and a different preset. `busy-filter` selects the 300-MR variation. The same provider command reads each bundle. Open the working set through the production entry points in [SLICE](first-review.md). Compare the final line, distinct review keys, native markers and raw bytes against the exported sources. Retain the required measurement evidence and remove the generated fixture directories when verification finishes. Fixture generation and visual inspection are development tools. The [test policy](../../../AGENTS.md) applies to any permanent cases added around them.

### Independent fixture checks

| Preset | MRs / registrations / queued intents | Provider manifest SHA-256 |
| --- | --- | --- |
| `empty` | 0 / 0 / 0 | `e9fa6cc39852f216d32e056cba7677daa0ef93886f27d9ef1ce8ff7f3128e5c6` |
| `worked` | 12 / 5 / 0 | `64b7a24292327cda087b88ffd85dc3e6a9c08398bbbdeec9ba3fc896bd5e188e` |
| `busy` | 67 / 506 / 200 | `f34175f77dbdaea27a4d20338c8959a66cbe137f1f2cdae494ef722b8e2684e8` |
| `busy-filter` | 300 / 506 / 200 | `00eacecac766c76783b1608271505d5b9f685e40b502351aa22362f320bfc94c` |
| `reader` | 12 / 9 / 0 | `07384c17338a416af514e17efa8b43d62ef450f58faac20a9308fd6420209068` |
| `rich` | 12 / 19 / 0 | `a50d36caedde3313c1123d13ddfd1e3914f2ce9efe361cd730a9eb0c69e716a4` |

- Active's native source ID is `af785ec1-cf3a-56f6-8329-0a800b8ef407` and run ID is `9eeca80e-4e18-5c48-a465-5dc4acbbff92`. Its child uses the same run and names Active as its native parent. Quiet's native source ID is `ca43678e-b86c-593f-8e90-3dc74f835451`. It has one Claude assistant record containing `Quiet complete`. Its published versions measure 3,194 bytes in 105 lines, then 4,458 bytes in 131 lines, each with three findings. Large has 240 initial Codex items and its own run.
- Busy queues 200 distinct leads. `busy-5` has a pending native question. The two closed branch-only MRs use `feature/RQ-641-fix` and `feature/RQ-6410-fix`. Match only the first to Jira `RQ-641`. Busy-filter has 15 titles with the prefix `Review the parser ` and 200 Active messages of 12,000 ASCII bytes each.
- Reader has 10,000 completed items and 400 matching command starts. Each 25th item is a user prompt, with equal text at 25 and 50 and distinct item IDs. The first, middle and last IDs are `reader-item-0`, `reader-item-5000` and `reader-item-9999`. Item 5's decoded output is exactly 327,680 bytes and ends with `READER-OUTPUT-TAIL`. The explicit page cue ends at `reader-item-10099`.
- Rich's renamed mixed patch is 6,482 bytes with 244 physical lines. Its initial large review has 205 discussions, including 200 distributed over the mixed file. The `crlf-limit.patch` longest line is exactly 16,384 bytes before CRLF. `over-bytes.patch` is 2,249,289 bytes, `over-lines.patch` has 20,002 physical lines, and `unicode-line.patch` has a 16,819-byte line. The provider manifest records each document's digest, line measurements and exact final bytes.
- Reader adds the Claude lead `reader-report` on Large. Its first version, 1,253 bytes, holds a finding whose issue contains `**one** &amp; two 🧪` at byte 714 (scalar 705), with visible text `one & two 🧪`, and whose details hold a blockquote and a Mermaid diagram. Its second version, 1,101 bytes, retires that finding as fixed and approves, so its digest differs.
- Rich adds one Claude lead per reader limit on Large. Each version is composed from ledger records within the ledger's own limits and ends in a unique marker. The server refuses the rich view of the first three; the browser parser counts 73,912 nodes in `specimen-nodes` and depth 132 in `specimen-depth`, and the diagrams hold 650 edges and 32,693 characters:

| Lead | Limit crossed | Bytes / lines / longest line | Marker, byte (scalar) offset |
| --- | --- | --- | --- |
| `specimen-bytes` | 1 MiB | 1,076,765 / 817 / 2,050 | `**one** &amp; two 🧪` 1,076,724 (1,076,605), then `REPORT-BYTE-TAIL` |
| `specimen-lines` | 10,000 lines | 131,724 / 10,123 / 111 | `REPORT-LINES-TAIL` 131,706 (131,700) |
| `specimen-line` | 16 KiB line | 27,264 / 57 / 26,340 | `REPORT-LINE-TAIL` 27,247 (22,438) |
| `specimen-edges` | 500 diagram edges | 11,133 / 709 / 111 | `REPORT-EDGES-TAIL` 11,115 (11,106) |
| `specimen-characters` | 30,000 diagram characters | 33,444 / 105 / 593 | `REPORT-CHARACTERS-TAIL` 33,421 (33,415) |
| `specimen-nodes` | 60,000 nodes | 100,197 / 123 / 8,187 | `REPORT-NODES-TAIL` 100,179 (100,155) |
| `specimen-depth` | 128 nesting levels | 1,183 / 57 / 264 | `REPORT-DEPTH-TAIL` 1,165 (1,156) |
| `specimen-windowed` | windowed reader at 64 KiB | 69,654 / 381 / 781 | `REPORT-WINDOWED-TAIL` 69,633 (69,543) |
| `specimen-changes` | windowed reader at 24 KiB with changes | 30,085 / 189 / 781, then 27,882 / 179 / 781 after one finding is fixed | `REPORT-CHANGES-TAIL` 30,065 (30,023), then 27,862 (27,822) |
| `specimen-blocks` | 512 display blocks | 100,083 / 2,445 / 167 | `REPORT-BLOCKS-TAIL` 100,064 (99,458) |

  A field line is limited to 8 KiB, so the only line over 16 KiB is the composed `**Location:**` line of eight long Unicode paths. A version opens in the windowed reader only when its display index fits 512 blocks. A composed card with a location and three short fields takes six blocks and a report with no notes takes 23 more, so such a report stays windowed through 81 findings (509 blocks) and falls back with `layout_budget` at 82, as measured through the window route. `specimen-windowed` (28 cards, 191 blocks) and the changed `specimen-changes` version (11 cards, 89 blocks) therefore reach their size thresholds with a few long cards and come back `ready`, while `specimen-blocks` keeps 200 short cards to cross the block budget on purpose and opens in the full reader. The manifest also records expected clipboard bytes, accessible reading order and viewport settings for the wide, narrow, wrapped and reduced-motion specimens. Record actual presentation through the browser.
