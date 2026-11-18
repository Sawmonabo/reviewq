"""Bounded loopback HTTP service for synthetic external sources and operator controls."""

import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import BoundedSemaphore
from urllib.parse import parse_qs, urlsplit

from .replay import ReplayError
from .routes import Response, gitlab_get, jira_get, json_response


class Server(ThreadingHTTPServer):
    def __init__(self, port, replay):
        self.replay = replay
        self.slots = BoundedSemaphore(24)
        super().__init__(("127.0.0.1", port), Handler)
        self.authority = f"127.0.0.1:{self.server_port}"
        self.origin = f"http://{self.authority}"

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            request.settimeout(12)
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        return

    def do_GET(self):
        self.serve_request("GET")

    def do_POST(self):
        self.serve_request("POST")

    def send_result(self, response):
        self.send_response(response.status)
        self.send_header("Content-Type", response.content_type)
        self.send_header("Content-Length", str(len(response.body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        for key, value in response.headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(response.body)

    def serve_request(self, method):
        self.close_connection = True
        try:
            result = self.dispatch(method)
            if result is not None:
                self.send_result(result)
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass
        except ReplayError as error:
            if self.path.startswith(("/rest/", "/_edge/")):
                result = json_response({"errorMessages": [str(error)], "errors": {}}, error.status)
            else:
                result = json_response({"message": str(error)}, error.status)
            try:
                self.send_result(result)
            except (OSError, TimeoutError):
                pass
        except ValueError:
            try:
                self.send_result(json_response({"message": "Invalid fixture request."}, 400))
            except (OSError, TimeoutError):
                pass

    def dispatch(self, method):
        replay = self.server.replay
        if self.headers.get_all("Host") != [self.server.authority] or self.headers.get("Origin") is not None:
            raise ReplayError(403, "Use the printed loopback origin from a local development client.")
        if len(self.path) > 4096 or not self.path.startswith("/") or self.path.startswith("//"):
            raise ReplayError(400, "Invalid fixture request target.")
        request = urlsplit(self.path)
        path, query = request.path, parse_qs(request.query, max_num_fields=50)
        if path.startswith("/fixture/"):
            return self.fixture_route(method, path, query)
        if method != "GET":
            raise ReplayError(405, "Fixture GitLab and Jira accept GET requests.")
        status = replay.authority(path)
        if status:
            raise ReplayError(status, f"Fixture external authority returned {status}.")
        if path.startswith("/api/v4/"):
            result = gitlab_get(replay, path, query, self.server.origin)
        elif path.startswith(("/rest/", "/_edge/")):
            result = jira_get(replay, path, query, self.server.origin)
        else:
            raise ReplayError(404, "Fixture route was not found.")
        replay.await_release(path, self.path)
        return result

    def read_json(self):
        if self.headers.get_content_type() != "application/json" or self.headers.get("Transfer-Encoding"):
            raise ReplayError(415, "Expected application/json with a Content-Length.")
        try:
            lengths = self.headers.get_all("Content-Length", [])
            length, = lengths
            size = int(length)
        except (TypeError, ValueError) as error:
            raise ReplayError(400, "Expected one valid Content-Length.") from error
        if not 0 < size <= 16384:
            raise ReplayError(413, "Fixture request body exceeds its size limit.")
        raw = self.rfile.read(size)
        if len(raw) != size:
            raise ReplayError(400, "Fixture request body is incomplete.")
        try:
            data = json.loads(raw)
        except ValueError as error:
            raise ReplayError(400, "Invalid fixture JSON body.") from error
        if not isinstance(data, dict):
            raise ReplayError(400, "Expected a JSON object.")
        return data

    def fixture_route(self, method, path, query):
        replay = self.server.replay
        if method == "POST" and path == "/fixture/control":
            return json_response(replay.control(self.read_json()))
        if method == "GET":
            if path == "/fixture/status":
                return json_response(replay.status())
            if path == "/fixture/registrations":
                return json_response(list(replay.registrations.values()))
            if path in ("/fixture/report-sources", "/fixture/outbox", "/fixture/timeline"):
                if path == "/fixture/report-sources" and replay.profile != "populated":
                    return json_response([])
                return json_response(replay.provider_json(path.split("/")[-1] + ".json"))
            if path == "/fixture/events":
                return self.send_stream("all", query)
        route = re.fullmatch(r"/fixture/native/([a-z0-9-]+)(?:/(stream|messages)(?:/([0-9a-f-]+))?)?", path)
        if route:
            alias, action, receipt = route.groups()
            if method == "GET" and not action:
                return Response(replay.native(alias), content_type="application/x-ndjson; charset=utf-8")
            if method == "GET" and action == "stream" and receipt is None:
                replay.registration(alias)
                return self.send_stream(alias, query)
            if method == "GET" and action == "messages" and receipt is not None:
                return json_response(replay.receipt(alias, receipt))
            if method == "POST" and action == "messages" and receipt is None:
                accepted, drop = replay.submit(alias, self.read_json())
                if not drop:
                    return json_response(accepted)
                return None
        raise ReplayError(404, "Fixture control route was not found.")

    def send_stream(self, scope, query):
        replay = self.server.replay
        raw_after = self.headers.get("Last-Event-ID", query.get("after", ["0"])[-1])
        try:
            after = int(raw_after)
        except ValueError as error:
            raise ReplayError(400, "Expected a nonnegative fixture event cursor.") from error
        if after < 0:
            raise ReplayError(400, "Expected a nonnegative fixture event cursor.")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        stream = replay.stream(scope, after)
        try:
            self.wfile.write(b": fixture connected\n\n")
            self.wfile.flush()
            for value in stream:
                if value is None:
                    self.wfile.write(b": fixture keepalive\n\n")
                else:
                    sequence, event = value
                    data = json.dumps(event, ensure_ascii=False, separators=(",", ":"))
                    encoded = f"id: {sequence}\nevent: {event['kind']}\ndata: {data}\n\n"
                    self.wfile.write(encoded.encode("utf-8"))
                self.wfile.flush()
        finally:
            stream.close()
