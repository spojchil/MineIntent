# agent-service

`agent-service` 是 D40 原型的本地模型进程。它调用 OpenAI-compatible Chat
Completions，让模型在聊天回复与两个有界身体工具之间选择：

- `look_relative`：相对转头；
- `move_input`：短时按住前、后、左、右之一。

工具由 Node 进程中只绑定 loopback 的回调服务执行。每次动作后都返回新视野；
Python 不直接连接 Minecraft，也不执行模型生成的任意代码。该目录只依赖
Python 标准库。

## 运行

从仓库根目录的 `.env.example` 创建 `.env`，至少配置：

```dotenv
MINEINTENT_MODEL_BASE_URL=https://服务商地址/v1
MINEINTENT_MODEL_API_KEY=只放在本地的模型密钥
MINEINTENT_MODEL=模型名
MINEINTENT_AGENT_SERVICE_TOKEN=独立生成的本地令牌，至少32字符
```

Agent Service 令牌不得与模型 API key 复用。启动两个进程：

```powershell
python agent-service/server.py
```

```powershell
pnpm start
```

## 本地接口

两个接口都只监听 `127.0.0.1`，并要求独立 bearer token：

- `POST /v1/decide`：运行一个 D40 模型/工具轮次；
- `POST /v1/cancel`：按 `runId` 取消因连接/世界失效、应用停止或请求超时而不能继续的轮次。

服务只保留一个权威轮次，作为并发请求的最后一道隔离；Node runtime 会把普通玩家聊天按顺序送入模型，而不是用新聊天抢占旧轮次。若仍收到新的 `runId`，服务会使旧轮次失效并中断本地与
上游模型的 socket；迟到的旧取消请求不会伤及新轮次。远端模型供应商是否在
TCP 断开后立即停止内部推理，不由本项目保证。整轮 deadline 为 180 秒。

## 测试

```powershell
python -m unittest discover -s agent-service -p "test_*.py"
```
