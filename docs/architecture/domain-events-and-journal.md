---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# 领域事件与事件日志协议

> 版本 1，2026-07-12；关联 Issue [#12](https://github.com/spojchil/maineintent/issues/12)，依据 [ADR 0002](../decisions/0002-event-driven-companion-runtime.md)。当前有 JSONL journal 和运行时事件记录，但本文的完整版本化事件族、投影、检查点与重放恢复尚未全部实现。

## 1. 目标

本设计定义 MineIntent 第一版领域事件层的稳定契约，包括：

- 领域事件与原始 Minecraft 状态的边界。
- 事件信封、命名、版本和来源语义。
- 单进程内顺序、因果关系和重复处理。
- 同步内存分发与异步 SQLite 日志。
- Reflex 关键路径的非阻塞记账。
- 当前状态投影、检查点、重放和恢复协调。
- schema 演进、隐私、背压和测试边界。

该协议是 Minecraft、Companion Runtime、Action Runtime、模型运行、语言和记忆之间的共同时间线。

## 2. 非目标

本设计不负责：

- 保存每个协议包、每 tick 位置或完整区块数据。
- 定义所有模块内部的临时状态。
- 实现跨机器的分布式事件系统。
- 使用 Kafka、Redis 或外部消息代理。
- 让事件日志取代 Minecraft 服务器作为当前世界事实来源。
- 记录模型私有推理过程。
- 通过重放再次执行聊天发送、游戏动作或模型调用。

## 3. 术语

### 3.1 Raw Signal

Mineflayer 回调、协议状态变化、计时器和内部控制器产生的原始信号。它们可能高频、重复、不稳定，也可能只对局部算法有意义。

示例：每 tick 位置、实体移动包、寻路器节点更新。

### 3.2 Domain Event

已经发生、对同伴连续性或系统恢复有意义的不可变事实。

示例：玩家发送聊天、动作被接受、威胁监督器接管身体、AI 完成采集。

事件使用过去式语义。命令和请求不能伪装成已经发生的结果。

### 3.3 Command

模块希望另一个模块执行某件事的请求，例如 `SubmitAction`、`CancelAction`。命令可能被接受或拒绝，本身不是事件。

命令处理结果必须产生事件，例如 `action.accepted` 或 `action.rejected`。

### 3.4 Projection

通过按顺序归约事件得到的当前领域状态，例如共同活动、当前动作和最近聊天。投影是可重建缓存，不是事件历史。

### 3.5 Effector

产生外部副作用的事件消费者，例如发送聊天、调用模型或执行 Mineflayer 动作。重放历史时禁止运行 Effector。

### 3.6 Journal

SQLite 中按进程顺序持久化的领域事件日志，用于审计、调试、投影重放和恢复。

## 4. 事件边界

### 4.1 应成为领域事件

满足任一条件时通常应产生领域事件：

- 改变 Companion Runtime 的当前状态。
- 开始、终止或显著改变一个动作。
- 影响玩家与同伴的语言、共同活动或关系连续性。
- 需要在进程重启后解释或恢复。
- 是验证成功、失败、取消或副作用的证据。
- 需要进入记忆候选或调试因果链。

### 4.2 不应成为领域事件

- 每 tick 或每包的普通位置更新。
- 寻路算法内部节点和碰撞迭代。
- 没有跨模块意义的缓存命中。
- 尚未达到稳定边界的临时数值。
- 可以从同一时刻权威快照直接恢复的高频状态。

### 4.3 聚合后成为事件

高频信号先由所属模块聚合：

```text
连续玩家位置更新
→ 玩家持续远离活动区域
→ companion.activity.alignment_became_uncertain

连续敌对实体移动
→ 预计伤害时间进入防护窗口
→ safety.protective_action_started

多个寻路内部更新
→ 导航连续一段时间没有进展
→ action.progress_stalled
```

## 5. 事件信封

### 5.1 TypeScript 契约

```ts
type EventAwareness =
  | 'runtime_only'
  | 'companion_observed'
  | 'socially_shared'

type EventPriority = 'critical' | 'high' | 'normal' | 'low'

interface EventOrigin {
  kind:
    | 'minecraft'
    | 'player'
    | 'companion'
    | 'action_runtime'
    | 'model_runtime'
    | 'memory'
    | 'system'
  component: string
  actorId?: string
}

interface DomainEvent<TType extends string, TPayload> {
  id: string
  sequence: number
  type: TType
  version: number

  occurredAt: string
  recordedAt: string

  sessionId: string
  worldId: string

  correlationId: string
  causationId?: string

  origin: EventOrigin
  awareness: EventAwareness
  priority: EventPriority

  payload: TPayload
}
```

### 5.2 字段语义

#### `id`

使用 `crypto.randomUUID()` 生成的全局唯一 UUID。消费者以 `id` 实现幂等处理。

事件排序不依赖 UUID。

#### `sequence`

Event Hub 接受事件时同步分配的进程级严格递增整数：

- 单进程内唯一且形成总顺序。
- Event Hub 启动时从 SQLite `MAX(sequence)` 恢复下一值。
- 分配只需要内存自增，不等待数据库写入。
- 第一版达到 JavaScript 安全整数上限不是现实风险。

#### `type`

使用点分命名：

```text
<namespace>.<subject>.<past-tense-event>
```

示例：

```text
minecraft.connection.established
player.chat.received
action.execution.completed
companion.activity.updated
```

版本不写入 `type`，由 `version` 独立表示。

#### `occurredAt`

事件对应事实在来源模块中发生的 UTC ISO 8601 时间。

#### `recordedAt`

Event Hub 接受并分配顺序号的 UTC ISO 8601 时间。延迟分析使用 `recordedAt - occurredAt`。

排序使用 `sequence`，不能依赖系统时钟。

#### `sessionId`

一次 MineIntent 进程运行的 ID。进程重启后改变。

#### `worldId`

稳定标识一个 Minecraft 世界。事件不能在没有 `worldId` 的情况下进入长期日志。连接尚未识别世界时使用明确的 pending world ID，识别后产生世界绑定事件，不能悄悄替换旧事件。

#### `correlationId`

连接一次用户交互或主动机会引发的完整工作链：

```text
聊天
→ 模型决策
→ 语言与动作
→ 动作结果
→ 记忆候选
```

若新事件没有继承既有工作链，Event Hub 为它创建新的 correlation ID。

#### `causationId`

直接导致本事件的上一个领域事件 ID。例如 `action.execution.started` 的 causation 是 `action.request.accepted`。

一个事件只有一个直接原因；多个相关证据放在 payload 的 `evidenceEventIds`，不能滥用 causation。

#### `origin`

标记事实由哪个系统边界产生。`actorId` 用于玩家、同伴或具体动作实例。origin 是审计来源，不代表同伴已经感知该事件。

#### `awareness`

- `runtime_only`：程序和底层控制已知，默认不进入同伴认知。
- `companion_observed`：同伴通过合理视野、声音、自身状态或动作结果感知。
- `socially_shared`：通过聊天、明确交付或共同确认进入共享情境。

awareness 描述认知边界，不是隐私权限。

#### `priority`

- `critical`：停止、死亡、紧急反射和不可丢失生命周期事件。
- `high`：玩家直接聊天、动作终止和共同活动关键变化。
- `normal`：普通活动、意图、观察和模型运行事件。
- `low`：可合并进度、统计与后台整理。

事件注册表定义默认 priority。普通发布者不能为了抢占队列任意提高优先级。

### 5.3 不进入事件的内容

- API 密钥、访问令牌和认证材料。
- 模型私有思维链。
- 完整协议区块或未压缩二进制数据。
- 未清理的异常对象和循环引用。
- 可以通过 ID 引用的大型附件。

大型调试附件写入单独文件，payload 只保存经过清理的附件引用、哈希和大小。

## 6. 事件定义注册表

所有事件类型集中注册：

```ts
interface EventDefinition<TPayload> {
  type: string
  currentVersion: number
  payloadSchema: z.ZodType<TPayload>
  defaultPriority: EventPriority
  journal: 'required' | 'coalesced' | 'none'
  allowedOrigins: EventOrigin['kind'][]
  upcast?: (oldEvent: UnknownStoredEvent) => CurrentStoredEvent
}
```

注册表负责：

- 校验事件类型和 payload。
- 设置而非信任发布者提供的默认优先级。
- 决定是否持久化或合并。
- 拒绝未注册事件。
- 在重放时把旧版本 upcast 到当前内存版本。

领域模块拥有自己的 payload schema；`events` 模块只组合注册，不反向依赖领域实现。

## 7. v0.1 事件目录

以下是第一版纵向场景需要的最小事件集合。详细 payload 在实现 Issue 中定义，但字段必须满足本协议。

### 7.1 Minecraft 生命周期

```text
minecraft.connection.requested
minecraft.connection.established
minecraft.connection.failed
minecraft.connection.closed
minecraft.player.spawned
minecraft.player.died
minecraft.player.respawned
minecraft.dimension.changed
minecraft.world.bound
minecraft.snapshot.reconciled
```

### 7.2 玩家交互

```text
player.presence.joined
player.presence.left
player.chat.received
player.gesture.observed
player.item.given
player.activity.signal_observed
```

`player.gesture.observed` 和 `player.activity.signal_observed` 记录可观察行为，不直接断言玩家意图。

### 7.2.1 认知感知

```text
perception.entity.observed
perception.entity.visibility_changed
perception.entity.lost
perception.block.observed
perception.block_change.observed
perception.sound.observed
perception.scene.updated
```

感知事件只包含通过认知边界验证的实体、方块或声音观察。Mineflayer 实体表、完整区块、`findBlocks` 命中、raw ray 和 Control View 不得进入 payload。高频重复观察默认 coalesced；首次发现、重要变化和活动相关发现按 [认知感知详细设计](./cognitive-perception.md) 的策略持久化。

### 7.3 Companion Runtime

```text
companion.attention.changed
companion.conversation.updated
companion.activity.proposed
companion.activity.updated
companion.activity.paused
companion.activity.completed
companion.activity.abandoned
companion.activity.alignment_became_uncertain
companion.activity.alignment_restored
companion.activity.alignment_diverged
companion.intent.changed
companion.recovery.requested
companion.recovery.completed
```

### 7.4 模型运行

```text
model.decision.requested
model.decision.started
model.decision.completed
model.decision.failed
model.decision.cancelled
model.decision.discarded_as_stale
```

模型事件保存供应商、模型 ID、上下文版本、耗时、用量和最终结构化结果引用，不保存私有思维链。

### 7.5 语言

```text
speech.message.requested
speech.message.scheduled
speech.message.sent
speech.message.failed
speech.message.cancelled
```

语言承诺关联已接受动作 ID；动作未接受时不能产生对应的 `speech.message.sent`。

### 7.6 动作

```text
action.request.submitted
action.request.accepted
action.request.rejected
action.execution.started
action.execution.progressed
action.execution.stalled
action.execution.suspended
action.execution.resumed
action.execution.completed
action.execution.failed
action.execution.cancelled
action.execution.interrupted
```

`action.execution.progressed` 默认为 coalesced，不保证每一条进度都进入长期日志。所有终止事件必须 journal required。

### 7.7 安全反射

```text
safety.threat.level_changed
safety.protective_action.started
safety.emergency_takeover.started
safety.emergency_takeover.completed
safety.threat.recovered
```

安全动作先执行，再发布事件；事件持久化不能位于身体反应的关键路径上。

### 7.8 记忆

```text
profile.version.activated
memory.candidate.created
memory.record.written
memory.record.status_changed
memory.record.superseded
memory.record.disputed
memory.record.deleted
memory.reflection.created
memory.world_fact.became_stale
memory.world_fact.superseded
memory.world_fact.became_disputed
```

模型只能产生 candidate；Memory System 验证后才产生 record written。档案内容不进入事件 payload；`profile.version.activated` 只记录 profile ID、version ID、内容哈希、生效时间和变更原因。记忆状态与档案版本的完整规则由 [记忆模型、档案版本与冲突协调](./memory-model-and-profile-versioning.md) 定义。

## 8. Event Hub

### 8.1 发布路径

```text
来源模块构造事件草稿
→ 注册表校验 type、version、origin 和 payload
→ Event Hub 分配 id、sequence、recordedAt 和缺省 correlationId
→ 事件进入最近事件环形缓冲区
→ 事件进入 Journal Writer 内存队列
→ 同步运行纯内存 Reducer
→ 放入按优先级调度的异步分发队列
→ 返回已接受事件
```

`publish()` 不等待 SQLite，不调用模型，不发送网络消息，也不执行任意异步订阅者。

Journal 队列入队位于 Reducer 之前，因此 Reducer 缺陷不会让已经接受的事实从日志路径消失。入队仍然只是内存操作，不等待数据库事务。

### 8.2 同步 Reducer 约束

同步 Reducer 用于立即更新 Companion Runtime 可读取的轻量投影。它必须：

- 是纯函数或受控内存更新。
- 不执行 I/O。
- 不调用模型或 Mineflayer。
- 不等待 Promise。
- 在可预测的小时间预算内完成。
- 对相同旧状态和事件产生相同新状态。

Reducer 抛错表示程序缺陷。Event Hub 保留事件并进入 degraded 状态，不能假装事件没有发生。

### 8.3 异步订阅者

异步订阅者分为：

- Observer：日志、指标和调试界面。
- Coordinator：Attention Router、动作协调和记忆候选。
- Effector：模型、聊天和 Minecraft 动作等外部副作用。

同一事件的订阅者不能依赖注册顺序传递隐藏状态。需要顺序的工作必须通过新事件显式连接。

### 8.4 分发顺序

- 每个订阅者按照 `sequence` 接收事件。
- 同一订阅者默认串行处理。
- 不同订阅者可以并行，但不能共享可变状态。
- 高优先级事件可以提前唤醒 Coordinator，但不能颠倒已经分配的全局 sequence。
- 抢占通过发布新的取消或中断事件实现，不通过跳过旧事件实现。

## 9. Reflex 非阻塞记账

安全反射的顺序是：

```text
Threat Supervisor 识别迫近危险
→ 直接抢占或调整本地控制
→ 构造并发布领域事件
→ 内存投影立即更新
→ Journal Writer 稍后落盘
```

安全控制不能等待：

- SQLite 事务。
- Event Hub 异步订阅者。
- 模型推理。
- 文件日志或调试客户端。

如果 Journal 队列拥塞，Threat Supervisor 仍继续工作。系统进入 degraded 状态并暂停新的非必要长期动作，但不能暂停防护和安全停止。

## 10. 背压与丢弃策略

### 10.1 队列分类

Event Hub 维护逻辑上的优先级队列和独立 Journal Writer 队列。

### 10.2 不可丢事件

以下事件不能因普通队列拥塞被丢弃：

- critical 事件。
- 玩家聊天。
- 动作接受、拒绝、开始和所有终止事件。
- 共同活动状态变化。
- 记忆正式写入、纠正和删除。

### 10.3 可合并事件

- 动作进度只保留最近进度、阶段边界和终止前摘要。
- 重复威胁数值更新合并为等级变化。
- 连续普通观察由 Perception Layer 聚合。
- 指标采样可以丢失，不影响领域状态。

### 10.4 队列耗尽

不可丢队列接近容量时：

1. 停止接纳新的低优先级后台工作。
2. 取消或暂停非必要模型调用。
3. 暂停启动新的普通动作。
4. 继续处理停止、安全反射、生命周期和当前动作终止。
5. 产生可观察的 system.runtime.degraded 事件。

第一版不在内存队列满时同步写数据库，因为这会把存储延迟带回实时关键路径。

## 11. SQLite Journal

### 11.1 表结构

```sql
CREATE TABLE event_journal (
  sequence       INTEGER PRIMARY KEY,
  id             TEXT NOT NULL UNIQUE,
  type           TEXT NOT NULL,
  version        INTEGER NOT NULL,
  occurred_at    TEXT NOT NULL,
  recorded_at    TEXT NOT NULL,
  session_id     TEXT NOT NULL,
  world_id       TEXT NOT NULL,
  correlation_id TEXT NOT NULL,
  causation_id   TEXT,
  origin_json    TEXT NOT NULL,
  awareness      TEXT NOT NULL,
  priority       TEXT NOT NULL,
  payload_json   TEXT NOT NULL
);

CREATE INDEX idx_event_journal_correlation
  ON event_journal(correlation_id, sequence);

CREATE INDEX idx_event_journal_type
  ON event_journal(type, sequence);

CREATE INDEX idx_event_journal_world
  ON event_journal(world_id, sequence);
```

### 11.2 写入

- 单一 Journal Writer 按 sequence 写入。
- 在小批次事务中批量提交。
- required 事件不能跨过较早 required 事件写入。
- 重复 `id` 视为幂等成功；同一 sequence 对应不同 id 是致命一致性错误。
- SQLite 使用 WAL 模式，具体同步等级由持久化详细实现验证。

### 11.3 内存已接受但尚未落盘

Event Hub 的 accepted 只表示事件已经进入当前进程的内存时间线，不表示已经 fsync。

需要确认持久化的管理操作使用单独的 `flushThrough(sequence)`，但普通动作、聊天和 Reflex 不调用它。

极端进程崩溃可能损失 Journal 队列末端的少量事件。恢复时通过 Minecraft 权威快照和 `minecraft.snapshot.reconciled` 记录差异，不能假装缺失事件已经落盘。

## 12. 投影与检查点

### 12.1 投影类型

- Companion State Projection。
- Current Action Projection。
- Conversation Projection。
- Shared Activity Projection。
- Connection and World Projection。
- Pending Commitment Projection。

### 12.2 投影所有权

一个投影只有一个模块拥有写入逻辑。其他模块通过只读快照访问，不直接修改。

### 12.3 检查点

```sql
CREATE TABLE projection_checkpoints (
  projection_name TEXT PRIMARY KEY,
  event_sequence  INTEGER NOT NULL,
  projection_json TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
```

检查点是启动优化，不是事实权威。加载失败、版本不匹配或校验失败时，从事件日志重新构建。

### 12.4 重放模式

重放时：

- 只运行 Reducer。
- 禁止 Effector、模型调用、聊天发送和游戏动作。
- Observer 可以在明确 replay 标记下运行诊断。
- upcast 后的事件进入当前 Reducer。
- 投影记录最后处理 sequence，重复重放不会重复应用。

## 13. 恢复协调

启动恢复：

```text
打开 SQLite
→ 读取 MAX(sequence)
→ 加载兼容的投影检查点
→ 重放后续事件
→ 将未终止动作投影标记为等待协调
→ 连接 Minecraft
→ 获取权威状态快照
→ 比较位置、维度、生命、背包和附近状态
→ 发布 reconciliation 事件
→ Companion Runtime 决定继续、解释或放弃旧活动
```

### 13.1 未终止动作

重启前只有 started、没有终止事件的动作不会继续执行。恢复协调产生 `action.execution.interrupted`，原因是 `process_restarted`，并记录实际世界状态。

### 13.2 世界事实冲突

投影立即按服务器状态修正；历史记忆不被静默改写。协调结果可以产生：

```text
memory.world_fact.became_stale
memory.world_fact.superseded
memory.world_fact.became_disputed
```

具体记忆状态规则由 #4 定义。

### 13.3 Journal 尾部丢失

若崩溃前动作已经对世界产生效果，但终止事件未落盘，权威快照和技能专用核对器负责识别当前事实。恢复事件说明“观察到结果但缺少完整执行记录”，不能补写一个伪造的历史完成事件。

## 14. Schema 演进

### 14.1 事件不可原地修改

已经写入 Journal 的 payload 永不修改。新版本通过同一 `type` 的更高 `version` 表达。

### 14.2 Upcaster

读取旧事件时按顺序执行纯函数 upcaster：

```text
v1 → v2 → v3 → current
```

Upcaster：

- 不执行 I/O。
- 不读取当前世界状态。
- 不产生新事件。
- 对相同输入产生相同结果。

### 14.3 兼容规则

- 可选字段增加通常可以保持版本不变，但必须有明确默认语义。
- 字段删除、重命名、语义改变或枚举收紧需要升版。
- event type 重命名视为新事件类型，并提供旧类型迁移策略。
- 当前运行时遇到无法 upcast 的 required 事件时拒绝正常启动，不能跳过。

## 15. 重复与幂等

- Event Hub 为每次接受生成唯一 event ID。
- 来源层负责把重复原始信号聚合或去重。
- Journal 以 event ID 唯一约束防止重复落盘。
- Reducer 通过连续 sequence 和检查点防止重复应用。
- Effector 必须以命令或动作 ID 幂等，而不能只依赖事件 ID。
- 重放永远不运行 Effector。

第一版不提供分布式 exactly-once 承诺。目标是在单进程、单 SQLite 写者条件下实现可预测的 at-least-once 异步通知和幂等领域处理。

## 16. 隐私与调试输出

领域 Journal 位于本地，但仍按私人数据处理：

- 调试 API 默认不返回完整聊天正文。
- Telemetry 记录事件类型、ID、耗时和状态，敏感 payload 按策略删减。
- API 密钥永不进入事件。
- 导出调试包前必须进行内容清理。
- 未来支持多玩家时，检索和调试需要按世界与玩家关系隔离。

## 17. 模块接口

```ts
interface EventPublisher {
  publish<T extends RegisteredEventType>(
    draft: EventDraft<T>,
  ): AcceptedEvent<T>
}

interface EventReader {
  subscribe<T extends RegisteredEventType>(
    filter: EventFilter<T>,
    handler: EventHandler<T>,
  ): Unsubscribe

  recent(filter?: EventFilter): readonly DomainEvent[]
}

interface EventJournal {
  appendBatch(events: readonly DomainEvent[]): Promise<void>
  readAfter(sequence: number): AsyncIterable<StoredEvent>
  maxSequence(): Promise<number>
  flushThrough(sequence: number): Promise<void>
}

interface ProjectionStore {
  load(name: string): Promise<ProjectionCheckpoint | null>
  save(checkpoint: ProjectionCheckpoint): Promise<void>
}
```

### 17.1 依赖规则

- 领域模块依赖 `EventPublisher`，不依赖 SQLite 实现。
- `events` 组合注册表，但不依赖 Companion Runtime 或 Action Runtime 的具体类。
- `persistence` 实现 Journal 和 ProjectionStore。
- Reducer 定义在拥有投影的领域模块中。
- Effector 不能从历史重放入口注册。
- 测试可以使用内存 Journal 替代 SQLite。

## 18. 错误处理

### 18.1 发布校验失败

未注册类型、错误版本、非法 origin 或 payload schema 错误会拒绝发布，并产生内部诊断。调用方必须将其作为程序缺陷处理。

### 18.2 Journal 写入失败

- 保留尚未落盘事件在有界队列中并重试。
- 进入 degraded 状态。
- 停止新增长期普通动作和后台模型工作。
- 保留停止、安全反射和断线清理能力。
- 持续失败时安全停止 MineIntent 进程，而不是继续产生无法审计的行为。

### 18.3 异步订阅者失败

订阅者失败不能回滚已经发生的事件。系统记录订阅者名称、事件 ID 和错误，按订阅者策略重试或产生明确失败事件。

### 18.4 Reducer 失败

Reducer 失败说明当前内存投影不可信。系统进入 degraded 状态，阻止新的模型决策和普通动作，保留安全停止，并尝试从最近检查点重建。

## 19. 测试设计

### 19.1 契约测试

- 每个事件 payload 的有效和无效样例。
- 未注册事件和非法 origin 被拒绝。
- 默认 priority 与 journal 策略由注册表控制。
- 序列号严格递增且重启后继续。

### 19.2 因果测试

- 玩家聊天、决策、动作和结果共享 correlation ID。
- causation 只指向直接原因。
- 多证据通过 evidence IDs 表达。

### 19.3 分发测试

- 单订阅者按 sequence 处理。
- 不同订阅者失败互不篡改状态。
- 新高优先级事件产生显式抢占事件，不倒置历史顺序。

### 19.4 Reflex 测试

- SQLite 人工阻塞时，Threat Supervisor 仍在限定时延内接管身体。
- Journal 队列拥塞时 critical 事件不被普通进度挤出。
- 反射事件稍后按原 sequence 落盘。

### 19.5 Journal 测试

- 批量事务成功、失败和重试。
- 重复 event ID 幂等。
- sequence 冲突使运行时进入故障状态。
- WAL 重启后可读取完整已提交批次。

### 19.6 重放测试

- 从零重放与检查点加增量重放得到相同投影。
- 重放不调用模型、聊天或 Mineflayer。
- 旧版本事件通过 upcaster 得到相同当前语义。
- 重复重放不重复改变投影。

### 19.7 恢复测试

- started 但未终止的动作变为 process restarted 中断。
- 世界快照差异产生 reconciliation 事件。
- 崩溃前世界已变化但 Journal 尾部丢失时不伪造历史成功。

## 20. 可观察性

Event Hub 提供：

- 当前 sequence。
- 各优先级队列深度。
- Journal 待写数量与最后落盘 sequence。
- 每个订阅者最后处理 sequence 和延迟。
- 投影最后处理 sequence。
- degraded 原因和持续时间。
- 每类事件发布、合并、拒绝和失败数量。

调试界面允许按 correlation ID 查看完整因果链。

## 21. 实施顺序

1. 定义信封、注册表与内存 Event Hub。
2. 建立单元测试和内存 Journal。
3. 实现 SQLite Journal 与批量写入。
4. 实现一个最小连接投影和动作投影。
5. 实现检查点和无副作用重放。
6. 接入 Minecraft Backend 的生命周期事件。
7. 接入 Action Runtime 和 Companion Runtime。
8. 加入背压、degraded 状态和恢复协调。

## 22. 验收映射

本设计满足 #12 的验收要求：

- 事件 ID、correlation、causation、origin、awareness、world 和 session 已定义。
- 高频协议状态与 durable domain events 已分离。
- Reflex 使用同步内存接受和异步 SQLite 落盘。
- 顺序、重复、投影重放和恢复均有测试设计。
- 设计与 ADR-0002 一致，未引入新的架构方向冲突。
