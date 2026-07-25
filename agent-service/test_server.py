import json
import pathlib
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

import server

SERVICE_TOKEN = "agent-service-test-token-0123456789"

class _Sink:
    """每个测试用例一份临时 transcript，绝不写真实 .mineintent/。"""

    def __init__(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = pathlib.Path(self.dir.name) / "agent-transcripts.jsonl"

    def config(self, **extra):
        return {"model": "x", "transcript_path": self.path, **extra}

    def records(self):
        if not self.path.exists():
            return []
        return [json.loads(line) for line in self.path.read_text(encoding="utf-8").splitlines() if line]


def tools():
    return [
        {"type": "function", "function": {"name": "look_relative", "description": "转头", "parameters": {"type": "object"}}},
        {"type": "function", "function": {"name": "move_input", "description": "移动", "parameters": {"type": "object"}}},
        {"type": "function", "function": {"name": "say", "description": "说话", "parameters": {"type": "object"}}},
    ]


class ServerTests(unittest.TestCase):
    def test_deepseek_replay_preserves_reasoning_and_tool_call_id(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        calls = []
        model_messages = []
        responses = [
            {"choices": [{"message": {
                "role": "assistant", "content": "", "reasoning_content": "need turn",
                "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "look_relative", "arguments": '{"yaw_degrees":90,"pitch_degrees":0}'}}],
            }}], "usage": {"prompt_tokens": 10, "completion_tokens": 2}},
            {"choices": [{"message": {"role": "assistant", "content": ""}}],
             "usage": {"prompt_tokens": 20, "completion_tokens": 3}},
        ]

        deadlines = []

        def completion(_config, messages, deadline, _run=None, tools=None):
            model_messages.append(json.loads(json.dumps(messages)))
            deadlines.append(deadline)
            return responses.pop(0)

        def execute(run_id, name, arguments, _deadline, tool_call_id="", round_id=0):
            calls.append((run_id, name, arguments, tool_call_id, round_id))
            return {"status": "completed", "viewport": {"visibleEntities": [["sheep", 0, 0, 3]]}}

        with patch.object(server, "model_completion", completion):
            closing, usage = server.run_tool_loop(sink.config(), "run-1", context(), execute, tools())
        self.assertEqual(closing, "")
        self.assertEqual(usage, {"prompt_tokens": 30, "completion_tokens": 5})
        self.assertEqual(deadlines[0], deadlines[1])
        # 关联链：模型的 tool_call_id 与轮次序号一路传到工具后端（D06）。
        self.assertEqual(calls, [("run-1", "look_relative", {"yaw_degrees": 90, "pitch_degrees": 0}, "call-1", 0)])
        replay = model_messages[1][-2]
        tool_result = model_messages[1][-1]
        self.assertEqual(replay["reasoning_content"], "need turn")
        self.assertEqual(tool_result["tool_call_id"], "call-1")
        self.assertIn("sheep", tool_result["content"])

    def test_stable_content_leads_and_frames_are_appended_never_re_rendered(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        seen = []
        frame = {"at": "2026-07-25T00:01:00Z", "world": {"dimension": "overworld"},
                 "events": [{"type": "self.health.dropped", "summary": "受到伤害"}], "omissions": []}
        responses = [
            {"choices": [{"message": {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "say", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "look_relative", "arguments": "{}"}},
            ]}}]},
            {"choices": [{"message": {"role": "assistant", "content": ""}}]},
        ]

        def completion(_config, messages, _deadline, _run=None, tools=None):
            seen.append(json.loads(json.dumps(messages)))
            return responses.pop(0)

        def execute(_run_id, name, _arguments, _deadline, _tool_call_id="", _round_id=0):
            if name == "say":
                return {"protocol": "mineintent.tool-response.v1", "result": {"status": "queued"}, "frame": frame}
            return {"status": "completed"}

        with patch.object(server, "model_completion", completion):
            server.run_tool_loop(sink.config(), "run-frame", context(), execute, tools())

        system, opening = seen[0][0], seen[0][1]
        # Slowest-changing content leads, because prefix caching is prefix-only.
        self.assertEqual(system["role"], "system")
        self.assertIn("朋友", system["content"])
        self.assertIn("玩家怕高", system["content"])
        self.assertEqual(opening["role"], "user")
        self.assertIn("看看羊", opening["content"])
        # The world is never restated inside the system message; it only ever arrives as a frame.
        self.assertNotIn("看看羊", system["content"])

        roles = [message["role"] for message in seen[1]]
        # Every tool result of the round first, then the frame: an assistant message with tool_calls
        # must be followed by one result per call, and interleaving there breaks the pairing.
        self.assertEqual(roles, ["system", "user", "assistant", "tool", "tool", "user"])
        self.assertEqual(json.loads(seen[1][-1]["content"]), frame)
        # The envelope is unwrapped: the model sees the tool's answer, not our transport around it.
        self.assertEqual(json.loads(seen[1][3]["content"]), {"status": "queued"})
        # Frames already in the conversation are byte-identical on the next round — appended once,
        # never re-rendered, which is the only reason a volatile frame is free under caching.
        self.assertEqual(seen[0][:2], seen[1][:2])

    def test_cache_counters_are_read_from_each_provider_shape_and_summed(self):
        # DeepSeek reports the hit at the top level; Zhipu and Moonshot use OpenAI's nested details.
        # Both must land on the same counter, or a hit rate cannot be compared across providers.
        self.assertEqual(
            server.normalize_usage({"prompt_tokens": 900, "completion_tokens": 5, "prompt_cache_hit_tokens": 640, "prompt_cache_miss_tokens": 260}),
            {"prompt_tokens": 900, "completion_tokens": 5, "cache_read_tokens": 640},
        )
        self.assertEqual(
            server.normalize_usage({"prompt_tokens": 900, "prompt_tokens_details": {"cached_tokens": 512, "cache_write_tokens": 388}}),
            {"prompt_tokens": 900, "cache_read_tokens": 512, "cache_write_tokens": 388},
        )
        # A provider below its own caching floor reports zero, which is a finding rather than a gap:
        # it must survive as 0 and not be dropped the way a missing key is.
        self.assertEqual(server.normalize_usage({"prompt_tokens_details": {"cached_tokens": 0}}), {"cache_read_tokens": 0})
        for junk in (None, [], {"prompt_tokens": -1}, {"prompt_tokens": True}, {"prompt_tokens_details": 7}):
            self.assertEqual(server.normalize_usage(junk), {})

        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        responses = [
            {"choices": [{"message": {"role": "assistant", "content": "",
                                      "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "say", "arguments": "{}"}}]}}],
             "usage": {"prompt_tokens": 700, "completion_tokens": 4, "prompt_cache_hit_tokens": 0}},
            {"choices": [{"message": {"role": "assistant", "content": ""}}],
             "usage": {"prompt_tokens": 1200, "completion_tokens": 6, "prompt_cache_hit_tokens": 704}},
        ]
        with patch.object(server, "model_completion", lambda *_a, **_k: responses.pop(0)):
            _closing, usage = server.run_tool_loop(
                sink.config(), "run-cache", context(), lambda *_a, **_k: {"status": "queued"}, tools(),
            )
        # Round one is a cold write, round two hits the prefix built by round one: the run's own
        # numbers are what make intra-run reuse visible without a provider dashboard.
        self.assertEqual(usage, {"prompt_tokens": 1900, "completion_tokens": 10, "cache_read_tokens": 704})
        self.assertEqual(sink.records()[0]["usage"]["cache_read_tokens"], 704)

    def test_parallel_calls_all_execute_in_order(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        model_messages = []
        responses = [
            {"choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "one", "function": {"name": "move_input", "arguments": '{"direction":"forward","duration_ms":50}'}},
                {"id": "two", "function": {"name": "look_relative", "arguments": '{"yaw_degrees":10,"pitch_degrees":0}'}},
            ]}}]},
            {"choices": [{"message": {"role": "assistant", "content": ""}}]},
        ]
        executed = []

        def completion(_config, messages, _deadline, _run=None, tools=None):
            model_messages.append(json.loads(json.dumps(messages)))
            return responses.pop(0)

        with patch.object(server, "model_completion", completion):
            server.run_tool_loop(
                sink.config(), "run-1", context(),
                lambda *args: executed.append(args) or {"status": "completed"}, tools(),
            )
        # 并行调用不再被预先拒绝：约束在提示词里是建议，失败由工具后端如实回报。
        self.assertEqual([call[1] for call in executed], ["move_input", "look_relative"])
        self.assertIn("completed", model_messages[1][-1]["content"])
        self.assertIn("completed", model_messages[1][-2]["content"])

    def test_arguments_are_forwarded_untouched_for_the_tool_side_to_judge(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        responses = [
            {"choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "bad", "function": {"name": "move_input", "arguments": '{"direction":"forward","duration_ms":5000}'}}
            ]}}]},
            {"choices": [{"message": {"role": "assistant", "content": ""}}]},
        ]
        seen = []
        with patch.object(server, "model_completion", lambda _config, _messages, _deadline, _run=None, tools=None: responses.pop(0)):
            server.run_tool_loop(
                sink.config(), "run-1", context(),
                lambda *args: seen.append(args) or {"status": "failed", "summary": "duration out of range"}, tools(),
            )
        # 工具契约归工具侧：agent 不预判参数合法性，越界值也交给后端回真实失败。
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0][2], {"direction": "forward", "duration_ms": 5000})

    def test_float_arguments_survive_the_json_boundary(self):
        # 回归：_validate_json 用 math.isfinite 拒 NaN/Inf；漏掉 import math 会让任何浮点请求崩溃。
        self.assertEqual(server.strict_json_loads(server.strict_json_dumps({"yaw": -30.5})), {"yaw": -30.5})
        for bad in [float("nan"), float("inf"), float("-inf")]:
            with self.assertRaises(ValueError):
                server.strict_json_dumps({"yaw": bad})

    def test_transcript_records_tools_rotates_and_honors_the_data_dir(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)
        responses = [{"choices": [{"message": {"role": "assistant", "content": "完成"}}]}]
        with patch.object(server, "model_completion",
                          lambda _c, _m, _d, _r=None, tools=None: responses.pop(0)):
            server.run_tool_loop(sink.config(), "run-1", context(), lambda *a: {}, tools())
        record = sink.records()[-1]
        # 声称记录模型所见的一切，就必须包含它当时被给了哪些工具。
        self.assertEqual(record["tools"], ["look_relative", "move_input", "say"])
        self.assertEqual(len(record["toolSchemas"]), 3)
        self.assertEqual(record["closing"], "完成")
        self.assertIsNone(record["error"])

        # 超过文件上限时轮转，而不是无限增长。
        sink.path.write_text("x" * (server._MAX_TRANSCRIPT_BYTES - 1), encoding="utf-8")
        server.append_transcript("run-2", "m", tools(), [], "", None, None, sink.path)
        self.assertTrue(sink.path.with_suffix(sink.path.suffix + ".1").exists())
        self.assertEqual(len(sink.records()), 1)

        # 数据目录可配，不再写死在当前工作目录。
        self.assertEqual(
            server.transcript_path({"MINEINTENT_DATA_DIR": "custom-data"}),
            pathlib.Path("custom-data") / "agent-transcripts.jsonl",
        )

    def test_transcript_records_the_run_even_when_it_fails(self):
        sink = _Sink()
        self.addCleanup(sink.dir.cleanup)

        def boom(_c, _m, _d, _r=None, tools=None):
            raise RuntimeError("upstream exploded")

        with patch.object(server, "model_completion", boom):
            with self.assertRaises(RuntimeError):
                server.run_tool_loop(sink.config(), "run-1", context(), lambda *a: {}, tools())
        record = sink.records()[-1]
        self.assertIn("upstream exploded", record["error"])
        self.assertIsNone(record["closing"])

    def test_leak_guard_catches_call_shaped_mentions_but_not_prose(self):
        # 两种实测泄漏形式：一家把内部标记原样吐成文本，另一家自造 JSON 字段承载工具调用。
        # 两者都携带一个真实但未执行的动作，静默丢弃等于伪造成功。
        names = ["look_relative", "move_input", "say"]
        leaks = [
            '好的\n<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name="look_relative">',
            '{"speech":"我先找找。","__tool__":"look_relative","__tool_args__":{"yaw_degrees":90}}',
            'look_relative({"yaw_degrees": 45, "pitch_degrees": 0})',
            '{"name":"move_input","arguments":{"direction":"forward"}}',
        ]
        for leaked in leaks:
            self.assertTrue(server.leaked_tool_call(leaked, names), leaked)
        # 收尾文本只进转录、不会被说出去，所以散文式提及不算泄漏。
        for prose in ["我刚才转了个头。", "move input 这个词很怪。", ""]:
            self.assertFalse(server.leaked_tool_call(prose, names), prose)

    def test_request_and_json_are_strict(self):
        self.assertEqual(server.require_request({"runId": "r", "context": context(), "tools": tools()})[0], "r")
        self.assertEqual(server.require_cancel_request({"runId": "r"}), "r")
        with self.assertRaises(server.RequestValidationError):
            server.require_request({"runId": "r", "context": context()})
        with self.assertRaises(server.RequestValidationError):
            server.require_request({"runId": "r", "context": context(), "tools": tools(), "extra": True})
        with self.assertRaises(server.RequestValidationError):
            server.require_request({"runId": "r", "context": context(), "tools": [{"type": "function", "function": {"name": "bad name"}}]})
        with self.assertRaises(server.RequestValidationError):
            server.require_cancel_request({"runId": "r", "extra": True})
        with self.assertRaises(ValueError):
            server.strict_json_loads('{"x":NaN}')
        with self.assertRaises(server.RequestValidationError):
            server.http_tool_executor("https://example.com/tool", "0123456789abcdef")

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

        def completion(_config, _messages, _deadline, _run=None, tools=None):
            nonlocal completion_count
            with completion_lock:
                completion_count += 1
                call_number = completion_count
            if call_number == 1:
                old_started.set()
                self.assertTrue(release_old.wait(2), "old model call was not released")
            return {"choices": [{"message": {"role": "assistant", "content": ""}}]}

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
            data=json.dumps({"runId": "run-1", "context": context(), "tools": tools()}).encode("utf-8"),
            method="POST",
            headers={
                "authorization": f"Bearer {SERVICE_TOKEN}",
                "content-type": "application/json",
                "x-mineintent-tool-executor-url": "http://127.0.0.1:9/v1/tool",
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
                except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
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
            data=json.dumps({"runId": run_id, "context": context(), "tools": tools()}).encode("utf-8"),
            method="POST",
            headers={
                "authorization": f"Bearer {SERVICE_TOKEN}",
                "content-type": "application/json",
                "x-mineintent-tool-executor-url": "http://127.0.0.1:9/v1/tool",
                "x-mineintent-tool-executor-token": "0123456789abcdef",
            },
        )


def context():
    return {
        "protocol": "mineintent.agent-context.v2",
        "stable": {
            "profile": {"content": "朋友"},
            "memories": [{"kind": "note", "summary": "玩家怕高", "createdAt": "2026-07-01T00:00:00Z"}],
        },
        "frame": {
            "at": "2026-07-25T00:00:00Z", "player": {"username": "Alex", "text": "看看羊"},
            "world": {"dimension": "overworld"},
            "self": {"position": [0, 64, 0], "yawDegrees": 0, "pitchDegrees": 0},
            "events": [], "omissions": [],
        },
    }


if __name__ == "__main__":
    unittest.main()
