# agent-service

`agent-service` 是 MineIntent 当前原型的本地模型进程。它调用支持 `tools`、`tool_choice` 和标准
`tool_calls` 的 OpenAI-compatible Chat Completions 接口，并根据 Node runtime 提供的工具定义选择
回复、观察、行动或记忆。
当前工具集为：

- `look_relative`：相对转头；
- `move_input`：短时同时按住一组移动键；
- `view`：获取当前有界的第一人称视野投影；
- `say`：在 Minecraft 聊天中发言；
- `remember`：写入持久化的结构化 episode 记忆。

Minecraft 工具由 Node 进程中只绑定 loopback 的回调服务执行。Python 不直接连接
Minecraft，也不执行模型生成的任意代码。该目录只依赖 Python 标准库。

## 运行

从仓库根目录的 `.env.example` 创建 `.env`，至少配置：

```dotenv
MINEINTENT_MODEL_BASE_URL=https://服务商地址/v1
MINEINTENT_MODEL_API_KEY=只放在本地的模型密钥
MINEINTENT_MODEL=模型名
MINEINTENT_AGENT_SERVICE_TOKEN=独立生成的本地令牌，至少32字符
```

Agent Service 令牌不得与模型 API key 复用。启动两个进程：

```shell
python3 agent-service/server.py
```

```shell
corepack pnpm start
```

## 本地接口

接口只监听 `127.0.0.1`。除健康检查外，均要求独立 bearer token：

- `GET /healthz`：确认进程已加载配置并正在监听；它不探测模型服务或 Node 回调；
- `POST /v1/decide`：运行一个模型/工具轮次；
- `POST /v1/cancel`：按 `runId` 取消因连接/世界失效、应用停止或请求超时而不能继续的轮次。

服务只保留一个权威轮次，作为并发请求的最后一道隔离；当前 Node runtime 会把配置的主要玩家的每条
公屏消息按顺序送入模型，其他玩家消息在写入日志前丢弃，新聊天不会抢占旧轮次。若仍收到新的 `runId`，服务会使旧轮次失效并中断本地与
上游模型的 socket；迟到的旧取消请求不会伤及新轮次。远端模型供应商是否在
TCP 断开后立即停止内部推理，不由本项目保证。整轮 deadline 为 180 秒。

## 数据与隐私

服务把逐轮重放记录写入 `MINEINTENT_DATA_DIR/agent-transcripts.jsonl`，轮转文件为
`agent-transcripts.jsonl.1`。它可能包含完整档案、玩家消息、记忆、视野、工具 schema 与结果、模型 reasoning
及 closing。文件以仅当前用户可读写模式创建并限制大小，但仍属于敏感数据；不要提交，分享前必须脱敏。

## 测试

```shell
python3 -m unittest discover -s agent-service -p "test_*.py"
```
