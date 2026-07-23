"""Loopback OpenAI-compatible transport for the D40 experiment."""
from __future__ import annotations

import json
import hmac
import math
import os
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from prompt import system_prompt

_MAX_JSON_BYTES = 1_048_576
_MAX_TOOL_RESULT_BYTES = 262_144
_MAX_TOOL_ROUNDS = 16
_ROUND_TIMEOUT_S = 180.0

# Extension point: a future observed-space navigation tool can be added beside these inputs.
# It must not silently replace move_input, and it is intentionally not implemented in D40.
D40_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "look_relative",
            "description": "Turn the first-person view briefly, then receive a fresh viewport. Positive yaw is right; positive pitch is down.",
            "parameters": {
                "type": "object",
                "properties": {
                    "yaw_degrees": {"type": "number", "minimum": -90, "maximum": 90},
                    "pitch_degrees": {"type": "number", "minimum": -90, "maximum": 90},
                },
                "required": ["yaw_degrees", "pitch_degrees"],
                "additionalProperties": False,
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "move_input",
            "description": "Hold one real movement input briefly, release it, then receive actual movement feedback and a fresh viewport.",
            "parameters": {
                "type": "object",
                "properties": {
                    "direction": {"type": "string", "enum": ["forward", "back", "left", "right"]},
                    "duration_ms": {"type": "integer", "minimum": 50, "maximum": 1500},
                    "sprint": {"type": "boolean"},
                },
                "required": ["direction", "duration_ms"],
                "additionalProperties": False,
            },
        },
    },
]


class ConfigError(RuntimeError):
    pass


class RequestValidationError(ValueError):
    pass


class RoundDeadlineExceeded(TimeoutError):
    pass


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):  # noqa: ANN001, ARG002
        return None


def strict_json_loads(raw: bytes | str) -> object:
    if isinstance(raw, bytes):
        if len(raw) > _MAX_JSON_BYTES:
            raise ValueError("JSON too large")
        text = raw.decode("utf-8", errors="strict")
    else:
        text = raw
        if len(text.encode("utf-8", errors="strict")) > _MAX_JSON_BYTES:
            raise ValueError("JSON too large")
    value = json.loads(text, parse_constant=lambda token: (_ for _ in ()).throw(ValueError(token)))
    _validate_json(value)
    return value


def strict_json_dumps(value: object) -> bytes:
    _validate_json(value)
    raw = json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")
    if len(raw) > _MAX_JSON_BYTES:
        raise ValueError("JSON too large")
    return raw


def _validate_json(root: object) -> None:
    pending = [root]
    while pending:
        value = pending.pop()
        if isinstance(value, str):
            value.encode("utf-8", errors="strict")
        elif isinstance(value, float) and not math.isfinite(value):
            raise ValueError("non-finite number")
        elif isinstance(value, int) and not isinstance(value, bool) and abs(value) > 9_007_199_254_740_991:
            raise ValueError("integer outside safe range")
        elif isinstance(value, list):
            pending.extend(value)
        elif isinstance(value, dict):
            pending.extend(value.values())


def _load_dotenv(path: Path) -> None:
    if not path.is_file():
        return
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and "=" in stripped:
            key, value = stripped.split("=", 1)
            os.environ.setdefault(key.strip(), value.strip())


def load_config(env: dict = os.environ) -> dict:
    required = {
        "base_url": env.get("MINEINTENT_MODEL_BASE_URL", "").strip().rstrip("/"),
        "api_key": env.get("MINEINTENT_MODEL_API_KEY", "").strip(),
        "model": env.get("MINEINTENT_MODEL", "").strip(),
        "service_token": env.get("MINEINTENT_AGENT_SERVICE_TOKEN", "").strip(),
    }
    for name, value in required.items():
        if not value:
            raise ConfigError(f"{name} is required")
    try:
        port = int(env.get("MINEINTENT_AGENT_SERVICE_PORT", "8765"))
    except ValueError as error:
        raise ConfigError("agent service port must be an integer") from error
    if not 1 <= port <= 65535:
        raise ConfigError("agent service port is outside 1-65535")
    token = required["service_token"]
    if not 32 <= len(token) <= 512 or any(ord(character) < 33 or ord(character) > 126 for character in token):
        raise ConfigError("agent service token must be 32-512 printable ASCII characters")
    if hmac.compare_digest(token.encode("utf-8"), required["api_key"].encode("utf-8")):
        raise ConfigError("agent service token must differ from the model API key")
    return {**required, "port": port}


def authorized(header: str | None, expected_token: str) -> bool:
    if not isinstance(header, str):
        return False
    return hmac.compare_digest(
        header.encode("utf-8", errors="surrogatepass"),
        f"Bearer {expected_token}".encode("utf-8"),
    )


def remaining_seconds(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RoundDeadlineExceeded("deadline_exceeded")
    return remaining


def require_request(value: object) -> tuple[str, dict]:
    if not isinstance(value, dict) or set(value) != {"runId", "context"}:
        raise RequestValidationError("request must contain only runId and context")
    run_id, context = value.get("runId"), value.get("context")
    if not isinstance(run_id, str) or not run_id or len(run_id) > 128:
        raise RequestValidationError("runId is invalid")
    if not isinstance(context, dict) or context.get("protocol") != "mineintent.d40-context.v1":
        raise RequestValidationError("context protocol is invalid")
    return run_id, context


def http_tool_executor(url: str, token: str):
    parsed = urllib.parse.urlsplit(url)
    if (
        len(url) > 2048 or parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "::1", "localhost"}
        or parsed.username is not None or parsed.password is not None or parsed.fragment
    ):
        raise RequestValidationError("tool executor must be an uncredentialed loopback HTTP URL")
    try:
        parsed.port
    except ValueError as error:
        raise RequestValidationError("tool executor port is invalid") from error
    if not 16 <= len(token) <= 512 or "\r" in token or "\n" in token:
        raise RequestValidationError("tool executor token is invalid")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())

    def execute(run_id: str, name: str, arguments: dict, deadline: float) -> object:
        request = urllib.request.Request(
            url, data=strict_json_dumps({"runId": run_id, "name": name, "arguments": arguments}), method="POST",
            headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
        )
        try:
            with opener.open(request, timeout=remaining_seconds(deadline)) as response:
                raw = response.read(_MAX_TOOL_RESULT_BYTES + 1)
                if len(raw) > _MAX_TOOL_RESULT_BYTES:
                    raise RuntimeError("tool result too large")
                result = strict_json_loads(raw)
                remaining_seconds(deadline)
                return result
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"tool executor failed ({error.code})") from None
        except (urllib.error.URLError, TimeoutError):
            if time.monotonic() >= deadline:
                raise RoundDeadlineExceeded("deadline_exceeded") from None
            raise RuntimeError("tool executor failed") from None

    return execute


def model_completion(config: dict, messages: list[dict], deadline: float) -> dict:
    body = strict_json_dumps({
        "model": config["model"], "response_format": {"type": "json_object"},
        "messages": messages, "tools": D40_TOOLS, "tool_choice": "auto",
    })
    request = urllib.request.Request(
        f"{config['base_url']}/chat/completions", data=body, method="POST",
        headers={"authorization": f"Bearer {config['api_key']}", "content-type": "application/json"},
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())
    try:
        with opener.open(request, timeout=remaining_seconds(deadline)) as response:
            payload = strict_json_loads(response.read(_MAX_JSON_BYTES + 1))
            remaining_seconds(deadline)
            return payload  # type: ignore[return-value]
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"model request failed ({error.code})") from None
    except (urllib.error.URLError, TimeoutError):
        if time.monotonic() >= deadline:
            raise RoundDeadlineExceeded("deadline_exceeded") from None
        raise RuntimeError("model request failed") from None


def run_tool_loop(
    config: dict,
    run_id: str,
    context: dict,
    execute_tool,
    deadline: float | None = None,
) -> tuple[dict, dict | None]:
    deadline = deadline if deadline is not None else time.monotonic() + _ROUND_TIMEOUT_S
    messages = [
        {"role": "system", "content": system_prompt()},
        {"role": "user", "content": json.dumps(context, ensure_ascii=False, separators=(",", ":"))},
    ]
    usage = {"prompt_tokens": 0, "completion_tokens": 0}
    has_usage = False
    for _round in range(_MAX_TOOL_ROUNDS):
        remaining_seconds(deadline)
        payload = model_completion(config, messages, deadline)
        choices = payload.get("choices") if isinstance(payload, dict) else None
        message = choices[0].get("message") if isinstance(choices, list) and choices and isinstance(choices[0], dict) else None
        if not isinstance(message, dict):
            raise RuntimeError("model response has no assistant message")
        raw_usage = payload.get("usage")
        if isinstance(raw_usage, dict):
            for key in usage:
                value = raw_usage.get(key)
                if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
                    usage[key] += value
                    has_usage = True
        tool_calls = message.get("tool_calls")
        if isinstance(tool_calls, list) and tool_calls:
            replay = {key: message[key] for key in ("role", "content", "reasoning_content", "tool_calls") if key in message}
            replay.setdefault("role", "assistant")
            messages.append(replay)
            for index, call in enumerate(tool_calls):
                call_id = call.get("id") if isinstance(call, dict) else None
                function = call.get("function") if isinstance(call, dict) else None
                if not isinstance(call_id, str) or not isinstance(function, dict):
                    raise RuntimeError("model returned an invalid tool call")
                if index > 0:
                    result = {"status": "failed", "summary": "parallel_body_tools_are_not_supported"}
                else:
                    try:
                        name = function.get("name")
                        arguments = strict_json_loads(function.get("arguments", ""))
                        if not isinstance(name, str) or not isinstance(arguments, dict):
                            raise ValueError("invalid tool call")
                        _validate_tool_arguments(name, arguments)
                        result = execute_tool(run_id, name, arguments, deadline)
                    except (ValueError, RequestValidationError) as error:
                        result = {"status": "failed", "summary": str(error)[:300]}
                messages.append({
                    "role": "tool", "tool_call_id": call_id,
                    "content": json.dumps(result, ensure_ascii=False, separators=(",", ":")),
                })
            continue
        content = message.get("content")
        if not isinstance(content, str):
            raise RuntimeError("model final content is missing")
        decision = strict_json_loads(content)
        remaining_seconds(deadline)
        return _validate_decision(decision), usage if has_usage else None
    raise RuntimeError("tool loop exceeded its round limit")


def _validate_tool_arguments(name: str, arguments: dict) -> None:
    if name == "look_relative":
        if set(arguments) != {"yaw_degrees", "pitch_degrees"}:
            raise RequestValidationError("look_relative arguments are invalid")
        values = (arguments["yaw_degrees"], arguments["pitch_degrees"])
        if any(isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or abs(value) > 90 for value in values):
            raise RequestValidationError("look_relative angles are invalid")
    elif name == "move_input":
        if not {"direction", "duration_ms"} <= set(arguments) <= {"direction", "duration_ms", "sprint"}:
            raise RequestValidationError("move_input arguments are invalid")
        if arguments["direction"] not in {"forward", "back", "left", "right"}:
            raise RequestValidationError("move_input direction is invalid")
        duration = arguments["duration_ms"]
        if isinstance(duration, bool) or not isinstance(duration, int) or not 50 <= duration <= 1500:
            raise RequestValidationError("move_input duration is invalid")
        if "sprint" in arguments and not isinstance(arguments["sprint"], bool):
            raise RequestValidationError("move_input sprint is invalid")
    else:
        raise RequestValidationError("unknown body tool")


def _validate_decision(value: object) -> dict:
    if not isinstance(value, dict) or set(value) != {"protocol", "speech"}:
        raise RuntimeError("final decision has invalid fields")
    if value.get("protocol") != "mineintent.d40-decision.v1":
        raise RuntimeError("final decision protocol is invalid")
    speech = value.get("speech")
    if speech is not None and (not isinstance(speech, str) or not speech.strip() or len(speech) > 500):
        raise RuntimeError("final speech is invalid")
    return value


class Handler(BaseHTTPRequestHandler):
    server_version = "MineIntentAgent/0.1"

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/decide":
            self._send(404, {"error": "not_found"})
            return
        config = self.server.config  # type: ignore[attr-defined]
        if not authorized(self.headers.get("authorization"), config["service_token"]):
            self._send(401, {"error": "unauthorized"})
            return
        decision_lock = self.server.decision_lock  # type: ignore[attr-defined]
        if not decision_lock.acquire(blocking=False):
            self._send(429, {"error": "agent_service_busy"})
            return
        deadline = time.monotonic() + _ROUND_TIMEOUT_S
        try:
            self.connection.settimeout(remaining_seconds(deadline))
            content_type = self.headers.get("content-type", "").split(";", 1)[0].strip().lower()
            if content_type != "application/json":
                raise RequestValidationError("content type must be application/json")
            length = int(self.headers.get("content-length", "-1"))
            if length < 0 or length > _MAX_JSON_BYTES:
                raise RequestValidationError("content length is invalid")
            raw_request = self.rfile.read(length)
            if len(raw_request) != length:
                raise RequestValidationError("request body is incomplete")
            remaining_seconds(deadline)
            run_id, context = require_request(strict_json_loads(raw_request))
            callback_url = self.headers.get("x-mineintent-tool-executor-url", "")
            callback_token = self.headers.get("x-mineintent-tool-executor-token", "")
            decision, raw_usage = run_tool_loop(
                config,
                run_id,
                context,
                http_tool_executor(callback_url, callback_token),
                deadline,
            )
            usage = None
            if isinstance(raw_usage, dict):
                usage = {
                    "inputTokens": raw_usage.get("prompt_tokens"),
                    "outputTokens": raw_usage.get("completion_tokens"),
                }
                usage = {
                    key: value for key, value in usage.items()
                    if isinstance(value, int) and not isinstance(value, bool) and value >= 0
                }
            self._send(200, {"decision": decision, "model": config["model"], **({"usage": usage} if usage else {})})
        except (RoundDeadlineExceeded, TimeoutError):
            self._send(504, {"error": "deadline_exceeded"})
        except (ValueError, UnicodeError, RequestValidationError) as error:
            self._send(400, {"error": "invalid_request", "detail": str(error)[:200]})
        except RuntimeError as error:
            self._send(502, {"error": str(error)[:200]})
        except Exception:  # noqa: BLE001
            self._send(502, {"error": "agent_service_failed"})
        finally:
            decision_lock.release()

    def log_message(self, format_string: str, *args: object) -> None:
        print(format_string % args, file=sys.stderr)

    def _send(self, status: int, value: object) -> None:
        payload = strict_json_dumps(value)
        self.send_response(status)
        self.send_header("content-type", "application/json; charset=utf-8")
        self.send_header("content-length", str(len(payload)))
        self.send_header("cache-control", "no-store")
        self.end_headers()
        self.wfile.write(payload)


def main() -> None:
    _load_dotenv(Path(__file__).resolve().parents[1] / ".env")
    config = load_config()
    server = ThreadingHTTPServer(("127.0.0.1", config["port"]), Handler)
    server.daemon_threads = True
    server.config = config  # type: ignore[attr-defined]
    server.decision_lock = threading.Lock()  # type: ignore[attr-defined]
    print(f"MineIntent agent service listening on http://127.0.0.1:{config['port']}", file=sys.stderr)
    server.serve_forever()


if __name__ == "__main__":
    main()
