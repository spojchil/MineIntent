# 当前实现结构

> 实现快照：本分支相对 `main@e2d1f89` 的当前实现（TypeScript 栈）；
> Rust 移植的实现快照见下方第 9 节，绑定 `crates/` 当前内容
>
> 本页只描述当前代码事实，没有产品权威。产品判断与候选架构见[《产品》](./产品.md)；两者不一致时，
> 应把这里记录为实现偏差，不能用既成实现反向解释产品。

## 1. 运行拓扑

```text
Minecraft Java 1.21.1
        ↕ Mineflayer
Node / TypeScript
世界连接、感知、运行作用域、工具执行、持久化
        ↕ 带独立令牌的 loopback HTTP
Python Agent Service
系统提示词、模型消息、标准 tool-call 循环
        ↕ OpenAI-compatible /chat/completions
模型供应商
```

Node 入口是 [`src/main.ts`](../src/main.ts)，对象装配位于
[`MineIntentApp.start()`](../src/app/mineintent-app.ts)。它创建 Minecraft Backend、事件日志、记忆、
Agent Service 客户端、工具回调和只读调试服务。

Python 入口是 [`agent-service/server.py:main()`](../agent-service/server.py)。它只监听 `127.0.0.1`；Node 不负责
启动 Python，Python 也不直接连接 Minecraft。`mcserver/` 管理器不是应用进程的一部分，只用于本地 Paper 服务端。

## 2. Node 与 Python 的边界

Node 通过 `POST /v1/decide` 把 `runId`、模型上下文和当前工具定义发送给 Python；取消或超时后尽力调用
`POST /v1/cancel`。Agent Service 地址必须是无凭据的 loopback HTTP 地址。产生方是
[`AgentServiceModelProvider`](../src/models/agent-service-client.ts)。

Python 收到模型工具调用后，通过 Node 临时创建的 `POST /v1/tool` 回调执行。回调只绑定 loopback，使用独立随机令牌，
并验证 run、tool call、工具名和参数。产生方是 [`ToolBridgeServer`](../src/models/tool-bridge.ts)及
[模型契约](../src/models/contracts.ts)。

Python 拥有模型消息序列、系统提示词和 tool-call 循环；Node 拥有世界状态、运行作用域和工具的实际效果。

## 3. 一次决策的生命周期

当前决策入口是任何玩家的被寻址公屏聊天；当前运行时按点名或单独在场判断寻址，不依据玩家身份。聊天契约保留无特权的
进行中对话机制，但本次运行时装配不传该值。产生方是 [`ParticipantRuntime`](../src/participant/runtime.ts)和
[`interpretPlayerChat()`](../src/speech/chat-input.ts)。

聊天与模型决策分别通过 Promise 队列保持顺序。新聊天等待已有决策完成，不主动抢占旧决策；连接、死亡、重生、维度
切换、重连、应用停止等作用域变化会取消当前 run 并释放身体输入。

每条触发消息创建一个新 run，并绑定 Node 会话、Minecraft 连接 epoch、worldId、dimension、触发事件和取消信号。
运行时按聊天文本检索同一世界最多五条结构化记忆，组成 opening frame 后调用模型。

不同 run 之间不重放聊天历史。跨 run 重新进入模型的长期状态只有本次检索出的记忆；诊断 transcript
不会被读回上下文。

## 4. 模型上下文与工具循环

上下文协议 `mineintent.agent-context.v3` 分为：

- `stable`：检索出的记忆；
- `frame`：本次玩家消息、世界、自身姿态、状态、背包、近期声音、待报告事件和遗漏说明。

协议类型见 [`src/models/contracts.ts`](../src/models/contracts.ts)，frame 组合见
[`composeAgentContext()`](../src/information/context-composer.ts)。完整视野不在 opening frame 中；opening 只含姿态和
坐标图例。

Python 将通用系统提示词和记忆组成 system message，再追加 opening frame。之后只追加 assistant tool calls 和
对应的 tool results，不重写旧消息。实现见 [`prompt.py`](../agent-service/prompt.py)与
[`run_tool_loop()`](../agent-service/server.py)。

每个工具回调只携带当前 `runId`、模型提供的 `toolCallId`、工具名和参数。`runId` 绑定当前决策及世界作用域，
`toolCallId` 把一个模型调用与它的结果及日志关联，并且在同一 run 内只能使用一次；Node 在执行前认领它，因而重放不会
再次产生世界副作用。协议不再为一条模型响应另外创建批次身份。

Node 在每个工具处理完成后采集一次世界观察，通过 `mineintent.tool-response.v2` 返回。Python 放入对应 tool message 的
模型可见内容固定为 `{result, observationAfter}`：`result` 是该调用的执行结果；`observationAfter` 只表示在调用处理后
采集到的世界状态和累计事件，不表示其中变化由该工具造成。采样失败或调用在 Python 本地即被拒绝时为 `null`。
观察不再作为独立 user message 追加，因此同一 assistant 响应中的每个调用都与自己的结果和后续观察按 `toolCallId`
一一配对。

同一模型响应中的全部合法调用仍按出现顺序执行。pending event 在采样时排空；当前 bridge 没有 observation ACK 或重投，
因此事件交付是 at-most-once，回调响应在送达前丢失时不会自动恢复。

当前上限为：每个 run 最多 16 次模型请求、每条模型响应最多 8 个工具调用、每个 run 最多 32 个工具调用，
整个 run 共用 180 秒期限。

当前没有上下文压缩、摘要或 checkpoint 恢复。上下文只在单个 run 内追加，直到正常结束、失败或达到限制；供应商返回的
缓存 token 计数仅用于记录。模型最终的普通文本只进入 transcript，不会发到游戏；游戏内说话必须调用 `say`。

## 5. 感知

[`MinecraftBackend`](../src/minecraft/minecraft-backend.ts)使用 Mineflayer 连接固定版本 `1.21.1`，维护连接状态、快照、
自动重连、实体/方块/声音/聊天事件和可取消身体输入。

Node 内部 Information Runtime 当前注册四类 provider：

- 当前生命、饥饿、氧气、经验和状态效果；
- 背包与快捷栏；
- 最近声音；
- 第一人称视野投影。

装配发生在 [`buildInformationRuntime()`](../src/participant/runtime.ts)。Information Runtime 还实现 catalog、help、selector、
cursor 和权限机制，但这些通用接口目前没有作为模型工具暴露。

模型通过专用 `view` 工具读取完整视野。当前投影包含姿态与世界坐标图例、脚下方块、准星方块、最多 8 个可见实体，
以及最多 256 个、32 格范围内的可见方块。投影每次完整重读，不是 delta；产生方是
[`ViewportInformationProvider`](../src/information/providers/viewport-provider.ts)。

声音缓存最多 20 条，并按连接和世界作用域过滤。当前主动加入 pending observation 的世界变化主要是参与者启动和自身生命下降；
实体移动、方块变化和其他复杂行为不会自行触发模型决策。

## 6. 模型可见工具

| 工具 | 当前实现 |
|---|---|
| `look_relative` | 水平和垂直各相对转动最多 ±90°，返回实测转动和新视野 |
| `move_input` | 按住前、后、左、右键组合 50–1500 ms，可疾跑，返回实测位移和新视野 |
| `view` | 不移动身体，完整重读当前视野 |
| `say` | 把最多 500 字符的文本加入异步聊天队列 |
| `remember` | 写入一条结构化 `episode` 记忆 |

能力在 [`src/participant/capabilities/`](../src/participant/capabilities/)注册；同一 registry 同时产生模型 schema 和执行分派。
身体、视野、聊天和记忆使用独立资源租约，资源冲突作为失败结果返回模型。

`move_input` 没有寻路、跳跃或自动避障。当前也没有挖掘、放置、交互、战斗、GUI、制作或装备能力。系统提示词建议
“每次模型响应最多一个动作工具”，但执行层不强制；同一模型响应中的合法调用会依次执行。

## 7. 记忆、日志与调试

默认 `.mineintent/` 数据：

| 文件 | 当前用途 |
|---|---|
| `memories.json` | 带 kind、summary、keywords、evidence、worldId 和时间的结构化记忆 |
| `events.jsonl` | 调用方显式追加的应用事件，不是完整世界事件存档 |
| `agent-transcripts.jsonl` | 每个模型 run 的完整诊断重放记录 |
| `agent-transcripts.jsonl.1` | 上一份轮转记录 |

`remember` 固定写入 `episode`，证据绑定触发本次 run 的聊天事件。记忆文件只在首次加载时读取，之后以内存记录为准；
运行期间直接修改文件不会自动重载，后续写入还可能覆盖外部修改。实现见
[`FileMemoryStore`](../src/memory/memory-store.ts)和 [`remember`](../src/participant/capabilities/remember.ts)。

transcript 可能包含提示词、聊天、记忆、视口、工具 schema、结果、reasoning 和 closing。单条过大时会省略消息/schema，
总文件达到约 32 MiB 时轮转；它不参与后续决策。

只读调试状态位于 loopback `/v1/state`，保存在内存中并按字段脱敏，不是持久数据。实现见
[`src/telemetry/`](../src/telemetry/)。

## 8. 验证边界

普通检查包括 TypeScript 类型检查、Markdown 本地链接检查、Node 测试和 Python Agent Service 测试。大部分测试使用
fake backend、fake model/provider 和临时文件，不启动 Minecraft 或真实模型。

手动 Paper workflow 使用隔离世界副本，验证 Backend 的连接、死亡、重生和重连，以及独立测试 Bot 的移动、取消、挖掘
和背包行为。挖掘场景不表示当前 Agent 已拥有挖掘工具。具体命令和破坏性边界见[验证指南](./guides/validation.md)。

当前没有可重复的“Paper + MineIntent Runtime + Python Agent Service + 真实模型”端到端验收。

## 9. Rust 移植（进行中，尚未接管运行）

仓库同时存在两套实现。**当前可运行、被验证指南覆盖的仍是上面的 TypeScript 栈**；
`crates/` 是全 Rust 单进程移植，已完成主体但未完成验收，因此不是"当前实现"，
也还没有替代任何一层。

```text
Minecraft Java 26.1.2（Paper，协议 775）
        ↕ Azalea（自有 fork，见 crates/backend/README.md）
Rust 单进程
  crates/backend   连接生命周期、命令、观察、视口投影
  crates/middle    Agent 循环、capability 派发、Information、记忆、语音、Participant runtime
  crates/contracts 三层之间的严格进程内契约
  crates/app       组合根与模型 provider
        ↕ OpenAI-compatible /chat/completions
模型供应商
```

与 TS 栈的**结构性差异**（均已有裁定，不是实现偏差）：

- Python agent-service 与 loopback HTTP 工具桥不再存在，Agent 循环内联为进程内 `AgentRunner`；
- 目标服务端版本从 1.21.1 升到 26.1.2，协议执行层从 Mineflayer 换成自有 fork 的 Azalea；
- 模型可见工具从五个增为六个（新增 `respawn`）；
- 观察面：`view` 工具支持 `full` / `directed` 两种模式，轮末追加一帧视口。

TS 栈在移植期的地位是**行为 oracle**：137 个行为测试是可执行的行为规范，
逐条对应关系见[历史索引](./history/index.md)中的迁移证据。

运行方式与开发者模式见[Rust workspace 指南](./guides/rust-workspace.md)。

## 已知实现偏差

相对于当前产品讨论，至少存在以下差异；这里仅记录，不替产品作决定：

1. 当前长期记忆仍是结构化多记录，不是单一、由 AI 直接编辑的文本记忆。
2. 当前所有模型共用同一份系统提示词，没有供应商、模型或版本专用提示词。
3. 决策只由被寻址的玩家聊天触发；世界事件不会产生后台主动 run。
4. 当前感知不能完整理解告示牌文字、玩家拿取物品等复杂行为，也不会把所有可获得的世界信息自动送入模型。
5. 当前动作能力远低于正常玩家的能力范围。
6. 运行期间没有上下文压缩，也没有跨 run 对话连续性。
7. 没有覆盖真实模型体验的可重复端到端验证。
