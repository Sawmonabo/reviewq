"""Raw patch, report, and Jira specimens."""

from .bundle import record_extents, sha256
from .source import REVIEW_SOURCES


QUOTE = "**one** &amp; two 🧪"
QUOTE_TEXT = "one & two 🧪"
ACTIVE_TICKET = "RQ-41"
QUIET_RECEIPT = "fixture-quiet-report-1"
RICH_REPORT = "reports/rich.md"


def mixed_patch():
    lines = [
        "@@ -1,240 +1,241 @@",
        " \tconst shared = 'keep';",
        "-\tconst mode = 'old';",
        "+\tconst mode = 'new';",
        "+\tconst wrapped = '" + "漢字 🧪 café אבג \u202brtl\u202c " * 24 + "';",
    ]
    lines.extend(f" \tconst mixed{i} = {i};" for i in range(3, 241))
    lines.append(r"\ No newline at end of file")
    return "\n".join(lines).encode("utf-8")


def generated_patch(count, content):
    header = f"@@ -0,0 +1,{count} @@\n".encode("ascii")
    return header + b"".join(b"+" + content + b"\n" for _ in range(count - 1)) + b"+patch-tail-marker"


def patch_documents(preset):
    documents = {"patches/mixed.patch": mixed_patch()}
    if preset == "rich":
        documents.update({
            "patches/empty.patch": b"",
            "patches/over-bytes.patch": generated_patch(550, b"x" * 4095),
            "patches/over-lines.patch": generated_patch(20001, b"line budget specimen"),
            "patches/unicode-line.patch": "@@ -0,0 +1,1 @@\n+".encode("ascii")
                                         + ("🧪" * 4200 + " unicode-line-tail").encode("utf-8"),
            "patches/crlf-limit.patch": b"@@ -0,0 +1,1 @@\r\n+" + b"x" * 16383 + b"\r\n",
        })
    return documents


def quote_range(body, last=False):
    offset = body.rindex(QUOTE) if last else body.index(QUOTE)
    prefix = body[:offset].encode("utf-8")
    raw = QUOTE.encode("utf-8")
    return {"start": offset, "end": offset + len(QUOTE), "byte_start": len(prefix),
            "byte_end": len(prefix) + len(raw), "source": QUOTE, "text": QUOTE_TEXT,
            "sha256": sha256(raw)}


def report_sources(reviews, preset):
    if not reviews:
        return {}, []
    specimen = (REVIEW_SOURCES / "specimens" / "report.md").read_text(encoding="utf-8")
    bodies = {"reports/quiet.md": specimen}
    aliases = {"reports/quiet.md": ("quiet", QUIET_RECEIPT, "review-1")}
    if preset in ("reader", "rich"):
        bodies.update({
            "reports/reader.md": specimen,
            "reports/reader-updated.md": specimen.replace("Request changes", "Approve")
                                                .replace("until the parser validates an empty instruction", "the parser changes"),
        })
        aliases.update({
            "reports/reader.md": ("reader", "fixture-reader-report-1", "review-2"),
            "reports/reader-updated.md": ("reader", "fixture-reader-report-2", "review-2"),
        })
    if preset == "rich":
        bodies.update({
            RICH_REPORT: specimen + "\n" + ("x" * 2030 + "\n") * 520
                         + "\n🧪 Quote at end. " + QUOTE + "\nREPORT-BYTE-TAIL\n",
            "reports/over-lines.md": specimen + "\n" + "Report line.\n" * 10001 + "REPORT-LINES-TAIL\n",
            "reports/unicode-line.md": specimen + "\n" + "🧪" * 4200 + " REPORT-LINE-TAIL\n",
            "reports/over-diagram.md": specimen + "\n~~~mermaid\nflowchart LR\n"
                                       + "\n".join(f"  N{i} --> N{i + 1}" for i in range(650))
                                       + "\n~~~\n",
            "reports/over-diagram-characters.md": specimen + "\n~~~mermaid\nflowchart LR\n"
                                                  + ("  %% " + "diagram label " * 42 + "\n") * 55
                                                  + "  A --> B\n~~~\n",
        })
        aliases.update({name: ("large", f"fixture-rich-report-{index}", "review-2")
                        for index, name in enumerate(bodies) if name not in aliases})
    by_alias = {row["alias"]: row for row in reviews}
    records = []
    for name, body in bodies.items():
        lead, receipt, review = aliases[name]
        record = {"lead": lead, "review": review, "receipt_id": receipt, "source": name,
                  "head": by_alias[review]["head"], "quote": quote_range(body, last=name == RICH_REPORT)}
        record.update(record_extents(body.encode("utf-8")))
        records.append(record)
    return {name: body.encode("utf-8") for name, body in bodies.items()}, records


def jira_wiki():
    return (REVIEW_SOURCES / "specimens" / "jira-wiki.txt").read_text(encoding="utf-8")


def clipboard_sources():
    lines = ["\tconst mode = 'old';", "\tconst mode = 'new';", "\tconst wrapped = '"]
    lines[2] += "漢字 🧪 café אבג \u202brtl\u202c " * 24 + "';"
    return {"lines": lines, "utf8_hex": "\n".join(lines).encode("utf-8").hex(),
            "accessible_order": ["Removed line 2", "Added line 2", "Added line 3"]}
