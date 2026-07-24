import json
import threading
import time
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

import server

SERVICE_TOKEN = "agent-service-test-token-0123456789"


class ServerTests(unittest.TestCase):
    def test_deepseek_replay_preserves_reasoning_and_tool_call_id(self):
        calls = []
        model_messages = []
        responses = [
            {"choices": [{"message": {
                "role": "assistant", "content": "", "reasoning_content": "need turn",
                "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "look_relative", "arguments": '{"yaw_degrees":90,"pitch_degrees":0}'}}],
            }}], "usage": {"prompt_tokens": 10, "completion_tokens": 2}},
            {"choices": [{"message": {"role": "assistant", "content": json.dumps({
                "protocol": "mineintent.d40-decision.v1", "speech": "看见了。",
            }, ensure_ascii=False)}}], "usage": {"prompt_tokens": 20, "completion_tokens": 3}},
        ]

        deadlines = []

        def completion(_config, messages, deadline, _run=None):
            model_messages.append(json.loads(json.dumps(messages)))
            deadlines.append(deadline)
            return responses.pop(0)

        def execute(run_id, name, arguments, _deadline):
            calls.append((run_id, name, arguments))
            return {"status": "completed", "viewport": {"visibleEntities": [{"name": "sheep", "relativePosition": [0, 0, 3]}]}}

        with patch.object(server, "model_completion", completion):
            decision, usage = server.run_tool_loop({"model": "x"}, "run-1", context(), execute)
        self.assertEqual(decision["speech"], "看见了。")
        self.assertEqual(usage, {"prompt_tokens": 30, "completion_tokens": 5})
        self.assertEqual(deadlines[0], deadlines[1])
        self.assertEqual(calls, [("run-1", "look_relative", {"yaw_degrees": 90, "pitch_degrees": 0})])
        replay = model_messages[1][-2]
        tool_result = model_messages[1][-1]
        self.assertEqual(replay["reasoning_content"], "need turn")
        self.assertEqual(tool_result["tool_call_id"], "call-1")
        self.assertIn("sheep", tool_result["content"])

    def test_parallel_calls_execute_only_first_and_return_failure_for_rest(self):
        model_messages = []
        responses = [
            {"choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "one", "function": {"name": "move_input", "arguments": '{"direction":"forward","duration_ms":50}'}},
                {"id": "two", "function": {"name": "look_relative", "arguments": '{"yaw_degrees":10,"pitch_degrees":0}'}},
            ]}}]},
            {"choices": [{"message": {"role": "assistant", "content": '{"protocol":"mineintent.d40-decision.v1","speech":null}'}}]},
        ]
        executed = []

        def completion(_config, messages, _deadline, _run=None):
            model_messages.append(json.loads(json.dumps(messages)))
            return responses.pop(0)

        with patch.object(server, "model_completion", completion):
            server.run_tool_loop({"model": "x"}, "run-1", context(), lambda *args: executed.append(args) or {"status": "completed"})
        self.assertEqual(len(executed), 1)
        self.assertIn("parallel_body_tools_are_not_supported", model_messages[1][-1]["content"])

    def test_invalid_arguments_are_returned_to_model_without_executing(self):
        responses = [
            {"choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "bad", "function": {"name": "move_input", "arguments": '{"direction":"forward","duration_ms":5000}'}}
            ]}}]},
            {"choices": [{"message": {"role": "assistant", "content": '{"protocol":"mineintent.d40-decision.v1","speech":null}'}}]},
        ]
        seen = []
        with patch.object(server, "model_completion", lambda _config, _messages, _deadline, _run=None: responses.pop(0)):
            server.run_tool_loop({"model": "x"}, "run-1", context(), lambda *args: seen.append(args))
        self.assertEqual(seen, [])

    def test_request_and_json_are_strict(self):
        self.assertEqual(server.require_request({"runId": "r", "context": context()})[0], "r")
        self.assertEqual(server.require_cancel_request({"runId": "r"}), "r")
        with self.assertRaises(server.RequestValidationError):
            server.require_request({"runId": "r", "context": context(), "extra": True})
        with self.assertRaises(server.RequestValidationError):
            server.require_cancel_request({"runId": "r", "extra": True})
        with self.assertRaises(ValueError):
            server.strict_json_loads('{"x":NaN}')
        with self.assertRaises(server.RequestValidationError):
            server.http_tool_executor("https://example.com/tool", "0123456789abcdef")
        with self.assertRaises(RuntimeError):
            server._validate_decision({"protocol": "mineintent.d40-decision.v1", "speech": None, "memory": None})

    def test_config_requires_an_independent_service_token(self):
        env = {
            "MINEINTENT_MODEL_BASE_URL": "https://api.example.test/v1",
            "MINEINTENT_MODEL_API_KEY": "model-secret-value",
            "MINEINTENT_MODEL": "model",
            "MINEINTENT_AGENT_SERVICE_TOKEN": SERVICE_TOKEN,
        }
        self.assertEqual(server.load_config(env)["service_token"], SERVICE_TOKEN)
        env["MINEINTENT_AGENT_SERVICE_TOKEN"] = env["MINEINTENT_MODEL_API_KEY"]
        with self.assertRaises(server.ConfigError):
            server.load_config(env)

    def test_decide_authentication_happens_before_body_validation(self):
        httpd = self._start_server()
        request = urllib.request.Request(
            f"http://127.0.0.1:{httpd.server_port}/v1/decide",
            data=b"not-json",
            method="POST",
            headers={"content-type": "application/json"},
        )
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=2)
        self.assertEqual(caught.exception.code, 401)

        request.add_header("authorization", f"Bearer {SERVICE_TOKEN}")
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=2)
        self.assertEqual(caught.exception.code, 400)

    def test_cancelled_run_does_not_block_its_replacement(self):
        httpd = self._start_server()
        old_started = threading.Event()
        release_old = threading.Event()
        completion_lock = threading.Lock()
        completion_count = 0

        def completion(_config, _messages, _deadline, _run=None):
            nonlocal completion_count
            with completion_lock:
                completion_count += 1
                call_number = completion_count
            if call_number == 1:
                old_started.set()
                self.assertTrue(release_old.wait(2), "old model call was not released")
            return {"choices": [{"message": {"role": "assistant", "content": json.dumps({
                "protocol": "mineintent.d40-decision.v1", "speech": None,
            })}}]}

        old_result = {}

        def request_old():
            try:
                with urllib.request.urlopen(self._decision_request(httpd, "run-old"), timeout=3) as response:
                    old_result["status"] = response.status
            except urllib.error.HTTPError as error:
                old_result["status"] = error.code

        with patch.object(server, "model_completion", completion):
            old_thread = threading.Thread(target=request_old)
            old_thread.start()
            self.assertTrue(old_started.wait(1), "old decision did not start")

            cancel_request = urllib.request.Request(
                f"http://127.0.0.1:{httpd.server_port}/v1/cancel",
                data=b'{"runId":"run-old"}',
                method="POST",
                headers={"authorization": f"Bearer {SERVICE_TOKEN}", "content-type": "application/json"},
            )
            with urllib.request.urlopen(cancel_request, timeout=1) as response:
                self.assertEqual(json.load(response), {"cancelled": True})

            with urllib.request.urlopen(self._decision_request(httpd, "run-new"), timeout=1) as response:
                self.assertEqual(response.status, 200)

            release_old.set()
            old_thread.join(2)

        self.assertFalse(old_thread.is_alive())
        self.assertEqual(old_result["status"], 409)

    def test_late_cancel_for_superseded_id_does_not_cancel_new_run(self):
        runs = server.DecisionRuns()
        old = runs.begin("run-old")
        new = runs.begin("run-new")
        self.assertIsNotNone(old)
        self.assertIsNotNone(new)
        self.assertTrue(old.cancelled.is_set())
        self.assertFalse(runs.cancel("run-old"))
        new.ensure_active()

    def test_decide_enforces_the_round_deadline(self):
        httpd = self._start_server()
        request = urllib.request.Request(
            f"http://127.0.0.1:{httpd.server_port}/v1/decide",
            data=json.dumps({"runId": "run-1", "context": context()}).encode("utf-8"),
            method="POST",
            headers={
                "authorization": f"Bearer {SERVICE_TOKEN}",
                "content-type": "application/json",
                "x-mineintent-tool-executor-url": "http://127.0.0.1:9/v1/d40/tool",
                "x-mineintent-tool-executor-token": "0123456789abcdef",
            },
        )
        with patch.object(server, "_ROUND_TIMEOUT_S", -1):
            with self.assertRaises(urllib.error.HTTPError) as caught:
                urllib.request.urlopen(request, timeout=2)
        self.assertEqual(caught.exception.code, 504)

    def test_model_transport_connects_directly_to_the_configured_endpoint(self):
        connections = []

        class Socket:
            def settimeout(self, timeout): self.timeout = timeout

        class Connection:
            def __init__(self, host, port, timeout):
                self.host, self.port, self.timeout = host, port, timeout
                self.sock = Socket()
                connections.append(self)

            def connect(self): pass
            def request(self, method, path, body, headers):
                self.request_value = (method, path, body, headers)
            def getresponse(self):
                return type("Response", (), {"status": 200, "read": lambda _self, _limit: b'{"choices":[]}'})()
            def close(self): pass

        with patch.object(server.http.client, "HTTPSConnection", Connection):
            server.model_completion(
                {"model": "x", "base_url": "https://api.example.test/v1", "api_key": "secret"},
                [],
                time.monotonic() + 1,
            )
        self.assertEqual((connections[0].host, connections[0].port), ("api.example.test", None))
        self.assertEqual(connections[0].request_value[:2], ("POST", "/v1/chat/completions"))

    def test_model_transport_cancellation_closes_a_blocked_upstream(self):
        upstream_started = threading.Event()
        release_upstream = threading.Event()

        class UpstreamHandler(server.BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802
                self.rfile.read(int(self.headers["content-length"]))
                upstream_started.set()
                release_upstream.wait(2)
                try:
                    payload = b'{"choices":[]}'
                    self.send_response(200)
                    self.send_header("content-length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, _format, *_args): pass

        upstream = server.ThreadingHTTPServer(("127.0.0.1", 0), UpstreamHandler)
        upstream.daemon_threads = True
        upstream_thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        upstream_thread.start()
        self.addCleanup(upstream.server_close)
        self.addCleanup(upstream.shutdown)
        self.addCleanup(release_upstream.set)

        run = server.DecisionRun("run-old")
        result = {}

        def request_model():
            try:
                server.model_completion({
                    "model": "x", "base_url": f"http://127.0.0.1:{upstream.server_port}/v1", "api_key": "secret",
                }, [], time.monotonic() + 5, run)
            except Exception as error:  # noqa: BLE001
                result["error"] = error

        model_thread = threading.Thread(target=request_model)
        model_thread.start()
        self.assertTrue(upstream_started.wait(1), "upstream request did not start")
        run.cancel()
        model_thread.join(1)
        release_upstream.set()

        self.assertFalse(model_thread.is_alive(), "cancel did not interrupt the upstream response wait")
        self.assertIsInstance(result.get("error"), server.RunCancelled)

    def _start_server(self):
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        httpd.daemon_threads = True
        httpd.config = {"service_token": SERVICE_TOKEN, "model": "x"}
        httpd.decision_runs = server.DecisionRuns()
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        return httpd

    def _decision_request(self, httpd, run_id):
        return urllib.request.Request(
            f"http://127.0.0.1:{httpd.server_port}/v1/decide",
            data=json.dumps({"runId": run_id, "context": context()}).encode("utf-8"),
            method="POST",
            headers={
                "authorization": f"Bearer {SERVICE_TOKEN}",
                "content-type": "application/json",
                "x-mineintent-tool-executor-url": "http://127.0.0.1:9/v1/d40/tool",
                "x-mineintent-tool-executor-token": "0123456789abcdef",
            },
        )


def context():
    return {
        "protocol": "mineintent.d40-context.v1", "player": {"username": "Alex", "text": "看看羊"},
        "profile": {"content": "朋友"}, "world": {"dimension": "overworld"},
        "observations": {"viewport": {"frame": {"axes": ["right", "up", "forward"]}}},
        "recentEvents": [], "memories": [],
    }


if __name__ == "__main__":
    unittest.main()
