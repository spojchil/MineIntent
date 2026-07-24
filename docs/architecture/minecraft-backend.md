---
status: accepted
authority: normative
implementation: diverged
last_verified: 2026-07-23
---

# Mineflayer Backend 生命周期与状态快照

> 这是 Issue [#14](https://github.com/spojchil/maineintent/issues/14) 的 v0.1 接受设计，目标版本为 Paper / Minecraft Java 1.21.1 / Mineflayer 4.37.1。Backend 主体已经实现，随后又按协议驱动方向缩减；本文仍含旧 Action Runtime、Control View 和快照契约，不能当作当前 API 参考。上游：[目标系统](./target-system.md)、[领域事件](./domain-events-and-journal.md)、[认知感知](./cognitive-perception.md)、[同伴运行时](./companion-runtime.md)。

## 1. 目的

本文定义 v0.1 的 `MinecraftBackend` 实现边界。Backend 独占 Mineflayer Bot，负责连接、登录、生成、断线、重连、死亡、重生、维度切换、权威快照和原始协议信号，并把所有输出转换为可验证、可取消、无 Mineflayer 对象引用的普通数据。

Backend 不是感知层、动作运行时或领域事件总线：它提供真实协议状态和最小查询能力；#24 决定哪些实体、方块和声音能成为同伴认知；Action Runtime 决定怎样操作身体；Event Hub 决定哪些稳定事实进入日志。

## 2. 范围与非目标

本文负责：

- 配置校验与 Mineflayer Bot 创建。
- 生命周期状态机、连接代次和 readiness。
- 断线原因归并、错误分类和可取消重连。
- `MinecraftSnapshotV1` 的一致性、修订和不可变 DTO。
- 实体、方块、声音、聊天、自身和世界原始事件 DTO。
- 提供给 Perception 与 Control 的受限只读接口。
- 停止、超时、监听器、控制状态和资源清理。
- 世界身份绑定和本地 Paper 1.21.1 验收接口。

本文不负责：

- 判定实体、方块或声音是否进入同伴认知。
- 自动寻路、挖掘、战斗、聊天回复或技能编排。
- 事件日志持久化和 Companion State reducer。
- 启动或管理生产 Minecraft 服务端。
- v0.1 多 Bot、代理网络、模组协议或 Bedrock。
- 在 Backend API 中暴露 `mineflayer.Bot`、Prismarine World/Entity/Block 或原始 packet。

## 3. 源码事实与设计结论

基于当前锁定 Mineflayer 源码：

- `loader.js` 将协议 client 的 `connect`、`error`、`end` 转发为 Bot 事件；`bot.end()` 直接结束 client。
- `game.js` 在 login packet 后设置 dimension/gameMode 并发出 `login`/`game`；respawn packet 更新同一 `bot.game` 并发出 `game`。
- `health.js` 在 respawn packet 到达时先发 `respawn` 并设 `isAlive=false`，健康恢复后发 `spawn`；首次正生命 health 也触发 `spawn`。
- `kick.js` 发出 `kicked`，连接随后仍可能发 `end`。
- Mineflayer 自带重连示例只在 `end` 后立即重新 `createBot`，不包含取消、退避或错误分类。

因此：

1. `connect`、`login`、`spawn` 和 Backend `ready` 不能合并。
2. 每次 `createBot` 都是一个新的 connection epoch，旧 Bot 永不复用。
3. kicked/error/end 合并成每代一个 close outcome。
4. respawn packet 不足以区分死亡重生与维度切换，必须结合 death、旧/新 dimension 和后续 spawn。
5. Backend 自己管理重连，不能在 Mineflayer event callback 中无条件递归创建 Bot。

## 4. 配置

```ts
interface MinecraftBackendConfig {
  worldId: string
  server: {
    host: string
    port: number
    version: '1.21.1'
  }
  identity: {
    username: string
    auth: 'offline' | 'microsoft'
    profilesFolder?: string
  }
  timeouts: {
    connectMs: number
    loginMs: number
    spawnMs: number
    stopMs: number
  }
  reconnect: {
    enabled: boolean
    initialDelayMs: number
    multiplier: number
    maxDelayMs: number
    jitterRatio: number
    stableResetMs: number
  }
}
```

v0.1 默认：

```text
version             1.21.1（固定，不自动探测）
connect timeout     10s
login timeout       20s
spawn timeout       30s
stop timeout        5s
reconnect delay     1s × 2，最大 30s，±20% jitter
stable reset        ready 持续 60s 后清零连续失败次数
```

- `worldId` 是用户配置的稳定身份，不从服务器地址推导。
- 离线身份只用于本地受控服务器；生产建议 Microsoft auth 和白名单。
- auth token、缓存凭证和 profiles folder 不进入事件、快照或普通日志。
- 配置由严格 runtime schema 校验；未知字段、无效端口、空 world ID 和非 1.21.1 版本拒绝启动。
- 时间和重连数值是运行配置，不属于同伴人格。

## 5. 身份与标识

本文使用的几何 DTO：

```ts
interface Vec3Value { x: number; y: number; z: number }
interface BlockPosition { x: number; y: number; z: number }
type AabbValue = [minX: number, minY: number, minZ: number, maxX: number, maxY: number, maxZ: number]
```

所有数值在出边界前验证为 finite；BlockPosition 必须为整数。DTO 不携带 Vec3 方法或共享可变引用。

```ts
interface BackendIdentity {
  processSessionId: string
  worldId: string
  pendingWorldId: string
  connectionEpoch: number
  connectionAttemptId: string
  botEntityKey?: string
  serverFingerprint?: ServerFingerprint
}

interface ServerFingerprint {
  endpointHash: string
  protocolVersion: number
  minecraftVersion: string
  serverBrand?: string
}
```

- `processSessionId` 是一次 MineIntent 进程运行 ID，与领域事件 `sessionId` 一致，重连不改变。
- `connectionEpoch` 在本进程每次创建 Bot 时递增，从 1 开始。
- `connectionAttemptId` 全局唯一，用于连接链路和迟到事件过滤。
- `worldId` 来自配置；连接前领域事件可使用 `pending:<connectionAttemptId>`，绑定后发布 `minecraft.world.bound`。
- fingerprint 用于诊断误连，不证明两个地址后面一定是同一世界，也不能替代 world ID。
- endpoint 只以规范化哈希进入持久事件，避免无必要记录家庭网络地址。

## 6. 生命周期状态机

### 6.1 状态

```ts
type BackendState =
  | { status: 'idle' }
  | { status: 'connecting'; epoch: number; attemptId: string; attempt: number }
  | { status: 'logging_in'; epoch: number; attemptId: string; attempt: number }
  | { status: 'spawning'; epoch: number; attemptId: string; attempt: number }
  | { status: 'ready'; epoch: number; attemptId: string; readyAt: string }
  | { status: 'dead'; epoch: number; attemptId: string; diedAt: string }
  | { status: 'reconnecting'; attempt: number; retryAt: string; lastClose: BackendClose }
  | { status: 'stopping'; epoch?: number; reason: string }
  | { status: 'stopped'; reason?: string }
  | { status: 'faulted'; failure: BackendFailure }
```

Backend state 是单写者内存状态。每次转换递增 `lifecycleRevision`，产生 Backend lifecycle event，再由适配器映射到领域事件。

### 6.2 主路径

```text
idle
→ connecting       createBot，安装本代监听器
→ logging_in       transport connect
→ spawning         login + game fields available
→ ready            spawn + readiness guards

ready
→ dead             death
→ ready            respawn packet + 后续有效 spawn

ready/dead/前置阶段
→ reconnecting     retryable close
→ connecting       backoff 到期，新 epoch

任意非终止状态
→ stopping
→ stopped

任意连接阶段
→ faulted          fatal config/auth/version 或重连关闭
```

### 6.3 事件时序

| Mineflayer 事件 | Backend 处理 |
|---|---|
| `connect` | connecting → logging_in；只表示 TCP/协议连接 |
| `login` | logging_in → spawning；记录版本、dimension、gameMode |
| `spawn` 首次 | readiness guard 后 spawning → ready |
| `death` | ready → dead；只发布一次 |
| `respawn` | 记录 transition candidate，等待 game/dimension 与 spawn |
| `game` | 比较 dimension/gameMode；必要时发布变化 |
| 后续 `spawn` | dead/transition → ready；区分 respawn/dimension transition |
| `kicked` | 保存 sanitized kick evidence，不立即重复 close |
| `error` | 保存 error evidence；若连接未结束可触发受控结束 |
| `end` | 完成本 epoch 唯一 close outcome，决定 retry/fault/stop |

所有 handler 先验证 `epoch === activeEpoch` 且 Bot 引用仍是 active。旧代迟到事件只计 telemetry，不更新状态或发布领域事件。

## 7. Readiness

`spawn` 之后仍须同步检查：

```ts
interface ReadinessGuards {
  entityPresent: boolean
  finitePosition: boolean
  finiteYawPitch: boolean
  positiveHealth: boolean
  foodKnown: boolean
  inventoryPresent: boolean
  dimensionKnown: boolean
  gameModeKnown: boolean
  versionMatches: boolean
}
```

- 全部通过才进入 `ready`，并生成首个完整快照。
- nearby players 为空是合法状态，不是 readiness 条件。
- server brand、weather、experience 和 chunk 数不是必需条件，可稍后更新。
- spawn 到达但 guard 暂未满足时在同一 event-loop turn 后重试，直到 spawn timeout。
- 超时形成 `spawn_timeout` close；新 Bot 重连，不能在半初始化对象上继续。
- `snapshot()` 在非 ready/dead 状态返回 typed `BackendNotReadyError`；调试使用 `inspectLifecycle()`，不伪造部分权威快照。

## 8. 快照一致性

### 8.1 快照

```ts
interface MinecraftSnapshotV1 {
  protocol: 'mineintent.minecraft.snapshot.v1'
  snapshotRevision: number
  lifecycleRevision: number
  capturedAt: string
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  world: WorldSnapshot
  self: SelfSnapshot
  inventory: InventorySnapshot
  trackedPlayers: TrackedPlayerSnapshot[]
}

interface WorldSnapshot {
  worldId: string
  dimension: string
  minecraftVersion: '1.21.1'
  protocolVersion: number
  gameMode: 'survival' | 'creative' | 'adventure' | 'spectator'
  difficulty?: 'peaceful' | 'easy' | 'normal' | 'hard'
  minY: number
  height: number
  serverViewDistance?: number
  timeOfDay?: number
  isRaining?: boolean
}

interface SelfSnapshot {
  entityKey: string
  username: string
  position: Vec3Value
  velocity: Vec3Value
  yaw: number
  pitch: number
  onGround: boolean
  alive: boolean
  health: number
  food: number
  foodSaturation: number
  oxygen?: number
  experience?: { level: number; progress: number; total: number }
  effects: Array<{ name: string; amplifier: number; durationTicks?: number }>
}

interface InventorySnapshot {
  selectedHotbarSlot: number
  slots: Array<{
    slot: number
    itemName: string
    count: number
    metadata?: number
    durabilityUsed?: number
  }>
}

interface TrackedPlayerSnapshot {
  playerKey: string
  uuid?: string
  username: string
  listed: boolean
  entityTracked: boolean
  position?: Vec3Value
  yaw?: number
  pitch?: number
  heldItemName?: string
}
```

`trackedPlayers` 明确是 Protocol/Control 信息，不是“附近可见玩家”。#24 必须经过实体感知后才能产生 Cognitive Observation。命名避免误导调用者。

### 8.2 一致性规则

- Node event loop 内同步复制所有字段，复制期间不 `await`。
- 快照只包含 plain object、array、number、string、boolean；不包含 Vec3 class、Item、Entity、Window、NBT object 或函数。
- 返回深度只读对象；每次调用不共享可变数组。
- required 字段出现 `NaN`、Infinity、undefined 或不支持枚举时拒绝快照并触发 reconciliation/failure，不静默填零。
- inventory 只保留正常物品字段；原始 NBT 默认不出 Backend。
- snapshot revision 在权威内容变化时递增；重复读取相同状态不递增。
- tracked player 位置可能来自不可见实体表，只能由受限消费者读取。

### 8.3 更新与发布

Backend 不在每 tick 发布完整快照。它维护当前投影：

- lifecycle、dimension、health、inventory、player list 等事件增量更新。
- self position/velocity/yaw/pitch 以固定采样上限更新内存。
- `snapshot.changed` 只在字段组 revision 变化时发布轻量通知。
- 消费者调用 `snapshot()` 获取一致副本。

默认 self pose 投影上限 10 Hz，足够 Perception/调试；物理与 Action Runtime 可通过专用 Control View 读取更实时值。

## 9. Backend Event

```ts
interface BackendEventEnvelope<T> {
  protocol: 'mineintent.minecraft.backend-event.v1'
  id: string
  kind: BackendEventKind
  occurredAt: string
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  worldId: string
  dimension?: string
  payload: T
}
```

```ts
type BackendEventKind =
  | 'lifecycle'
  | 'self'
  | 'world'
  | 'entity'
  | 'block'
  | 'sound'
  | 'chat'
  | 'player_list'
  | 'snapshot_changed'
```

- Backend events 是进程内、版本化的适配器协议，不全部进入领域日志。
- Event Mapper 将稳定生命周期和玩家交互映射为 DomainEvent。
- Perception Engine 消费 entity/block/sound。
- #15 Speech/Chat 消费 chat，并由独立发送接口输出语言。
- Action Runtime 消费 self/world 与 Control View。

## 10. 生命周期事件 DTO

```ts
type BackendLifecyclePayload =
  | { type: 'connection_requested'; attempt: number }
  | { type: 'transport_connected' }
  | { type: 'logged_in'; version: string; dimension: string }
  | { type: 'ready'; snapshotRevision: number }
  | { type: 'died' }
  | { type: 'respawn_transition_started'; fromDimension: string }
  | { type: 'respawned'; dimension: string }
  | { type: 'dimension_changed'; from: string; to: string }
  | { type: 'reconnect_scheduled'; attempt: number; retryAt: string; closeCode: string }
  | { type: 'connection_closed'; close: BackendClose }
  | { type: 'faulted'; failure: BackendFailure }
  | { type: 'stopped'; reason: string }
```

领域映射：

| Backend | DomainEvent |
|---|---|
| connection_requested | `minecraft.connection.requested` |
| transport_connected | `minecraft.connection.established` |
| fatal/attempt failure | `minecraft.connection.failed` |
| connection_closed | `minecraft.connection.closed` |
| first ready | `minecraft.player.spawned` |
| died | `minecraft.player.died` |
| dead 后 ready | `minecraft.player.respawned` |
| dimension change | `minecraft.dimension.changed` |
| configured identity accepted | `minecraft.world.bound` |
| reconnect ready snapshot | `minecraft.snapshot.reconciled` |

`connection.established` 只证明 transport 已建立；系统 Presence 仍保持 connecting，直到 player spawned/ready。

## 11. 原始实体事件

```ts
interface ProtocolEntitySnapshot {
  entityKey: string
  protocolEntityId: number
  type: string
  name?: string
  username?: string
  uuid?: string
  position: Vec3Value
  velocity: Vec3Value
  yaw: number
  pitch: number
  headYaw?: number
  width: number
  height: number
  onGround: boolean
  pose?: string
  heldItemName?: string
  equipment: Array<{ slot: number; itemName: string; count: number }>
  valid: boolean
}

type ProtocolEntityEvent =
  | { type: 'spawned'; entity: ProtocolEntitySnapshot }
  | { type: 'moved'; entity: ProtocolEntitySnapshot }
  | { type: 'updated'; entity: ProtocolEntitySnapshot; changed: string[] }
  | { type: 'animation'; entityKey: string; animation: string }
  | { type: 'hurt'; entityKey: string; possibleSourceEntityKey?: string }
  | { type: 'removed'; entityKey: string; last: ProtocolEntitySnapshot; reason: 'protocol_removed' }
```

- `entityKey = <epoch>:<protocolEntityId>`，防止重连后 ID 复用。
- raw metadata、ChatMessage、Item、passengers object graph 不出边界。
- moved 事件可在 Backend distributor 中合并；Perception tracker 不需要每个协议增量。
- removed 不推断 death/despawn/out-of-range，#24 只得到 protocol_removed。

## 12. 原始方块与世界事件

```ts
interface ProtocolBlockSnapshot {
  position: BlockPosition
  name: string
  stateId: number
  properties: Record<string, string | number | boolean>
  collisionShapes: AabbValue[]
  transparentHint: boolean
  boundingBox: 'block' | 'empty'
}

type ProtocolBlockEvent =
  | { type: 'updated'; oldBlock: ProtocolBlockSnapshot | null; newBlock: ProtocolBlockSnapshot | null }
  | { type: 'chunk_loaded'; chunkX: number; chunkZ: number }
  | { type: 'chunk_unloaded'; chunkX: number; chunkZ: number }
```

- chunk load/unload 只描述缓存边界，awareness 为 runtime_only。
- block snapshot 不包含 sign/book/container NBT 和未清理 block entity。
- unknown/unloaded 与 air 分开：读取接口返回 tagged result，不用 `null` 同时表示二者。
- collision shape 是几何输入，不声明光学透明；#24 的 VisionMaterialPolicy 决定遮挡。

```ts
type BlockReadResult =
  | { status: 'loaded'; block: ProtocolBlockSnapshot }
  | { status: 'unloaded' }
  | { status: 'out_of_world' }
```

## 13. 原始声音事件

```ts
interface ProtocolSoundPayload {
  type: 'heard'
  soundKey: string
  soundName?: string
  soundId?: number
  category?: string
  sourcePosition: Vec3Value
  volume: number
  pitch: number
  protocolSource: 'named_sound_effect' | 'sound_effect'
}
```

- Backend 订阅 Mineflayer 的规范化 sound events，但要处理 named packet 同时触发兼容 hardcoded callback 的重复。
- 短时间内同位置、volume、pitch、名称/ID相同的双回调合并成一个 Backend event。
- Backend 不做距离、方向、遮挡和语义分类；这些由 #24 完成。
- 非有限位置和参数拒绝，记录 telemetry，不向 Perception 发布。

## 14. 聊天、玩家与自身事件

```ts
interface ProtocolChatEvent {
  senderUsername?: string
  senderUuid?: string
  plainText: string
  position?: 'chat' | 'system' | 'game_info'
  verified?: boolean
}
```

- 只输出 Mineflayer 已解析的 plain text 和身份字段，不输出循环 ChatMessage object。
- 聊天的主要玩家识别、寻址、命令语义和发送节流由 #15 负责。
- playerJoined/playerLeft 输出 player list DTO；不把 joined 等同实体可见。
- health、food、inventory、effects、experience、gameMode 和 dimension 变化输出 self/world DTO，并驱动 snapshot revision。
- 容器内容只在明确交互会话内由 Action Runtime 专用接口读取，不进入普通 snapshot。

## 15. Perception Source

#24 只能依赖：

```ts
interface ProtocolObservationSource {
  identity(): Readonly<BackendIdentity>
  selfPose(): Readonly<SelfPoseSnapshot>
  listTrackedEntities(): readonly Readonly<ProtocolEntitySnapshot>[]
  readBlock(position: BlockPosition): Readonly<BlockReadResult>
  subscribe(
    listener: (event: BackendEventEnvelope<ProtocolEntityEvent | ProtocolBlockEvent | ProtocolSoundPayload>) => void,
  ): Unsubscribe
}
```

- 不提供 `bot.world`、`bot.entities`、`findBlocks` 或任意 packet listener。
- `listTrackedEntities` 只生成感知候选；其结果不能越过 Perception Boundary。
- DDA 可以在 Perception 中通过 `readBlock` 实现，使光学策略独立于 Mineflayer raycast matcher。
- 为避免大量逐格对象分配，实现可增加 `readBlocksAlongRay` 批量方法，但返回类型与 unknown 语义不变。
- Source 仅在 ready/dead 且当前 epoch 有效；重连时旧 Source 失效并抛 `StaleBackendEpochError`。

## 16. Control View

```ts
interface MinecraftControlView {
  epoch: number
  self(): Readonly<SelfSnapshot>
  readBlock(position: BlockPosition): Readonly<BlockReadResult>
  listTrackedEntities(): readonly Readonly<ProtocolEntitySnapshot>[]
  withBot<T>(capability: InternalCapabilityToken<T>): T
}
```

`withBot` 不是公开逃生口：只有 `src/minecraft/`、`src/actions/` 和受审查的 `src/skills/` 内部 capability adapter 可以持有不可构造 token。对外导出的 Backend interface 没有该方法。Action Runtime 后续应优先使用注册的 atomic controls，而非传递 Bot。

编译依赖规则禁止 Companion、Context、Models、Memory 和 Speech 导入 Control View。

## 17. 对外接口

```ts
interface MinecraftBackend {
  start(signal: AbortSignal): Promise<BackendReady>
  stop(reason: string): Promise<void>
  state(): Readonly<BackendState>
  snapshot(): Readonly<MinecraftSnapshotV1>
  subscribe(listener: (event: BackendEventEnvelope<unknown>) => void): Unsubscribe
  observationSource(): ProtocolObservationSource
}

interface BackendReady {
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  snapshot: Readonly<MinecraftSnapshotV1>
}
```

### 17.1 `start`

- 只能从 idle/stopped 调用；并发 start 共享同一个 in-flight promise 或拒绝为 already_started，v0.1 选择共享。
- promise 在首次 ready 时 resolve，不等待后续进程生命周期结束。
- signal abort 在 ready 前拒绝 `AbortError` 并完成 stop；ready 后仍持续监听 signal，触发受控 stop。
- retryable 失败可在 promise 内经历多次重连；fatal failure 立即 reject。

### 17.2 `stop`

- 幂等；多个调用共享 stop promise。
- 设置 stop intent 后取消 connect/login/spawn deadline 和 reconnect timer。
- 通知上层先取消 Action Runtime 是 composition root 的责任；Backend 自身立即清除 control states。
- 对 active Bot 调用 `quit(reason)`；超时后调用 client end 的受控 adapter。
- 等待本 epoch `end` 或 stop deadline，完成自身 listener 清理并丢弃 Bot。
- stop 过程中任何 end/error 都不触发重连。

### 17.3 `subscribe`

- 返回幂等 Unsubscribe。
- listener 异常被隔离并进入 telemetry，不能中断 Mineflayer handler。
- lifecycle/self/world 使用可靠有界队列；entity moved 等可合并；溢出时绝不能阻塞协议线程。
- stop 后不再回调；已排队事件用 epoch/stop token 丢弃。

## 18. 关闭归并与错误分类

```ts
interface BackendClose {
  epoch: number
  at: string
  code: string
  retryable: boolean
  deliberate: boolean
  kick?: { text: string; duringLogin: boolean }
  error?: { name: string; message: string; code?: string }
  endReason?: string
}

interface BackendFailure {
  code:
    | 'invalid_config'
    | 'unsupported_version'
    | 'authentication_failed'
    | 'permission_denied'
    | 'connection_timeout'
    | 'login_timeout'
    | 'spawn_timeout'
    | 'protocol_error'
    | 'reconnect_disabled'
  message: string
  retryable: boolean
}
```

- 每 epoch 只有一个 CloseAccumulator。
- kicked/error 只追加 sanitized evidence；`end` 或受控 timeout 最终 seal。
- seal 后迟到 evidence 仅调试记录，不产生第二个 `connection.closed`。
- Error 序列化只保留 name/message/code，去除 stack 中凭证、路径和 packet。
- kick reason 解析 ChatMessage 为有长度限制的 plain text。

默认分类：

| 情况 | 处理 |
|---|---|
| ECONNREFUSED、ECONNRESET、server stop、普通 end | retryable |
| connect/login/spawn timeout | retryable |
| Microsoft 凭证失败 | fatal |
| banned/whitelist/invalid session | fatal，除非管理侧重启 |
| 明确版本不支持 | fatal |
| 用户 stop / AbortSignal | deliberate，不重连 |
| 无法识别 kick | 默认 fatal，防止高速重试封禁 |

分类器独立、可单元测试；不能只对 reason 文本做宽泛关键词猜测。

## 19. 重连

```text
close sealed
→ classify
→ deliberate       stopped
→ fatal            faulted
→ retryable
   → reconnecting + schedule
   → timer / AbortSignal race
   → new epoch + createBot
```

- 使用指数退避和 bounded jitter；随机源可注入以便测试。
- retry timer 使用可取消 scheduler，不使用阻塞 sleep。
- attempt 只在 ready 连续稳定超过 `stableResetMs` 后归零。
- v0.1 对 retryable failure 持续重试，最大间隔封顶；用户可 stop。
- reconnect disabled 时第一个 retryable close 进入 faulted/reconnect_disabled。
- 重连成功后首个快照与上次持久投影协调，发布 snapshot reconciled；旧动作不自动恢复。
- 维度、实体 key、chunk cache 和 observation source 都绑定 epoch，不跨重连复用。

## 20. 监听器与资源清理

每代创建 `ConnectionResources`：

```ts
interface ConnectionResources {
  epoch: number
  bot: MineflayerBotInternal
  disposers: Array<() => void>
  deadlines: Set<CancelableTimer>
  closeAccumulator: CloseAccumulator
  stopped: boolean
}
```

规则：

1. 创建 Bot 后立即安装 Backend error/end/kicked handler，避免未处理 error。
2. Backend 使用 helper 记录每个 `on/once` 对应 disposer。
3. cleanup 只移除 Backend 自己的 listener，不对存活 Bot 调用全局 `removeAllListeners()`。
4. 停止 physics controls：`clearControlStates()`，取消尚未结束的原子控制 adapter。
5. 关闭窗口/容器和后续插件资源由拥有它们的 Action Runtime cleanup 负责。
6. 结束 client 后丢弃整个 Bot；不复用内部 plugin 状态。
7. 清理所有 deadline、stable timer、reconnect timer 和 signal handler。
8. cleanup 幂等；即使 quit/end/error 顺序变化也只运行一次。

Mineflayer 创建时设置 `logErrors: false`，由结构化 telemetry 统一处理，避免库默认 console 输出和 Backend 日志重复。

## 21. 并发与背压

- 生命周期变更全部经过同步 state reducer；event handler 不直接 `await`。
- 每个 handler 先复制 DTO，再放入 distributor；不能把可变 Bot object 异步传出。
- lifecycle、death、dimension、chat、sound、block update 不可因 moved flood 丢失。
- entity moved 按 entityKey 合并为最新值；self pose 按 revision 合并。
- distributor 有独立优先级和容量指标；溢出时先丢普通 moved，再丢重复 snapshot_changed。
- Perception、Event Mapper 或 Debug consumer 变慢不能阻塞协议 client。
- stop 与 reconnect 使用状态 token 仲裁，保证 stop intent 优先。

## 22. 世界与快照协调

首次 ready：

```text
验证配置 worldId
→ 计算 server fingerprint
→ publish world.bound
→ build snapshot
→ publish player.spawned
```

重连 ready：

```text
验证 worldId / fingerprint 警告
→ build authoritative snapshot
→ 与 Event Projection 最后检查点比较
→ publish snapshot.reconciled
→ Companion Runtime 进入 recovering
```

- Backend 只产生实际快照和差异，不决定恢复旧活动。
- endpoint fingerprint 变化默认告警；world ID 仍由用户配置负责。
- Minecraft 服务器不能可靠提供存档 UUID 时，v0.1 不伪造一个自动身份。
- actual server.properties 的 `online-mode` 不进入世界身份；它只影响 auth 配置和测试安全。

## 23. 领域事件 payload 最小字段

```ts
interface ConnectionRequestedPayload { attemptId: string; attempt: number; endpointHash: string }
interface ConnectionEstablishedPayload { attemptId: string; epoch: number; protocolVersion: number }
interface ConnectionClosedPayload { attemptId: string; epoch: number; code: string; retryable: boolean; deliberate: boolean }
interface PlayerSpawnedPayload { epoch: number; snapshotRevision: number; dimension: string }
interface PlayerDiedPayload { epoch: number; position: Vec3Value; dimension: string }
interface PlayerRespawnedPayload { epoch: number; snapshotRevision: number; dimension: string }
interface DimensionChangedPayload { epoch: number; from: string; to: string; snapshotRevision: number }
interface WorldBoundPayload { configuredWorldId: string; fingerprint: ServerFingerprint }
interface SnapshotReconciledPayload { epoch: number; snapshotRevision: number; changedGroups: string[] }
```

完整 snapshot 不复制进每个 DomainEvent；事件引用 snapshot revision，Telemetry/Projection 保存所需字段。

## 24. 测试架构

### 24.1 单元测试

Backend 构造函数注入：

```ts
interface MineflayerBotFactory { create(options: SafeBotOptions): MineflayerBotInternal }
interface Clock { now(): Date; monotonicMs(): number }
interface Scheduler { timeout(ms: number, callback: () => void): CancelableTimer }
interface RandomSource { next(): number }
```

使用 FakeBot/EventEmitter 测试：

- connect/login/spawn/ready 正常序列。
- spawn 前 snapshot 拒绝。
- readiness 缺字段和 timeout。
- death/respawn、纯维度切换、death 后维度变化组合。
- kicked → error → end 只形成一次 close。
- stop/end、abort/connect、retry timer/stop 竞态。
- 旧 epoch 迟到 event 不改变状态。
- backoff、jitter、stable reset 和 fatal classification。
- snapshot deep copy、revision、NaN/undefined 拒绝。
- raw Mineflayer/Vec3/Item/Entity 不出 DTO。
- 100 次连接/清理后 listener、timer 和 signal handler 数回到基线。

### 24.2 Contract 测试

- TypeScript/ESLint 禁止 Backend 外部导入 mineflayer raw type。
- 所有 BackendEvent 通过版本化 runtime schema。
- entity/block/sound DTO 与 #24 schema fixture 对齐。
- unknown chunk 与 air 区分。
- sound 双回调去重。
- inventory NBT、auth、endpoint 明文和异常 stack 不进入输出。

### 24.3 本地 Paper 集成测试

使用 `mcserver/mc.ps1` 管理 Paper 1.21.1。测试默认 local-only、offline auth 与唯一测试用户名；生产安全建议不因此改变。

场景：

1. `mc.ps1 start`，Backend 连接并达到 ready，验证版本、位置、生命、饥饿、背包、维度、world ID。
2. 控制台 `/kill MineIntentBot`，验证 died → respawned，快照 alive/health 修复。
3. 控制台将 Bot 切换维度，验证 dimension changed，不误报普通 death。
4. `mc.ps1 stop`，验证一个 retryable close 和 backoff；重启 Paper 后进入新 epoch 并 snapshot reconciled。
5. 服务端 kick，验证 reason 清理与分类。
6. Backend stop，验证服务端看到正常离开、不重连、listener/timer 清零。
7. 重复连接/停止至少 20 次，检查 MaxListeners、端口、进程和事件重复。
8. 生成实体、更新方块和播放声音，验证 #24 Source 收到 plain DTO，不含 Bot/World/Entity/Block class。

测试管理员的命令只用于布置和断言，不计入同伴自主行为。CI 不自动启动本地 Paper；CI 运行 fake/contract tests，Paper suite 由 #8 的集成框架接管。

## 25. 可观察性

只读调试状态：

- backend state、lifecycle revision、epoch、attempt 和 uptime。
- 当前 deadline、retryAt、连续失败数和最后 close code。
- snapshot revision 和各字段组 revision。
- listener、timer、subscriber 和队列计数。
- entity moved coalesced/dropped、事件队列高水位。
- 旧 epoch 事件丢弃数。
- DTO/schema 拒绝数和原因。

日志不包含 auth、完整 endpoint、原始 packet、容器 NBT 或未清理异常。网络与 Mineflayer 默认日志统一进入 TelemetrySink。

## 26. 建议代码结构

```text
src/minecraft/
├── contracts/
│   ├── backend-v1.ts
│   ├── events-v1.ts
│   └── snapshot-v1.ts
├── config.ts
├── minecraft-backend.ts
├── lifecycle-reducer.ts
├── connection-resources.ts
├── close-accumulator.ts
├── reconnect-policy.ts
├── snapshot-builder.ts
├── event-mapper.ts
├── dto/
│   ├── entity.ts
│   ├── block.ts
│   ├── sound.ts
│   ├── chat.ts
│   └── inventory.ts
├── observation-source.ts
├── internal-control-view.ts
└── mineflayer-bot-factory.ts

test/
├── minecraft/
│   ├── lifecycle.test.ts
│   ├── reconnect.test.ts
│   ├── snapshot.test.ts
│   ├── cleanup.test.ts
│   └── contracts.test.ts
└── integration/paper/backend.test.ts
```

运行时 schema 是 DTO 权威，TypeScript 类型由 schema 推导。Mineflayer adapter 集中在 factory/DTO/control 文件，不扩散到领域模块。

## 27. 实现顺序

### P0：可测试边界

1. 配置、contracts、FakeBot factory 和 lifecycle reducer。
2. 单次 connect/login/spawn/ready/stop。
3. 首个 snapshot builder 与 deep plain DTO 测试。
4. 生命周期领域事件 mapper。

### P1：持续连接

1. CloseAccumulator、错误分类和 timeout。
2. reconnect policy、epoch guard 和 AbortSignal。
3. death/respawn/dimension/reconciliation。
4. cleanup、listener/timer 泄漏测试。

### P2：消费者接口

1. entity/block/sound/chat/self DTO。
2. ProtocolObservationSource 与 #24 fixtures。
3. bounded distributor/coalescing/metrics。
4. Paper 1.21.1 集成场景和删除独立 smoke 脚本。

## 28. #14 完成定义

#14 是功能 Issue，本文合并只完成编码前设计，不自动关闭 Issue。最终关闭必须同时满足：

1. 真实实现替代 `src/smoke-connect.ts`。
2. 生命周期、快照、DTO、重连和清理具有自动单元/contract 测试。
3. Paper 1.21.1 完成 connect、death/respawn、dimension、server restart 和 stop 验收。
4. #24 能通过 ProtocolObservationSource 接收实体、方块和声音 DTO。
5. 原始 Mineflayer 对象和敏感字段未越过 Backend 边界。
6. CI 不再使用占位 `No tests configured yet`。
