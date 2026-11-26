"""A development ReviewQ service in its own profile, driven through its production routes.

The service runs with `serve --validation` against the fixture GitLab replay and a synthetic
Claude profile, so it reads no installed profile, starts no native host and reaches no network
beyond loopback. Leads are linked with the plugin's link helper and ledger calls go through
the plugin's bridge action, `reviewq internal claude-bridge`, the production client of the
`native/ledger` routes. Reads use the browser's HTTP routes.
"""

import json
import os
import subprocess
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from .native import claude_message
from .replay import Replay
from .server import Server


ROOT = Path(__file__).resolve().parents[3]
READY_SECONDS = 60


def build_binary():
    """Builds the checkout's `reviewq` binary and returns its path.

    `CARGO_DRIVER=mbx` builds through mbx into the target folder's `mbx` directory, as the
    Makefile does, so plain Cargo never rebuilds what mbx cached.
    """
    driver = os.environ.get("CARGO_DRIVER", "cargo")
    env = os.environ.copy()
    if driver == "mbx":
        env["CARGO_TARGET_DIR"] = str(Path(env.get("CARGO_TARGET_DIR") or ROOT / "target") / "mbx")
    subprocess.run([driver, "build", "--locked", "-q", "-p", "reviewq", "--bin", "reviewq"],
                   cwd=ROOT, env=env, check=True)
    metadata = subprocess.run([driver, "metadata", "--locked", "--no-deps", "--format-version", "1"],
                              cwd=ROOT, env=env, check=True, capture_output=True)
    return Path(json.loads(metadata.stdout)["target_directory"]) / "debug" / "reviewq"


class DevService:
    """A provider replay and a validation service under `root`, a new directory.

    `root/profile` is the ReviewQ profile, `root/claude` the synthetic Claude profile and
    `root/checkout` the working directory every linked lead reports.
    """

    def __init__(self, root, inputs, binary):
        self.root = Path(root).resolve()
        self.inputs = Path(inputs)
        self.binary = Path(binary)
        self.profile = self.root / "profile"
        self.claude = self.root / "claude"
        self.checkout = self.root / "checkout"
        self.process = None
        self.projects = []
        self.provider = None
        self.browser = None
        self.csrf = None
        # Each lead's companion proof, which the plugin makes with its generation.
        self.proofs = {}

    def __enter__(self):
        self.root.mkdir(mode=0o700)
        for path in (self.profile, self.claude / "projects" / "fixture", self.checkout, self.root / "web"):
            path.mkdir(mode=0o700, parents=True)
        (self.root / "web" / "index.html").write_text("<title>ReviewQ fixture service</title>\n")
        replay = Replay(self.inputs, "populated")
        self.provider = Server(0, replay)
        # Ephemeral ports have five digits, so every composed MR link has the same length.
        if len(str(self.provider.server_port)) != 5:
            raise ValueError("the fixture provider needs a five-digit loopback port")
        threading.Thread(target=self.provider.serve_forever, daemon=True).start()
        projects = sorted({row["project_id"] for row in replay.reviews.values()})
        self.reviews = replay.reviews
        self.start(projects)
        return self

    def __exit__(self, *_error):
        self.stop()
        if self.provider:
            self.provider.shutdown()
            self.provider.server_close()

    @property
    def origin(self):
        return self.provider.origin

    def start(self, projects=None):
        """Starts the service, or restarts it on the same profile after `stop`."""
        if projects is not None:
            self.projects = projects
        command = [self.binary, "serve", "--validation", "--runtime-root", self.profile,
                   "--web", self.root / "web", "--gitlab", self.origin,
                   "--claude-home", self.claude, "--port", "0", "--native-port", "0"]
        for project in self.projects:
            command += ["--project", str(project)]
        log = (self.root / "service.log").open("ab")
        self.process = subprocess.Popen(command, cwd=self.checkout, stdout=log, stderr=log)
        log.close()
        deadline = time.monotonic() + READY_SECONDS
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f"the service exited; read {self.root / 'service.log'}")
            status = self.cli("status", check=False)
            if status.returncode == 0:
                data = json.loads(status.stdout)["data"]
                if data.get("ready"):
                    self.browser = f"http://{data['browser']}"
                    self.csrf = None
                    return
            time.sleep(0.2)
        raise RuntimeError("the service did not become ready")

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process = None

    def cli(self, *args, stdin=None, check=True):
        env = {**os.environ, "CLAUDE_CONFIG_DIR": str(self.claude)}
        env.pop("REVIEWQ_CLAUDE_LINKED", None)
        result = subprocess.run([self.binary, "--profile", self.profile, *args], cwd=self.checkout,
                                env=env, input=stdin, capture_output=True)
        if check and result.returncode:
            raise RuntimeError(f"reviewq {args[0]} {args[1] if len(args) > 1 else ''} failed: "
                               f"{result.stderr.decode(errors='replace').strip()}")
        return result

    def refresh(self):
        self.cli("refresh")

    def link(self, native_id, review):
        """Writes the lead's transcript and links it to `review` with the plugin's link helper.
        Returns `(review_id, run_id)`."""
        record = claude_message(native_id, 0, "Fixture review session.")
        record["cwd"] = str(self.checkout)
        transcript = self.claude / "projects" / "fixture" / f"{native_id}.jsonl"
        transcript.write_text(json.dumps(record) + "\n")
        url = f"{self.origin}/forge/{review['project']}/-/merge_requests/{review['iid']}"
        reply = json.loads(self.cli("internal", "claude-companion", native_id, "--target", url).stdout)
        query = parse_qs(urlsplit(reply["message"].splitlines()[1]).query)
        return query["reviewId"][0], query["runId"][0]

    def heartbeat(self, native_id, phase, sequence, turn=None):
        """Reports the lead's activity as the plugin's heartbeat does."""
        body = {"generation": native_id.replace("-", ""), "sequence": sequence, "phase": phase,
                "hasQuestions": False, "latestPromptManual": False, "proof": self.proof(native_id)}
        if turn:
            body["turnId"] = turn
        self.cli("internal", "claude-bridge", "heartbeat", native_id, stdin=json.dumps(body).encode())

    def ledger(self, native_id, tool, value, call_id, agent=None, turn=None):
        """One ledger tool call through the bridge. Returns the route's reply, a result under
        `data` or a refusal under `error`."""
        call = {"tool": tool, "callId": call_id, "input": value, "proof": self.proof(native_id)}
        if agent:
            call["agentId"] = agent
        if turn:
            call["turnId"] = turn
        result = self.cli("internal", "claude-bridge", "ledger", native_id,
                          stdin=json.dumps(call).encode(), check=False)
        if result.returncode:
            raise RuntimeError(f"{tool} failed: {result.stderr.decode(errors='replace').strip()}")
        return json.loads(result.stdout)

    def proof(self, native_id):
        """The lead's companion proof, as the plugin keeps it beside its generation."""
        return self.proofs.setdefault(native_id, os.urandom(32).hex())

    def request(self, method, path, body=None, headers=None):
        """One request to the browser listener. Returns `(status, headers, bytes)`."""
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(f"{self.browser}{path}", data=data, method=method,
                                         headers=headers or {})
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.status, dict(response.headers), response.read()
        except urllib.error.HTTPError as error:
            return error.code, dict(error.headers), error.read()

    def get(self, path):
        status, _headers, data = self.request("GET", path)
        if status != 200:
            raise RuntimeError(f"GET {path} returned {status}: {data[:400]!r}")
        return json.loads(data)["data"]

    def mutate(self, method, path, body):
        """A browser mutation with the same-origin proof the browser obtains from `/session`."""
        same_origin = {"Sec-Fetch-Site": "same-origin"}
        if self.csrf is None:
            self.csrf = self.get_with("/api/v1/session", same_origin)["csrfToken"]
        headers = {**same_origin, "Origin": self.browser, "X-ReviewQ-CSRF": self.csrf}
        status, _headers, data = self.request(method, path, body, headers)
        return status, json.loads(data) if data else None

    def get_with(self, path, headers):
        status, _headers, data = self.request("GET", path, headers=headers)
        if status != 200:
            raise RuntimeError(f"GET {path} returned {status}")
        return json.loads(data)["data"]

