"""The ledger calls that produce each fixture review's report versions.

A plan is a list of steps for one Claude lead. A `turn` step is one lead turn: the calls it
makes, in order, the last of which may close it with `finish_review`. A `disposition` step is
the reviewer's choice saved through the browser's route between turns. ReviewQ composes and
publishes every version; nothing here writes report text.
"""

QUOTE = "**one** &amp; two 🧪"
QUOTE_TEXT = "one & two 🧪"
FIXED_AT = "17a2b3c417a2b3c417a2b3c417a2b3c417a2b3c4"


def call(tool, value, agent=None):
    step = {"tool": tool, "input": value}
    if agent:
        step["agent"] = agent
    return step


def finding(title, severity, blocking, section, issue, consequence, evidence, kind="observed", **extra):
    value = {"title": title, "severity": severity, "blocking": blocking, "section": section,
             "issue": issue, "consequence": consequence, "evidence": {"kind": kind, "text": evidence}}
    value.update(extra)
    return value


def note(section, body, title=None):
    value = {"section": section, "body": body}
    if title is not None:
        value["title"] = title
    return value


def finish(head, verdict="request_changes", **extra):
    return call("finish_review", {"verdict": verdict, "revision": {"sha": head}, **extra})


def worked(head):
    """The record set `crates/reviewq-app/src/ledger/compose/tests.rs` builds, filed as a full
    review and then an update that changes F-01, settles F-01 and F-03 and adds F-04."""
    first = [
        call("record_finding", finding(
            "Retry writes the same invoice twice", "P1", True, "functional",
            "The retry builds a fresh idempotency key after the first insert commits.",
            "A lost acknowledgement creates a duplicate invoice.",
            "Replaying the request after a dropped reply inserted two rows.",
            locations=[{"path": "src/invoices.rs", "line": 74}])),
        call("record_finding", finding(
            "`retry_key` is rebuilt on every attempt", "P2", True, "quality",
            "`retry_key` is called inside the loop, so each attempt gets a new key.",
            "The server cannot tell a retry from a new request.",
            "The unit test `retries_reuse_key` fails at the reviewed commit.",
            category="Reliability",
            locations=[{"path": "src/retry.rs", "line": 12}, {"path": "docs/retry`notes.md"}]), agent="agent-7"),
        call("record_finding", finding(
            "Two retry paths build separate keys", "P1", True, "functional",
            "The sync and async paths each build their own key.",
            "A retry on the other path writes a second invoice.",
            "Both paths call `new_key`.", locations=[{"path": "src/retry.rs", "line": 30}])),
        call("record_note", note("title", "Retry invoice writes without duplicates")),
        call("record_note", note("summary", "Adds a retry loop around invoice writes so a dropped acknowledgement "
                                            "no longer fails the request.\n\nThe retry reuses the request's idempotency key.")),
        call("record_note", note("rationale", "The retry still creates a new key on one path, which can write a "
                                              "duplicate invoice.")),
        call("record_note", note("flow", "1. `POST /invoices` reaches `InvoiceService::create`.\n"
                                         "2. The service writes the row and returns the receipt.")),
        call("record_note", note("changes", "- Adds `retry_write` in `src/retry.rs`.\n"
                                            "- Moves key creation into `src/invoices.rs`.")),
        call("record_note", note("questions", "Should a retry give up after three attempts or after a time limit?")),
        call("record_note", note("verification", "Ran `cargo test -p invoices` at the reviewed commit. The replay "
                                                 "check ran against a local database only.")),
        call("record_note", note("extra", "Ship behind the `invoice_retry` flag.\n", "Rollout plan for *v2* & `beta` #")),
        call("record_note", note("publication", "No comments are posted yet.")),
        finish(head),
    ]
    second = [
        call("record_finding", {
            "id": "F-01", "expectedRevision": 1,
            "proposedComment": "Reuse the key stored with the first attempt instead of building a new one.\n",
            "details": "\nThe timeline below shows the race.\n\n## Timeline\n\n1. The first insert commits.\n"
                       "2. The reply is lost and the client retries.\n\nEach <runner> keeps the body as a "
                       "Vec<u8> until the write commits.\n\n"}),
        call("assess_finding", {"id": "F-01", "expectedRevision": 2, "outcome": "still_present",
                                "reason": "The retry path still builds a new key.", "discussionIds": ["D9"]}),
        call("assess_finding", {"id": "F-03", "expectedRevision": 1, "outcome": "fixed",
                                "reason": "Both paths now share one key | verified by replay.",
                                "discussionIds": ["D7"], "checkedSha": FIXED_AT}),
        call("record_finding", finding(
            "Retry delay is not configurable", "P3", False, "quality",
            "The delay between attempts is a constant.", "Operators cannot slow retries during an outage.",
            "The production retry settings were not available to check.", kind="gap")),
        call("record_note", note("threads", "The author replied on D7 that the shared key landed in the second commit.")),
        finish(head),
    ]
    disposition = {"finding": "F-02", "blocking": False, "publication": "withheld",
                   "reason": "Agreed with the author\nto fix after the release."}
    return [{"turn": first}, {"disposition": disposition}, {"turn": second}]


def reader(head):
    """A short review with a quote and a diagram, then an update with a new verdict."""
    first = [
        call("record_note", note("title", "Worked review")),
        call("record_note", note("rationale", "Request changes until the parser validates an empty instruction.")),
        call("record_finding", finding(
            "The parser accepts incomplete input", "P1", True, "functional",
            f"The parser accepts {QUOTE} before checking that input is complete.",
            "An empty instruction reaches the review step.", "A replayed empty instruction was accepted.",
            details="> The original instruction remains visible in the terminal.\n\n"
                    "```mermaid\nflowchart LR\n  Input --> Parse --> Check --> Result\n```")),
        finish(head),
    ]
    second = [
        call("record_note", {**note("rationale", "Approve: the parser changes validate an empty instruction."),
                             "expectedRevision": 1}),
        call("assess_finding", {"id": "F-01", "expectedRevision": 1, "outcome": "fixed",
                                "reason": "The parser now checks for an empty instruction first."}),
        finish(head, "approve"),
    ]
    return [{"turn": first}, {"turn": second}]


def paragraph(count, width, char="x"):
    return "\n".join(char * width for _ in range(count))


def filler(index, size_lines, title_prefix="Filler"):
    """A finding whose five Markdown fields are close to their byte limits."""
    text = paragraph(size_lines, 2030)
    return finding(f"{title_prefix} {index:02d}", "P3", False, "quality", text, text, text,
                   proposedComment=text, details=paragraph(size_lines * 2, 2030))


def closing(head, tail, calls):
    return [{"turn": [*calls, call("record_note", note("publication", tail)), finish(head)]}]


def specimens(head):
    """One plan per reader limit. Each crosses one limit and stays inside the others."""
    bytes_over = [call("record_finding", filler(i, 4)) for i in range(22)]
    lines_over = [call("record_note", note(section, "\n".join(["Report line."] * 2520)))
                  for section in ("flow", "changes", "questions", "verification")]
    line_over = [call("record_finding", finding(
        "Locations in long Unicode paths", "P3", False, "quality", "Every path is long.", "The line is long.",
        "The location line exceeds 16 KiB.",
        locations=[{"path": "🧪" * 511 + str(i)} for i in range(8)]))]
    edges = "\n".join(f"  N{i} --> N{i + 1}" for i in range(650))
    edges_over = [call("record_finding", finding(
        "A diagram with 650 edges", "P3", False, "quality", "The flow has many steps.", "It is hard to read.",
        "The diagram below has 650 edges.", details=f"```mermaid\nflowchart LR\n{edges}\n```"))]
    labels = ("  %% " + "diagram label " * 42 + "\n") * 55
    characters_over = [call("record_note", note("verification", f"```mermaid\nflowchart LR\n{labels}  A --> B\n```"))]
    emphasis = " ".join(["*a*"] * 2047)
    nodes_over = [call("record_finding", finding(
        f"Dense emphasis {i}", "P3", False, "quality", "Many inline nodes follow.", "The tree is large.",
        "Each line holds about 2,000 emphasis nodes.", details=f"{emphasis}\n{emphasis}")) for i in range(6)]
    depth_over = [call("record_finding", finding(
        "A deeply nested quote", "P3", False, "quality", "The quote nests deeply.", "The tree is deep.",
        "The quote below nests 130 levels.", details="> " * 130 + "deep"))]
    def cards(count):
        return [call("record_finding", finding(
            f"Windowed finding {i:03d}", "P3", False, "quality",
            f"Finding {i} reads a short paragraph about the parser's handling of one input. " * 2,
            "A reader checks that every card is reachable.", f"Observed while reading input {i}."))
            for i in range(count)]
    # Below 64 KiB a version opens windowed only when it changes findings: a first version
    # of 100 cards stays whole, and the update that settles one card crosses 24 KiB with a change.
    changes = closing(head, "REPORT-CHANGES-TAIL", cards(100))
    changes.append({"turn": [call("assess_finding", {"id": "F-01", "expectedRevision": 1, "outcome": "fixed",
                                                     "reason": "The first card's input is now handled."}),
                             finish(head)]})
    return {
        "specimen-bytes": closing(head, f"🧪 Quote at end. {QUOTE}\n\nREPORT-BYTE-TAIL", bytes_over),
        "specimen-lines": closing(head, "REPORT-LINES-TAIL", lines_over),
        "specimen-line": closing(head, "REPORT-LINE-TAIL", line_over),
        "specimen-edges": closing(head, "REPORT-EDGES-TAIL", edges_over),
        "specimen-characters": closing(head, "REPORT-CHARACTERS-TAIL", characters_over),
        "specimen-nodes": closing(head, "REPORT-NODES-TAIL", nodes_over),
        "specimen-depth": closing(head, "REPORT-DEPTH-TAIL", depth_over),
        "specimen-windowed": closing(head, "REPORT-WINDOWED-TAIL", cards(200)),
        "specimen-changes": changes,
    }


def specimen_sessions(preset):
    """The Claude leads that publish the reader and size-limit versions on Large."""
    aliases = []
    if preset in ("reader", "rich"):
        aliases.append("reader-report")
    if preset == "rich":
        aliases.extend(specimens("0" * 40))
    return [{"alias": alias, "review": "review-2", "provider": "claude", "state": "completed", "parent": None}
            for alias in aliases]


def ledger_plans(reviews, sessions):
    """Every Claude lead's plan, in publishing order, for the leads that have one."""
    heads = {row["alias"]: row["head"] for row in reviews}
    plans = []
    for session in sessions:
        alias, review = session["alias"], session["review"]
        if alias == "quiet":
            steps, markers = worked(heads[review]), [None, None]
        elif alias == "reader-report":
            steps, markers = reader(heads[review]), [QUOTE, None]
        elif alias.startswith("specimen-"):
            steps = specimens(heads[review])[alias]
            tail = steps[0]["turn"][-2]["input"]["body"]
            markers = [QUOTE if alias == "specimen-bytes" else tail] + [tail] * (len(steps) - 1)
        else:
            continue
        plans.append({"lead": alias, "review": review, "steps": steps, "markers": markers})
    return plans


# What each lead's versions measured when last published, in publishing order: the product's
# composed text, read back through the report routes. Five-digit provider ports keep these
# fixed; receipts, digests and the checked time differ on every run.
LEDGER_VERSIONS = {
    "quiet": [
        {"bytes": 2353, "lines": 91, "max_line_bytes": 104, "code_points": 2338, "findings": 3},
        {"bytes": 3238, "lines": 107, "max_line_bytes": 183, "code_points": 3216, "findings": 3},
    ],
    "reader-report": [
        {"bytes": 1218, "lines": 64, "max_line_bytes": 92, "code_points": 1206, "findings": 1,
         "marker": {"text": "**one** &amp; two 🧪", "scalar": 670, "byte": 679}},
        {"bytes": 1054, "lines": 51, "max_line_bytes": 201, "code_points": 1046, "findings": 0},
    ],
    "specimen-bytes": [
        {"bytes": 1075960, "lines": 773, "max_line_bytes": 2052, "code_points": 1075882, "findings": 22,
         "marker": {"text": "**one** &amp; two 🧪", "scalar": 1075844, "byte": 1075919}},
    ],
    "specimen-lines": [
        {"bytes": 131689, "lines": 10123, "max_line_bytes": 76, "code_points": 131683, "findings": 0,
         "marker": {"text": "REPORT-LINES-TAIL", "scalar": 131665, "byte": 131671}},
    ],
    "specimen-line": [
        {"bytes": 17293, "lines": 57, "max_line_bytes": 16404, "code_points": 5020, "findings": 1,
         "marker": {"text": "REPORT-LINE-TAIL", "scalar": 5003, "byte": 17276}},
    ],
    "specimen-edges": [
        {"bytes": 11098, "lines": 709, "max_line_bytes": 76, "code_points": 11089, "findings": 1,
         "marker": {"text": "REPORT-EDGES-TAIL", "scalar": 11071, "byte": 11080}},
    ],
    "specimen-characters": [
        {"bytes": 33409, "lines": 105, "max_line_bytes": 593, "code_points": 33403, "findings": 0,
         "marker": {"text": "REPORT-CHARACTERS-TAIL", "scalar": 33380, "byte": 33386}},
    ],
    "specimen-nodes": [
        {"bytes": 100162, "lines": 123, "max_line_bytes": 8187, "code_points": 100138, "findings": 6,
         "marker": {"text": "REPORT-NODES-TAIL", "scalar": 100120, "byte": 100144}},
    ],
    "specimen-depth": [
        {"bytes": 1148, "lines": 57, "max_line_bytes": 264, "code_points": 1139, "findings": 1,
         "marker": {"text": "REPORT-DEPTH-TAIL", "scalar": 1121, "byte": 1130}},
    ],
    "specimen-windowed": [
        {"bytes": 71266, "lines": 2045, "max_line_bytes": 167, "code_points": 70660, "findings": 200,
         "marker": {"text": "REPORT-WINDOWED-TAIL", "scalar": 70639, "byte": 71245}},
    ],
    "specimen-changes": [
        {"bytes": 35765, "lines": 1045, "max_line_bytes": 165, "code_points": 35459, "findings": 100,
         "marker": {"text": "REPORT-CHANGES-TAIL", "scalar": 35439, "byte": 35745}},
        {"bytes": 35642, "lines": 1037, "max_line_bytes": 171, "code_points": 35337, "findings": 99,
         "marker": {"text": "REPORT-CHANGES-TAIL", "scalar": 35317, "byte": 35622}},
    ],
}
