"""Loopback agent service: the companion agent proper (prompt, model calls, tool loop).

The tool contract arrives with each request from the tool backend; this side owns none of it.
"""
from __future__ import annotations

import hashlib
import hmac
import http.client
import json
import math
import os
import re
import socket
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from prompt import stable_context, system_prompt

_MAX_JSON_BYTES = 1_048_576
_MAX_TOOL_RESULT_BYTES = 262_144
_MAX_MODEL_REQUESTS_PER_RUN = 16
# Model requests are not the only way a run can grow: one assistant message may carry many calls, and each
# is executed. A body input is real time in the world, so the count of executed calls needs its own
# ceiling rather than inheriting the HTTP size limit as an accidental one.
_MAX_TOOL_CALLS_PER_RESPONSE = 8
_MAX_TOOL_CALLS_PER_RUN = 32
_RUN_TIMEOUT_S = 180.0
_MAX_TOOLS = 32
_CONTEXT_PROTOCOL = "mineintent.agent-context.v3"
_TOOL_RESPONSE_PROTOCOL = "mineintent.tool-response.v2"
_RUN_PROTOCOL = "mineintent.agent-run.v1"
_TOOL_NAME_PATTERN = re.compile(r"[A-Za-z0-9_-]{1,64}")
_TOOL_CALL_ID_PATTERN = re.compile(r"[\x21-\x7e]{1,128}")
_TRANSCRIPT_NAME = "agent-transcripts.jsonl"
_MAX_TRANSCRIPT_CHARS = 262_144
_MAX_TRANSCRIPT_BYTES = 32 * 1_024 * 1_024

# Vendor markup that must never reach `content`: a tool call the provider failed to lift out of
# the raw generation. Seen live as verbatim internal markup on one endpoint and as invented JSON
# fields on another; both carry a real action the transport dropped.
_LEAK_MARKUP = re.compile(r"DSML|<\s*tool_calls|invoke\s+name=|__tool__")

# The only terminations that mean "the model finished saying what it meant to say". Everything else
# — `length`, `content_filter`, a provider's own resource codes, anything unrecognised — produced a
# message that was cut short by something other than the model, so treating it as a normal close
# would hand the player a half-sentence or drop a call. An allowlist rather than a denylist because
# the failure mode of a missed code is silent, and the three providers do not agree on the set.
_ACCEPTED_FINISH_REASONS = frozenset({"stop", "tool_calls", "function_call"})


def accept_finish_reason(reason: object) -> str | None:
    """Returns a failure summary when a completion did not end on its own terms, else None.

    A missing or null reason is accepted: it is what a provider sends when it declines to report
    one, and refusing those would fail runs over a field no request depends on.
    """
    if reason is None:
        return None
    if not isinstance(reason, str) or not reason:
        return f"model completion reported an invalid finish_reason: {reason!r}"
    if reason in _ACCEPTED_FINISH_REASONS:
        return None
    return f"model completion ended on finish_reason={reason}"


def leaked_tool_call(text: str, tool_names: list[str]) -> bool:
    """True when closing text carries an unexecuted tool call in a high-confidence form.

    Only call-shaped mentions count (quoted as a JSON value, or followed by an argument bracket);
    the closing text is discarded rather than spoken, so a prose mention of a tool name is noise,
    not a dropped action.
    """
    if _LEAK_MARKUP.search(text):
        return True
    if not tool_names:
        return False
    names = "|".join(re.escape(name) for name in tool_names)
    return re.search(rf'"(?:{names})"|(?:{names})\s*[([{{]', text) is not None


class ConfigError(RuntimeError):
    pass


class RequestValidationError(ValueError):
    pass


class RunDeadlineExceeded(TimeoutError):
    pass


class RunCancelled(RuntimeError):
    pass


class DecisionRun:
    def __init__(self, run_id: str):
        self.run_id = run_id
        self.cancelled = threading.Event()
        self._cancel_lock = threading.Lock()
        self._cancel_callbacks = set()

    def cancel(self) -> None:
        with self._cancel_lock:
            if self.cancelled.is_set():
                return
            self.cancelled.set()
            callbacks = tuple(self._cancel_callbacks)
            self._cancel_callbacks.clear()
        for callback in callbacks:
            callback()

    def on_cancel(self, callback):
        with self._cancel_lock:
            cancelled = self.cancelled.is_set()
            if not cancelled:
                self._cancel_callbacks.add(callback)
        if cancelled:
            callback()

        def remove() -> None:
            with self._cancel_lock:
                self._cancel_callbacks.discard(callback)

        return remove

    def ensure_active(self) -> None:
        if self.cancelled.is_set():
            raise RunCancelled("run_cancelled")


class DecisionRuns:
    """Tracks the one authoritative run without waiting for superseded I/O."""

    def __init__(self):
        self._lock = threading.Lock()
        self._active: DecisionRun | None = None

    def begin(self, run_id: str) -> DecisionRun | None:
        with self._lock:
            if self._active is not None and self._active.run_id == run_id:
                return None
            if self._active is not None:
                self._active.cancel()
            run = DecisionRun(run_id)
            self._active = run
        return run

    def cancel(self, run_id: str) -> bool:
        with self._lock:
            if self._active is None or self._active.run_id != run_id:
                return False
            run = self._active
        run.cancel()
        return True

    def finish(self, run: DecisionRun) -> None:
        with self._lock:
            if self._active is run:
                self._active = None


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
    effort = env.get("MINEINTENT_MODEL_REASONING_EFFORT", "").strip()
    if effort and effort not in {"low", "medium", "high"}:
        raise ConfigError("reasoning effort must be low, medium or high")
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
    return {**required, "reasoning_effort": effort, "port": port}


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
        raise RunDeadlineExceeded("deadline_exceeded")
    return remaining


def require_request(value: object) -> tuple[str, dict, list]:
    if not isinstance(value, dict) or set(value) != {"runId", "context", "tools"}:
        raise RequestValidationError("request must contain only runId, context and tools")
    run_id, context, tools = value.get("runId"), value.get("context"), value.get("tools")
    if not isinstance(run_id, str) or not run_id or len(run_id) > 128:
        raise RequestValidationError("runId is invalid")
    if not isinstance(context, dict) or context.get("protocol") != _CONTEXT_PROTOCOL:
        raise RequestValidationError("context protocol is invalid")
    if not isinstance(tools, list) or len(tools) > _MAX_TOOLS:
        raise RequestValidationError("tools are invalid")
    for tool in tools:
        function = tool.get("function") if isinstance(tool, dict) else None
        name = function.get("name") if isinstance(function, dict) else None
        if (not isinstance(tool, dict) or tool.get("type") != "function"
                or not isinstance(name, str) or not _TOOL_NAME_PATTERN.fullmatch(name)):
            raise RequestValidationError("tools are invalid")
    return run_id, context, tools


def require_cancel_request(value: object) -> str:
    if not isinstance(value, dict) or set(value) != {"runId"}:
        raise RequestValidationError("cancel request must contain only runId")
    run_id = value.get("runId")
    if not isinstance(run_id, str) or not run_id or len(run_id) > 128:
        raise RequestValidationError("runId is invalid")
    return run_id


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

    def execute(run_id: str, name: str, arguments: dict, deadline: float,
                tool_call_id: str = "") -> object:
        request = urllib.request.Request(
            url,
            data=strict_json_dumps({
                "runId": run_id, "toolCallId": tool_call_id,
                "name": name, "arguments": arguments,
            }),
            method="POST",
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
                raise RunDeadlineExceeded("deadline_exceeded") from None
            raise RuntimeError("tool executor failed") from None

    return execute


def model_completion(
    config: dict,
    messages: list[dict],
    deadline: float,
    run: DecisionRun | None = None,
    tools: list | None = None,
) -> dict:
    # Ordinary OpenAI-compatible tool calling: tools are offered on every request and the closing
    # response is plain assistant text. `response_format` is deliberately never sent. Asking the model
    # to author a JSON envelope in a request that also offers tools puts two output contracts in
    # competition, and measured across seven models on three providers, six then stopped emitting
    # structured `tool_calls` — arguably correctly, since a tool call is not a JSON object in the
    # content channel. Speech goes through the `say` tool; the closing text is transcript only.
    request: dict = {
        "model": config["model"], "messages": messages,
        "tools": tools or [], "tool_choice": "auto",
    }
    if config.get("reasoning_effort"):
        request["reasoning_effort"] = config["reasoning_effort"]
    body = strict_json_dumps(request)
    parsed = urllib.parse.urlsplit(f"{config['base_url']}/chat/completions")
    if (
        parsed.scheme not in {"http", "https"} or parsed.hostname is None
        or parsed.username is not None or parsed.password is not None or parsed.fragment or parsed.query
    ):
        raise RuntimeError("model endpoint is invalid")
    try:
        port = parsed.port
    except ValueError:
        raise RuntimeError("model endpoint is invalid") from None
    connection_type = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    connection = connection_type(parsed.hostname, port, timeout=remaining_seconds(deadline))

    def cancel_connection() -> None:
        active_socket = connection.sock
        if active_socket is None:
            return
        if os.name == "nt":
            # HTTPResponse owns a socket.makefile() object, so socket.close() defers closing the
            # Winsock handle. Detach it and use the descriptor-level socket.close() instead;
            # os.close() is not valid for Windows socket handles.
            try:
                socket_handle = active_socket.detach()
            except OSError:
                return
            if socket_handle >= 0:
                try:
                    socket.close(socket_handle)
                except OSError:
                    pass
            return
        try:
            active_socket.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    remove_cancel = run.on_cancel(cancel_connection) if run is not None else lambda: None
    try:
        if run is not None:
            run.ensure_active()
        connection.connect()
        if run is not None:
            run.ensure_active()
        if connection.sock is not None:
            connection.sock.settimeout(remaining_seconds(deadline))
        path = urllib.parse.urlunsplit(("", "", parsed.path or "/", "", ""))
        connection.request("POST", path, body=body, headers={
            "authorization": f"Bearer {config['api_key']}",
            "content-type": "application/json",
        })
        response = connection.getresponse()
        if run is not None:
            run.ensure_active()
        if not 200 <= response.status < 300:
            raise RuntimeError(f"model request failed ({response.status})")
        raw = response.read(_MAX_JSON_BYTES + 1)
        if run is not None:
            run.ensure_active()
        if len(raw) > _MAX_JSON_BYTES:
            raise RuntimeError("model response too large")
        payload = strict_json_loads(raw)
        if run is not None:
            run.ensure_active()
        remaining_seconds(deadline)
        return payload  # type: ignore[return-value]
    except RunCancelled:
        raise
    except RuntimeError:
        if run is not None:
            run.ensure_active()
        raise
    except (ValueError, UnicodeError):
        if run is not None:
            run.ensure_active()
        raise RuntimeError("model response is invalid") from None
    except (OSError, http.client.HTTPException, TimeoutError):
        if run is not None:
            run.ensure_active()
        if time.monotonic() >= deadline:
            raise RunDeadlineExceeded("deadline_exceeded") from None
        raise RuntimeError("model request failed") from None
    finally:
        remove_cancel()
        connection.close()


def model_tool_output(value: object) -> dict:
    """Validates the executor envelope and removes its transport version before model replay."""
    expected = {"protocol", "result", "observationAfter"}
    if not isinstance(value, dict) or set(value) != expected or value.get("protocol") != _TOOL_RESPONSE_PROTOCOL:
        raise RuntimeError("tool executor returned an invalid response")
    observation = value.get("observationAfter")
    if observation is not None and not isinstance(observation, dict):
        raise RuntimeError("tool executor returned an invalid observationAfter")
    return {"result": value.get("result"), "observationAfter": observation}


def claim_tool_call_batch(tool_calls: list, seen_ids: set[str]) -> list[tuple[str, dict]]:
    """Validates and claims every correlation id before any call can take effect."""
    calls: list[tuple[str, dict]] = []
    batch_ids: set[str] = set()
    for call in tool_calls:
        call_id = call.get("id") if isinstance(call, dict) else None
        function = call.get("function") if isinstance(call, dict) else None
        if not isinstance(call_id, str) or not _TOOL_CALL_ID_PATTERN.fullmatch(call_id) or not isinstance(function, dict):
            raise RuntimeError("model returned an invalid tool call")
        if call_id in seen_ids or call_id in batch_ids:
            raise RuntimeError("model reused a tool call id")
        batch_ids.add(call_id)
        calls.append((call_id, function))
    seen_ids.update(batch_ids)
    return calls


def _non_negative_int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def normalize_usage(raw_usage: object) -> dict:
    """Maps one provider's usage payload onto our four counters.

    All three providers speak OpenAI's wire format but report cache hits in two different places:
    DeepSeek puts `prompt_cache_hit_tokens` at the top level, while Zhipu and Moonshot follow
    OpenAI's `prompt_tokens_details.cached_tokens`. Recording the hit count is what turns cache
    behaviour into something measured instead of argued about, and it matters here more than usual:
    every provider refuses to cache a prefix below a floor (256 tokens on Moonshot, 512 on Zhipu,
    64-token units on DeepSeek), so a prompt shaped like ours can hit exactly zero — and without
    this counter the only other evidence of that is the bill.

    A cache-write counter is not documented for any of the three; it is read anyway because
    DeepSeek-style proxies do report one, and an absent key simply yields no entry.
    """
    if not isinstance(raw_usage, dict):
        return {}
    details = raw_usage.get("prompt_tokens_details")
    details = details if isinstance(details, dict) else {}
    candidates = {
        "prompt_tokens": (raw_usage.get("prompt_tokens"),),
        "completion_tokens": (raw_usage.get("completion_tokens"),),
        "cache_read_tokens": (raw_usage.get("prompt_cache_hit_tokens"), details.get("cached_tokens")),
        "cache_write_tokens": (details.get("cache_write_tokens"),),
    }
    counters: dict[str, int] = {}
    for target, sources in candidates.items():
        for source in sources:
            value = _non_negative_int(source)
            if value is not None:
                counters[target] = value
                break
    return counters


def run_tool_loop(
    config: dict,
    run_id: str,
    context: dict,
    execute_tool,
    tools: list,
    deadline: float | None = None,
    run: DecisionRun | None = None,
) -> tuple[str, dict | None]:
    deadline = deadline if deadline is not None else time.monotonic() + _RUN_TIMEOUT_S
    tool_names = [tool["function"]["name"] for tool in tools]
    # Static first, volatile last, and nothing volatile ever re-rendered: the system message carries
    # behaviour plus slow-changing memory, then the opening frame is appended, then
    # later requests append after that. Every provider's cache is prefix-only, so the ordering is the design.
    messages = [
        {"role": "system", "content": system_prompt() + stable_context(context.get("stable"))},
        {"role": "user", "content": json.dumps(context.get("frame", {}), ensure_ascii=False, separators=(",", ":"))},
    ]
    # Summed across requests: the run's hit rate is only meaningful
    # against the run's total prompt tokens.
    usage: dict[str, int] = {}
    closing: str | None = None
    error_summary: str | None = None
    executed_calls = 0
    seen_tool_call_ids: set[str] = set()
    try:
        for _ in range(_MAX_MODEL_REQUESTS_PER_RUN):
            if run is not None:
                run.ensure_active()
            remaining_seconds(deadline)
            payload = model_completion(config, messages, deadline, run, tools=tools)
            if run is not None:
                run.ensure_active()
            choices = payload.get("choices") if isinstance(payload, dict) else None
            choice = choices[0] if isinstance(choices, list) and choices and isinstance(choices[0], dict) else None
            message = choice.get("message") if choice is not None else None
            if not isinstance(message, dict):
                raise RuntimeError("model response has no assistant message")
            for key, value in normalize_usage(payload.get("usage")).items():
                usage[key] = usage.get(key, 0) + value
            # Checked before anything in the message is acted on: a truncated or filtered completion
            # can carry both half-written call arguments and half-written closing text.
            rejection = accept_finish_reason(choice.get("finish_reason"))
            if rejection is not None:
                raise RuntimeError(rejection)
            tool_calls = message.get("tool_calls")
            if isinstance(tool_calls, list) and tool_calls:
                if len(tool_calls) > _MAX_TOOL_CALLS_PER_RESPONSE:
                    raise RuntimeError(
                        f"model requested {len(tool_calls)} tool calls in one response, over the limit of {_MAX_TOOL_CALLS_PER_RESPONSE}"
                    )
                if executed_calls + len(tool_calls) > _MAX_TOOL_CALLS_PER_RUN:
                    raise RuntimeError(
                        f"model exceeded the run limit of {_MAX_TOOL_CALLS_PER_RUN} tool calls"
                    )
                calls = claim_tool_call_batch(tool_calls, seen_tool_call_ids)
                executed_calls += len(tool_calls)
                replay = {key: message[key] for key in ("role", "content", "reasoning_content", "tool_calls") if key in message}
                replay.setdefault("role", "assistant")
                messages.append(replay)
                # Every requested call executes, in order. Parallel calls are not pre-rejected:
                # constraints belong in the prompt as advice, failures belong to the world, and the
                # tool backend answers invalid or conflicting requests with honest failed results.
                for call_id, function in calls:
                    try:
                        name = function.get("name")
                        arguments = strict_json_loads(function.get("arguments", ""))
                        if (
                            not isinstance(name, str) or not _TOOL_NAME_PATTERN.fullmatch(name)
                            or not isinstance(arguments, dict)
                        ):
                            raise ValueError("invalid tool call")
                        if run is not None:
                            run.ensure_active()
                        output = model_tool_output(
                            execute_tool(run_id, name, arguments, deadline, call_id)
                        )
                        if run is not None:
                            run.ensure_active()
                    except (ValueError, RequestValidationError) as error:
                        output = {
                            "result": {"status": "failed", "summary": str(error)[:300]},
                            "observationAfter": None,
                        }
                    messages.append({
                        "role": "tool", "tool_call_id": call_id,
                        "content": json.dumps(output, ensure_ascii=False, separators=(",", ":")),
                    })
                continue
            content = message.get("content")
            if not isinstance(content, str):
                raise RuntimeError("model final content is missing")
            closing = content.strip()
            # Speech went through the say tool during the loop; the closing text is transcript-only.
            # A call-shaped tool mention in it is a real action the provider dropped, though, and
            # silently discarding that would be a fabricated success.
            if leaked_tool_call(closing, tool_names):
                raise RuntimeError("closing message carries a tool call that was never executed")
            if run is not None:
                run.ensure_active()
            remaining_seconds(deadline)
            return closing, (usage or None)
        raise RuntimeError("tool loop exceeded its model request limit")
    except BaseException as error:
        error_summary = f"{type(error).__name__}: {error}"[:300]
        raise
    finally:
        append_transcript(
            run_id, config.get("model", ""), tools, messages, closing,
            usage or None, error_summary, config.get("transcript_path"),
        )


def transcript_path(env: dict = os.environ) -> Path:
    """Honors MINEINTENT_DATA_DIR so transcripts land beside the journal, not in the CWD."""
    directory = env.get("MINEINTENT_DATA_DIR", "").strip()
    return (Path(directory) if directory else Path(".mineintent")) / _TRANSCRIPT_NAME


def append_transcript(
    run_id: str,
    model: str,
    tools: list,
    messages: list,
    closing: str | None,
    usage: dict | None,
    error_summary: str | None,
    path: Path | None = None,
) -> None:
    """Bounded per-run replay log: what the model was offered, saw and said — failures included.

    This closes the audit gap the live experiment hit: poses were journaled but the model-visible
    viewport per model request was not, so "what did it see at step 7" could only be reconstructed. The
    tool definitions are recorded too, since a schema change silently alters what the model could
    have done. Diagnostics must never fail a run, hence the blanket OSError swallow.

    Contents are sensitive — player chat, memories, viewports and reasoning traces — so
    the file is created 0600 and rotated rather than allowed to grow without bound.
    """
    target = path if path is not None else transcript_path()
    record = {
        "protocol": "mineintent.agent-transcript.v1", "runId": run_id, "model": model,
        "endedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "tools": [tool.get("function", {}).get("name") for tool in tools if isinstance(tool, dict)],
        "toolSchemas": tools,
        "closing": closing, "usage": usage, "error": error_summary, "messages": messages,
    }
    line = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
    if len(line) > _MAX_TRANSCRIPT_CHARS:
        # Keep the run auditable even when the replay itself is oversized.
        record["messages"] = None
        record["toolSchemas"] = None
        record["truncated"] = True
        line = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists() and target.stat().st_size + len(line) > _MAX_TRANSCRIPT_BYTES:
            target.replace(target.with_suffix(target.suffix + ".1"))
        descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(descriptor, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")
    except OSError:
        pass


class Handler(BaseHTTPRequestHandler):
    server_version = "MineIntentAgent/0.1"

    def do_POST(self) -> None:  # noqa: N802
        if self.path not in {"/v1/decide", "/v1/cancel"}:
            self._send(404, {"error": "not_found"})
            return
        config = self.server.config  # type: ignore[attr-defined]
        if not authorized(self.headers.get("authorization"), config["service_token"]):
            self._send(401, {"error": "unauthorized"})
            return
        deadline = time.monotonic() + _RUN_TIMEOUT_S
        try:
            self.connection.settimeout(remaining_seconds(deadline))
            value = self._read_json_request()
        except (RunDeadlineExceeded, TimeoutError):
            self._send(504, {"error": "deadline_exceeded"})
            return
        except (ValueError, UnicodeError, RequestValidationError) as error:
            self._send(400, {"error": "invalid_request", "detail": str(error)[:200]})
            return
        decision_runs = self.server.decision_runs  # type: ignore[attr-defined]
        if self.path == "/v1/cancel":
            try:
                run_id = require_cancel_request(value)
            except RequestValidationError as error:
                self._send(400, {"error": "invalid_request", "detail": str(error)[:200]})
                return
            self._send(200, {"cancelled": decision_runs.cancel(run_id)})
            return

        run = None
        try:
            remaining_seconds(deadline)
            run_id, context, tools = require_request(value)
            callback_url = self.headers.get("x-mineintent-tool-executor-url", "")
            callback_token = self.headers.get("x-mineintent-tool-executor-token", "")
            execute_tool = http_tool_executor(callback_url, callback_token)
            run = decision_runs.begin(run_id)
            if run is None:
                self._send(409, {"error": "run_already_active"})
                return
            _closing, raw_usage = run_tool_loop(
                config,
                run_id,
                context,
                execute_tool,
                tools,
                deadline,
                run,
            )
            run.ensure_active()
            usage = None
            if isinstance(raw_usage, dict):
                usage = {
                    "inputTokens": raw_usage.get("prompt_tokens"),
                    "outputTokens": raw_usage.get("completion_tokens"),
                    "cacheReadTokens": raw_usage.get("cache_read_tokens"),
                    "cacheWriteTokens": raw_usage.get("cache_write_tokens"),
                }
                usage = {
                    key: value for key, value in usage.items()
                    if isinstance(value, int) and not isinstance(value, bool) and value >= 0
                }
            # Speech already happened through the say tool while the run lived; the closing text
            # stays in the transcript. The response reports completion only.
            self._send(200, {"protocol": _RUN_PROTOCOL, "model": config["model"], **({"usage": usage} if usage else {})})
        except RunCancelled:
            self._send(409, {"error": "run_cancelled"})
        except (RunDeadlineExceeded, TimeoutError):
            self._send(504, {"error": "deadline_exceeded"})
        except (ValueError, UnicodeError, RequestValidationError) as error:
            self._send(400, {"error": "invalid_request", "detail": str(error)[:200]})
        except RuntimeError as error:
            self._send(502, {"error": str(error)[:200]})
        except Exception:  # noqa: BLE001
            self._send(502, {"error": "agent_service_failed"})
        finally:
            if run is not None:
                decision_runs.finish(run)

    def do_GET(self) -> None:  # noqa: N802
        if self.path != "/healthz":
            self._send(404, {"error": "not_found"})
            return
        # startedAt + envSha256 let the caller detect a stale process serving a rotated .env —
        # a failure mode hit twice live, where SO_REUSEADDR let an old instance keep the port.
        self._send(200, self.server.health)  # type: ignore[attr-defined]

    def log_message(self, format_string: str, *args: object) -> None:
        print(format_string % args, file=sys.stderr)

    def _send(self, status: int, value: object) -> None:
        payload = strict_json_dumps(value)
        try:
            self.send_response(status)
            self.send_header("content-type", "application/json; charset=utf-8")
            self.send_header("content-length", str(len(payload)))
            self.send_header("cache-control", "no-store")
            self.end_headers()
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _read_json_request(self) -> object:
        content_type = self.headers.get("content-type", "").split(";", 1)[0].strip().lower()
        if content_type != "application/json":
            raise RequestValidationError("content type must be application/json")
        try:
            length = int(self.headers.get("content-length", "-1"))
        except ValueError as error:
            raise RequestValidationError("content length is invalid") from error
        if length < 0 or length > _MAX_JSON_BYTES:
            raise RequestValidationError("content length is invalid")
        raw_request = self.rfile.read(length)
        if len(raw_request) != length:
            raise RequestValidationError("request body is incomplete")
        return strict_json_loads(raw_request)


class AgentHTTPServer(ThreadingHTTPServer):
    # On Windows SO_REUSEADDR lets a second instance bind the same port and silently share
    # traffic with a stale one — observed live with an instance still holding a rotated key.
    # Exclusive bind on nt; POSIX keeps reuse for painless restarts out of TIME_WAIT.
    allow_reuse_address = os.name != "nt"


def main() -> None:
    env_file = Path(__file__).resolve().parents[1] / ".env"
    _load_dotenv(env_file)
    config = load_config()
    server = AgentHTTPServer(("127.0.0.1", config["port"]), Handler)
    server.daemon_threads = True
    server.config = config  # type: ignore[attr-defined]
    server.decision_runs = DecisionRuns()  # type: ignore[attr-defined]
    server.health = {  # type: ignore[attr-defined]
        "status": "ok",
        "startedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "pid": os.getpid(),
        "envSha256": hashlib.sha256(env_file.read_bytes()).hexdigest() if env_file.is_file() else None,
    }
    print(f"MineIntent agent service listening on http://127.0.0.1:{config['port']}", file=sys.stderr)
    server.serve_forever()


if __name__ == "__main__":
    main()
