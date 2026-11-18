"""Synthetic Codex app-server and Claude transcript source records."""

import json
from uuid import NAMESPACE_URL, uuid5

from .bundle import encode_lines
from .source import STAMP


def fixture_id(kind, alias):
    return str(uuid5(NAMESPACE_URL, f"reviewq.example.invalid/{kind}/{alias}"))


def registrations(sessions):
    records = {}
    for session in sessions:
        alias = session["alias"]
        parent = records.get(session["parent"])
        provider = session["provider"]
        native_id = fixture_id(provider, alias)
        records[alias] = {
            **session,
            "run_id": parent["run_id"] if parent else fixture_id("run", alias),
            "native_id": native_id,
            "native_parent_id": parent["native_id"] if parent else None,
            "source": f"native/{provider}/{native_id}.jsonl",
        }
    return list(records.values())


def codex_event(native_id, turn, item, phase="completed"):
    return {"method": f"item/{phase}", "params": {"threadId": native_id, "turnId": turn, "item": item}}


def codex_message(native_id, turn, item_id, text, *, user=False, client_id=None):
    if user:
        item = {"type": "userMessage", "id": item_id, "content": [
            {"type": "text", "text": text, "text_elements": []},
        ]}
        if client_id is not None:
            item["clientUserMessageId"] = client_id
    else:
        item = {"type": "agentMessage", "id": item_id, "text": text, "phase": None}
    return codex_event(native_id, turn, item)


def claude_message(native_id, index, text):
    is_user = index == 0
    role = "user" if is_user else "assistant"
    record = {
        "type": role, "sessionId": native_id, "uuid": fixture_id("claude-message", f"{native_id}-{index}"),
        "parentUuid": fixture_id("claude-message", f"{native_id}-{index-1}") if index else None,
        "isSidechain": False, "timestamp": STAMP, "cwd": "/fixture/source",
        "message": {"role": role, "content": [{"type": "text", "text": text}]},
    }
    if not is_user:
        record["message"].update({"id": f"msg_fixture_{index}", "type": "message", "model": "fixture-claude",
                                  "stop_reason": "end_turn", "stop_sequence": None, "usage": {
                                      "input_tokens": 1, "output_tokens": 4,
                                  }})
    return record


def observation_event(native_id, entry):
    return codex_message(native_id, entry["turn"], entry["id"], entry["text"])


def reader_events(native_id):
    for index in range(10000):
        turn = f"reader-turn-{index // 25}"
        identity = f"reader-item-{index}"
        if index in (25, 50):
            yield codex_message(native_id, turn, identity, "Review the parser again.", user=True)
        elif index % 25 == 5:
            command = {"type": "commandExecution", "id": identity, "command": "git status --short",
                       "cwd": "/fixture/source", "processId": None, "commandActions": [],
                       "source": "agent", "status": "inProgress", "aggregatedOutput": None,
                       "exitCode": None, "durationMs": None}
            yield codex_event(native_id, turn, command, phase="started")
            text = f"reader-result-{index}"
            if index == 5:
                text = "O" * (320 * 1024 - len("\nREADER-OUTPUT-TAIL")) + "\nREADER-OUTPUT-TAIL"
            yield codex_event(native_id, turn, {
                **command, "status": "completed", "aggregatedOutput": text, "exitCode": 0, "durationMs": 2,
            })
        else:
            yield codex_message(native_id, turn, identity, f"Reader marker {index}.",
                                user=index % 25 == 0)


def native_inventory(rows, observations):
    observations_by_alias = {}
    for entry in observations:
        observations_by_alias.setdefault(entry["lead"], []).append(entry)
    files = {}
    for record in rows:
        alias = record["alias"]
        identity = record["native_id"]
        entries = observations_by_alias.get(alias, ())
        if record["provider"] == "claude":
            items = [claude_message(identity, 0, "Read the selected parser.")]
            items.extend(claude_message(identity, i, entry["text"]) for i, entry in enumerate(entries, 1))
        elif alias == "reader":
            items = reader_events(identity)
        elif alias == "large":
            items = (codex_message(identity, f"large-turn-{i}", f"large-item-{i}", f"Large history {i}.")
                     for i in range(240))
        elif entries:
            items = (observation_event(identity, entry) for entry in entries)
        else:
            items = [codex_message(identity, f"{alias}-turn-0", f"{alias}-item-0", f"Visible text for {alias}.")]
        files[record["source"]] = encode_lines(items)
    return files


def decode_native(data):
    return [json.loads(line) for line in data.splitlines()]
