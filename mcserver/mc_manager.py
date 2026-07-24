#!/usr/bin/env python3
"""Small dependency-free CLI/process manager for the Paper server in this folder."""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent
RUNTIME = ROOT / ".mc-server"
STATE = RUNTIME / "state.json"
LOG_DIR = ROOT / "logs"
CONSOLE_LOG = LOG_DIR / "console-latest.log"
CONFIG = ROOT / "mc-config.json"


DEFAULT_CONFIG = {
    "jar": "paper-1.21.1-133.jar",
    "java": "java",
    "minMemory": "2G",
    "maxMemory": "4G",
    "javaArgs": ["-XX:+UseG1GC"],
    "serverArgs": ["--nogui"],
}


def load_config() -> dict:
    config = DEFAULT_CONFIG.copy()
    if CONFIG.exists():
        config.update(json.loads(CONFIG.read_text(encoding="utf-8")))
    if not (ROOT / str(config["jar"])).is_file():
        jars = sorted(ROOT.glob("paper-*.jar"))
        if len(jars) == 1:
            config["jar"] = jars[0].name
    return config


def save_default_config() -> None:
    if not CONFIG.exists():
        CONFIG.write_text(json.dumps(DEFAULT_CONFIG, indent=2) + "\n", encoding="utf-8")


def read_state() -> dict | None:
    try:
        return json.loads(STATE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def request(command: str, argument: str = "", timeout: float = 3.0) -> dict:
    state = read_state()
    if not state:
        raise ConnectionError("服务器未运行")
    payload = {"token": state["token"], "command": command, "argument": argument}
    try:
        with socket.create_connection(("127.0.0.1", int(state["port"])), timeout) as conn:
            conn.sendall((json.dumps(payload) + "\n").encode())
            conn.settimeout(timeout)
            data = b""
            while b"\n" not in data:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                data += chunk
        return json.loads(data.decode().splitlines()[0])
    except (OSError, ValueError, KeyError) as exc:
        raise ConnectionError("管理进程没有响应；可运行 .\\mc.ps1 cleanup 清理失效状态") from exc


def java_command(config: dict) -> list[str]:
    jar = ROOT / str(config["jar"])
    if not jar.is_file():
        raise FileNotFoundError(f"找不到服务端 JAR：{jar.name}")
    java = str(config["java"])
    if not shutil.which(java) and not Path(java).is_file():
        raise FileNotFoundError(f"找不到 Java：{java}")
    return [
        java,
        f"-Xms{config['minMemory']}",
        f"-Xmx{config['maxMemory']}",
        *map(str, config.get("javaArgs", [])),
        "-jar",
        str(jar),
        *map(str, config.get("serverArgs", ["--nogui"])),
    ]


def accepted_eula() -> bool:
    path = ROOT / "eula.txt"
    if not path.exists():
        return False
    return any(line.strip().lower() == "eula=true" for line in path.read_text(encoding="utf-8").splitlines())


def daemon() -> int:
    RUNTIME.mkdir(exist_ok=True)
    LOG_DIR.mkdir(exist_ok=True)
    config = load_config()
    token = secrets.token_urlsafe(32)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    listener.settimeout(0.5)
    port = listener.getsockname()[1]

    log = CONSOLE_LOG.open("a", encoding="utf-8", buffering=1)
    log.write(f"\n===== manager start {time.strftime('%Y-%m-%d %H:%M:%S')} =====\n")
    try:
        server_creation_flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        server = subprocess.Popen(
            java_command(config), cwd=ROOT, stdin=subprocess.PIPE, stdout=log,
            stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace",
            bufsize=1, creationflags=server_creation_flags,
        )
        state = {"pid": os.getpid(), "serverPid": server.pid, "port": port, "token": token,
                 "started": time.time(), "jar": config["jar"]}
        STATE.write_text(json.dumps(state, indent=2), encoding="utf-8")

        stopping = False
        while server.poll() is None:
            try:
                conn, _ = listener.accept()
            except socket.timeout:
                continue
            with conn:
                try:
                    raw = b""
                    while b"\n" not in raw and len(raw) < 65536:
                        raw += conn.recv(4096)
                    msg = json.loads(raw.decode().splitlines()[0])
                    if not secrets.compare_digest(str(msg.get("token", "")), token):
                        reply = {"ok": False, "message": "unauthorized"}
                    elif msg.get("command") == "status":
                        reply = {"ok": True, "serverPid": server.pid, "uptime": time.time() - state["started"],
                                 "jar": config["jar"]}
                    elif msg.get("command") == "send":
                        command = str(msg.get("argument", "")).strip()
                        if not command:
                            reply = {"ok": False, "message": "命令不能为空"}
                        else:
                            assert server.stdin is not None
                            server.stdin.write(command + "\n")
                            server.stdin.flush()
                            reply = {"ok": True, "message": f"已发送：{command}"}
                    elif msg.get("command") == "stop":
                        assert server.stdin is not None
                        server.stdin.write("stop\n")
                        server.stdin.flush()
                        stopping = True
                        reply = {"ok": True, "message": "已发送 stop，正在保存世界"}
                    else:
                        reply = {"ok": False, "message": "未知管理命令"}
                    conn.sendall((json.dumps(reply, ensure_ascii=False) + "\n").encode())
                except Exception as exc:
                    try:
                        conn.sendall((json.dumps({"ok": False, "message": str(exc)}, ensure_ascii=False) + "\n").encode())
                    except OSError:
                        pass
            if stopping:
                # Keep the daemon alive while Paper saves; no new commands are necessary.
                server.wait()
                break
        return server.returncode or 0
    except Exception as exc:
        log.write(f"Manager error: {exc!r}\n")
        return 1
    finally:
        listener.close()
        log.close()
        try:
            STATE.unlink()
        except FileNotFoundError:
            pass


def cmd_init(_: argparse.Namespace) -> int:
    save_default_config()
    if (ROOT / "eula.txt").exists():
        print(f"EULA 文件已经存在：{ROOT / 'eula.txt'}")
        return 0
    print("首次运行 Paper 以生成 eula.txt（服务器会自动退出）……")
    result = subprocess.run(java_command(load_config()), cwd=ROOT)
    if (ROOT / "eula.txt").exists():
        print("已生成 eula.txt。请阅读 Mojang EULA 后，将 eula=false 改为 eula=true。")
        return 0
    print("未生成 eula.txt，请查看上方 Paper 输出。", file=sys.stderr)
    return result.returncode or 1


def cmd_start(_: argparse.Namespace) -> int:
    save_default_config()
    try:
        reply = request("status")
        print(f"服务器已经运行（PID {reply['serverPid']}）")
        return 0
    except ConnectionError:
        pass
    if not accepted_eula():
        print("尚未接受 EULA。先运行 .\\mc.ps1 init，然后阅读并编辑 eula.txt。", file=sys.stderr)
        return 2
    RUNTIME.mkdir(exist_ok=True)
    try:
        STATE.unlink()
    except FileNotFoundError:
        pass
    flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
    subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "_daemon"], cwd=ROOT,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     creationflags=flags, close_fds=True)
    deadline = time.time() + 15
    while time.time() < deadline:
        time.sleep(0.25)
        try:
            reply = request("status")
            print(f"服务器已启动（PID {reply['serverPid']}）")
            print(f"日志：{CONSOLE_LOG}")
            return 0
        except ConnectionError:
            continue
    print(f"启动失败或超时，请查看 {CONSOLE_LOG}", file=sys.stderr)
    return 1


def cmd_status(_: argparse.Namespace) -> int:
    try:
        reply = request("status")
        seconds = int(reply["uptime"])
        print(f"运行中 | PID {reply['serverPid']} | {seconds // 3600:02d}:{seconds % 3600 // 60:02d}:{seconds % 60:02d} | {reply['jar']}")
        return 0
    except ConnectionError as exc:
        print(f"已停止（{exc}）")
        return 1


def cmd_send(args: argparse.Namespace) -> int:
    try:
        reply = request("send", " ".join(args.minecraft_command))
        print(reply.get("message", reply))
        return 0 if reply.get("ok") else 1
    except ConnectionError as exc:
        print(exc, file=sys.stderr)
        return 1


def cmd_stop(args: argparse.Namespace) -> int:
    try:
        print(request("stop").get("message"))
    except ConnectionError as exc:
        print(exc, file=sys.stderr)
        return 1
    deadline = time.time() + args.timeout
    while time.time() < deadline:
        time.sleep(0.5)
        if not STATE.exists():
            print("服务器已安全停止。")
            return 0
    print("服务器仍在保存；可稍后运行 status 检查。", file=sys.stderr)
    return 1


def tail_file(follow: bool) -> int:
    if not CONSOLE_LOG.exists():
        print("还没有控制台日志。")
        return 1
    with CONSOLE_LOG.open("r", encoding="utf-8", errors="replace") as stream:
        lines = stream.readlines()
        print("".join(lines[-80:]), end="")
        if not follow:
            return 0
        try:
            while True:
                line = stream.readline()
                if line:
                    print(line, end="")
                else:
                    time.sleep(0.2)
        except KeyboardInterrupt:
            print("\n已退出日志跟随；服务器继续运行。")
    return 0


def cmd_logs(args: argparse.Namespace) -> int:
    return tail_file(args.follow)


def cmd_console(_: argparse.Namespace) -> int:
    try:
        request("status")
    except ConnectionError as exc:
        print(exc, file=sys.stderr)
        return 1
    print("交互控制台：输入 Minecraft 命令；输入 :detach 退出，服务器继续运行。")
    print("提示：另开一个终端运行 .\\mc.ps1 logs -f 可实时查看输出。")
    try:
        while True:
            line = input("> ").strip()
            if line in {":detach", ":quit", ":exit"}:
                return 0
            if line:
                print(request("send", line).get("message"))
    except (KeyboardInterrupt, EOFError):
        print("\n已脱离控制台；服务器继续运行。")
        return 0


def cmd_cleanup(_: argparse.Namespace) -> int:
    try:
        request("status")
        print("服务器仍在运行，拒绝清理。", file=sys.stderr)
        return 1
    except ConnectionError:
        try:
            STATE.unlink()
            print("已清理失效状态。")
        except FileNotFoundError:
            print("没有需要清理的状态。")
        return 0


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="mc.ps1", description="当前目录 Paper 服务端 CLI 管理器")
    sub = p.add_subparsers(dest="action", required=True)
    sub.add_parser("init", help="首次运行并生成 eula.txt").set_defaults(func=cmd_init)
    sub.add_parser("start", help="后台启动服务器").set_defaults(func=cmd_start)
    sub.add_parser("status", help="查看运行状态").set_defaults(func=cmd_status)
    send = sub.add_parser("send", help="向控制台发送 Minecraft 命令")
    send.add_argument("minecraft_command", nargs="+")
    send.set_defaults(func=cmd_send)
    stop = sub.add_parser("stop", help="保存世界并安全停止")
    stop.add_argument("--timeout", type=int, default=60)
    stop.set_defaults(func=cmd_stop)
    logs = sub.add_parser("logs", help="显示最近的控制台日志")
    logs.add_argument("-f", "--follow", action="store_true")
    logs.set_defaults(func=cmd_logs)
    sub.add_parser("console", help="进入命令输入模式").set_defaults(func=cmd_console)
    sub.add_parser("cleanup", help="清理崩溃后残留的状态文件").set_defaults(func=cmd_cleanup)
    sub.add_parser("_daemon", help=argparse.SUPPRESS).set_defaults(func=lambda _: daemon())
    return p


if __name__ == "__main__":
    try:
        args = parser().parse_args()
        raise SystemExit(args.func(args))
    except (FileNotFoundError, json.JSONDecodeError) as exc:
        print(exc, file=sys.stderr)
        raise SystemExit(1)
