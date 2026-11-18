"""GitLab REST source records for the deterministic review workloads."""

import hashlib

from .source import STAMP


USER = {"id": 101, "username": "fixture-reviewer", "name": "Fixture Reviewer", "state": "active"}
AUTHOR = {"id": 102, "username": "fixture-author", "name": "Fixture Author", "state": "active"}
PROJECTS = [{"id": number, "name": name, "path": name, "path_with_namespace": f"forge/{name}",
             "namespace": {"id": 90, "name": "Forge", "path": "forge", "full_path": "forge"},
             "default_branch": "main", "visibility": "private",
             "permissions": {"project_access": {"access_level": 40}, "group_access": None}}
            for number, name in ((11, "alpha"), (22, "beta"))]
CHANGED_HEAD = "492fb4ff1ac94f01a04828d7ba268713dad812c7"
MIXED_PATH = "src/example-mixed.ts"
OLD_PATH = "src/example-old.ts"


def refs(review):
    return {f"{key}_sha": review[key] for key in ("base", "start", "head")}


def mr_number(review):
    return review["project_id"] * 10000 + review["iid"]


def mr_record(review):
    ticket = review["ticket"]
    return {
        "id": mr_number(review), "iid": review["iid"], "project_id": review["project_id"],
        "source_project_id": review["project_id"], "target_project_id": review["project_id"],
        "title": review["title"], "state": review.get("state", "opened"), "author": AUTHOR,
        "description": review.get("description", f"Fixture for {ticket}."), "reviewers": [USER],
        "assignees": [USER], "assignee": USER, "target_branch": "main",
        "source_branch": review.get("branch", f"feature/{ticket}-parser"),
        "created_at": STAMP, "updated_at": STAMP, "draft": False, "work_in_progress": False,
        "merge_status": "can_be_merged", "detailed_merge_status": "mergeable",
        "sha": review["head"], "diff_refs": refs(review), "labels": [],
        "references": {"short": f"!{review['iid']}", "full": f"forge/{review['project']}!{review['iid']}"},
    }


def diff_record(old, new, source, **flags):
    return {
        "source": source, "old_path": old, "new_path": new, "a_mode": "100644", "b_mode": "100644",
        "new_file": False, "renamed_file": False, "deleted_file": False, "generated_file": False,
        "collapsed": False, "too_large": False, **flags,
    }


def diff_files(review, preset):
    path = review["path"]
    files = [diff_record(path, path, f"../{review['patch']}", new_file=True)]
    if review["alias"] != "review-2":
        return files
    files.append(diff_record(OLD_PATH, MIXED_PATH, "patches/mixed.patch", renamed_file=True))
    if preset == "rich":
        for name in ("empty", "over-bytes", "over-lines", "unicode-line", "crlf-limit"):
            new_path = f"src/{name}.ts"
            files.append(diff_record(new_path, new_path, f"patches/{name}.patch", new_file=True))
        binary = "assets/parser.bin"
        files.append(diff_record(binary, binary, None, binary=True))
        oversized = "src/provider-limited.ts"
        files.append(diff_record(oversized, oversized, None, too_large=True))
        collapsed = "src/provider-collapsed.ts"
        files.append(diff_record(collapsed, collapsed, None, collapsed=True))
    return files


def discussion_id(alias):
    return hashlib.sha256(alias.encode("utf-8")).hexdigest()[:40]


def discussion(review, index, alias, *, new_line=None, old_line=None, mixed=False,
               head=None, body=None, reply=None):
    position = None
    if new_line is not None or old_line is not None:
        position = {
            **refs(review), "position_type": "text", "new_line": new_line, "old_line": old_line,
            "old_path": OLD_PATH if mixed else review["path"],
            "new_path": MIXED_PATH if mixed else review["path"],
            "line_range": None,
        }
        if head is not None:
            position["head_sha"] = head
    note = {
        "id": mr_number(review) * 1000 + index, "type": "DiffNote" if position else "DiscussionNote",
        "body": body or f"Review discussion {alias}.", "author": USER, "created_at": STAMP,
        "updated_at": STAMP, "system": False, "resolvable": True, "resolved": False,
        "noteable_id": mr_number(review), "noteable_type": "MergeRequest", "noteable_iid": review["iid"],
        "project_id": review["project_id"], "confidential": False, "position": position,
    }
    notes = [note]
    if reply is not None:
        notes.append({**note, "id": note["id"] + 500, "author": AUTHOR, "body": reply, "position": None})
    return {"id": discussion_id(alias), "individual_note": False, "notes": notes}


def initial_discussions(review, entries, preset):
    threads = [
        discussion(review, i + 1, entry["id"], new_line=entry["new_line"])
        for i, entry in enumerate(entries) if entry["review"] == review["alias"]
    ]
    if review["alias"] != "review-2" or preset != "rich":
        return threads
    threads.append(discussion(review, 10, "large-general", body="Where should the parser report errors?"))
    threads.append(discussion(review, 11, "large-old-side", old_line=2, mixed=True))
    threads.append(discussion(review, 12, "large-older", new_line=3, mixed=True, head="f" * 40))
    for i in range(200):
        reply = None
        if i == 7:
            reply = "A longer answer about the parser.\n\n" + "Follow the selected input. " * 125
            reply += "\n\n~~~mermaid\nflowchart LR\n  User --> Parser --> Review\n~~~\n"
        threads.append(discussion(review, 20 + i, f"mixed-{i}", new_line=i + 2, mixed=True, reply=reply))
    return threads


def gitlab_sources(source, preset):
    records = {}
    for review in source["reviews"]:
        records[review["alias"]] = {
            "review": mr_record(review),
            "diffs": diff_files(review, preset),
            "discussions": initial_discussions(review, source["discussions"], preset),
        }
    return {"user": USER, "projects": PROJECTS, "merge_requests": records}
