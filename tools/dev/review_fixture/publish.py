"""Run the ledger plans against a development service and measure what it published."""

import json
from urllib.parse import quote
from uuid import NAMESPACE_URL, uuid5


class Refused(Exception):
    pass


def run_plan(service, plan, native_id):
    """Links the plan's lead, runs its turns and dispositions, and returns the run with the
    receipt of every version its closing calls published, oldest first."""
    review_id, run = service.link(native_id, service.reviews[plan["review"]])
    lead = plan["lead"]
    versions, turns, calls, sequence = [], 0, 0, 0
    for step in plan["steps"]:
        if "disposition" in step:
            save_disposition(service, run, lead, step["disposition"])
            continue
        turns += 1
        turn = f"{lead}-turn-{turns}"
        sequence += 1
        service.heartbeat(native_id, "working", sequence, turn)
        for item in step["turn"]:
            calls += 1
            reply = service.ledger(native_id, item["tool"], item["input"], f"{lead}-call-{calls}",
                                   agent=item.get("agent"), turn=turn)
            if "data" not in reply:
                refusal = reply.get("fieldErrors") or reply.get("error")
                raise Refused(f"{lead} call {calls} ({item['tool']}) was refused: {json.dumps(refusal)}")
            if item["tool"] == "submit_review":
                versions.append(reply["data"]["receiptId"])
        sequence += 1
        service.heartbeat(native_id, "idle", sequence, turn)
    return {"lead": lead, "review_id": review_id, "run_id": run, "versions": versions}


def save_disposition(service, run, lead, choice):
    """Saves the reviewer's disposition on the current version, as Findings does."""
    context = service.get(f"/api/v1/runs/{run}/report/context")
    current = service.get(f"/api/v1/runs/{run}/report")["report"]
    patch = {key: choice[key] for key in ("blocking", "publication", "reason") if key in choice}
    body = {"submissionId": str(uuid5(NAMESPACE_URL, f"reviewq.example.invalid/disposition/{lead}/{choice['finding']}")),
            "expectedVersion": context["userDispositionVersion"], "receiptId": current["receiptId"],
            "findingId": choice["finding"], "disposition": patch}
    status, reply = service.mutate("PUT", f"/api/v1/runs/{run}/report/dispositions", body)
    if status != 200:
        raise Refused(f"{lead} disposition on {choice['finding']} returned {status}: {json.dumps(reply)}")


def measure(service, run, receipt, marker):
    """The stored version's own measurements, its summary's, and where `marker` begins."""
    base = f"/api/v1/runs/{run}/report/versions/{receipt}"
    summary = service.get(f"{base}?view=summary")
    summary = summary.get("report", summary)
    status, headers, data = service.request("GET", f"{base}/source?download=1")
    if status != 200:
        raise Refused(f"source download for {receipt} returned {status}")
    text = data.decode("utf-8")
    lines = text.split("\n")
    if text.endswith("\n"):
        lines.pop()
    measured = {"bytes": len(data), "lines": len(lines),
                "max_line_bytes": max((len(line.removesuffix("\r").encode()) for line in lines), default=0),
                "code_points": len(text), "findings": summary["findingCount"]}
    problems = [f"summary {name} {summary[key]} differs from the stored bytes' {measured[name]}"
                for name, key in (("bytes", "byteLength"), ("lines", "lineCount"),
                                  ("max_line_bytes", "maxLineBytes"), ("code_points", "totalCodePoints"))
                if summary[key] != measured[name]]
    if headers.get("etag", headers.get("ETag", "")).strip('"') != summary["sha256"]:
        problems.append("the download's ETag is not the summary's SHA-256")
    if marker:
        offset = text.find(marker)
        if offset < 0:
            problems.append(f"marker {marker!r} is missing")
        else:
            measured["marker"] = {"text": marker, "scalar": offset, "byte": len(text[:offset].encode())}
            section = service.get(f"/api/v1/runs/{run}/report/section?receiptId={quote(receipt)}"
                                  f"&offset={offset}&characters={len(marker)}")
            if section["text"] != marker:
                problems.append(f"the section route at {offset} returned {section['text']!r}")
    return measured, problems


def verify(service, result, markers, expected):
    """Measures each published version and compares it with the pinned values. Returns the
    measurements and every difference."""
    report = []
    problems = []
    for index, receipt in enumerate(result["versions"]):
        marker = markers[index] if index < len(markers) else None
        measured, found = measure(service, result["run_id"], receipt, marker)
        pinned = expected[index] if index < len(expected) else None
        if pinned is not None and pinned != measured:
            found.append(f"pinned {json.dumps(pinned)} but measured {json.dumps(measured)}")
        problems.extend(f"{result['lead']} version {index + 1}: {problem}" for problem in found)
        report.append({"receipt": receipt, **measured})
    return report, problems
