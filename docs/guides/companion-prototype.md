# 运行世界参与原型

当前原型用于开发和游戏内验证，尚不是可长期日常运行的完整 Minecraft AI 参与系统。

## 环境

- Node.js 22 或更高版本，以及 Corepack。
- Python 3.9 或更高版本。
- 可连接的 Minecraft Java Edition 1.21.1 服务器。
- 支持 `tools`、`tool_choice` 和标准 `tool_calls` 的 OpenAI-compatible Chat Completions 模型接口。

连接远端服务器不需要在本机安装 Java。只有本机运行 Paper 或执行 Paper 集成测试时才需要 Java 21。

## 安装与配置

```sh
corepack pnpm install --frozen-lockfile
cp .env.example .env
```

编辑 `.env`，至少确认 Minecraft 连接、模型服务和令牌配置。生成独立的 Agent Service 令牌：

```sh
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

不要复用模型 API key。`.env`、`.mineintent/`、认证资料、私人聊天和世界存档不得提交。

当前代码仍强制要求 `MINEINTENT_PRIMARY_PLAYER`。它只是尚未完成的迁移配置：该玩家的每条公屏消息都会
触发模型，其他玩家消息在写入事件日志前就被丢弃。这与产品宪法草案拟议的程序平等和信息边界不一致，
不能据此推导产品规则。

## 启动

先启动 Python Agent Service：

```sh
python3 agent-service/server.py
```

可检查它是否响应：

```sh
curl http://127.0.0.1:8765/healthz
```

再在另一个终端启动 MineIntent：

```sh
corepack pnpm start
```

当前 Node 启动流程不会预先检查 Agent Service；若 Bot 已上线但首次聊天没有响应，先检查 `/healthz`、
两个进程的 `.env` 是否一致，以及 Agent Service 终端中的错误。

## 当前工具与限制

模型当前可以调用：

- `look_relative`：相对转头。
- `move_input`：短时按住一组移动键。
- `view`：不移动身体，重新读取当前视野。
- `say`：向游戏聊天发送模型措辞。
- `remember`：写入当前结构化记忆原型。

当前没有启动决策、主动行为、寻路、跟随、采木、跳跃、挖掘、战斗或 GUI 操作。游戏内“停下”只是普通文本，
不会走本地特权控制路径；连接、世界或应用作用域失效以及 180 秒模型轮次超时等客观条件会硬取消轮次并
释放身体输入。

## 数据与调试

默认数据目录为 `.mineintent/`：

- `events.jsonl`：事件、调用和失败记录；包含收到的主要玩家聊天全文。
- `memories.json`：当前结构化记忆原型。
- `agent-transcripts.jsonl` 及轮转文件 `.jsonl.1`：完整模型重放记录，可能含档案、聊天、记忆、视野、
  工具 schema/结果、reasoning 和 closing。

这些文件都可能含私人全文。不要提交；分享日志、artifact 或调试响应前必须脱敏。

本地只读接口：

```text
GET http://127.0.0.1:3211/health
GET http://127.0.0.1:3211/v1/state
```

接口仅绑定 `127.0.0.1`，但分享响应或日志前仍需人工检查敏感内容。

## 验证

```sh
corepack pnpm check
corepack pnpm check:docs
corepack pnpm test
python3 -m unittest discover -s agent-service -p 'test_*.py'
```

上述检查不启动 Minecraft，也不证明真实模型体验。真实 Paper 验证见 [Paper 集成测试](./paper-integration.md)。
