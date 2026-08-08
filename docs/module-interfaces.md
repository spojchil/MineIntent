# 模块接口清单：被消费的表面

> 无产品权威。绑定分支 `experiment/no-panic-live`（提取自 `ebd463c` 后的工作树，
> 2026-08-08）。结构变化时按 `AGENTS.md` 约定更新或标为历史。
>
> **方法**：逐条抓取 crate 边界上的 `use mineintent_*` / `use toolloop` 与限定路径，
> 再回到定义处核对签名。列出的是**被消费的表面**；`pub` 声明在本仓库不可信
> （5110 行 `pub` 死代码的教训见[代码梳理](./rust-code-audit.md) §4、§9），
> 名义与实际的差距单独列在 §6。

## 0. 真实依赖图（与直觉不同的两处）

```
           ┌──────────── app（组合根，唯一知道所有人的地方）
           │      │  │
           ▼      ▼  ▼
        backend  middle ──→ toolloop
           │      │            │
           └──────┴────────────┴──→ contracts
```

- **`middle` 不依赖 `backend`。** 两者互不认识；middle 只持有 contracts 里的
  trait 对象（`Arc<dyn MinecraftBackendApi>` 等），实现体由 app 在装配时注入。
  `docs/architecture.md` 的堆叠图容易读成 middle→backend 的编译依赖，实际没有。
- **`toolloop` 的包名就是 `toolloop`**，不带 `mineintent-` 前缀
  （`crates/middle/Cargo.toml` 里是 `toolloop.workspace = true`）。
  它只依赖 contracts，且 app 不直接依赖它——app 拿到的循环类型
  都是经 `middle::agent` 转出的。

## 1. contracts：系统的行为契约

数据类型几百个不逐列；**行为接口只有一处**：`minecraft/api.rs` 的五个 trait。
这就是 backend 与 middle 之间的全部动词。

### `MinecraftBackendApi`（`api.rs:245`，后端主门面）

| 方法 | 语义 |
|---|---|
| `start(control) -> BackendReady` | 只在就绪/取消/超时/终失败时完成 |
| `stop(reason, control)` | 资源释放且终态可见后才完成 |
| `state() -> BackendState` | |
| `snapshot() -> MinecraftSnapshotV1` | |
| `capture_frame_facts() -> MinecraftFrameFacts` | 默认实现 = snapshot + None |
| `subscribe(listener) -> Subscription` | 事件推送入口 |
| `observation_source() -> Arc<dyn ProtocolObservationSource>` | |
| `motor() -> Arc<dyn MinecraftMotorDriverApi>` | |
| `send_chat(message)` | |

### `ProtocolObservationSource`（`api.rs:131`，观察，epoch 绑定）

`epoch()` / `self_pose()` / `list_tracked_entities()` / `read_block(position)` /
`subscribe(listener)` / `read_viewport(control)` / `read_directed_viewport(positions, control)`

### `MinecraftMotorDriverApi`（`api.rs:227`，动作）

`look_relative(request, control)` / `move_input(request, control)` /
`release_all()`（同步、幂等） / `respawn(control)`

参数类型 `LookRelativeRequest`、`MoveInputRequest` 连同校验（±90°、50..=1500ms、
方向 1..=4 不重复）也在本文件，即**动作的合法域定义在契约层，不在实现层**。

### 订阅与监听（`api.rs:104-128`）

`Subscription::{unsubscribe, is_closed}`；`BackendEventListener::on_event(BackendEventEnvelope)`；
`ObservationEventListener::on_event(ObservationEvent)`（Entity/Block/Sound 三类）。

### 各 crate 消费 contracts 的分布（`use` 根路径计数）

| | `minecraft` | `agent` | `capability` | `information` |
|---|---|---|---|---|
| backend | 7 | — | 2 | — |
| middle | 12 | 6 | — | 4 |
| app | 2 | 4 | 1 | — |
| toolloop | — | 3 | — | — |

toolloop 只碰 `agent`（工具调用的 wire 形状），碰不到 minecraft——通用性是这么守住的。

## 2. backend：对外一个类型

**app 从 backend 引入的条目全表**（含限定路径）：

```rust
use mineintent_backend::facade::MinecraftBackendFacade;   // 唯一一条
```

`MinecraftBackendFacade` 自身的固有方法只有 `new(config)`（`facade.rs:327`）；
其余能力全部经 §1 的 trait 提供（`facade.rs:343` 起的四个 impl 块）。

也就是说：**backend 的模块边界 = `new` + 五个 contracts trait**。
`viewport`、`snapshot`、`runtime`、`protocol` 这些 `pub mod`
没有任何 crate 外的生产消费者（消费者是 crate 内、tests 与 `examples/viewport_cost.rs`）。

## 3. middle：出口十个模块路径，入口三个注入点

### 3.1 app 消费的出口（全表）

| 路径 | 条目 |
|---|---|
| `agent` | `AgentModelRequest`, `ModelCompletion`†, `BackendRoundViewportSampler`, `ConcreteAgentRunner` |
| `capability` | `build_production_capability_registry`, `ProductionCapabilityServices`, `ViewportReader`, `CapabilityActionIdSource`, `CapabilityScopeAssembly`, `CapabilityUtcTimestampSource`, `ExplicitCapabilityInvocationAssembler`, `RegistryToolDispatcher` |
| `participant` | `ParticipantRuntime`, `ParticipantRuntimeConfig`, `ParticipantAgentAssembly`, `ParticipantAgentFactory`, `ParticipantFrameSource`, `ParticipantObservationAfterSource`, `ParticipantScope`, `ParticipantScopedAgentRunner`, `ProductionParticipantFrameSource`, `ParticipantClock`, `SystemUtcClock`, `WakeRegistry` |
| `events` | `JsonlEventJournal` |
| `memory` | `MemoryStore` |
| `speech` | `SpeechScheduler`, `SpeechSchedulerOptions`, `SpeechTransport` |
| `telemetry` | `DebugStateStore` |

† `ModelCompletion` 实为 toolloop 类型，经 `middle::agent` 转出。

`ParticipantRuntime` 的生产方法面：`new/try_new`、`start_worker`、
`ingest_backend_event`、`ingest_event`、`emit_internal`、`lifecycle`、
`current_scope`、`current_generation`、`fact_owner`、`wake_registry`、
`tool_definitions`、`ingest_counters`、`worker_gate`、`subscribe_failures`、
`debug_snapshot`（`participant/runtime/mod.rs:132-627`）。
另有 5 个 `*_for_test` 方法混在同一个 pub 面上，见 §6。

### 3.2 middle 等 app 来实现/注入的入口（依赖倒置点）

| 端口 | 签名 | app 侧的实现/来源 |
|---|---|---|
| `ParticipantAgentFactory`（`ports.rs:189`） | `registry()`, `build(scope, generation, trigger_event_id) -> ParticipantScopedAgentRunner` | `AppAgentFactory`（选模型 provider） |
| `SpeechTransport`（`speech/contracts.rs:112`） | `send(&str)`，关联 `Error` 类型 | 后端聊天通道适配 |
| `ParticipantClock`（`ports.rs:139`） | `now() -> String`，仅诊断用 | `SystemUtcClock`（middle 自带） |
| backend 五 trait 的实现体 | §1 | `MinecraftBackendFacade`，装配时以 `Arc<dyn …>` 交给 `ViewportReader::new` / `ProductionCapabilityServices` 等 |

`ParticipantFrameSource`（`ports.rs:60`：`chat_context` / `capture` / `retain_trigger`）
是端口但生产实现在 middle 自己（`ProductionParticipantFrameSource`），app 只装配。

## 4. toolloop：三个模块的转出面

`lib.rs` 全部出口（middle 是唯一消费者）：

| 来源模块 | 条目 |
|---|---|
| `control` | `await_with_control`, `is_run_control_error` |
| `run` | `AgentRun`, `AgentRunStep`, `AgentToolResult`, `ModelCompletion`, `PlannedToolCall`, `summarize_error`, 预算常量 `MAX_MODEL_REQUESTS_PER_RUN`(16) / `MAX_TOOL_CALLS_PER_RESPONSE`(8) / `MAX_TOOL_CALLS_PER_RUN`(32) |
| `transcript` | `AgentTranscriptRecord`, `FileTranscriptStore`, `TranscriptSink`, `TranscriptUsage`, `transcript_path*`, `MAX_TRANSCRIPT_*`, `TRANSCRIPT_*` |

另有 `utc_timestamp_now`。`middle/agent/mod.rs:13-19` 原样转出，注释明言迁移意图：
「待 driver/runner 也脱离领域之后，本模块只剩领域侧的 context/prompt/viewport」。

## 5. app：只出不进

app 无库出口（`main.rs` + `bin/fake_player.rs` 两个可执行）。它的全部职责：
读配置与密钥路径 → `MinecraftBackendFacade::new` → 造 middle 的三个注入点
→ `ParticipantRuntime::new` → `start_worker`。模型 provider
（`ResponsesModelProvider` / `ScriptedModelProvider`）也在 app，经
`ParticipantAgentFactory` 进入系统。

## 6. 名义 pub 与实际消费的差距（重梳时的处置清单）

| 位置 | 名义 | 实际 | 处置建议 |
|---|---|---|---|
| `middle::information` 控制面 | 5110 行 pub | 生产零消费（摘 `mod` 编译实验证实） | 重梳时先裁决去留，别照着它画图 |
| `contracts` V3/V4 | 两代协议类型 | 活的只有 V2 | 同上 |
| `backend::{viewport,snapshot,runtime,protocol}` 的 pub | crate 外可见 | 仅 crate 内 + tests + example 消费 | 若维持 facade 是唯一门，可收窄为 `pub(crate)`，让编译器守住 §2 的结论 |
| `ParticipantRuntime::*_for_test` ×5 | 与生产方法同一 pub 面 | 仅测试 | `#[cfg(test)]` 或 feature 门 |
| 四层事件队列 | 每层各有 pub 面 | 一条事件穿四层（[代码梳理](./rust-code-audit.md) §3） | job 模型改造顺手拆环（[job 讨论](./background-job-design-notes.md) §4.3） |

## 7. 一句话读法

这套系统的真实接口窄得出乎意料：**backend 一个类型五个 trait；middle
十个模块路径出、三个注入点进；toolloop 三个模块转出；contracts 是唯一的公共语言；
app 认识所有人而没人认识 app。** 重梳架构时，动词都在 `contracts/minecraft/api.rs`
一个文件里——那里是杠杆最大的地方。
