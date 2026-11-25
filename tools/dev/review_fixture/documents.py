"""Raw patch and Jira specimens."""

from .source import REVIEW_SOURCES


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


def jira_wiki():
    return (REVIEW_SOURCES / "specimens" / "jira-wiki.txt").read_text(encoding="utf-8")


def clipboard_sources():
    lines = ["\tconst mode = 'old';", "\tconst mode = 'new';", "\tconst wrapped = '"]
    lines[2] += "漢字 🧪 café אבג \u202brtl\u202c " * 24 + "';"
    return {"lines": lines, "utf8_hex": "\n".join(lines).encode("utf-8").hex(),
            "accessible_order": ["Removed line 2", "Added line 2", "Added line 3"]}
