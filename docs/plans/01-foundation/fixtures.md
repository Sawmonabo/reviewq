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
| Quiet, `forge/beta!41`, ticket `RQ-42` | Different project, same displayed IID and similar MR title. Completed run, 20 additions, short history, a saved report, no pending activity or live output. | Exactly 20 added lines. The last line contains `example19 = 19`. The run, report and draft differ from Active. Reading this review never starts a turn. |
| Large, `forge/alpha!43`, ticket `RQ-43` | 12,000 additions in `src/example.ts`, discussions at 8 and 600, a separate mixed/wrapped file and a long read-only history. | One selected patch with 12,000 code lines plus the hunk header. Find reaches `example11999 = 11999` before visiting the last screen. A switch to the mixed file transfers only that file's patch. |

| Patch | Raw bytes | SHA-256 | Zero-based byte ranges for added lines, excluding LF |
| --- | --- | --- | --- |
| `quiet.patch` | 596 | `d2beb4febf98c5a723cb01ce717537855da75c23ce883e4a3ff0f68f318782f5` | Line 20 `[567, 596)` |
| `active.patch` | 38,598 | `860e21b1c8ecc9c3fc758c0bd9ebbaf0dd4ca7a2af18af71e0871e5beec5d8d9` | Line 820 `[26007, 26038)`, line 1200 `[38565, 38598)` |
| `large.patch` | 409,799 | `28301b2f3985845ed768817514257a5334d3af9c1f2cfba98a00450c4b9fc0f3` | Line 12000 `[409764, 409799)` |

The ranges start at the patch's `+` prefix. Each patch has one hunk header and ends at the final line's semicolon. The worked review aliases `review-0`, `review-1` and `review-2` use project ID/IID pairs `(11, 41)`, `(22, 41)` and `(11, 43)`. Row index `i` uses `10 + i` for head, `1000 + i` for base and `2000 + i` for start, each zero-padded to 40 hexadecimal digits. Active's initial markers run from `Update 0` through `Update 99` with corresponding `active-item-0`…`active-item-99` and `active-turn-0`…`active-turn-99`. Quiet's sole observation is `quiet-item-0` in `quiet-turn-0`, with text `Quiet complete`. The timeline adds `active-item-100` (`Live message 1`), closes the stream, then adds `active-item-101` (`Live message 2`). The initial outbox is empty.

### Provider replay and release profiles

The worked export supplies the source observations and cues. The provider driver maps them to real revision-bound thread positions and provider-native shapes. It also supplies Quiet's saved report and Large's mixed file and long history. To trigger an interruption, use controls on the driver's listener and the actual browser reconnect path.

Bind each worked review to a distinct GitLab instance/project ID/IID, revision triple, path, ReviewQ run, native root/session, turn/item and report receipt. Deliver both additional items to Active. For the real native exercise, register actual host IDs, verify delivery from native evidence and record their mapping to the synthetic review aliases in private output.

The **measurement profile** projects these inputs into 12 queue MRs, exactly one working registered lead with 100 entries, empty outbox and one selected file with discussions at 8 and 600. For each fresh launch pick either the byte-exact 1,200- or 12,000-line patch for that file. The production shell, Files/All conversations controls, inline open/close, working Agent, live append/reconnect, draft, send-once and return must all operate. Use the populated three-review profile for switching and physical desktop scenarios. Supply both profiles without a resident service or connection per review.

| Preset | Additional data and exercise |
| --- | --- |
| `empty` / `worked` | Fresh profile, then the 12-row set above. Supply fixture provider reads to the actual application adapters and a small scrub-free native source example per supported host. Preserve source bytes independently of decoded receipts. |
| `busy` | 67 MRs, 506 session registrations across projects and 200 outbox rows. Include two different MRs per ticket, two runs on one MR, an eligible ticket `RQ-641` with no open MR and two closed MRs with neutral titles/descriptions and source branches `feature/RQ-641-fix` and `feature/RQ-6410-fix` (only the first matches), a pending input, malformed or missing optional data, and duplicate event delivery. Exercise scoped queue/detail reads. A secondary 300-MR variation uses 15 shared-prefix titles and 200 messages of 12,000 ASCII characters each to catch an aggregate transfer during typing/filtering. |
| `reader` | Build upon the three worked reviews. Add 10,000 ordered native items with deterministic user prompts every 25 items, paired tool starts/results, long output of 320 KiB, two equal prompts with distinct native IDs, and independently identifiable first/middle/last messages. Include a child and a second lead. Produce a known Markdown report with a quote, diagram and receipt, and a changed report with a new digest. |
| `rich` | Alongside the byte-exact additions, supply context, insert/delete hunks, renamed file, no-final-newline marker, empty/binary/provider-limited files, long hard-wrapping Unicode, bidi and tabs. Supply old/new side anchors, an earlier-revision comment, a general thread, 200 discussions spread over the mixed file, a long expanded reply with a diagram, and a changed head. Include a Jira v2 description and comment with a wiki heading, code, table, list, labeled link and unknown macro. Include a report selection spanning `**one** &amp; two 🧪` with its raw source coordinates and different visible text. Include expected clipboard bytes and native accessible reading order for a short known selection. |

For bounded source mode add one patch or report crossing each independent [full-document limit](../../specs/browser-experience.md#full-document-and-bounded-source-modes), including a CRLF-terminated line of exactly 16 KiB of content (eligible on the line-length criterion), a single very long Unicode line, an oversized report/diagram and a long body whose final range contains a unique marker. Write the expected final bytes/digest in the source manifest. Supply a transcript where selection/focus pins the requested six pages or byte budget and an incoming seventh page can be triggered explicitly.

Expose the fixture through a loopback-only provider driver, using the same Rust read paths and SQL writes as ordinary operation. Support ordered release of the selected response, a forced stream disconnect, observer counts, an external-authority error and a terminal submission whose response is lost after acceptance. Keep controls in the harness. The application must have a single read/send path in a fixture session. Provide visual specimens from these data for wide, narrow, wrapped and reduced-motion review.

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
companion = json.loads((provider / "manifest.json").read_bytes())
for name, expected in companion["files"].items():
    data = (provider / name).read_bytes()
    assert len(data) == expected["bytes"], name
    assert hashlib.sha256(data).hexdigest() == expected["sha256"], name
print("Worked baseline and provider input bytes match")
PY
```

Generate `empty`, `busy`, `busy-filter`, `rich` and `reader` with the same command and a different preset. `busy-filter` selects the 300-MR variation. The provider driver reads these bundles in the next slice. Open the working set through the production entry points in [SLICE](first-review.md). Compare the final line, distinct review keys, native markers and raw bytes against the exported sources. Retain the required measurement evidence and remove the generated fixture directories when verification finishes. Fixture generation and visual inspection are development tools. The [test policy](../../../AGENTS.md) applies to any permanent cases added around them.
