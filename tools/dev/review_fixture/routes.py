"""GitLab and Jira HTTP read routes over the synthetic provider sources."""

from dataclasses import dataclass, field
import re
from urllib.parse import unquote

from .bundle import encode_json
from .replay import ReplayError


@dataclass
class Response:
    body: bytes
    status: int = 200
    content_type: str = "application/json; charset=utf-8"
    headers: dict = field(default_factory=dict)


def json_response(value, status=200, headers=None):
    return Response(encode_json(value), status, headers={} if headers is None else headers)


def query_number(query, name, default, minimum=0, maximum=100000):
    try:
        number = int(query.get(name, [default])[-1])
    except ValueError as error:
        raise ReplayError(400, f"Expected an integer {name} parameter.") from error
    return min(maximum, max(minimum, number))


def gitlab_page(values, query):
    size = query_number(query, "per_page", 20, minimum=1, maximum=100)
    page = query_number(query, "page", 1, minimum=1)
    start, stop = (page - 1) * size, page * size
    headers = {
        "X-Page": str(page), "X-Per-Page": str(size),
        "X-Next-Page": str(page + 1) if stop < len(values) else "",
        "X-Prev-Page": str(page - 1) if page > 1 else "",
        "X-Total": str(len(values)), "X-Total-Pages": str((len(values) + size - 1) // size),
    }
    return values[start:stop], headers


def project_link(origin, project):
    return f"{origin}/{project['path_with_namespace']}"


def linked_project(origin, project):
    url = project_link(origin, project)
    return {**project, "web_url": url, "http_url_to_repo": f"{url}.git"}


def linked_review(origin, project, record):
    url = f"{project_link(origin, project)}/-/merge_requests/{record['iid']}"
    return {**record, "web_url": url}


def merge_request_matches(review, query):
    for key in ("state", "source_branch", "target_branch"):
        if key in query and query[key][-1] not in ("all", review[key]):
            return False
    term = query.get("search", [""])[-1].casefold()
    return not term or any(term in (review.get(key) or "").casefold() for key in ("title", "description"))


def patch_versions(current, original, file_count):
    def version(candidate, number):
        return {
            "id": number, "head_commit_sha": candidate["head_sha"], "base_commit_sha": candidate["base_sha"],
            "start_commit_sha": candidate["start_sha"], "state": "collected", "real_size": str(file_count),
        }

    history = [version(original, 1)]
    if current != original:
        history.insert(0, version(current, 2))
    return history


def wire_diff(replay, row):
    source = row["source"]
    wire = {field: val for field, val in row.items() if field != "source"}
    if source is not None:
        patch_text = replay.input_bytes(source).decode("utf-8")
    elif row.get("binary"):
        patch_text = f"Binary files a/{row['old_path']} and b/{row['new_path']} differ\n"
    else:
        patch_text = ""
    return {**wire, "diff": patch_text}


def gitlab_review(replay, alias, record, project, operation, query, origin):
    review = record["review"]
    override = replay.settings.get("patch")
    diffs = record["diffs"]
    discussions = record["discussions"]
    if not operation:
        return json_response(linked_review(origin, project, review))
    if operation == "discussions":
        subset, headers = gitlab_page(discussions, query)
        return json_response(subset, headers=headers)
    if operation.startswith("discussions/"):
        obtain = next((row for row in discussions if row["id"] == operation[12:]), None)
        if obtain is not None:
            return json_response(obtain)
    if operation in ("approvals", "approval_state"):
        approvals = {"id": review["id"], "iid": review["iid"], "project_id": review["project_id"],
                     "approvals_required": 1, "approvals_left": 1, "approved": False, "approved_by": []}
        if operation == "approval_state":
            approvals.update({"approval_rules_overwritten": False, "rules": []})
        return json_response(approvals)
    if override and alias == override["review"]:
        diffs = [{**diffs[0], "source": override["source"]}]
    original = replay.reviews[alias]
    original_refs = {f"{name}_sha": original[name] for name in ("head", "base", "start")}
    versions = patch_versions(review["diff_refs"], original_refs, len(diffs))
    if operation == "versions":
        subset, headers = gitlab_page(versions, query)
        return json_response(subset, headers=headers)
    if operation.startswith("versions/"):
        version = next((row for row in versions if str(row["id"]) == operation[9:]), None)
        if version is not None:
            return json_response({**version, "diffs": [wire_diff(replay, row) for row in diffs], "commits": []})
    if operation == "diffs":
        page, headers = gitlab_page(diffs, query)
        return json_response([wire_diff(replay, row) for row in page], headers=headers)
    if operation == "raw_diffs":
        patches = []
        for row in diffs:
            if row["source"] is None:
                continue
            patch = replay.input_bytes(row["source"])
            head = f"diff --git a/{row['old_path']} b/{row['new_path']}\n"
            patches.append(head.encode("utf-8") + (patch if patch.endswith(b"\n") else patch + b"\n"))
        return Response(b"".join(patches), content_type="text/plain; charset=utf-8")
    raise ReplayError(404, "Fixture GitLab route was not found.")


def gitlab_get(replay, path, query, origin):
    projects = replay.gitlab["projects"]
    if path == "/api/v4/user":
        return json_response(replay.gitlab["user"])
    if path == "/api/v4/projects":
        search = query.get("search", [""])[-1].casefold()
        selected, headers = gitlab_page([linked_project(origin, row) for row in projects
                                        if search in row["path_with_namespace"].casefold()], query)
        return json_response(selected, headers=headers)
    route = re.fullmatch(r"/api/v4/projects/([^/]+)(?:/merge_requests(?:/([0-9]+)(?:/(.*))?)?)?", path)
    if route:
        locator = unquote(route[1])
        project = next((p for p in projects if locator in (str(p["id"]), p["path_with_namespace"])), None)
        if project is None:
            raise ReplayError(404, "Fixture project was not found.")
        if path.count("/") == 4:
            return json_response(linked_project(origin, project))
    else:
        project = None
        if path != "/api/v4/merge_requests":
            raise ReplayError(404, "Fixture GitLab route was not found.")
    if route and route[2] is not None:
        original = next((row for row in replay.reviews.values() if row["project_id"] == project["id"]
                         and str(row["iid"]) == route[2]), None)
        if original is None:
            raise ReplayError(404, "Fixture merge request was not found.")
        alias = original["alias"]
        return gitlab_review(replay, alias, replay.review(alias), project, route[3], query, origin)
    by_number = {p["id"]: p for p in projects}
    requested = [record["review"] for record in replay.reviews_snapshot()
                 if project is None or record["review"]["project_id"] == project["id"]]
    linked = [linked_review(origin, by_number[review["project_id"]], review)
              for review in requested if merge_request_matches(review, query)]
    page, headers = gitlab_page(linked, query)
    return json_response(page, headers=headers)


def jira_page(values, query, key, agile=False):
    start = query_number(query, "startAt", 0)
    size = query_number(query, "maxResults", 50, minimum=1, maximum=100)
    result = {"startAt": start, "maxResults": size, "total": len(values), key: values[start:start + size]}
    if agile:
        result["isLast"] = start + size >= len(values)
    return result


def jira_search(issues, query):
    jql = query.get("jql", [""])[-1]
    match = re.search(r'\bproject\s*=\s*"?([A-Z0-9_]+)"?', jql, re.IGNORECASE)
    if match and match[1].upper() not in ("RQ", "510"):
        return []
    sprint = re.search(r"\bsprint\s*=\s*(\d+)", jql, re.IGNORECASE)
    if sprint and sprint[1] != "71":
        return []
    match = re.search(r'\b(?:issuekey|key)\s*=\s*"?([A-Z]+-\d+)"?', jql, re.IGNORECASE)
    return [value for key, value in issues.items() if not match or key == match[1].upper()]


def board_issues(saved, board_id, sprint_id, query):
    if not any(str(board["id"]) == board_id for board in saved["boards"]):
        raise ReplayError(404, "Fixture Jira board was not found.")
    jql = query.get("jql", [""])[-1]
    rows = jira_search(saved["issues"], query)
    selectors = {
        "key": lambda issue: issue["key"],
        "status": lambda issue: str(issue["fields"]["status"]["id"]),
        "project": lambda issue: issue["fields"]["project"]["key"],
    }
    for field, value in selectors.items():
        clause = re.search(rf"\b{field}\s+IN\s*\(([^)]*)\)", jql, re.IGNORECASE)
        if clause:
            allowed = {part.strip().strip('"').upper() for part in clause[1].split(",")}
            rows = [row for row in rows if value(row).upper() in allowed]
    if sprint_id is not None:
        rows = [row for row in rows
                if str((row["fields"].get("sprint") or {}).get("id")) == sprint_id]
    if re.search(r"\bsprint\s+IS\s+EMPTY\b", jql, re.IGNORECASE):
        rows = [row for row in rows if not row["fields"].get("sprint")]
    start = query_number(query, "nextPageToken", 0)
    size = query_number(query, "maxResults", 50, minimum=1, maximum=100)
    end = start + size
    page = {"issues": rows[start:end], "isLast": end >= len(rows)}
    if end < len(rows):
        page["nextPageToken"] = str(end)
    return json_response(page)


def jira_get(replay, path, query, origin):
    saved = replay.jira
    if path == "/_edge/tenant_info":
        return json_response({"cloudId": saved["cloud_id"]})
    if path == "/rest/api/2/myself":
        return json_response(saved["account"])
    if path == "/rest/api/2/search":
        return json_response(jira_page(jira_search(saved["issues"], query), query, "issues"))
    if path == "/rest/api/2/filter/search":
        records = [{"id": "44", "name": "Fixture parser work", "jql": "project = RQ", "owner": saved["account"]}]
        return json_response(jira_page(records, query, "values"))
    if path == "/rest/api/2/filter/44":
        return json_response({"id": "44", "name": "Fixture parser work", "jql": "project = RQ"})
    if path == "/rest/api/2/status":
        return json_response([{"id": "3", "name": "In Progress"}])
    board = re.fullmatch(r"/rest/software/1.0/board/([0-9]+)(?:/sprint/([0-9]+))?/issue", path)
    if board:
        return board_issues(saved, board[1], board[2], query)
    if path == "/rest/agile/1.0/board":
        return json_response(jira_page(saved["boards"], query, "values", agile=True))
    if path == "/rest/agile/1.0/board/31":
        return json_response(saved["boards"][0])
    if path == "/rest/agile/1.0/board/31/configuration":
        return json_response({"id": 31, "filter": {"id": "44", "self": f"{origin}/rest/api/2/filter/44"},
                              "columnConfig": {"columns": []}, "estimation": {"type": "field",
                                                   "field": {"fieldId": "customfield_10016"}}})
    if path == "/rest/agile/1.0/board/31/features":
        return json_response({"features": [{"feature": "SPRINTS", "state": "ENABLED"}]})
    if path == "/rest/agile/1.0/board/31/sprint":
        return json_response(jira_page(saved["sprints"], query, "values", agile=True))
    if path in ("/rest/agile/1.0/sprint/71/issue", "/rest/agile/1.0/board/31/sprint/71/issue"):
        return json_response(jira_page(jira_search(saved["issues"], query), query, "issues", agile=True))
    route = re.fullmatch(r"/rest/api/2/issue/([^/]+)(/comment)?", path)
    if route:
        requested = unquote(route[1]).upper()
        issue = saved["issues"].get(requested)
        if issue is None:
            issue = next((row for row in saved["issues"].values() if row["id"] == requested), None)
        if issue is not None:
            if route[2]:
                return json_response(jira_page(saved["comments"][issue["key"]], query, "comments"))
            return json_response({**issue, "self": f"{origin}/rest/api/2/issue/{issue['key']}"})
    raise ReplayError(404, "Fixture Jira route was not found.")
