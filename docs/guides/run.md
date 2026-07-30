# 运行当前原型

> 本页只描述当前代码怎样安装、配置和运行，不定义产品。当前实现结构和已知偏差见
> [架构说明](../architecture.md)。

## 环境要求

- Node.js 22 或更高版本，以及 Corepack。
- Python 3.9 或更高版本，用于 Agent Service。
- 可连接的 Minecraft Java Edition 1.21.1 服务器。
- 支持 Chat Completions `tools`、`tool_choice` 和标准 `tool_calls` 的 OpenAI-compatible 模型接口。
- 只有在本机运行 Paper 时才需要 Java 21。
- Windows `mc.ps1` 服务端管理器另需 Python 3.10 或更高版本。

## 安装

```sh
corepack pnpm install --frozen-lockfile
cp .env.example .env
```

Windows PowerShell：

```powershell
corepack pnpm install --frozen-lockfile
Copy-Item .env.example .env
```

生成独立的 Agent Service 令牌并写入 `.env`：

```sh
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

令牌必须是 32–512 个可打印 ASCII 字符，且不得等于或复用模型 API key。

## 配置

`.env.example` 是当前配置入口：

| 分组 | 变量 | 说明 |
|---|---|---|
| Minecraft | `MINEINTENT_WORLD_ID` | 本次连接使用的世界作用域标识 |
| Minecraft | `MINEINTENT_MC_HOST`、`MINEINTENT_MC_PORT` | 服务器地址与端口 |
| Minecraft | `MINEINTENT_MC_USERNAME`、`MINEINTENT_MC_AUTH` | Bot 名称与 `offline`/`microsoft` 登录方式 |
| Minecraft | `MINEINTENT_MC_PROFILES_FOLDER` | 可选的 Microsoft 登录资料目录 |
| 当前兼容配置 | `MINEINTENT_PRIMARY_PLAYER` | 当前实现只用该玩家的公屏消息触发模型 |
| 当前兼容配置 | `MINEINTENT_PROFILE` | 当前实现仍读取的独立档案文件 |
| 本地运行 | `MINEINTENT_DATA_DIR`、`MINEINTENT_DEBUG_PORT` | 数据目录与只读调试端口 |
| 本地运行 | `MINEINTENT_AGENT_SERVICE_URL`、`MINEINTENT_AGENT_SERVICE_TOKEN` | Node 与 Python 服务的 loopback 地址和独立令牌 |
| 模型 | `MINEINTENT_MODEL_BASE_URL`、`MINEINTENT_MODEL_API_KEY`、`MINEINTENT_MODEL` | 模型端点、密钥和模型名 |
| 模型 | `MINEINTENT_MODEL_REASONING_EFFORT` | 可选：`low`、`medium` 或 `high` |
| 模型 | `MINEINTENT_AGENT_SERVICE_PORT` | Python 服务端口，默认 `8765` |

`MINEINTENT_PRIMARY_PLAYER` 和 `MINEINTENT_PROFILE` 是当前代码仍要求的兼容配置，不因此成为产品定义；具体差异见
[架构说明的“已知实现偏差”](../architecture.md#已知实现偏差)。

## 启动

先启动 Python Agent Service：

```sh
python3 agent-service/server.py
```

确认进程已经加载配置并监听：

```sh
curl http://127.0.0.1:8765/healthz
```

`/healthz` 不会探测模型供应商或 Node 工具回调。另开一个终端启动 Node 进程：

```sh
corepack pnpm start
```

Node 启动时不会预检 Agent Service。两个本地服务当前都只接受 loopback 通信。

## 调试与本地数据

只读调试接口：

```text
GET http://127.0.0.1:3211/health
GET http://127.0.0.1:3211/v1/state
```

默认数据目录是 `.mineintent/`：

| 文件 | 当前内容 |
|---|---|
| `events.jsonl` | 事件、工具调用和失败记录，可能包含聊天全文 |
| `memories.json` | 当前结构化记忆实现 |
| `agent-transcripts.jsonl` | 模型逐轮重放记录 |
| `agent-transcripts.jsonl.1` | 上一份轮转记录 |

转录可能包含档案、聊天、记忆、视口、工具 schema 与结果、模型 reasoning 和 closing。`.env`、`.mineintent/`、
认证资料、私人聊天、运行日志和世界存档不得提交；分享调试响应或 artifact 前必须人工脱敏。接口只绑定 loopback
并不表示其内容适合公开。

## 可选：在 Windows 管理本地 Paper

进入 `mcserver/` 后使用 `mc.ps1`：

| 操作 | 命令 |
|---|---|
| 查看完整帮助 | `.\mc.ps1 --help` |
| 首次初始化 | `.\mc.ps1 init` |
| 后台启动 | `.\mc.ps1 start` |
| 查看状态 | `.\mc.ps1 status` |
| 查看或跟踪日志 | `.\mc.ps1 logs` / `.\mc.ps1 logs -f` |
| 发送控制台命令 | `.\mc.ps1 send "命令"` |
| 进入交互控制台 | `.\mc.ps1 console` |
| 安全停止 | `.\mc.ps1 stop` |
| 延长停止期限 | `.\mc.ps1 stop --timeout 120` |
| 清理失效管理状态 | `.\mc.ps1 cleanup` |

首次 `init` 后应自行阅读 [Minecraft EULA](https://aka.ms/MinecraftEULA)，确认接受后再修改 `eula.txt`。使用
`stop` 保存世界，不要从任务管理器强杀 Java。只有确认 Java 服务端已经停止后才使用 `cleanup`；管理器也会拒绝
清理仍在运行的实例。

JVM 与内存设置在 `mcserver/mc-config.json`。不要把最大内存设为机器全部内存；修改 JVM 配置或
`server.properties` 后应安全重启。`.mc-server/` 含本地控制令牌，运行时不要修改或分享。公开服务器前建议保持
`online-mode=true`、启用白名单、备份世界，并避免公开家庭公网 IP。

## 常见排障

### Bot 已上线，但聊天没有响应

1. 检查 `http://127.0.0.1:8765/healthz`。
2. 比较两个进程是否读取了同一份 `.env`；健康响应中的 `startedAt`、`pid` 和 `envSha256` 可帮助识别旧进程。
3. 查看 Agent Service 终端中的模型或配置错误。
4. 确认发言者与当前 `MINEINTENT_PRIMARY_PLAYER` 完全匹配。

### Agent Service 无法启动

- 检查模型端点、API key、模型名和服务令牌是否齐全。
- 检查服务令牌的长度、字符范围以及是否错误复用了模型 key。
- 检查端口是否已被旧进程占用。

### 模型轮次被取消或超时

连接、世界或应用作用域失效会取消当前轮次；整轮期限为 180 秒。取消或超时不是游戏动作成功。

### 下一步

运行自动化或 Paper 检查见[验证指南](./validation.md)。
