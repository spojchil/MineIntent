# agent-service

**这一侧是 agent 本体。**提示词构造、模型调用与结果校验都在这里；`src/` 下的 TypeScript 实现
agent 调用的工具——Mineflayer 协议驱动、感知投影、身体输入。拆分的理由就是语言选择：写 agent
用 Python 方便，而工具要贴着 Mineflayer，所以留在 TypeScript。

因此这是一个普通 agent 加一个工具后端。与其他 agent 的差别主要在工具类型和提示词，而不在架构。

当前 `main` 上的形态：把 `CompanionRuntime` 传来的 `DecisionContext` 转成一次
OpenAI-compatible Chat Completions 调用，验证并返回 `CompanionDecision`。与 `src/minecraft/`
彻底分离，只通过 `src/models/agent-service-client.ts` 这一层 HTTP JSON 接口交互，因此可以独立于
Node 进程运行、审查和测试。

只依赖 Python 标准库，无需安装任何包。

## 运行

与 Node 进程一起运行时是两个独立进程：

```powershell
python agent-service/server.py
```

另开一个终端：

```powershell
pnpm start
```

配置从仓库根目录的 `.env` 读取（`MINEINTENT_MODEL_BASE_URL`、`MINEINTENT_MODEL_API_KEY`、
`MINEINTENT_MODEL`、`MINEINTENT_AGENT_SERVICE_PORT`，默认端口 8765）。Node 侧通过
`MINEINTENT_AGENT_SERVICE_URL` 找到这个服务，默认 `http://127.0.0.1:8765`。

## 接口

- `POST /v1/decide`：请求体是完整的 `DecisionContext`（见 `src/models/contracts.ts`），
  返回 `{decision, model, usage}`。`decision` 保证通过 `schema.py` 的
  `mineintent.companion-decision.v1` 校验，字段约束与 Node 侧 zod schema 逐项对应。
- `GET /healthz`：存活探测，返回 `{"status": "ok"}`。

## 测试

```powershell
python -m unittest agent-service/test_server.py -v
```
