# FIXTURE · Make a review another developer can open

**Outcome.** One command generates a portable review workspace with known code, conversations and expected results. The first runnable review and release qualification use the same inputs. A reviewer can derive the expected text and routing independently of the browser or database. FIXTURE is ready independently of CONTRACT. The lead schedules it within the checkout limit.

**Ownership.** One `gpt-6.2-sol` worker at `xhigh` owns the synthetic inputs and fixture driver. A separate `gpt-6.2-astra` reviewer at `xhigh` regenerates and inspects the expected bytes. FIXTURE can close independently. SLICE checks compatibility with the integrated service contracts.

## Source material

Implement `tools/dev/seed-review` and keep the input manifest and small hand-authored examples under `fixtures/reviews/`. Use only synthetic text, fixed UTC timestamps, LF and UTF-8. Give the generator a required output directory. Fail on an occupied destination. Accept a deterministic preset, use relative paths in its manifest and write standard provider-shaped inputs, expected IDs, version, byte lengths and SHA-256 digests. The generated inputs can travel to a second host without access to the original checkout. Generate long bodies locally. Keep the timeline of actions separate from the initial data so an independent reviewer can cause a missed event, delivery or late reply on cue.

This Python 3 recipe creates the worked source manifest, patch files, native-input observations and event script in a new directory. Copy the block unchanged into Python on either machine. It prints the directory and deterministic manifest digest. The document hashes cover the raw UTF-8 patch bodies. GitLab discussion wrappers, response headers and encoded transfer size have separate measurements.

```python
import hashlib
import json
import tempfile
from pathlib import Path

root = Path(tempfile.mkdtemp(prefix="reviewq-worked-"))
def put_json(name, obj):
    data = json.dumps(obj, sort_keys=True, ensure_ascii=False, indent=2) + "\n"
    (root / name).write_bytes(data.encode("utf-8"))

cases = (
    ("quiet", 20, 596, "d2beb4febf98c5a723cb01ce717537855da75c23ce883e4a3ff0f68f318782f5"),
    ("active", 1200, 38598, "860e21b1c8ecc9c3fc758c0bd9ebbaf0dd4ca7a2af18af71e0871e5beec5d8d9"),
    ("large", 12000, 409799, "28301b2f3985845ed768817514257a5334d3af9c1f2cfba98a00450c4b9fc0f3"),
)
for name, count, size, digest in cases:
    header = f"@@ -0,0 +1,{count} @@"
    additions = [f"+export const example{i} = {i};" for i in range(count)]
    data = "\n".join([header, *additions]).encode("utf-8")
    assert len(data) == size and hashlib.sha256(data).hexdigest() == digest
    (root / f"{name}.patch").write_bytes(data)

reviews = []
worked = [("alpha", 41, 41, "active"), ("beta", 41, 42, "quiet"), ("alpha", 43, 43, "large")]
for i in range(12):
    project, iid, ticket, case = worked[i] if i < 3 else ("alpha", 100+i, 100+i, "quiet")
    reviews.append(dict(project=project, project_id={"alpha": 11, "beta": 22}[project],
                        iid=iid, ticket=f"RQ-{ticket}", title="Review the parser" if i < 2 else f"Review {i}",
                        head=f"{10+i:040x}", base=f"{1000+i:040x}", start=f"{2000+i:040x}",
                        path="src/example.ts", patch=f"{case}.patch", alias=f"review-{i}"))
put_json("reviews.json", reviews)
sessions = [
    ("active", "review-0", "codex", "working", None),
    ("child", "review-0", "codex", "completed", "active"),
    ("earlier", "review-0", "codex", "completed", None),
    ("quiet", "review-1", "claude", "completed", None),
    ("large", "review-2", "codex", "completed", None),
]
put_json("sessions.json", [dict(alias=a, review=r, provider=p, state=s, parent=b)
                            for a, r, p, s, b in sessions])
def entry(lead, i, text):
    return dict(lead=lead, id=f"{lead}-item-{i}", turn=f"{lead}-turn-{i}",
                text=text, observed_at="2026-11-01T00:00:00Z")
put_json("history.json", [entry("active", i, f"Update {i} " + "Visible conversation text. " * 10)
                           for i in range(100)] + [entry("quiet", 0, "Quiet complete")])
put_json("discussions.json", [dict(review=r, id=f"{r}-thread-{n}", new_line=n)
                               for r in ("review-0", "review-2") for n in (8, 600)])
put_json("timeline.json", [
    {"action": "invalidate", "review": "review-0"},
    {"action": "add-thread", "review": "review-0", "new_line": 1060},
    {"action": "add-thread", "review": "review-0", "new_line": 12},
    {"action": "append", "entry": entry("active", 100, "Live message 1")},
    {"action": "close-stream", "lead": "active"},
    {"action": "append", "entry": entry("active", 101, "Live message 2")},
])
put_json("manifest.json", {"version": 1, "outbox": [], "files": {
    p.name: {"bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
    for p in sorted(root.iterdir())
}})
print(root, hashlib.sha256((root / "manifest.json").read_bytes()).hexdigest())
```

The worked manifest digest is `7aff00aad90e93bbb4abc2297c1305077696786fce28c2f3122f3fb45e92b313`. It lists eight input files containing 12 review rows, five session/child aliases and 101 initial history entries across Active and Quiet. The driver maps these source observations to real revision-bound thread positions and provider-native shapes. The application exposes its ordinary versioned projections. To interrupt the browser stream, use the actual browser reconnect path and expose fixture controls through the driver's own listener.

### Worked review set

| Review | Inputs | Independent expectations |
| --- | --- | --- |
| Active, `forge/alpha!41`, ticket `RQ-41` | Working Codex lead, a child and an earlier completed run. The current file is `src/example.ts`, 1,200 additions. Start with current inline discussions at new lines 8 and 600. | New line 820 reads `export const example819 = 819;`. During reading, publish a same-revision update with identical data, then insert a thread at 1060, then one at 12. The target stays at its reading offset. Native Find reaches `example1199 = 1199`. |
| Quiet, `forge/beta!41`, ticket `RQ-42` | Different project, same displayed IID and similar MR title. Completed run, 20 additions, short history, a saved report, no pending activity or live output. | Exactly 20 added lines. The last line contains `example19 = 19`. The run, report and draft differ from Active. Reading this review never starts a turn. |
| Large, `forge/alpha!43`, ticket `RQ-43` | 12,000 additions in `src/example.ts`, discussions at 8 and 600, a separate mixed/wrapped file and a long read-only history. | One selected patch with 12,000 code lines plus the hunk header. Find reaches `example11999 = 11999` before visiting the last screen. A switch to the mixed file transfers only that file's patch. |

Use a stable, distinct GitLab instance/project ID/IID, revision triple, path, ReviewQ run, native root/session, turn/item and report receipt for every worked identity. The version-1 source indices are `0..11` for 12 queue entries and `0..99` for 100 native entries on Active. Give each entry its own ID, turn and visible `Update i` marker. The basic profile mounts the ordinary working run with the two original discussions and empty outbox. Deliver two further native items with unique IDs, the second across reconnect. Quiet and Large receive neither item. For the real native exercise, register actual host IDs and record their mapping to the synthetic review aliases in private output. A fabricated file never demonstrates native delivery.

The **reference profile** projects these inputs into 12 queue MRs, exactly one working registered lead with 100 entries, empty outbox and one selected file with discussions at 8 and 600. For each fresh launch pick either the byte-exact 1,200- or 12,000-line patch for that file. The production shell, Files/All conversations controls, inline open/close, working Agent, live append/reconnect, draft, send-once and return must all operate. Keep this frozen comparison profile separate from the populated three-review switching and physical desktop scenarios. Supply both profiles without a resident service or connection per review.

| Preset | Additional data and exercise |
| --- | --- |
| `empty` / `worked` | Fresh profile, then the 12-row set above. Supply fixture provider reads to the actual application adapters and a small scrub-free native source example per supported host. Preserve source bytes independently of decoded receipts. |
| `busy` | 67 MRs, 506 session registrations across projects and 200 outbox rows. Include two different MRs per ticket, two runs on one MR, an eligible ticket `RQ-641` with no open MR and two closed MRs with neutral titles/descriptions and source branches `feature/RQ-641-fix` and `feature/RQ-6410-fix` (only the first matches), a pending input, malformed or missing optional data, and duplicate event delivery. Exercise scoped queue/detail reads. A secondary 300-MR variation uses 15 shared-prefix titles and 200 messages of 12,000 ASCII characters each to catch an aggregate transfer during typing/filtering. |
| `reader` | Build upon the three worked reviews. Add 10,000 ordered native items with deterministic user prompts every 25 items, paired tool starts/results, long output of 320 KiB, two equal prompts with distinct native IDs, and independently identifiable first/middle/last messages. Include a child and a second lead. Produce a known Markdown report with a quote, diagram and receipt, and a changed report with a new digest. |
| `rich` | Alongside the byte-exact additions, supply context, insert/delete hunks, renamed file, no-final-newline marker, empty/binary/provider-limited files, long hard-wrapping Unicode, bidi and tabs. Supply old/new side anchors, an earlier-revision comment, a general thread, 200 discussions spread over the mixed file, a long expanded reply with a diagram, and a changed head. Include a Jira v2 description and comment with a wiki heading, code, table, list, labeled link and unknown macro. Include a report selection spanning `**one** &amp; two 🧪` with its raw source coordinates and different visible text. Include expected clipboard bytes and native accessible reading order for a short known selection. |

For bounded source mode add one patch or report crossing each independent [full-document limit](../../specs/browser-experience.md#full-document-and-bounded-source-modes), including a CRLF-terminated line of exactly 16 KiB of content (eligible on the line-length criterion), a single very long Unicode line, an oversized report/diagram and a long body whose final range contains a unique marker. Write the expected final bytes/digest in the source manifest. Supply a transcript where selection/focus pins the requested six pages or byte budget and an incoming seventh page can be triggered explicitly.

Expose the fixture through a loopback-only provider driver, using the same Rust read paths and SQL writes as ordinary operation. Support ordered release of the selected response, a forced stream disconnect, observer counts, an external-authority error and a terminal submission whose response is lost after acceptance. Keep controls in the harness. The application must have a single read/send path in a fixture session. Provide visual specimens from these data for wide, narrow, wrapped and reduced-motion review.

## Handoff

Publish the implemented invocation as `tools/dev/seed-review --preset worked --out <new-directory>`. Move the worked recipe into that generator at handoff and replace this document’s code block with its invocation. Keep the independent manifest digest and expected observations here. Document the actual source files, port discovery, independent manifest checker and the command to run the temporary provider. Provide `empty`, `busy`, `rich` and `reader` with the same output convention. The generator supplies inputs, never pre-rendered review rows or a screenshot as the UI. Run twice in separate directories, compare normalized manifests and input bytes, then open the working set in [SLICE](first-review.md). Verify an independent reader can obtain the final line, distinct review keys, expected native markers and raw bytes from the exported inputs in an empty directory. Retain the required measurement evidence and remove the generated fixture directories when verification finishes. Fixture generation and visual inspection are development tools. The [test policy](../../../AGENTS.md) applies to any permanent cases added around them.
