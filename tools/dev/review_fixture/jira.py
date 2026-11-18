"""Jira v2 and Agile REST source records."""

from .documents import jira_wiki
from .source import STAMP


ACCOUNT = {"accountId": "fixture-jira-reviewer", "displayName": "Fixture Reviewer", "active": True}
BOARD = {"id": 31, "name": "ReviewQ", "type": "scrum", "location": {"projectKey": "RQ"}}
SPRINT = {"id": 71, "name": "Parser sprint", "state": "active", "originBoardId": 31,
          "startDate": STAMP, "goal": "Verify parser review workflow."}


def jira_records(reviews, preset):
    tickets = sorted({row["ticket"] for row in reviews if row["ticket"]})
    if preset in ("busy", "busy-filter"):
        tickets.append("RQ-641")
    wiki = jira_wiki() if preset == "rich" else None
    issues = {}
    comments = {}
    for key in tickets:
        number = int(key.split("-")[1])
        fields = {
            "project": {"id": "510", "key": "RQ", "name": "ReviewQ"},
            "summary": f"Review parser work for {key}",
            "description": wiki if key == "RQ-43" and wiki else f"Synthetic parser work for {key}.",
            "issuetype": {"id": "1", "name": "Task", "subtask": False},
            "status": {"id": "3", "name": "In Progress", "statusCategory": {
                "id": 4, "key": "indeterminate", "name": "In Progress",
            }},
            "assignee": ACCOUNT, "reporter": ACCOUNT, "created": STAMP, "updated": STAMP,
            "customfield_10016": 0 if key == "RQ-42" else 3, "sprint": SPRINT,
        }
        if preset in ("busy", "busy-filter") and key == "RQ-641":
            fields["customfield_10016"] = "Estimate pending"
        if key == "RQ-43":
            fields.pop("customfield_10016")
        issues[key] = {"id": str(5000 + number), "key": key, "fields": fields}
        comments[key] = [] if key != "RQ-43" or not wiki else [{
            "id": "9043", "body": wiki, "author": ACCOUNT, "updateAuthor": ACCOUNT,
            "created": STAMP, "updated": STAMP,
        }]
    return {"account": ACCOUNT, "boards": [BOARD], "sprints": [SPRINT],
            "issues": issues, "comments": comments, "cloud_id": "23ae2c54-79f4-5df1-a63f-a66a5faf424f"}
