---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_pr: 66
source_commit: c31a814
---

# 声音与客户端生命周期信息模块设计

> 原始状态声明（PR 未合并）：v0.2 实现就绪设计
>
> 日期：2026-07-14
>
> 对应 Issue：[#59](https://github.com/spojchil/mineintent/issues/59)
>
> 上游：[Information Runtime](../../architecture/information-runtime.md)、[合法信息接口](../../architecture/information-access-and-ui.md)、[认知感知模型](../../architecture/cognitive-perception.md)、[同伴运行时](../../architecture/companion-runtime.md)
>
> 协作边界：[#54 UI Context](https://github.com/spojchil/mineintent/issues/54)、[#34 第一人称视觉](https://github.com/spojchil/mineintent/issues/34)、[#57 验收矩阵](https://github.com/spojchil/mineintent/issues/57)
>
> 共享基础设施前置：[#63 按 Provider scopeDependencies 绑定分页 Cursor](https://github.com/spojchil/mineintent/issues/63)

## 1. 决定

v0.2 交付两个彼此独立、统一受 `InformationRuntime` 治理的接口：

- `sound_information` 读取当前未过期的听觉认知投影；
- `lifecycle_information` 读取当前连接、登录、出生、死亡/重生、世界/维度、资源包和断线状态，以及当前 epoch 内的有界事件序列。

两者不共享一个大快照。Driver 先把 Mineflayer 回调或协议包转换成最小内部信号，投影器再建立合法认知状态，Provider 最后只读取安全投影：

```text
Mineflayer / protocol callbacks
  ├── normalized sound candidates（内部可含计算所需坐标）
  └── normalized lifecycle signals（不含 Bot/raw packet）
                 │
                 ▼
       projection-owned reducers
  ├── SoundProjectionEngine
  └── SessionLifecycleCoordinator
                 │ session scope slice
                 ▼
 composition-owned InformationScopeCoordinator
   + UiContextProjectionStore 的 ui/screen slice
                 │ full scope / ordered invalidation
                 ▼
       narrow read-only source ports
  ├── SoundInformationSource
  └── LifecycleInformationSource
                 │
                 ▼
  SoundProvider / LifecycleProvider
                 │
                 ▼
          InformationRuntime
```

关键决定：

1. 收到合法声音事件是默认的可听证据。不能复用视觉 DDA，以“墙挡住视线”为理由删除墙后声音。
2. 精确声源坐标、packet sound ID、raw volume/pitch 和 Mineflayer 对象只存在于 Driver/声音投影内部；安全投影只保留粗方向、距离带、响度带和不确定性。
3. 墙体是否产生与原版客户端等价的听觉衰减必须通过 Paper 与真人对照校准。v0.2 默认策略不做墙体拒绝或视觉遮挡惩罚。
4. `connectionEpoch` 只在新的连接尝试开始时增加。死亡、重生、同连接内的维度变化和资源包状态变化不创建新 epoch。
5. 死亡/重生通过清空当前声音、提升两个 Provider 的 `informationRevision`、TTL 和签发源 revision 使旧 cursor/ref 失效，不伪造 `connection_changed`。
6. Provider 不互相调用，也不读取 `MinecraftSnapshotV1`。#54、#34 与本模块通过共享只读投影端口协作，而不是递归调用 `information`。

### 1.1 设计权威关系

- [Information Runtime](../../architecture/information-runtime.md) 是 Catalog/Help/Read、Provider、scope、sourceKinds、Ref/Cursor Store 与 Tool Session 的公共契约权威；本设计不得另建第二套运行时协议。
- 本文是 v0.2 `sound_information`、`lifecycle_information`、听觉空间化策略和生命周期失效顺序的实现权威。
- [认知感知模型](../../architecture/cognitive-perception.md) 继续定义声音作为第三类外界感知、语义类别、跨模态证据关联和认知事件；其中 v0.1 草案“用视觉式粗射线让墙体降低声音 certainty/扩大距离带”与本文冲突的部分，由本文取代。完成独立听觉实测前，不启用该视觉遮挡推断。
- [同伴运行时](../../architecture/companion-runtime.md) 继续拥有 Presence/Recovery 对 lifecycle 领域事件的归约；它是安全投影的消费者，不是连接事实或 Provider revision 的所有者。

后续应在 #59 实现合并时同步旧文档的冲突段落；在此之前，代码审查以本节的权威顺序为准。

### 1.2 分页基础设施前置

当前 `InformationCursorStore` 会无条件复制签发时所有非空 world、dimension 和 screen scope，且没有保存 `uiRevision`。因此本文的分页契约是 #63 完成后的目标行为，不是当前实现能力：

- Runtime 必须把 Registry 中密封 Provider definition 的 `scopeDependencies` 传给 Cursor Store；
- process session 始终绑定；connection、world、dimension、ui、screen 只按 Provider 声明绑定；
- `sound_information` 与 `lifecycle_information` 都不声明 `ui` 或 `screen`，所以打开/关闭普通 screen、screen revision 或 UI overlay 变化不能使其 cursor 失效；
- principal、grant、audience、interface、fields、limit、selector shape、`informationRevision`、TTL、一次性消费和容量约束保持不变；
- Provider 不得自行构造 cursor、把 scope 塞进 page state，或用手工比较绕过 #63；
- selector 若未来加入，仍先由 RefStore 按 selector 自己的 principal/epoch/world/screen/签发源 revision 规则解析；Cursor 的依赖裁剪不能复制、替代或放宽 selector 约束。

在 #63 合并前可以实现无分页的投影、Provider 字段与契约测试，但不能合并 sound/lifecycle 的生产分页路径。

## 2. 范围与非目标

### 2.1 本模块负责

- 1.21.1 声音注册表到认知类别的版本化映射；
- 位置性和无位置声音的合法归一化；
- 相对方向、垂直提示、距离带、感知响度带和不确定性量化；
- 协议兼容双回调去重、语义聚合、TTL、容量与发布预算；
- 连接、登录/configuration、初始 spawn、死亡/重生、维度/世界切换、资源包和断线 reducer；
- `sound_information`、`lifecycle_information` 的字段、availability、revision、source 和分页；
- 与 Information scope、Ref/Cursor Store 和 Runtime invalidation 的一致提交；
- 单元、Provider 契约、Paper 和真人验收设计。

### 2.2 本模块不负责

- 像素渲染、真实音频播放、HRTF 或完整客户端混音器；
- 从声音直接判定具体实体、方块或玩家意图；
- 将脚步硬绑定为主要玩家，或把僵尸声音绑定 raw entity；
- 用声音坐标创建可执行目标、路线或方块位置；
- UI 主表面、resource-pack screen 控件和死亡 screen 内容；这些分别属于 #54 和 #56；
- 视觉 FOV、材质光学规则和 viewport observation；这些属于 #34；
- 保留旧 `MinecraftSnapshotV1`、`ProtocolObservationSource` 或 Backend 事件作为模型旁路。

## 3. 不变量

1. `packet received`、`audible cue`、`identified source` 和 `visible source` 是不同事实。
2. 声音包位置只能用于当前事件的相对听觉投影，不能出现在 Provider 值、selector、cursor、evidence、普通日志或错误消息中。
3. 无位置声音永远返回 `around/unknown`，不能从上一次同名位置性声音继承方向。
4. 声音注册名只能经版本化表映射为语义；不能拆字符串后把任意名称当成事实。
5. 收到墙后声音不会因为视觉不可见而被删除；听觉衰减策略不能调用 `ViewportProvider` 或视觉 `VisibilityService`。
6. 聚合只压缩已经通过 session、世界、范围与策略校验的可听 cue；被拒绝的 raw 包不能通过计数、revision 或 `truncated` 形成侧信道。
7. `informationRevision` 只因模型可见投影或 availability 改变而增加；raw 回调、重复包和被过滤候选不增加公开 revision。
8. lifecycle reducer 是 connection/avatar/world 语义及 session scope slice 的单一写者；composition-owned `InformationScopeCoordinator` 是完整 `InformationScopeSource` 的唯一写者，并把 session slice、#54 ui/screen slice 与 Runtime invalidation 按同一提交顺序原子合成。
9. 新连接尝试产生新 `connectionEpoch`；disconnect 本身、死亡/重生和维度切换保持当前 epoch。
10. disconnect、death、respawn transition、world/dimension change 立即清除当前声音；停止声不产生“声源已消失”的事实。
11. Lifecycle 的断线信息只返回玩家可见的净化消息和粗分类，不返回服务器地址、异常栈、系统路径或内部错误对象。
12. Provider 只消费自己的安全 source port；禁止 Provider 递归、raw Mineflayer/协议对象和 snapshot adapter。

## 4. 模块边界与端口

### 4.1 Driver 到声音投影的内部候选

下面的 DTO 只允许位于 Driver 与 `src/perception/sounds/` 之间。它不是 Information contract，也不得从 `src/perception/index.ts` 导出给认知消费者：

```ts
interface SoundCandidateEnvelope {
  candidateId: string
  processSessionId: string
  connectionEpoch: number
  worldId: string
  dimension: string
  occurredAt: string
  listener: {
    position: { x: number; y: number; z: number }
    yaw: number
  }
  sound:
    | {
        spatial: 'positioned'
        sourcePosition: { x: number; y: number; z: number }
        registryName?: string
        numericId?: number
        protocolCategory?: string
        volume: number
        pitch: number
      }
    | {
        spatial: 'non_positional'
        registryName?: string
        numericId?: number
        protocolCategory?: string
        volume: number
        pitch: number
      }
  protocolSource: 'named_sound_effect' | 'sound_effect' | 'non_positional_sound'
}

interface SoundCandidateSource {
  subscribe(listener: (candidate: Readonly<SoundCandidateEnvelope>) => void): Unsubscribe
}
```

Driver 在边界上拒绝非有限位置/volume/pitch、未知当前 epoch、缺失 listener pose 和错误世界/维度。`candidateId` 只供 privileged trace；安全投影会生成新的 opaque evidence ID。

### 4.2 安全声音投影

```ts
type HorizontalSoundDirection =
  | 'front' | 'front_left' | 'left' | 'back_left'
  | 'back' | 'back_right' | 'right' | 'front_right'
  | 'around' | 'unknown'

interface AudibleSoundCueV1 {
  cueId: string
  category:
    | 'footstep'
    | 'block_break'
    | 'block_place'
    | 'door_or_container'
    | 'entity_vocalization'
    | 'combat'
    | 'projectile'
    | 'explosion'
    | 'weather'
    | 'ambient'
    | 'music'
    | 'unknown'
  semanticHint?: string
  direction: HorizontalSoundDirection
  vertical: 'above' | 'level' | 'below' | 'unknown'
  distance: 'near' | 'medium' | 'far' | 'unknown'
  loudness: 'quiet' | 'normal' | 'loud' | 'very_loud' | 'unknown'
  uncertainty: {
    overall: 'low' | 'medium' | 'high'
    reasons: Array<
      | 'non_positional'
      | 'weak_signal'
      | 'direction_boundary'
      | 'range_boundary'
      | 'ambiguous_source'
      | 'spatial_model_uncalibrated'
    >
  }
  pattern: 'single' | 'repeated' | 'continuous'
  countBand: 'one' | 'few' | 'several' | 'many'
  window: { startedAt: string; endedAt: string }
  validUntil: string
}

type SoundProjectionSnapshot =
  | {
      available: false
      reason: 'not_connected' | 'not_currently_displayed'
      informationRevision: number
      sourceRevision: number
      observedAt: string
    }
  | {
      available: true
      informationRevision: number
      sourceRevision: number
      observedAt: string
      cues: readonly Readonly<AudibleSoundCueV1>[]
      coverage: {
        windowStartedAt: string
        windowEndedAt: string
        truncated: boolean
        policyRevision: string
      }
    }

interface SoundInformationSource {
  capture(): Readonly<SoundProjectionSnapshot>
}
```

此 source port 已经没有坐标、raw ID、volume、pitch 或 raw source handle。`sourceRevision` 只跟随安全投影提交，不暴露被拒绝候选数量。

### 4.3 Lifecycle 内部信号与安全投影

Driver Adapter 发布小型判别联合，而不是把 Backend event 或 Bot 交给 reducer：

```ts
type LifecycleSignal =
  | { type: 'connection_attempt_started'; epoch: number; occurredAt: string }
  | { type: 'transport_connected'; epoch: number; occurredAt: string }
  | { type: 'login_completed'; epoch: number; occurredAt: string }
  | { type: 'configuration_entered'; epoch: number; occurredAt: string }
  | { type: 'initial_spawn_started'; epoch: number; occurredAt: string }
  | { type: 'play_ready'; epoch: number; worldId: string; dimension: string; occurredAt: string }
  | { type: 'died'; epoch: number; occurredAt: string }
  | { type: 'respawn_started'; epoch: number; fromDimension: string; occurredAt: string }
  | { type: 'respawned'; epoch: number; worldId: string; dimension: string; occurredAt: string }
  | { type: 'dimension_transition_started'; epoch: number; from: string; occurredAt: string }
  | { type: 'dimension_changed'; epoch: number; from: string; to: string; occurredAt: string }
  | { type: 'world_changed'; epoch: number; from?: string; to: string; dimension: string; occurredAt: string }
  | { type: 'resource_pack_requested'; epoch: number; required?: boolean; occurredAt: string }
  | { type: 'resource_pack_status_changed'; epoch: number; status: ResourcePackStatus; occurredAt: string }
  | { type: 'connection_closed'; epoch: number; reason: SafeDisconnectReason; occurredAt: string }
  | { type: 'reconnect_scheduled'; epoch: number; retryAt: string; occurredAt: string }
  | { type: 'faulted'; epoch: number; category: LifecycleFaultCategory; occurredAt: string }
  | { type: 'stopped'; epoch: number; occurredAt: string }
```

其中资源包 URL、hash、raw chat component、异常对象和 endpoint 不属于 `LifecycleSignal` 的安全部分。Driver 可在 privileged telemetry 保存它们，但进入 reducer 前必须先转换成 `required?`、状态和净化后的玩家可见断线理由。

```ts
type LifecycleConnectionPhase =
  | 'disconnected'
  | 'connecting'
  | 'logging_in'
  | 'configuration'
  | 'spawning'
  | 'play'
  | 'reconnect_wait'
  | 'stopping'
  | 'faulted'

type ResourcePackStatus =
  | 'none'
  | 'requested'
  | 'accepted'
  | 'declined'
  | 'downloading'
  | 'applied'
  | 'failed'

interface LifecycleProjectionSnapshot {
  informationRevision: number
  sourceRevision: number
  observedAt: string
  processSessionId: string
  connectionEpoch: number
  connection: {
    phase: LifecycleConnectionPhase
    reconnectAt?: string
    disconnectReason?: SafeDisconnectReason
  }
  avatar: {
    state: 'not_spawned' | 'alive' | 'dead' | 'respawning'
    lastSpawnedAt?: string
    diedAt?: string
  }
  world:
    | { state: 'unavailable' | 'transitioning' }
    | { state: 'current'; worldId: string; dimension: string }
  resourcePack: {
    status: ResourcePackStatus
    required?: boolean
    updatedAt?: string
  }
  recentEvents: readonly Readonly<LifecyclePublicEventV1>[]
}

interface LifecycleInformationSource {
  capture(): Readonly<LifecycleProjectionSnapshot>
}
```

`SessionLifecycleCoordinator` 只拥有 lifecycle projection 与内部 `SessionScopeSlice {processSessionId,connectionState,connectionEpoch,worldId?,dimension?}`。#54 的 `UiContextProjectionStore` 只拥有 ui/screen slice；`SoundProjectionEngine` 只拥有声音清理/投影。composition-owned `InformationScopeCoordinator` 是完整 `InformationScopeSource` 的唯一写者，但**不是**这些 slice/projection 的第二写者：它只能接受各 owner 已 stage 的 immutable candidate/ack，不能自行清空、补值或修改 UI、avatar、lifecycle 或 sound。

每个 lifecycle-triggered transition 分配内部 `transitionId` 和单调 `sessionScopeRevision`。跨模块屏障使用以下 envelope：

```ts
interface CompositionTransitionEnvelopeV1 {
  transitionId: string
  session: Readonly<SessionScopeSliceCandidate>       // lifecycle owner
  ui: Readonly<UiScopeSliceCandidate & {
    basedOnSessionScopeRevision: number
    transitionId: string
  }>                                                  // #54 owner
  acknowledgements: Readonly<{
    soundClearedThroughTransitionId: string
    viewportClearedThroughTransitionId?: string
  }>
  invalidations: readonly InformationInvalidationEvent[]
}
```

composition transition barrier 只收集 candidate/ack；它和 scope coordinator 都不能修改 candidate。验证 transitionId 全相等、UI `basedOnSessionScopeRevision` 精确匹配、所需清理 ack 齐全且各 revision 未倒退后，barrier 进入不可重入的同步 commit section：

1. 调用每个 owner 的 commit callback 安装 staged immutable projection，但不通知 subscriber；写权限仍在 owner；
2. scope coordinator 仅以 session/UI candidates 安装完整 `InformationScopeSnapshot`；
3. 执行 envelope 中的 Ref/Cursor invalidation；
4. 确认所有 store/scope 均已可见后退出 commit section，再按固定顺序释放各 projection owner 的 subscriber notification。

同步 commit section 内禁止 Runtime Read/capture、reducer callback 和 subscriber 重入，因此外部只能看到“全部旧”或“全部新”，不会观察到新 projection + 旧 scope 或新 scope + 旧 projection。`InformationScopeSource` v1 只有 `capture()`，本文不发明 scope subscriber；若未来增加，必须先升级公共端口，并把其 notification 同样延迟到步骤 4。任何缺失/错配都 fail closed，不安装任何 candidate。内部 transition/revision/ack 不进入模型结果或公共 scope。

## 5. 声音投影算法

### 5.1 处理流水线

```text
candidate
→ process session / epoch / world / dimension 校验
→ 版本化 SoundSemanticPolicy 查表
→ 当前虚拟听觉设置与合理最大范围校验
→ 相对向量与 listener yaw 量化
→ distance / loudness / uncertainty 分带
→ 兼容回调去重
→ 500 ms 语义窗口聚合
→ TTL、容量和发布预算
→ safe SoundProjectionSnapshot
```

事件已由服务器发送并不意味着可无限距离接受。每条策略包含 `maxAudibleRange`、类别、语义提示、位置性规则和基础增益；异常插件声音仍受版本化上限。策略未识别时可发布 `unknown`，但不能把 registry 字符串自行解释为实体或方块事实。

v0.2 固定使用系统级 `java-default-audibility:1` 虚拟听觉 profile：master 与普通声类启用，基础 gain 为 1。它描述 AI 客户端自己的听觉设置，不读取主要玩家的本地选项，也不能由人格或模型临时修改。将来若允许 operator 在进程启动时配置 profile，变化必须清空声音投影、提升其公开 revision，并把 profile revision 反映在 `coverage.policyRevision`。

### 5.2 方向、距离与响度

- 位置性声音使用事件时刻 listener pose，而不是 Read 时刻姿态；先求相对向量，再以 listener yaw 旋转到身体参考系。
- 水平角分成八个 45° sector。位于边界校准带内时选择确定性 sector，同时增加 `direction_boundary`。
- 垂直方向以相对高度和仰角的版本化阈值分成 `above/level/below`，不用返回方块差值。
- 距离带由 `distance / maxAudibleRange` 和声类衰减共同确定，不返回 block 数。初始阈值必须经 Paper fixture 校准，不能作为人格配置。
- 感知响度由 raw volume、声类增益和距离衰减计算，再量化为五档；raw 数值和 pitch 不进入安全投影。
- 非位置性声音固定为 `around + unknown vertical + unknown distance`。响度仍可在有可靠混音语义时量化，否则为 `unknown`。

边界附近的分带必须确定性且保留不确定性；不能为了看似精确而输出小数角、距离或音量。

### 5.3 墙后、遮挡和无位置声音

原版客户端收到位置性声音后会进行空间化；这与视觉射线能否到达声源不是同一件事。因此 v0.2 基线是：

- 只要事件通过 session、范围和输入校验，墙体不使 cue 消失；
- 不调用视觉 DDA，不读取 `VisibilityProof`，也不把视觉材质的 opaque/transparent 当成声学规则；
- 墙后 Paper 场景必须正向断言“仍然有 cue”，并与同距离无遮挡场景对照方向、距离和响度；
- 若真人对照证明目标客户端版本确有可重复的听觉衰减，再新增独立、版本化的 `AuditorySpatialPolicy`；启用前默认 `spatial_model_uncalibrated`，不得推断具体墙体或遮挡物；
- 未加载区块不用于探测墙体，不能因声学计算反向泄露隐藏地形。

无位置音乐、天气或全局事件没有空间事实。即使协议层为了 API 兼容提供了虚拟坐标，Adapter 也必须按协议语义标成 `non_positional`，不能伪造方向。

### 5.4 去重、聚合和 TTL

分两层去重：

1. Driver compatibility dedupe：相同 packet 通过 named/hardcoded 两个回调重复出现时，在极短窗口内按内部包事实去重；该 fingerprint 不外露。
2. Projection aggregation：按 `category + semanticHint + direction + vertical + distance + loudness` 在默认 500 ms 窗口合并。

规则：

- 单个窗口的原始数量只映射为 `one/few/several/many`；不返回包计数；
- 一个连续聚合最长 3 秒，之后关闭旧 cue 并开始新 cue；
- 单次 cue 默认 TTL 2 秒，repeated/continuous 默认最长 5 秒；
- cue 到期只从当前投影移除，不发布“声源消失”；
- 同一安全 cue 内容与有效期未发生可见变化时不提升 `informationRevision`；
- 输出按 `window.endedAt`、类别和 `cueId` 确定性排序，保证重放一致。

### 5.5 预算与过载

初始策略配置必须有硬上限：

| 项 | 初始上限 | 超限行为 |
|---|---:|---|
| raw candidate ring | 512 | 丢弃最旧低优先候选，仅 privileged telemetry 计数 |
| 每秒候选处理 | 256 | 保留 combat/explosion/主要玩家相关候选，其他 coalesce |
| 同时有效 cue | 64 | 先移除已过期，再按 salience 丢弃低优先 cue |
| 单窗口聚合时长 | 3 s | 关闭旧 cue，开启新 cue |
| 单次 Read cue | 默认 8，最大 16 | Runtime 分页，不能导出 raw 历史 |
| Provider 结果 | 32 KiB | Runtime 拒绝越界结果 |

`coverage.truncated` 只在已经合法投影的可听 cues 因公开预算被省略时为 true。raw 无效包、错误世界事件和兼容重复回调不得改变该字段。

## 6. Lifecycle 状态机

### 6.1 连接主状态机

```text
disconnected
  → connecting                 新连接尝试，connectionEpoch + 1
  → logging_in
  → configuration
  → spawning
  → play

play/configuration/spawning
  → disconnected              close/kick/network end；epoch 不变
  → reconnect_wait
  → connecting                 下一尝试才 connectionEpoch + 1

任意非 stopped
  → stopping → disconnected
  → faulted
```

映射到公共 `InformationScopeSnapshot.connectionState`：

| Lifecycle phase | Scope connectionState |
|---|---|
| `disconnected/reconnect_wait/stopping/faulted` | `disconnected` |
| `connecting/logging_in` | `connecting` |
| `configuration/spawning` | `configuration` |
| `play`（含 death/respawn 的独立 avatar 子状态） | `play` |

`lifecycle_information.connection_state` 在所有 phase 都可读。其他 Provider 可以在 `play` 之外返回 `not_connected` 或 `not_currently_displayed`，不能读取旧快照补齐。

### 6.2 Spawn、死亡和重生

Avatar 子状态：

```text
not_spawned → alive            initial play_ready
alive → dead                   death
dead → respawning              respawn_started
respawning → alive             respawned + coherent world state
```

- initial spawn 与 respawn 是不同事件；不能把每次 Mineflayer `spawn` 回调都解释为重生。
- death/respawn 保持当前 `connectionEpoch`。
- death 时各 owner 先以同一 transitionId stage：SoundProjectionEngine 的 cleared projection/ack、SessionLifecycleCoordinator 的 `avatar=dead` lifecycle candidate 与 session slice candidate、#54 的 death-screen/transition UI candidate。barrier 验证配对后在同步 commit section 中先由各 owner 安装 projection（不通知），再由 scope coordinator 安装完整 scope并执行 screen invalidation，最后释放 owner notifications。avatar 不属于 `SessionScopeSlice`，scope coordinator 不读取或写入它。
- respawn 只有在自身实体、世界和维度已经协调一致后才进入 `alive`；不以单个早到回调发布 ready。
- 旧声音 cursor 因 sound `informationRevision` 改变失效；旧 lifecycle cursor 因 lifecycle revision 改变失效。无需发送虚假的 connection invalidation。

### 6.3 世界与维度

同一连接中的维度变化采用两阶段提交：

```text
dimension_transition_started
→ lifecycle stage world=transitioning（尚不通知 subscriber）
→ Sound/Viewport owner stage 清空并给出 transition ack
→ lifecycle stage 清空 worldId/dimension 的 session slice
→ #54 stage `mainSurface=none(reason: transition)`，并绑定相同 transitionId/sessionScopeRevision
→ barrier 同步安装 owner projections → 完整 Scope → `world_changed(undefined)`，随后释放 notifications
→ 新世界状态协调完成
→ lifecycle 与 #54 为 ready transition stage 新 worldId/dimension 和匹配 UI candidate
→ barrier 同步安装 lifecycle.world=current/UI/空观察 projections → 新完整 scope → Runtime.invalidate(world_changed(new scope))
→ 退出 commit section 后释放各 owner notifications
```

- dimension/world 改变不增加 `connectionEpoch`，但改变 Information scope 并提升相关 Provider revisions；
- transition 期间 sound fields 返回 `not_currently_displayed`；
- 旧世界的 cue、cursor 和 ref 不得迁移到新世界；
- 仅 chunk load/unload 不构成 world change，也不生成 lifecycle event。

### 6.4 Resource pack 子状态机

```text
none → requested
requested → accepted → downloading → applied
requested → declined
accepted/downloading → failed
任意状态 → none               新 connection epoch 或明确卸载
```

- `required` 只有在协议/客户端行为向玩家可知时才公开；未知就省略，而不是猜测 false。
- URL、hash、下载路径、HTTP 错误和认证信息永不进入普通 Provider。
- resource-pack prompt 是否当前显示由 #54 `ui_context` 表达；标题、提示文本与按钮属于 #56 `current_screen_information`。
- 生命周期只表达请求和处理状态，不模拟点击接受/拒绝，也不自动产生动作。
- 同一次请求的乱序/重复 status 由 reducer 幂等处理；非法逆向转换进入 privileged diagnostics，不污染公开事件。

### 6.5 断线和重连

```ts
interface SafeDisconnectReason {
  category:
    | 'deliberate'
    | 'server_shutdown'
    | 'kicked'
    | 'authentication'
    | 'network'
    | 'timeout'
    | 'unsupported_version'
    | 'unknown'
  displayMessage?: string
}
```

`displayMessage` 只允许来自玩家客户端本可看到的 server kick/disconnect 文本，展平格式、移除控制字符并限制 500 字符。内部 exception message、host/port、stack、token 和路径不能借 `unknown` 类别回退输出。

disconnect 提交顺序：

1. 停止接受当前 epoch 的新 sound candidates，并为 transition stage cleared sound projection/ack；
2. lifecycle stage `connection_closed`、安全原因、disconnected/reconnect_wait projection 与清空 world/dimension 的 session candidate；
3. #54 stage 绑定同一 transitionId/sessionScopeRevision 的 screen 清理 candidate；
4. composition barrier 校验 candidates/ack，在同步 commit section 中依次安装 owner projections、完整 scope并执行 `world_changed(undefined)`；退出后释放 owner notifications；
5. 结束当前 Tool Session/grant；同 epoch 的旧 ref/cursor 由签发源 revision、availability 和 grant 结束拒绝，不发送无效的同 epoch `connection_changed`；
6. 若重连，直到下一次 `connection_attempt_started` 才分配新 epoch，并以新 epoch 的 `connection_changed` 清理上一 epoch 引用。

## 7. Information Provider 契约

### 7.1 `sound_information`

```ts
interface SoundInformationValuesV1 {
  recent_cues: AudibleSoundCueV1[]
  coverage: {
    windowStartedAt: string
    windowEndedAt: string
    truncated: boolean
    policyRevision: string
  }
}
```

Provider 定义：

| 属性 | 值 |
|---|---|
| `id` | `sound_information` |
| `schemaRevision` | `sound-information:1` |
| `audiences` | `companion`, `controller` |
| `scopeDependencies` | `connection`, `world`, `dimension` |
| selectors | v0.2 不接受 selector |
| pagination | default 8，max 16；只分页当前安全 cue 集合；生产实现依赖 #63 |
| source kind | `sound_projection` |
| acquisition | `current_perception` |
| max fields / bytes / timeout | 2 / 32 KiB / 50 ms |

字段 Help：

| 字段 | precision | sourceKinds | availability 与说明 |
|---|---|---|---|
| `recent_cues` | `quantized` | `sound_projection` | 只含当前未过期、安全投影后的 cues；无 cue 时返回空数组，不代表世界绝对安静 |
| `coverage` | `inferred` | `sound_projection` | 当前投影窗口、策略版本和公开预算是否截断；不含 raw 丢包计数 |

availability：

- Scope 为 disconnected：`not_connected`；connecting/configuration：`not_currently_displayed`；
- death/respawn/world transition 或 listener pose 尚未可信：`not_currently_displayed`；
- 正常 play 且投影为空：`available`，`recent_cues: []`；
- Provider 每次只 capture 一次安全 snapshot，不能在 Read 中搜索世界或等待新声音。

Read 的 `validUntil` 取已返回 cue 的最早有效期；空集合初始使用 `observedAt + 500 ms`。`evidenceIds` 是当前返回 cues 的有界安全证据 ID 并集，不能解析回 raw packet。

在 #63 完成后，Cursor 始终绑定 process session，并按本 Provider 的 `connection/world/dimension` dependencies 绑定 connection epoch/state、world 和 dimension；它不绑定 ui/screen。普通 screen 开关不得误伤续页。principal、grant、interface、字段、limit 和 sound `informationRevision` 仍按公共契约绑定。任何新 cue、TTL 清理、death、world transition 或公开截断变化都会使旧页失效；cursor 不能成为声音历史订阅。

### 7.2 `lifecycle_information`

```ts
interface LifecycleInformationValuesV1 {
  connection_state: LifecycleProjectionSnapshot['connection']
  avatar_state: LifecycleProjectionSnapshot['avatar']
  world_context: LifecycleProjectionSnapshot['world']
  resource_pack_state: LifecycleProjectionSnapshot['resourcePack']
  recent_events: LifecyclePublicEventV1[]
}

interface LifecyclePublicEventV1 {
  eventId: string
  type:
    | 'connection_started'
    | 'transport_connected'
    | 'login_completed'
    | 'configuration_entered'
    | 'spawned'
    | 'died'
    | 'respawn_started'
    | 'respawned'
    | 'dimension_transition_started'
    | 'dimension_changed'
    | 'world_changed'
    | 'resource_pack_requested'
    | 'resource_pack_status_changed'
    | 'disconnected'
    | 'reconnect_scheduled'
    | 'faulted'
    | 'stopped'
  occurredAt: string
  detail?: {
    dimension?: string
    fromDimension?: string
    resourcePackStatus?: ResourcePackStatus
    disconnectReason?: SafeDisconnectReason
    reconnectAt?: string
  }
}
```

Provider 定义：

| 属性 | 值 |
|---|---|
| `id` | `lifecycle_information` |
| `schemaRevision` | `lifecycle-information:1` |
| `audiences` | `companion`, `controller` |
| `scopeDependencies` | `connection`, `world`, `dimension` |
| selectors | v0.2 不接受 selector |
| pagination | recent events default 16，max 32；生产实现依赖 #63 |
| source kind | `lifecycle_event` |
| acquisition | `immediate_client_state` |
| max fields / bytes / timeout | 5 / 32 KiB / 50 ms |

字段 Help：

| 字段 | precision | 当前 availability |
|---|---|---|
| `connection_state` | `displayed` | 始终可读，包括 disconnected/faulted |
| `avatar_state` | `displayed` | spawn 前或断线时 `not_connected` |
| `world_context` | `displayed` | 登录前 `not_connected`；切换中返回 `{state:'transitioning'}`，不用旧维度补齐 |
| `resource_pack_state` | `displayed` | 始终可读；无请求为 `none` |
| `recent_events` | `displayed` | 当前 process session、当前/刚关闭 epoch 的有界公开事件 |

所有字段的 `sourceKinds` 都只允许 `lifecycle_event`。`recent_events` 最多在内存保留当前 epoch 的 64 条公开事件；新的连接尝试开始时清空旧 epoch 事件。需要跨连接保留的经历应由 Domain Event/Memory 另行记录，不能通过 Information cursor 恢复。

在 #63 完成后，Lifecycle cursor 始终绑定 process session，并按本 Provider 的 `connection/world/dimension` dependencies 绑定相关 scope，不绑定 ui/screen；打开背包、聊天输入或其他普通 screen 不得使 lifecycle 续页失败。它仍绑定当前 `informationRevision`，任何公开状态或事件变化都会使旧页失效。v0.2 没有 lifecycle selector；将来若签发 ref，query 必须先通过 RefStore 独立校验 selector scope 与签发源 revision，死亡/重生不依赖伪造 connection epoch 才能失效。

### 7.3 Provider 实现约束

- Definition 的 Zod schema 必须使用 strict object；Runtime 采用解析后的 data 重建输出。
- Provider 只能返回请求字段，且实际 source kind 必须在每个字段的 `sourceKinds` 中。
- 正常 unavailable 不抛异常；scope 在 Read 前后改变时由 Runtime 返回 `scope_changed`。
- Provider 不订阅 Driver、不更新 reducer、不等待事件；只做 snapshot 到字段的纯转换。
- Provider 不发行包含坐标或 raw handle 的 ref，不把安全投影外的值塞入 evidence/error。
- `availability()` 和 `read()` 使用同一 projection revision 语义；不能直接复用 raw callback sequence。

## 8. Revision、epoch 与失效矩阵

| 事件 | connectionEpoch | sound revision / cues | lifecycle revision | Runtime invalidation |
|---|---:|---|---|---|
| 新连接尝试 | +1 | 清空并变 unavailable | phase/事件改变 | `connection_changed` |
| transport/login/configuration | 不变 | unavailable | phase/事件改变 | 无；scope race 由 connection dependency 处理 |
| initial spawn ready | 不变 | 开始新空投影 | avatar/world/事件改变 | world 首次建立时 `world_changed` |
| 普通合法声音 | 不变 | 可见 cue 改变才 +1 | 不变 | 无 |
| raw 重复/被拒绝声音 | 不变 | 不变 | 不变 | 无 |
| death | 不变 | 清空并 +1 | avatar/事件 +1 | screen 失效由 #54；非 connection invalidation |
| respawn started | 不变 | 保持空/不可用 | avatar/world +1 | 必要时 `screen_changed`，非 connection invalidation |
| respawn ready，同维度 | 不变 | 新空投影 +1 | avatar/world/事件 +1 | 依赖 source revision/TTL |
| dimension/world change | 不变 | 清空并 +1 | transitioning/current +1 | transition 开始时 `world_changed(undefined)`，ready 时提交新 scope |
| resource-pack 状态 | 不变 | 通常不变 | 状态/事件 +1 | resource-pack screen 由 #54 管理 |
| disconnect | 不变 | 清空并 unavailable | phase/原因/事件 +1 | `world_changed(undefined)`、screen/grant/source revision；不发送同 epoch `connection_changed` |
| process restart | 从新 process session 建立 | 不恢复 current cues | 不恢复旧 current projection | processSessionId 自然拒绝全部旧引用 |

RefStore 继续独立校验 selector 的 principal、grant、epoch、world、可选 screen 和签发源 revision；#63 后 Cursor Store 校验公共查询约束及 Provider 声明的相关 scope。本模块不额外实现第二 token 系统。

## 9. 与 #54 和 #34 的边界

### 9.1 #54 UI Context

`SessionLifecycleCoordinator` 向 #54 和 composition barrier 提供只读 lifecycle transition candidate/session-slice subscription；#54 生成显式 `basedOnSessionScopeRevision` 的 UI candidate：

- #54 根据连接 phase 决定 `mainSurface: none/world/screen` 和 `inputTarget`；
- death、resource-pack 和 disconnect screen 的存在与 instance/revision 由 #54 所有；
- 本模块只表达 death/resource-pack/disconnect 生命周期事实，不复制 screen title、控件或输入焦点；
- `subtitlesEnabled` 属于 UI overlay 设置。`sound_information` 表达虚拟玩家的当前听觉 cue，不从字幕文本反向制造声音；字幕内容若实现，属于 #56 HUD 信息；
- 两个 Provider 都读共享投影，不互相 Query。
- #54 不能写 connection/world/dimension，lifecycle 也不能写 ui/screen；只有 composition-owned `InformationScopeCoordinator` 合成并发布完整 scope。

### 9.2 #34 第一人称视觉

- #34 与声音投影可以共享 lifecycle 的只读 `SessionScopeSliceSource` 和事件时 listener pose source；需要完整 Information scope 的消费者只读 composition coordinator；
- #34 拥有 visual FOV、光学 DDA 和 viewport observation；#59 不调用这些 Provider/Service 判定声音是否存在；
- 声音与视觉的来源关联只能基于双方已经发布的 opaque observation/evidence ID，由后续认知关联器产生带置信度的关系；不得把声音坐标交给 viewport 搜索实体；
- 声音可支持“右侧似乎有僵尸”的低精度候选，不能生成 entity ref、block ref 或精确目标；
- world/dimension/death transition 由 lifecycle coordinator 提议 session slice，清理与发布顺序由 composition commit barrier 串行化，两个投影不能各自改写完整 scope。

### 9.3 Context、Companion 与 Domain Events

- Context 只通过 `InformationRuntime` 读取 cues/lifecycle，不订阅 Driver source；
- 显著声音可以发布 `perception.sound.observed`，但事件 payload 仍是安全 cue；
- lifecycle 可以发布领域事件供 Presence/Recovery 归约；Information Provider 不替代 Domain Event Journal；
- 普通 cue 轮询和每次 lifecycle Read 不写满 Journal；只有显著观察和状态转换进入领域事件；
- Memory 若保留“刚才听到爆炸”，必须引用安全 evidence；不能持久化声源坐标。

## 10. 文件布局与依赖方向

```text
src/
├── minecraft/driver/
│   ├── sound-candidate-adapter.ts
│   ├── lifecycle-signal-adapter.ts
│   └── resource-pack-adapter.ts
├── lifecycle/
│   ├── contracts.ts
│   ├── reducer.ts
│   ├── coordinator.ts
│   ├── projection.ts
│   └── reducer.test.ts
├── app/information/
│   ├── scope-coordinator.ts
│   └── scope-coordinator.test.ts
├── perception/sounds/
│   ├── contracts.ts
│   ├── sound-policy.ts
│   ├── normalizer.ts
│   ├── quantizer.ts
│   ├── deduplicator.ts
│   ├── aggregator.ts
│   ├── projection.ts
│   └── *.test.ts
└── information/
    ├── source-ports/
    │   ├── sound.ts
    │   └── lifecycle.ts
    └── providers/
        ├── sound.ts
        ├── sound.test.ts
        ├── lifecycle.ts
        └── lifecycle.test.ts
```

依赖方向：

```text
information/providers → information/source-ports
perception/sounds projection implements SoundInformationSource
lifecycle projection implements LifecycleInformationSource
minecraft/driver adapters → perception/lifecycle input ports
app composition wires concrete instances
```

禁止：

- `information/providers/*` 导入 `minecraft/*`、Mineflayer 或其他 Provider；
- `companion/context/models/memory` 导入声音候选或 lifecycle internal signal；
- 创建 `MinecraftSnapshotV1 → Sound/LifecycleProjection` adapter；
- 将旧 `BackendLifecyclePayload` 直接作为 Provider 值；
- 为兼容旧路径做 dual-write 或 Provider fallback。

## 11. 测试设计

### 11.1 声音单元测试

- yaw 旋转、八方向边界、垂直带和非位置声音；
- 类别/语义映射只来自锁定 1.21.1 policy；未知名称保持 unknown；
- 最大范围、距离带、响度带和阈值边界确定性；
- named/hardcoded 双回调只生成一个候选；
- 500 ms 聚合、3 秒切窗、count band 和 2/5 秒 TTL；
- wall fixture 在默认策略下不删除收到的 cue，也不调用 visual DDA；
- raw 坐标、numeric ID、volume、pitch、registry 原串和 raw source handle 不存在于安全 projection JSON；
- 错 epoch/world/dimension、非有限输入和缺失 listener pose 被拒绝，且不改变公开 revision/coverage；
- raw 过载不影响高 salience cues；只有公开预算裁剪才设置 truncated；
- 重放相同候选和时钟得到相同 cue 顺序与内容。

### 11.2 Lifecycle reducer 单元测试

- 正常 connecting → login → configuration → spawn → play；
- 重复/乱序 signal 幂等或进入 privileged diagnostic，不产生非法公开转换；
- disconnect 保持 epoch，下一 connection attempt 才 +1；
- death → respawning → alive 保持 epoch并清空声音；
- 维度两阶段 transition 不把旧 dimension 冒充 current；
- resource pack accepted/declined/applied/failed 状态机与新 epoch 清理；
- disconnect message 净化和长度上限；内部异常、endpoint 和 stack 不进入 projection；
- process restart 不恢复 current cues/recent lifecycle cursor；
- 相同 reducer 输入序列产生相同 projection revision 和公开事件顺序。
- session/UI/sound projection 分别只有一个写者；scope coordinator 拒绝倒退 revision，且不会用 lifecycle commit 覆盖 ui/screen、用 UI commit 覆盖 connection/world/dimension，或自行生成 clear/avatar 值。
- death、dimension-start/ready、disconnect 的 transitionId 或 basedOnSessionScopeRevision 不匹配、ack 缺失时整次 fail closed；匹配时顺序固定为 owner projection install（无通知）→ full scope install → invalidation → 退出同步段 → owner notifications，测试在每个可重入钩子断言外部只能看见全部旧或全部新。

### 11.3 Provider 契约与架构测试

两个 Provider 都运行公共契约套件，并额外验证：

- Catalog/Help 可发现全部字段、精度、sourceKinds 和当前 availability；
- 只返回请求字段，Zod strict 解析不会保留额外嵌套属性；
- 错误 source kind、额外字段、超限结果和 Provider throw 被 Runtime 阻断；
- sound/lifecycle cursor 不能跨 principal、grant、epoch、world、dimension 或 information revision；
- #63 回归：普通 screen/ui 变化不影响两个 Provider 的 cursor；声明的 connection/world/dimension 变化仍拒绝续页；
- 若未来 cursor 带 selector，先独立通过 RefStore 的 selector scope 与签发源 revision 校验，scopeDependencies 裁剪不能放宽它；
- 无 cursor 的普通 Read 中发生 death/respawn，完整 scope 即使保持 play/同 world/dimension，也因 Runtime 后验公开 `informationRevision` 不一致返回 `scope_changed`；续页返回 `invalid_page`，绝不包装旧 cue 或 `avatar_state:alive`；
- dimension/disconnect 的 scope 前后复核继续返回 `scope_changed`；后验检查只比较公开 revision，不因内部 provenance/source churn 拒绝结果；
- 无 cue 返回合法空数组，不返回“没有声音”的世界断言；
- architecture scan 禁止 Provider 导入 minecraft、其他 Provider、snapshot 或 raw contract；
- Context/Companion/Model 没有 direct Backend/snapshot/observation source 旁路。

### 11.4 Paper 1.21.1 场景

| 场景 | 刺激 | 断言 |
|---|---|---|
| 八方向 | 固定半径依次 `playsound` | direction 与 yaw 对齐，无坐标泄漏 |
| 距离与响度 | 同类声在多个距离/volume | 只返回稳定 bands，不返回数值 |
| 墙后声音 | 同距离无遮挡/石墙后触发 | 两者都产生 cue；差异只能来自经批准的听觉策略 |
| 高频脚步/挖掘 | 短时 burst | 聚合成 repeated/continuous，不逐包灌入模型 |
| 双回调 | 捕获 named + compatibility | 单一 cue |
| 无位置声音 | 插件/协议 fixture 发无位置事件 | around/unknown，绝不继承坐标 |
| 错世界/旧 epoch | 重连前后的延迟 fixture | 不进入新投影，公开 revision 无侧信道 |
| 初始连接 | 完整 login/config/spawn | lifecycle 状态顺序合法 |
| 死亡/重生 | `/kill` 后 respawn | epoch 不变，旧声音/cursor 失效，avatar 恢复 alive |
| 维度切换 | Portal 或命令切换 | transition 期间无旧 cue，新维度零复用 |
| 断线/重连 | kick/server restart | 原因净化；下一 attempt 新 epoch |
| 资源包 | Paper resource-pack fixture | 请求/status 可见，URL/hash 不可见，UI 由 #54 验证 |

Paper 服务器真相、精确坐标和 protocol trace 只作为测试 oracle，必须与模型可见 JSON 分轨保存。

### 11.5 真人验收

真人客户端和 Bot 置于相同姿态，对照记录：

- 前后左右、上下、近中远的主观方向与距离带；
- 同距离无遮挡与石墙后的实际可听差异，确认默认“墙不删除声音”；
- 爆炸、脚步、门、挖掘、实体叫声、天气和音乐的类别/响度；
- 高频声音聚合后是否仍保留自然可理解的节奏；
- death、respawn、Portal、resource-pack prompt 和 disconnect 时机；
- 模型结果中不存在精确声源、服务器地址、资源包 URL/hash 或内部异常。

校准只修改版本化 policy 与阈值，不改变权限边界。

## 12. 可观察性

普通 trace 只记录：

- interface/read ID、safe source revision、cue 数、aggregation/truncation 状态；
- lifecycle phase、公开事件类型、epoch/world/dimension revision；
- 失效原因、scope race、budget rejection 和 Provider contract failure。

Privileged diagnostics 可以记录：

- raw candidate 数、分类失败、兼容重复、range rejection；
- 量化前相对距离/角度和阈值命中；
- lifecycle signal 乱序、Mineflayer callback 顺序和资源包 adapter 状态；
- 内部 close/error 详情。

两类日志必须使用不同类型与输出通道。普通 trace 禁止坐标、volume/pitch、raw registry/numeric ID、endpoint、resource-pack URL/hash 和异常栈。

## 13. 实施切片

### S0：Lifecycle 公共会话底座

1. 定义 `LifecycleSignal`、纯 reducer、projection 和 source port。
2. 实现 connection/avatar/world 状态机与只读 `SessionScopeSliceSource`；不得实现第二个完整 `InformationScopeSource`。
3. 在 composition 层实现不可重入 transition barrier 与完整 scope 唯一写者；只接受 owner-staged 且 transition/revision 匹配的 candidates/ack，按 owner install（静默）→ full scope → invalidation → owner notification 顺序提交。
4. 实现 `LifecycleProvider`、Help/Read 和无分页字段契约测试；公共 Runtime 对所有 Read 做公开 revision 后验复核。
5. #63 合并后接入 recent-events cursor，并验证普通 screen/ui 变化不误伤续页。
6. 向 #54 提供 projection/subscription；不接 UI 实现。

### S1：声音安全投影

1. 定义 1.21.1 `SoundSemanticPolicy` 与内部 candidate adapter。
2. 实现方向/距离/响度/不确定性量化、双回调去重和聚合/TTL。
3. 实现 `SoundInformationSource`、`SoundProvider` 和泄漏断言。
4. death/disconnect/dimension 清理接入 Lifecycle coordinator。
5. #63 合并后接入 cue cursor，不在 Provider page state 中手工复制 scope。

### S2：资源包与完整生命周期

1. 调研并实现 1.21.1 configuration/resource-pack 协议适配。
2. 增加净化后的 disconnect 分类、reconnect 状态和 world transfer。
3. 与 #54 对齐 death/resource-pack/disconnect screen 失效顺序。

### S3：集成与校准

1. Paper 场景覆盖八方向、墙后、高频、无位置、死亡/重连/维度/资源包。
2. 真人客户端完成声音 bands 与事件顺序对照。
3. #57 纳入 Provider contract、非泄漏矩阵和架构扫描。
4. #58 composition 注册两个 Provider；不保留 snapshot fallback。

S0 与 S1 可在文件所有权隔离后并行；两条线的投影与无分页 Provider 不被 #63 阻塞，但生产分页接入必须等待 #63。S2 依赖 #54 的 UI lifecycle projection 契约；S3 依赖 #34/#54 的最小投影接线，但不依赖它们的 Provider Query。

## 14. 开放问题与实现前 spike

1. Mineflayer 1.21.1 对 configuration、resource-pack request/status 和无位置声音的公开事件覆盖不足时，Driver 是否需要最小协议 packet listener。结论不能改变“只输出安全 signal”的边界。
2. `soundEffectHeard` 与 `hardcodedSoundEffectHeard` 在锁定版本中的真实参数和重复时序，需要录制 fixture 固定 adapter test。
3. 墙体或水下环境是否对目标原版客户端产生稳定可复现的听觉差异。未完成真人/Paper 校准前不得启用视觉遮挡替代方案。
4. 跨服务器 transfer 是否能在同一 transport/epoch 内改变 `worldId`；若支持，必须由明确 world signal 驱动，不能从 dimension 名猜测。

开放问题只影响 adapter 和 policy 校准，不阻塞 Provider schema、epoch/revision 语义或非泄漏边界。

## 15. 完成定义

1. 模型能通过 Catalog → Help → Read 发现两个接口及字段、精度、sourceKinds 和 availability。
2. `sound_information` 只返回当前、量化、可过期的 cue；精确坐标、raw ID、volume/pitch 和 raw entity/block 永不进入模型。
3. 墙后收到的声音不会被视觉遮挡误删；无位置声音不虚构方向。
4. 高频声音有确定性去重、聚合、TTL 和预算，raw 事件流不会直接进入 Context。
5. Lifecycle 状态机覆盖连接、login/configuration、spawn、死亡/重生、世界/维度、资源包、断线和重连。
6. 新连接尝试才提升 `connectionEpoch`；death/respawn 通过 revision、TTL 和投影清理使旧状态失效。
7. world/dimension/connection race 会丢弃旧 Read，selector/cursor 不能跨 scope 或签发源 revision 使用。
8. #54/#34 只共享只读投影/端口，不发生 Provider 递归或认知旁路。
9. 单元、公共 Provider 契约、Paper 与真人验收同时覆盖正向可用性和 raw 非泄漏。
10. 代码中不存在 snapshot adapter、dual path 或 raw Mineflayer/协议对象到模型的路径。
11. #63 已完成；sound/lifecycle cursor 只绑定各自 `scopeDependencies`，不因无关 ui/screen 变化失效，且 selector 约束仍由 RefStore 独立强制。
