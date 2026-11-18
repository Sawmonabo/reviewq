"""Deterministic review observations for the development fixture presets."""

import json

from .bundle import encode_json, index_files
from .native import fixture_id
from .source import STAMP, worked_files


PRESETS = ("empty", "worked", "busy", "busy-filter", "reader", "rich")


def busy_review(i):
    project_id, project = (11, "alpha") if i % 2 else (22, "beta")
    return {
        "project": project, "project_id": project_id, "iid": 200 + i, "ticket": f"RQ-{200 + i // 2}",
        "title": f"Review the parser {i % 15:02d}" if i < 27 else f"Synthetic review {i}",
        "head": f"{10000 + i:040x}", "base": f"{11000 + i:040x}", "start": f"{12000 + i:040x}",
        "path": "src/example.ts", "patch": "quiet.patch", "alias": f"review-{i}",
    }


def busy_session(i, reviews):
    review = reviews[0] if i in (5, 6) else reviews[i % (len(reviews) - 2)]
    return {
        "alias": f"busy-{i}", "review": review["alias"], "provider": "codex",
        "state": "waiting_input" if i == 5 else "completed", "parent": None,
    }


def busy_outbox(sessions):
    leads = [session for session in sessions if session["parent"] is None]
    return [
        {"submission_id": fixture_id("submission", str(i)), "lead": lead["alias"],
         "review": lead["review"], "operation": "message", "state": "queued",
         "text": f"Queued fixture message {i}.", "created_at": STAMP}
        for i, lead in enumerate(leads[:200])
    ]


def filtering_history():
    return [
        {"lead": "active", "id": f"filter-item-{i}", "turn": f"filter-turn-{i}",
         "text": (f"Filtering marker {i}. " + "x" * 12000)[:12000], "observed_at": STAMP}
        for i in range(200)
    ]


def initial_source(preset):
    source = {} if preset == "empty" else worked_files()
    reviews, sessions, history, discussions, timeline = [
        json.loads(source.get(f"{name}.json", b"[]"))
        for name in ("reviews", "sessions", "history", "discussions", "timeline")
    ]
    outbox = []
    if preset in ("busy", "busy-filter"):
        target = 300 if preset == "busy-filter" else 67
        reviews.extend(busy_review(i) for i in range(len(reviews), target))
        for review, branch in zip(reviews[-2:], ("feature/RQ-641-fix", "feature/RQ-6410-fix")):
            review.update({"ticket": None, "state": "closed", "title": "Parser cleanup",
                           "description": "General code maintenance.", "branch": branch})
        reviews[3]["ticket"] = "RQ-41"
        reviews[4]["description"] = None
        sessions.extend(busy_session(i, reviews) for i in range(len(sessions), 506))
        outbox = busy_outbox(sessions)
        timeline.append({"action": "duplicate-last-event"})
        if preset == "busy-filter":
            history = filtering_history()
    if preset in ("reader", "rich"):
        sessions.extend([
            {"alias": "reader", "review": "review-2", "provider": "codex", "state": "working", "parent": None},
            {"alias": "reader-child", "review": "review-2", "provider": "codex",
             "state": "completed", "parent": "reader"},
            {"alias": "reader-second", "review": "review-2", "provider": "codex",
             "state": "completed", "parent": None},
        ])
        timeline.append({"action": "append-seventh-page", "lead": "reader"})
    if preset == "rich":
        timeline.append({"action": "change-head", "review": "review-2"})
    values = {"reviews": reviews, "sessions": sessions, "history": history,
              "discussions": discussions, "timeline": timeline}
    if preset != "worked":
        source.update({f"{name}.json": encode_json(value) for name, value in values.items()})
        source.pop("manifest.json", None)
        source["manifest.json"] = encode_json({"version": 1, "outbox": outbox, "files": index_files(source)})
    return source, values, outbox
