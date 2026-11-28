"""The reviewing tool calls that produce each fixture review's report versions.

A plan is a list of steps for one Claude lead. A `turn` step is one lead turn: the calls it
makes, in order, the last of which may close it with `submit_review`. A `disposition` step is
the user's choice saved through the browser's route between turns. ReviewQ composes and
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


def finding(title, severity, blocking, kind, issue, consequence, evidence, basis="observed", **extra):
    value = {"title": title, "severity": severity, "blocking": blocking, "kind": kind,
             "issue": issue, "consequence": consequence, "evidence": {"kind": basis, "text": evidence}}
    value.update(extra)
    return value


def note(section, body, title=None):
    value = {"section": section, "body": body}
    if title is not None:
        value["title"] = title
    return value


# What a plan saves for each section a first version or full review needs before it is submitted.
CLOSING_NOTES = {
    "summary": "Checks each parser instruction before the review step reads it.",
    "rationale": "Request changes until every reported input is handled.",
    "flow": "```mermaid\nflowchart LR\n  Instruction --> Tokenizer --> Parser --> Checker\n```\n\n"
            "`Parser::read` hands each instruction to `Checker::check` before the review step.",
    "changes": "| Before | After |\n| --- | --- |\n| The parser reads input unchecked. | `Checker` reads it first. |\n\n"
               "| Affected operation | What changes | Source |\n| --- | --- | --- |\n"
               "| `Parser::read` | Calls `Checker::check` first. | `src/parser.rs` |",
    "verification": "Replayed the fixture inputs at the reviewed commit. No live provider was checked.",
}


def closing_notes(*own):
    """The save_section calls for the sections a first version or full review needs, except the
    sections in `own`, which the plan saves itself."""
    return [call("save_section", note(section, body)) for section, body in CLOSING_NOTES.items()
            if section not in own]


def finish(head, verdict="request_changes", **extra):
    return call("submit_review", {"verdict": verdict, "revision": {"sha": head}, **extra})


def worked(head):
    """The record set `crates/reviewq-app/src/ledger/compose/tests.rs` builds, saved as a full
    review and then a second full review that changes F-01, assesses F-01, F-02 and F-03,
    settles F-03 and adds F-04. A full review must assess every finding it does not change."""
    first = [
        call("save_finding", finding(
            "Retry writes the same invoice twice", "P1", True, "functional",
            "The retry builds a fresh idempotency key after the first insert commits.",
            "A lost acknowledgement creates a duplicate invoice.",
            "Replaying the request after a dropped reply inserted two rows.",
            locations=[{"path": "src/invoices.rs", "line": 74}])),
        call("save_finding", finding(
            "`retry_key` is rebuilt on every attempt", "P2", True, "quality",
            "`retry_key` is called inside the loop, so each attempt gets a new key.",
            "The server cannot tell a retry from a new request.",
            "The unit test `retries_reuse_key` fails at the reviewed commit.",
            category="Reliability", proposedComment="Build the key once, before the loop.",
            locations=[{"path": "src/retry.rs", "line": 12}, {"path": "docs/retry`notes.md"}]), agent="agent-7"),
        call("save_finding", finding(
            "Two retry paths build separate keys", "P1", True, "functional",
            "The sync and async paths each build their own key.",
            "A retry on the other path writes a second invoice.",
            "Both paths call `new_key`.", locations=[{"path": "src/retry.rs", "line": 30}])),
        call("save_section", note("title", "Retry invoice writes without duplicates")),
        call("save_section", note("summary", "Adds a retry loop around invoice writes so a dropped acknowledgement "
                                            "no longer fails the request.\n\nThe retry reuses the request's idempotency key.")),
        call("save_section", note("rationale", "The retry still creates a new key on one path, which can write a "
                                              "duplicate invoice.")),
        call("save_section", note("flow", "```mermaid\nflowchart LR\n  Client --> InvoiceService --> Database\n```\n\n"
                                         "`POST /invoices` reaches `InvoiceService::create`, which writes the row "
                                         "and returns the receipt.")),
        call("save_section", note("changes", "| Before | After |\n| --- | --- |\n"
                                            "| A dropped acknowledgement fails the request. | The write is retried "
                                            "with the stored key. |\n\n"
                                            "| Affected operation | What changes | Source |\n| --- | --- | --- |\n"
                                            "| `POST /invoices` | Retries the write once. | `src/retry.rs` |\n"
                                            "| `InvoiceService::create` | Builds the key before the first attempt. | "
                                            "`src/invoices.rs` |")),
        call("save_section", note("questions", "Should a retry give up after three attempts or after a time limit?")),
        call("save_section", note("verification", "Ran `cargo test -p invoices` at the reviewed commit. The replay "
                                                 "check ran against a local database only.")),
        call("save_section", note("extra", "Ship behind the `invoice_retry` flag.\n", "Rollout plan for *v2* & `beta` #")),
        call("save_section", note("publication", "No comments are posted yet.")),
        finish(head),
    ]
    second = [
        call("save_finding", {
            "id": "F-01", "expectedRevision": 1,
            "proposedComment": "Reuse the key stored with the first attempt instead of building a new one.\n",
            "details": "\nThe timeline below shows the race.\n\n## Timeline\n\n1. The first insert commits.\n"
                       "2. The reply is lost and the client retries.\n\nEach <runner> keeps the body as a "
                       "Vec<u8> until the write commits.\n\n"}),
        call("set_finding_outcome", {"id": "F-01", "expectedRevision": 2, "outcome": "still_present",
                                "reason": "The retry path still builds a new key.", "discussionIds": ["D9"]}),
        call("set_finding_outcome", {"id": "F-02", "expectedRevision": 1, "outcome": "still_present",
                                "reason": "`retry_key` is still called inside the loop."}),
        call("set_finding_outcome", {"id": "F-03", "expectedRevision": 1, "outcome": "fixed",
                                "reason": "Both paths now share one key | verified by replay.",
                                "discussionIds": ["D7"], "checkedSha": FIXED_AT}),
        call("save_finding", finding(
            "Retry delay is not configurable", "P3", False, "quality",
            "The delay between attempts is a constant.", "Operators cannot slow retries during an outage.",
            "The production retry settings were not available to check.", basis="gap",
            proposedComment="Make the delay a setting.\n\n- Read it from `RETRY_DELAY_MS`.\n"
                            "- Keep 200 ms as the default.\n")),
        call("save_section", note("threads", "The author replied on D7 that the shared key landed in the second commit.")),
        finish(head, fullReview=True),
    ]
    disposition = {"finding": "F-02", "blocking": False, "publication": "withheld",
                   "reason": "Agreed with the author\nto fix after the release."}
    return [{"turn": first}, {"disposition": disposition}, {"turn": second}]


def reader(head):
    """A short review with a quote and a diagram, then an update with a new verdict."""
    first = [
        call("save_section", note("title", "Worked review")),
        call("save_section", note("rationale", "Request changes until the parser validates an empty instruction.")),
        call("save_finding", finding(
            "The parser accepts incomplete input", "P1", True, "functional",
            f"The parser accepts {QUOTE} before checking that input is complete.",
            "An empty instruction reaches the review step.", "A replayed empty instruction was accepted.",
            details="> The original instruction remains visible in the terminal.\n\n"
                    "```mermaid\nflowchart LR\n  Input --> Parse --> Check --> Result\n```")),
        *closing_notes("rationale"),
        finish(head),
    ]
    second = [
        call("save_section", {**note("rationale", "Approve: the parser changes validate an empty instruction."),
                             "expectedRevision": 1}),
        call("set_finding_outcome", {"id": "F-01", "expectedRevision": 1, "outcome": "fixed",
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


def closing(head, tail, calls, own=()):
    """One turn that saves `calls`, the closing sections the plan does not save itself, and the
    publication section that ends in `tail`, then submits the first version."""
    notes = closing_notes(*own)
    return [{"turn": [*calls, *notes, call("save_section", note("publication", tail)), finish(head)]}]


def specimens(head):
    """One plan per reader limit. Each crosses one limit and stays inside the others."""
    bytes_over = [call("save_finding", filler(i, 4)) for i in range(22)]
    # The flow section needs its Mermaid diagram, and the changes section a table with the two
    # columns Before and After, then a table with the columns Affected operation, What changes
    # and Source. Each opening takes the place of enough lines to keep the section within its
    # 32 KiB.
    opening = {"flow": ("```mermaid\nflowchart LR\n  Report --> Reader\n```\n\n", 2516),
               "changes": ("| Before | After |\n| --- | --- |\n| Short. | Long. |\n\n"
                           "| Affected operation | What changes | Source |\n| --- | --- | --- |\n"
                           "| Report | Grows. | `report.md` |\n\n", 2508)}
    lines_over = [call("save_section", note(section, head + "\n".join(["Report line."] * count)))
                  for section in ("flow", "changes", "questions", "verification")
                  for head, count in [opening.get(section, ("", 2520))]]
    line_over = [call("save_finding", finding(
        "Locations in long Unicode paths", "P3", False, "quality", "Every path is long.", "The line is long.",
        "The location line exceeds 16 KiB.",
        locations=[{"path": "🧪" * 200 + str(i)} for i in range(8)]))]
    edges = "\n".join(f"  N{i} --> N{i + 1}" for i in range(650))
    edges_over = [call("save_finding", finding(
        "A diagram with 650 edges", "P3", False, "quality", "The flow has many steps.", "It is hard to read.",
        "The diagram below has 650 edges.", details=f"```mermaid\nflowchart LR\n{edges}\n```"))]
    labels = ("  %% " + "diagram label " * 42 + "\n") * 55
    characters_over = [call("save_section", note("verification", f"```mermaid\nflowchart LR\n{labels}  A --> B\n```"))]
    emphasis = " ".join(["*a*"] * 2047)
    nodes_over = [call("save_finding", finding(
        f"Dense emphasis {i}", "P3", False, "quality", "Many inline nodes follow.", "The tree is large.",
        "Each line holds about 2,000 emphasis nodes.", details=f"{emphasis}\n{emphasis}")) for i in range(6)]
    depth_over = [call("save_finding", finding(
        "A deeply nested quote", "P3", False, "quality", "The quote nests deeply.", "The tree is deep.",
        "The quote below nests 130 levels.", details="> " * 130 + "deep"))]
    # The windowed reader needs the display index inside 512 blocks, about six per card, so the
    # windowed specimens reach their size with few long cards. `specimen-blocks` keeps 200 short
    # cards, which cross the block budget and open in the full reader.
    blocks_over = [call("save_finding", finding(
        f"Short finding {i:03d}", "P3", False, "quality",
        f"Finding {i} reads a short paragraph about the parser's handling of one input. " * 2,
        "A reader checks that every card is reachable.", f"Observed while reading input {i}.",
        locations=[{"path": f"src/parser/input_{i:03d}.rs", "line": i + 1}])) for i in range(200)]
    def long_cards(count):
        def prose(i, sentence):
            return " ".join(sentence.format(i=i, step=step) for step in range(1, 11))
        return [call("save_finding", finding(
            f"Windowed finding {i:02d}", "P3", False, "quality",
            prose(i, "Step {step} of input {i} passes through the tokenizer, the parser and the checker."),
            prose(i, "A reader of card {i} scrolls past step {step} and expects the next card to load."),
            prose(i, "Reading input {i} at step {step} returned the same tokens on every replay."),
            locations=[{"path": f"src/parser/input_{i:02d}.rs", "line": i + 1}])) for i in range(count)]
    # Below 64 KiB a version opens windowed only when it changes findings: a first version
    # of 12 cards stays whole, and the update that settles one card crosses 24 KiB with a change.
    changes = closing(head, "REPORT-CHANGES-TAIL", long_cards(12))
    changes.append({"turn": [call("set_finding_outcome", {"id": "F-01", "expectedRevision": 1, "outcome": "fixed",
                                                     "reason": "The first card's input is now handled."}),
                             finish(head)]})
    return {
        "specimen-bytes": closing(head, f"🧪 Quote at end. {QUOTE}\n\nREPORT-BYTE-TAIL", bytes_over),
        "specimen-lines": closing(head, "REPORT-LINES-TAIL", lines_over, ("flow", "changes", "verification")),
        "specimen-line": closing(head, "REPORT-LINE-TAIL", line_over),
        "specimen-edges": closing(head, "REPORT-EDGES-TAIL", edges_over),
        "specimen-characters": closing(head, "REPORT-CHARACTERS-TAIL", characters_over, ("verification",)),
        "specimen-nodes": closing(head, "REPORT-NODES-TAIL", nodes_over),
        "specimen-depth": closing(head, "REPORT-DEPTH-TAIL", depth_over),
        "specimen-windowed": closing(head, "REPORT-WINDOWED-TAIL", long_cards(28)),
        "specimen-changes": changes,
        "specimen-blocks": closing(head, "REPORT-BLOCKS-TAIL", blocks_over),
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
        {"bytes": 3194, "lines": 105, "max_line_bytes": 251, "code_points": 3177, "findings": 3},
        {"bytes": 4458, "lines": 131, "max_line_bytes": 251, "code_points": 4429, "findings": 3},
    ],
    "reader-report": [
        {"bytes": 1756, "lines": 77, "max_line_bytes": 111, "code_points": 1744, "findings": 1,
         "marker": {"text": "**one** &amp; two 🧪", "scalar": 1140, "byte": 1149}},
        {"bytes": 1604, "lines": 64, "max_line_bytes": 213, "code_points": 1597, "findings": 0},
    ],
    "specimen-bytes": [
        {"bytes": 1077324, "lines": 832, "max_line_bytes": 2050, "code_points": 1077202, "findings": 22,
         "marker": {"text": "**one** &amp; two 🧪", "scalar": 1077164, "byte": 1077283}},
    ],
    "specimen-lines": [
        {"bytes": 131841, "lines": 10124, "max_line_bytes": 111, "code_points": 131835, "findings": 0,
         "marker": {"text": "REPORT-LINES-TAIL", "scalar": 131817, "byte": 131823}},
    ],
    "specimen-line": [
        {"bytes": 27823, "lines": 72, "max_line_bytes": 26340, "code_points": 23014, "findings": 1,
         "marker": {"text": "REPORT-LINE-TAIL", "scalar": 22997, "byte": 27806}},
    ],
    "specimen-edges": [
        {"bytes": 11692, "lines": 724, "max_line_bytes": 111, "code_points": 11683, "findings": 1,
         "marker": {"text": "REPORT-EDGES-TAIL", "scalar": 11665, "byte": 11674}},
    ],
    "specimen-characters": [
        {"bytes": 33935, "lines": 120, "max_line_bytes": 593, "code_points": 33929, "findings": 0,
         "marker": {"text": "REPORT-CHARACTERS-TAIL", "scalar": 33906, "byte": 33912}},
    ],
    "specimen-nodes": [
        {"bytes": 100756, "lines": 138, "max_line_bytes": 8187, "code_points": 100732, "findings": 6,
         "marker": {"text": "REPORT-NODES-TAIL", "scalar": 100714, "byte": 100738}},
    ],
    "specimen-depth": [
        {"bytes": 1742, "lines": 72, "max_line_bytes": 264, "code_points": 1733, "findings": 1,
         "marker": {"text": "REPORT-DEPTH-TAIL", "scalar": 1715, "byte": 1724}},
    ],
    "specimen-windowed": [
        {"bytes": 70213, "lines": 396, "max_line_bytes": 781, "code_points": 70123, "findings": 28,
         "marker": {"text": "REPORT-WINDOWED-TAIL", "scalar": 70102, "byte": 70192}},
    ],
    "specimen-changes": [
        {"bytes": 30644, "lines": 204, "max_line_bytes": 781, "code_points": 30602, "findings": 12,
         "marker": {"text": "REPORT-CHANGES-TAIL", "scalar": 30582, "byte": 30624}},
        {"bytes": 28441, "lines": 194, "max_line_bytes": 781, "code_points": 28401, "findings": 11,
         "marker": {"text": "REPORT-CHANGES-TAIL", "scalar": 28381, "byte": 28421}},
    ],
    "specimen-blocks": [
        {"bytes": 100642, "lines": 2460, "max_line_bytes": 167, "code_points": 100036, "findings": 200,
         "marker": {"text": "REPORT-BLOCKS-TAIL", "scalar": 100017, "byte": 100623}},
    ],
}
