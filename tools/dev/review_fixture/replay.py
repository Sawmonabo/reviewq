"""Thread-safe fixture controls and native observations."""

import json
import re
from collections import Counter, deque
from threading import Condition, Event
from uuid import UUID

from .bundle import encode_lines, read_manifest
from .gitlab import CHANGED_HEAD, discussion
from .native import codex_message, fixture_id, observation_event


MAX_NATIVE_MESSAGE_SCALARS = 12_000


class ReplayError(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


class Replay:
    def __init__(self, directory, profile):
        self.directory = directory
        self.provider = directory / "provider"
        self.source_files = read_manifest(directory)["files"]
        self.provider_files = read_manifest(self.provider)["files"]
        settings = self.provider_json("presets.json")
        self.preset = settings["preset"]
        self.profile = profile
        try:
            self.settings = settings["profiles"][profile]
        except KeyError as error:
            raise ValueError(f"profile {profile!r} is not defined for the {self.preset!r} preset") from error
        self.gitlab = self.provider_json("gitlab.json")
        self.jira = self.provider_json("jira.json")
        self.reviews = {row["alias"]: row for row in json.loads(self.input_bytes("../reviews.json"))}
        self.registrations = {row["alias"]: row for row in self.provider_json("registrations.json")
                              if row["alias"] in self.settings["registrations"]}
        self.timeline = self.provider_json("timeline.json")
        self.step_index = 0
        self.condition = Condition()
        self.counts = Counter()
        self.observers = Counter()
        self.gates = {}
        self.errors = {}
        self.drop_next = set()
        self.disconnected = 0
        self.sequence = 0
        self.events = deque(maxlen=1024)
        self.native_append = {}
        self.receipts = {}

    def input_bytes(self, name):
        if name.startswith("../"):
            name = name[3:]
            directory, allowed = self.directory, self.source_files
        else:
            directory, allowed = self.provider, self.provider_files
        if name not in allowed:
            raise ValueError(f"source {name!r} is absent from the fixture manifest")
        return (directory / name).read_bytes()

    def provider_json(self, name):
        return json.loads(self.input_bytes(name))

    def authority(self, path):
        with self.condition:
            key = path if path in self.counts or len(self.counts) < 512 else "(additional paths)"
            self.counts[key] += 1
            return self.errors.pop(path, None)

    def await_release(self, path, target):
        with self.condition:
            for gate_id, (matches, event, waiting) in self.gates.items():
                if matches in (path, target) and not waiting:
                    self.gates[gate_id] = (matches, event, True)
                    break
            else:
                return
        allowed = event.wait(timeout=20)
        with self.condition:
            self.gates.pop(gate_id, None)
        if not allowed:
            raise ReplayError(504, "Fixture response hold expired.")

    def review(self, alias):
        with self.condition:
            return self.gitlab["merge_requests"][alias]

    def reviews_snapshot(self):
        with self.condition:
            return list(self.gitlab["merge_requests"].values())

    def registration(self, alias):
        if alias not in self.registrations:
            raise ReplayError(404, "Fixture session is unavailable.")
        return self.registrations[alias]

    def native(self, alias):
        record = self.registration(alias)
        with self.condition:
            return self.input_bytes(record["source"]) + encode_lines(self.native_append.get(alias, []))

    def publish(self, kind, **data):
        self.sequence += 1
        event = {"source_event_id": self.sequence, "kind": kind, **data}
        self.events.append((self.sequence, event))
        self.condition.notify_all()

    def append(self, alias, items):
        self.native_append.setdefault(alias, []).extend(items)
        for item in items:
            self.publish("native", lead=alias, record=item)

    def status(self):
        with self.condition:
            return {
                "preset": self.preset, "profile": self.profile, "timeline_position": self.step_index,
                "timeline_steps": len(self.timeline), "event_sequence": self.sequence,
                "request_counts": dict(sorted(self.counts.items())),
                "observers": {key: count for key, count in sorted(self.observers.items()) if count},
                "holds": [{"id": key, "path": value[0], "waiting": value[2]} for key, value in self.gates.items()],
                "submissions_accepted": len(self.receipts),
            }

    def submit(self, alias, payload):
        record = self.registration(alias)
        if record["provider"] != "codex" or record["parent"] is not None:
            raise ReplayError(409, "Choose a Codex fixture lead.")
        if payload.get("threadId") != record["native_id"]:
            raise ReplayError(409, "The fixture lead and native thread differ.")
        try:
            submission_id = str(UUID(payload["clientUserMessageId"]))
            text_input, = payload["input"]
            if text_input["type"] != "text":
                raise ValueError("expected text input")
            message = text_input["text"]
        except (KeyError, TypeError, ValueError, AttributeError) as error:
            raise ReplayError(400, "Expected Codex turn/start parameters with one text input.") from error
        if not isinstance(message, str) or len(message) > MAX_NATIVE_MESSAGE_SCALARS or not message.strip():
            raise ReplayError(400, f"Expected nonblank text of up to {MAX_NATIVE_MESSAGE_SCALARS} Unicode scalars.")
        try:
            message.encode("utf-8")
        except UnicodeEncodeError as error:
            raise ReplayError(400, "Expected text containing only Unicode scalar values.") from error
        key = (alias, submission_id)
        with self.condition:
            if key in self.receipts:
                original, receipt = self.receipts[key]
                if message != original:
                    raise ReplayError(409, "The submission UUID already has different text.")
                return receipt, False
            if len(self.receipts) >= 4096:
                raise ReplayError(429, "Fixture submission limit reached.")
            native_id = record["native_id"]
            item_id = fixture_id("input", f"{native_id}/{submission_id}")
            turn_id = fixture_id("turn", f"{native_id}/{submission_id}")
            receipt = {
                "submissionId": submission_id, "threadId": native_id, "turnId": turn_id,
                "itemId": item_id, "state": "accepted",
            }
            self.receipts[key] = (message, receipt)
            self.append(alias, [codex_message(native_id, turn_id, item_id, message, user=True,
                                              client_id=submission_id)])
            drop = alias in self.drop_next
            self.drop_next.discard(alias)
            return receipt, drop

    def receipt(self, alias, submission_id):
        self.registration(alias)
        with self.condition:
            accepted = self.receipts.get((alias, submission_id))
        if accepted is None:
            raise ReplayError(404, "Fixture submission was not observed.")
        return accepted[1]

    def disconnect(self):
        self.disconnected += 1
        self.condition.notify_all()

    def control(self, payload):
        with self.condition:
            action = payload.get("action")
            if action == "step":
                self.advance()
            elif action == "hold":
                gate_id = payload.get("id")
                route = payload.get("path")
                if (not isinstance(gate_id, str) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", gate_id)
                        or not isinstance(route, str) or not route.startswith(("/api/v4/", "/rest/"))):
                    raise ReplayError(400, "Expected a hold id and provider request path.")
                if gate_id in self.gates or len(self.gates) >= 12:
                    raise ReplayError(409, "The hold already exists or the limit has been reached.")
                self.gates[gate_id] = (route, Event(), False)
            elif action == "release":
                gate_id = payload.get("id")
                gate = self.gates.get(gate_id) if isinstance(gate_id, str) else None
                if gate is None or not gate[2]:
                    raise ReplayError(409, "The hold has not captured a response.")
                gate[1].set()
            elif action == "fail-next":
                route = payload.get("path")
                status = payload.get("status")
                if (not isinstance(route, str) or not route.startswith(("/api/v4/", "/rest/"))
                        or status not in (401, 403, 429, 503)):
                    raise ReplayError(400, "Expected a provider path and status 401, 403, 429, or 503.")
                if len(self.errors) >= 12:
                    raise ReplayError(409, "The provider error limit has been reached.")
                self.errors[route] = status
            elif action == "drop-next-ack":
                alias = payload.get("lead")
                if not isinstance(alias, str):
                    raise ReplayError(400, "Expected a fixture lead alias.")
                self.registration(alias)
                self.drop_next.add(alias)
            elif action == "disconnect":
                self.disconnect()
            else:
                raise ReplayError(400, "Unknown fixture control.")
            return self.status()

    def advance(self):
        if self.step_index == len(self.timeline):
            raise ReplayError(409, "Fixture timeline is complete.")
        cue = self.timeline[self.step_index]
        action = cue["action"]
        if action == "invalidate":
            self.publish("invalidate", review=cue["review"])
        elif action == "add-thread":
            alias = cue["review"]
            existing = self.review(alias)
            ordinal = 700 + self.step_index
            added = discussion(self.reviews[alias], ordinal, f"{alias}-timeline-{ordinal}",
                               new_line=cue["new_line"])
            self.gitlab["merge_requests"][alias] = {
                **existing, "discussions": [*existing["discussions"], added],
            }
            self.publish("discussions", review=alias)
        elif action == "append":
            entry = cue["entry"]
            alias = entry["lead"]
            identity = self.registration(alias)["native_id"]
            self.append(alias, [observation_event(identity, entry)])
        elif action == "close-stream":
            self.disconnect()
        elif action == "change-head":
            alias = cue["review"]
            existing = self.review(alias)
            new_refs = {**existing["review"]["diff_refs"], "head_sha": CHANGED_HEAD}
            updated = {**existing["review"], "sha": CHANGED_HEAD, "diff_refs": new_refs}
            prior = self.reviews[alias]
            shifted = discussion(prior, 990, "large-changed-side", new_line=4, mixed=True, head=CHANGED_HEAD)
            self.gitlab["merge_requests"][alias] = {
                **existing, "review": updated, "discussions": [*existing["discussions"], shifted],
            }
            self.publish("revision", review=alias)
        elif action == "duplicate-last-event":
            if self.events:
                self.sequence += 1
                self.events.append((self.sequence, self.events[-1][1]))
                self.condition.notify_all()
        elif action == "append-seventh-page":
            alias = cue["lead"]
            identity = self.registration(alias)["native_id"]
            items = [codex_message(identity, f"reader-turn-{i // 25}", f"reader-item-{i}",
                                   f"Reader marker {i}.", user=i % 25 == 0)
                     for i in range(10000, 10100)]
            self.append(alias, items)
        else:
            raise ValueError(f"unsupported timeline action {action!r}")
        self.step_index += 1

    def stream(self, scope, after):
        with self.condition:
            started = self.disconnected
            self.observers[scope] += 1
        try:
            while True:
                with self.condition:
                    if self.sequence <= after and self.disconnected == started:
                        self.condition.wait(timeout=3)
                    if self.disconnected != started:
                        return
                    batch = [item for item in self.events if item[0] > after]
                    after = self.sequence
                if scope != "all":
                    batch = [item for item in batch if item[1].get("lead") == scope]
                if batch:
                    yield from batch
                else:
                    yield None
        finally:
            with self.condition:
                self.observers[scope] -= 1
