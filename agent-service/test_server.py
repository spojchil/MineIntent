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

        def completion(_config, messages, deadline):
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

        def completion(_config, messages, _deadline):
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
        with patch.object(server, "model_completion", lambda _config, _messages, _deadline: responses.pop(0)):
            server.run_tool_loop({"model": "x"}, "run-1", context(), lambda *args: seen.append(args))
        self.assertEqual(seen, [])

    def test_request_and_json_are_strict(self):
        self.assertEqual(server.require_request({"runId": "r", "context": context()})[0], "r")
        with self.assertRaises(server.RequestValidationError):
            server.require_request({"runId": "r", "context": context(), "extra": True})
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

    def test_decide_authentication_happens_before_body_and_busy_check(self):
        httpd = self._start_server()
        httpd.decision_lock.acquire()
        self.addCleanup(lambda: httpd.decision_lock.locked() and httpd.decision_lock.release())
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
        self.assertEqual(caught.exception.code, 429)

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

    def test_model_transport_disables_proxy_and_redirects(self):
        seen_handlers = []

        class Response:
            def __enter__(self): return self
            def __exit__(self, *_args): return False
            def read(self, _limit): return b'{"choices":[]}'

        class Opener:
            def open(self, _request, timeout):
                self.timeout = timeout
                return Response()

        def build_opener(*handlers):
            seen_handlers.extend(handlers)
            return Opener()

        with patch.object(server.urllib.request, "build_opener", build_opener):
            server.model_completion(
                {"model": "x", "base_url": "https://api.example.test", "api_key": "secret"},
                [],
                time.monotonic() + 1,
            )
        self.assertTrue(any(isinstance(handler, urllib.request.ProxyHandler) for handler in seen_handlers))
        self.assertTrue(any(isinstance(handler, server._NoRedirect) for handler in seen_handlers))

    def _start_server(self):
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        httpd.daemon_threads = True
        httpd.config = {"service_token": SERVICE_TOKEN, "model": "x"}
        httpd.decision_lock = threading.Lock()
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        return httpd


def context():
    return {
        "protocol": "mineintent.d40-context.v1", "player": {"username": "Alex", "text": "看看羊"},
        "profile": {"content": "朋友"}, "world": {"dimension": "overworld"},
        "observations": {"viewport": {"frame": {"axes": ["right", "up", "forward"]}}},
        "recentEvents": [], "memories": [],
    }


if __name__ == "__main__":
    unittest.main()
