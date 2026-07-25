---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-25
applies_to: main@9c46b9f
---

# 运行同伴原型

当前原型用于本地开发和游戏内验证，还不是可供长期游玩的完整 Minecraft Agent。

## 环境要求

- Node.js 22 或更高版本、pnpm 11.3.0
- Python 3.10 或更高版本
- Minecraft Java Edition 1.21.1 服务器
- 支持 Chat Completions 与 `response_format: json_object` 的 OpenAI-compatible 模型接口

离线服使用 `MINEINTENT_MC_AUTH=offline`。正版认证使用 `microsoft`，并通过 `MINEINTENT_MC_PROFILES_FOLDER` 指定认证资料目录。

## 配置

复制配置样例：

```powershell
Copy-Item .env.example .env
```

至少确认以下值：

```dotenv
MINEINTENT_MC_HOST=127.0.0.1
MINEINTENT_MC_PORT=25565
MINEINTENT_MC_USERNAME=MineIntentBot
MINEINTENT_PRIMARY_PLAYER=你的游戏名
MINEINTENT_MODEL_BASE_URL=https://服务商地址/v1
MINEINTENT_MODEL_API_KEY=只放在本地的密钥
MINEINTENT_MODEL=模型名
```

不要提交 `.env`、认证目录、私人聊天、世界存档或运行日志。

## 启动与停止

先启动决策服务：

```powershell
python agent-service/server.py
```

再打开另一个终端启动 MineIntent：

```powershell
pnpm start
```

同伴连接服务器后会进行一次启动决策，之后处理配置的主要玩家发送的聊天。当前原型可以尝试跟随玩家、收集附近木材、等待，以及返回共同活动开始的位置；动作是否成功以游戏状态验证为准。

`Ctrl+C` 会取消模型与游戏动作、释放身体资源、停止聊天调度、断开 Minecraft 并刷新本地事件日志。

## 数据与调试

默认运行数据位于 `.mineintent/`：

- `events.jsonl`：本地运行事件与失败摘要
- `memories.json`：按世界保存的原型记忆

只读调试接口默认监听本机：

```text
GET http://127.0.0.1:3211/health
GET http://127.0.0.1:3211/v1/state
```

接口不提供游戏控制。模型密钥、令牌和完整私人正文不应出现在响应中，但错误日志仍可能包含供应商返回的内容，分享前必须人工检查。

## 当前限制

当前能力只覆盖原型场景，没有完整生存能力树、成熟战斗、建造、跨维度计划或长期自主规划。明确的停止表达会走本地停止路径；低生命值时也有本地危险反射。两者都是当前 `main` 的实现事实，不代表最终交互设计。
