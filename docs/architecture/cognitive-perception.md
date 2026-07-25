---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# 实体、方块、声音与玩家行为的认知感知模型

> 2026-07-23 核对：本设计由 [PR #25](https://github.com/spojchil/maineintent/pull/25) 合并；当前代码只实现其中有限的被动视口、声音和 source-port 边界，不能把全文当作能力清单。对应 Issue：[#24](https://github.com/spojchil/maineintent/issues/24)。研究依据：[Minecraft 认知感知源码调研](../history/research-cognitive-perception.md)。上游：[目标系统设计](./target-system.md)、[领域事件](./domain-events-and-journal.md)、[同伴运行时](./companion-runtime.md)、[决策与上下文](./decision-contract-and-context.md)。

## 1. 目的

本文定义 MineIntent v0.1 从 Mineflayer Protocol State 到 Cognitive Observation 的实现级边界。外界感知输入严格分为实体、方块和声音三类；局部场景与玩家活动是由三类观察派生的认知结果，不是额外感官。身体状态、受伤和交互结果属于运行时反馈，也不伪装成外界感知。

系统使同伴能以一个身体在场的玩家视角理解世界，而不是把客户端缓存或寻路器知识当作自己的所见所闻。

设计追求“公平、可解释、连续且成本有界”，不追求像素级复刻 Minecraft 渲染器。宁可偶尔少发现普通目标，也不能稳定报告墙后矿物、不可见实体、未打开容器或跨世界缓存。

## 2. 范围与非目标

本文负责：

- Protocol State、Control View 与 Cognitive Observation 的代码边界。
- 观察统一信封和实体、方块、空间场景、玩家活动 schema。
- 声音协议事件的规范化、可听性、分类、聚合和来源关联。
- 虚拟第一人称视野、射线、光学遮挡和可见性置信度。
- 实体可见、遮挡、丢失、重新发现的时间状态机。
- 方块变化、资源发现、场景聚类与地点线索。
- 玩家行为证据窗口和不确定活动假设。
- 主动观察查询、调度预算、聚合、去重与失效。
- 领域事件、Context Package 和 Memory Candidate 的连接点。
- Paper 1.21.1 防泄漏及正向验收场景。

本文不负责：

- 屏幕像素、OCR、着色器或完整 Minecraft 渲染。
- 把所有视觉细节逐 tick 交给模型。
- 由感知层判断玩家最终意图、共同活动状态或社交含义。
- 由感知层执行转头、移动和交互动作。
- 让人格档案修改可见性、安全或事实边界。
- v0.1 的自由第三人称、旁观模式或跨模组视觉兼容。

## 3. 核心不变量

1. `loaded`、`tracked`、`pathfindable` 和 `visible` 是不同状态。
2. 只有带感知证据的内容能进入 Cognitive Observation。
3. 未加载数据是 `unknown`，不是空气、不可见或不存在。
4. `entityGone` 只表示协议跟踪结束，不能自动宣称死亡或看见离开。
5. 当前观察、最近观察、推断和记忆必须保留不同来源。
6. 玩家行为观察可以产生假设，不能直接决定玩家意图或活动已放弃。
7. Safety Control View 只能校验即将执行的单个局部动作；不能批量或远程探测、参与普通路线选择，结果也不能流入 Epistemic Map、语言、活动或记忆。
8. 驱动或 v0.1 兼容代码通过 `findBlocks` 得到的坐标不是认知发现，也不能直接成为 grounded referent；Controller 只能解析已经由合法观察/Grounding 选中的对象。
9. 维度切换、重连和世界切换立即清除当前可见状态。
10. 感知调度不得阻塞 Threat Supervisor、协议处理或动作取消。

## 4. 三层数据边界

```text
MineflayerBackend
└── Protocol State
    ├── bot.world / loaded chunks / raw block updates
    ├── bot.entities / packet metadata / movement
    ├── self state / chat / sound / interaction feedback
    └── connection / dimension / game mode
             │
             ├──────────────→ Safety Control View
             │                collision / fall prevention / immediate threat / protocol validity
             │
             └─ Perception Boundary
                candidate generation
                → FOV / range / modality
                → optical occlusion
                → temporal tracking
                → salience / aggregation
                → Cognitive Observation
                     ↓
                Epistemic Map / Event Hub / Companion Runtime / Context Composer / Memory candidates
```

### 4.1 代码约束

- `Bot`、Prismarine `World`、raw entity、raw block 和 raw chunk 类型只能存在于 `src/minecraft/driver/`。
- `src/perception/` 只读取 `ProtocolObservationSource` 和候选 DTO；`src/grounding/`、`src/behavior/`、`src/motor/`、`src/navigation/`、`src/actions/` 只使用版本化领域端口；原型 `src/skills/` 直接删除。
- `src/companion/`、`src/context/`、`src/models/`、`src/memory/`、`src/speech/` 只接收 Cognitive Observation、已验证事件和有来源记忆。
- 跨边界只能使用本文 schema、领域事件、动作结果或已验证 MemoryRecord。
- 通过 ESLint import restriction 和依赖图测试强制此边界；不建立旧目录适配器或第二入口。

### 4.2 信息权限矩阵

| 信息 | Protocol | Control | Cognitive |
|---|---:|---:|---:|
| 已加载区块完整方块 | 是 | 按需 | 否 |
| 墙后碰撞 | 是 | 仅对下一局部动作返回最小安全结果 | 否 |
| 未探索路线 | 是 | 禁止用于普通规划 | `unknown` |
| `findBlocks` 全量命中 | 是 | 仅驱动内部候选；不可成为 grounded target | 否 |
| 实体表精确坐标 | 是 | 仅当前准星编码/局部安全校验 | 仅可见对象的相对位置或最近估计 |
| 未打开容器内容 | 可能有缓存/历史 | 交互时 | 否 |
| 当前视野可见表面 | 可计算 | 可用 | 是 |
| 受伤与直接交互反馈 | 是 | 是 | 是 |
| 历史地点 | 否 | 可由动作使用 | 作为 memory，不冒充观察 |

## 5. 三类感知与统一信封

```text
外界感知输入
├── Entity Observation  实体存在、外观、运动与可见性连续性
├── Block Observation   方块表面、变化、资源与空间结构
└── Sound Observation   声音类别、方向、距离与重复模式

派生认知
├── Spatial Scene       由实体、方块和声音压缩出的局部场景
└── Player Activity     由实体、方块、声音与聊天上下文形成的行为假设

身体反馈（独立）
└── self state / damage / interaction result / verified action result
```

身体反馈可以触发注意、危险处理和决策，但不进入本文三类 `ObservationEnvelope`。例如“我受伤了”是权威自身事件；“右后方听到僵尸声”才是 Sound Observation。

### 5.1 协议版本

```ts
type PerceptionProtocol = 'mineintent.perception.v1'

type ExternalObservationKind = 'entity' | 'block' | 'sound'
type DerivedCognitiveKind = 'scene' | 'player_activity'
type PerceptionModality = 'visual' | 'auditory' | 'interaction' | 'inferred'

interface ObservationEnvelope<T> {
  protocol: PerceptionProtocol
  id: string
  kind: ExternalObservationKind | DerivedCognitiveKind
  worldId: string
  dimension: string
  sessionId: string
  observerEntityId: string
  observedAt: string
  validUntil: string
  modality: PerceptionModality
  certainty: number
  salience: number
  evidence: PerceptionEvidence[]
  content: T
}

interface PerceptionEvidence {
  id: string
  kind:
    | 'protocol_event'
    | 'visibility_test'
    | 'block_change'
    | 'sound_event'
    | 'interaction_result'
    | 'observation'
  occurredAt: string
  detail: string
}
```

- `certainty` 表示这项观察或推断受证据支持的程度，不改变权限。
- `salience` 只影响聚合与上下文选择，不表示事实更真实。
- `validUntil` 到期后内容可转为最近观察或 Memory，不再作为当前所见。
- 精确协议包正文不直接放入 envelope；evidence 只保存受控摘要和可追踪 ID。
- `entity`、`block`、`sound` 是基础观察；`scene` 和 `player_activity` 的 evidence 必须引用基础 observation ID。

### 5.2 通用空间值

```ts
interface Vec3Value { x: number; y: number; z: number }
interface BlockPosition { x: number; y: number; z: number }

type RelativePosition = [right: number, up: number, forward: number]

interface RelativeLocationSummary {
  direction: 'front' | 'front_left' | 'left' | 'back_left' | 'back' | 'back_right' | 'right' | 'front_right' | 'around' | 'unknown'
  vertical: 'above' | 'level' | 'below' | 'unknown'
  distanceBand: 'touching' | 'near' | 'medium' | 'far' | 'unknown'
}

interface RelativeBounds {
  center: RelativePosition
  sizeBand: 'small' | 'medium' | 'large'
}

type VisibilityProofSummary = Pick<VisibilityProof,
  'result' | 'sampledPoints' | 'visiblePoints' | 'accumulatedAttenuation'>
```

`RelativePosition` 是模型侧默认的空间表示。原点是该次 Read 捕获时玩家的脚部位置；`right/forward` 由当时身体 yaw 定义，`up` 始终与世界竖直轴一致，不随 pitch 旋转；实体使用脚部锚点，方块使用其方块坐标锚点。公开值默认量化到 0.5 格，并由包含它的 `readId + informationRevision + observedAt/validUntil` 绑定观察帧。转身、移动或来源 revision 改变后，新投影会使旧 ref 失效，不能把不同观察帧的相对坐标直接拼接成地图。

这种数值表示用于弥补纯方向词对几何关系描述不准的问题，同时避免持续传输图像。`RelativeLocationSummary`、`distanceBand` 等枚举可以作为低成本摘要保留，但不是几何事实的唯一载体。相对坐标只描述已通过感知边界的当前证据：模型用它比较和理解候选，用同项签发的 opaque context ref 选择对象；不得把相对坐标提交为 selector 或动作目标。

绝对坐标只在 driver 物理/协议编码、Perception 几何计算、由合法观察建立的 Epistemic Map 和已授权 Controller 的 scoped spatial resolution 内使用。Grounding 输出 opaque handle，普通模型上下文只使用合法 Information Read、`RelativePosition` 与 opaque context ref；Controller 可以为已选对象解析 `BlockPosition`，但不能搜索其他坐标或把实现值回流为认知事实。

## 6. 虚拟第一人称视口

Mineflayer 没有渲染相机。v0.1 以角色眼睛位置、身体 yaw 和 pitch 构造确定性虚拟视口：

```ts
interface PerceptionViewport {
  eye: Vec3Value
  yaw: number
  pitch: number
  focusedHorizontalFovDeg: number
  focusedVerticalFovDeg: number
  peripheralHorizontalFovDeg: number
  peripheralVerticalFovDeg: number
  nearOmnidirectionalRadius: number
  maxEntityRange: number
  maxBlockDetailRange: number
  maxSceneRange: number
}
```

v0.1 内部默认值：

| 参数 | 默认值 | 含义 |
|---|---:|---|
| focused FOV | 水平 90° / 垂直 70° | 可识别实体、方块和细节 |
| peripheral FOV | 水平 160° / 垂直 110° | 只发现显著移动、威胁或玩家存在 |
| 近身 360° | 2.5 blocks | 碰撞、直接交互和近身实体存在 |
| 实体最大视觉距离 | 64 blocks | 仍受大小、遮挡和显著性限制 |
| 方块细节距离 | 24 blocks | 单体方块名称和变化 |
| 场景粗略距离 | 64 blocks | 地形/树林等聚类，不提供隐藏细节 |

这些是系统调优配置，不是人格配置。服务端视距更小时自然受已加载数据限制。v0.1 无论用户客户端设置如何，都采用这一稳定身体视角，保证重放和测试一致。

## 7. 光学可见性

### 7.1 粗到细流程

```text
候选实体 / 方块 / 区域
→ 世界、维度、chunk loaded 校验
→ 距离与 near-field
→ focused / peripheral FOV 粗筛
→ 目标包围盒或表面采样
→ Prismarine DDA 穿越体素
→ VisionMaterialPolicy 累积遮挡
→ 形成 visible / partial / blocked / unknown 证明
```

FOV 只做候选裁剪，不能证明可见。射线遇到未加载区块返回 `unknown`，不能穿透继续。

### 7.2 视觉材质策略

碰撞 shape 不等于光学 shape。维护随 Minecraft 版本锁定的策略：

```ts
interface VisionMaterialRule {
  blockName: string
  mode: 'opaque' | 'partial' | 'transparent'
  attenuation: number
  useCollisionShapes: boolean
  opticalShapesOverride?: AabbValue[]
}
```

- `opaque`：有效 shape 交点后终止射线。
- `partial`：累积 attenuation，达到阈值后视为 blocked。
- `transparent`：记录穿过但不遮挡。
- 未知方块默认 opaque，防止新版或模组方块造成透视。
- 策略至少覆盖空气、普通实心方块、玻璃、树叶、栅栏、门、活板门、液体、火和植物。
- `block.transparent` 可作生成策略的输入，不能单独作为最终判断。

### 7.3 采样结果

```ts
interface VisibilityProof {
  result: 'visible' | 'partially_visible' | 'blocked' | 'unknown'
  sampledPoints: number
  visiblePoints: number
  nearestVisibleDistance?: number
  accumulatedAttenuation: number
  blockingBlock?: { name: string; position: BlockPosition }
  checkedAt: string
}
```

普通认知观察不暴露 `blockingBlock` 的隐藏名称；该字段只供调试。对同伴可以表达“被墙挡住了”，不能因为遮挡测试命中墙后目标而泄露目标本身。

## 8. 实体感知

### 8.1 Schema

```ts
interface EntityObservation {
  entityKey: string
  category: 'player' | 'hostile' | 'passive' | 'item' | 'projectile' | 'vehicle' | 'other'
  identity?: { playerId?: string; username?: string; entityName?: string }
  visibility: 'focused' | 'peripheral' | 'nearby' | 'occluded_recently' | 'lost'
  exposure: 'clear' | 'partial' | 'unknown'
  relativePosition: RelativePosition
  distanceBand: 'touching' | 'near' | 'medium' | 'far'
  motion: 'still' | 'approaching' | 'leaving' | 'crossing' | 'unknown'
  facing?: 'toward_observer' | 'away' | 'sideways' | 'unknown'
  heldItem?: string
  pose?: string
  threatHint?: 'none' | 'possible' | 'immediate'
  firstObservedAt: string
  lastVisuallyConfirmedAt?: string
}
```

模型默认不需要协议 entity ID 或绝对坐标。它获得由当前观察帧约束的量化 `relativePosition`，并用 opaque context ref 选择目标。`entityKey` 只在内部作为会话内稳定引用；玩家身份映射由 Backend 的已知玩家表提供，但只有可见、聊天或共享交互时进入观察。

### 8.2 包围盒多点采样

实体使用 width/height 构造 AABB，并采样：

- 头/上部中心。
- 胸部中心。
- 下部中心。
- 对大实体增加左右边缘点。

focused 观察至少一个点通过视觉证明；暴露比例决定 clear/partial。只命中极小边缘且距离较远时降低 certainty。peripheral 只提供类别、方向、运动和威胁提示，不提供手持物等细节。

### 8.3 时间状态机

```text
untracked
  → candidate
  → visible_focused | visible_peripheral | sensed_nearby
  → occluded_recently
  → recently_lost
  → unknown

occluded_recently / recently_lost
  → visible_*        重新发现
  → unknown          过期
```

内部 tracker：

```ts
interface EntityPercept {
  entityKey: string
  state: 'candidate' | 'visible_focused' | 'visible_peripheral' | 'sensed_nearby' | 'occluded_recently' | 'recently_lost' | 'unknown'
  lastKnownPosition?: Vec3Value
  lastVelocity?: Vec3Value
  lastConfirmedAt?: string
  uncertaintyRadius: number
  lostReason?: 'occluded' | 'out_of_fov' | 'out_of_range' | 'protocol_removed' | 'dimension_changed' | 'unknown'
}
```

- 遮挡或离开 FOV 后可短暂保留“刚才在那里”，但不继续读取 raw entity 精确位置。
- 估计位置只用最后确认速度短时外推，uncertainty radius 随时间增长。
- `entityGone` 将原因设为 protocol_removed；除非同时存在死亡/离开证据，否则对认知只产生 lost/unknown。
- 玩家聊天不自动使玩家身体可见，但可产生独立 shared chat 事件。
- 默认普通实体最近观察窗口 5 秒，主要玩家 10 秒，威胁由 Threat Supervisor 独立管理；窗口是调优项。

## 9. 方块感知

### 9.1 Schema

```ts
interface BlockObservation {
  observationType: 'surface' | 'change' | 'resource_cluster' | 'interaction_target' | 'landmark_clue'
  blockName?: string
  blockCategory: string
  position?: BlockPosition
  relativePosition: RelativePosition
  distanceBand: 'touching' | 'near' | 'medium' | 'far'
  visibleFaces?: Array<'top' | 'bottom' | 'north' | 'south' | 'west' | 'east'>
  change?: { fromCategory: string; toCategory: string; occurredAt: string }
  cluster?: { estimatedCountBand: 'one' | 'few' | 'several' | 'many'; extent: RelativeBounds }
  visibility: VisibilityProofSummary
}
```

只有 Grounding 后的内部交互计划确需精确定位时才保留绝对 `position`；给模型的普通场景使用量化相对坐标、方向/距离摘要、聚类和 opaque ref，在保留空间精度的同时限制上下文体积与越权查询。

### 9.2 候选生成

不能遍历全部 loaded blocks 并把匹配项当所见。v0.1 使用四种候选：

1. 视口射线扇：按固定角度网格采样第一可见表面。
2. 关注区域：对准星、玩家指向或当前 gaze 目标提高采样密度。
3. 事件候选：`blockUpdate`、挖掘、放置、门/容器交互产生位置候选，再做模态和可见性验证。
4. 驱动候选：底层可用全量查询生成待检查候选，但只有重新通过 Perception Boundary 后才能成为观察，并经 Grounding 后才可供 Behavior 选目标；Controller 只解析该已选目标的局部实现坐标。

### 9.3 方块变化

`blockUpdate` 形成认知变化必须满足至少一项：

- 更新前或更新后位置处于当前 focused/nearby 可见区域。
- 同伴是直接交互动作的执行者并收到验证结果。
- 有可定位的合理声音事件，且只生成粗略 auditory change，不泄露具体隐藏方块。
- 玩家行为证据窗口已在观察该位置。

否则只更新 Protocol State。加载/卸载 chunk 不产生 block observation。

### 9.4 资源和结构聚类

相邻同类可见表面聚成 `resource_cluster`，例如“前方有几棵橡树”。聚类只能包含各自通过可见证明的方块；不能借一个可见原木扩展到整棵隐藏树。v0.1 结构语义限于可确定性识别的类别：树林/树群、洞口、墙面、门、道路状地面、水体边缘、工作站和玩家明确命名地点线索。

## 10. 局部空间场景

```ts
interface SpatialScene {
  sceneId: string
  observerPoseRevision: number
  generatedAt: string
  sectors: Array<{
    direction: 'front' | 'front_left' | 'left' | 'back_left' | 'back' | 'back_right' | 'right' | 'front_right'
    traversability: 'open' | 'constrained' | 'blocked' | 'unknown'
    terrain: string[]
    notableObservationIds: string[]
  }>
  openings: Array<{ kind: 'doorway' | 'cave_mouth' | 'passage' | 'edge'; relativePosition: RelativePosition; evidenceIds: string[] }>
  resourceClusters: string[]
  visibleEntities: string[]
  audibleCues: string[]
  uncertaintyNotes: string[]
}
```

Scene Builder 只压缩已经产生的观察，不直接读取全量 world。后方 sector 通常来自近身感知或最近观察，必须标明非当前 focused 视觉；Context Composer 展示时不能统一写成“眼前”。

场景更新触发：

- 移动超过 2 blocks。
- yaw 改变超过 30° 或 pitch 改变超过 20°。
- 显著实体/方块变化。
- 显著声音开始或方向/类别变化。
- 主动观察完成。
- 最长 1 秒周期刷新。

## 11. 玩家行为信号

### 11.1 原始证据

只使用合理可观察的信号：

- 可见位置、速度、朝向和距离趋势。
- 可见手持物与装备变化。
- 手臂动作、姿态和交互动画。
- 同一可见区域内的方块变化。
- 掉落物出现/拾取的可见结果。
- 与玩家位置或行为时间相关、但未强行绑定来源的声音观察。
- 伤害、战斗和直接物品交付。
- 玩家聊天和共同活动上下文。

raw packet 中不可见的精确状态不能进入证据窗口。

### 11.2 Schema

```ts
interface PlayerActivitySignal {
  playerId: string
  hypothesis:
    | 'moving_toward'
    | 'moving_away'
    | 'waiting'
    | 'following'
    | 'gathering'
    | 'building'
    | 'fighting'
    | 'exploring'
    | 'offering_item'
    | 'unknown_activity'
  confidence: number
  window: { startedAt: string; endedAt: string }
  evidenceObservationIds: string[]
  alternatives: Array<{ hypothesis: string; confidence: number }>
  relationToActivity?: 'supports_alignment' | 'possible_drift' | 'unrelated' | 'unknown'
}
```

### 11.3 推断规则

- 单次挥手不能推出 gathering/building。
- gathering 至少需要面向/接近资源、重复手臂动作、相关方块消失或物品结果中的多个证据。
- building 至少需要持有可放置物、重复交互和新方块出现的组合。
- moving_away 只描述距离趋势，不等于离开共同活动。
- possible_drift 只触发 Companion Runtime 的 alignment uncertain 评估，不能直接 diverged/abandoned。
- 低于发布阈值的假设保留在短期窗口，不进入领域事件。
- 同一窗口可以保留替代解释，模型看到的是带不确定性的信号，不是假确定任务名。

默认窗口 1–8 秒，按活动类型滚动；相同 hypothesis 只在置信度跨阈值、方向改变或重要证据新增时发布。

## 12. 声音感知

声音是与实体和方块并列的第三种外界输入，不是视觉失败后的补丁。它可以让同伴在目标不可见时合理知道“附近有某种动静”，但其空间和来源精度必须低于协议包提供的精确坐标。

### 12.1 Backend 原始声音

#14 Backend 将 Mineflayer 的 `soundEffectHeard` / `hardcodedSoundEffectHeard` 规范化为：

```ts
interface ProtocolSoundPayload {
  soundName?: string
  soundId?: number
  category?: string
  sourcePosition: Vec3Value
  volume: number
  pitch: number
  protocolSource: 'named_sound_effect' | 'sound_effect'
}
```

- Backend 保留精确位置供距离计算和调试，不直接发送给模型。
- named 和兼容 hardcoded 回调可能指向同一声音；按发生时间、量化位置、volume、pitch 和名称/ID 去重。
- 非有限坐标、负 volume、错误维度或已结束 session 的事件拒绝。
- 收到协议事件并不自动证明 sound name 的自然语言解释正确；分类表随 Minecraft 版本锁定。

### 12.2 Sound Observation Schema

```ts
interface SoundObservation {
  soundKey: string
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
  relativeDirection: 'front' | 'front_left' | 'left' | 'back_left' | 'back' | 'back_right' | 'right' | 'front_right' | 'around' | 'unknown'
  verticalHint: 'above' | 'level' | 'below' | 'unknown'
  distanceBand: 'near' | 'medium' | 'far' | 'unknown'
  pattern: 'single' | 'repeated' | 'continuous'
  count: number
  window: { startedAt: string; endedAt: string }
  possibleSource?: {
    kind: 'entity' | 'block' | 'environment'
    entityKey?: string
    observationId?: string
    confidence: number
  }
}
```

`semanticHint` 只能来自版本化 sound registry 映射，例如 `entity.zombie.ambient` 可表达“像僵尸的声音”；不能由任意字符串拆分后直接生成事实。模型得到方向和距离带，不得到声源精确坐标。

### 12.3 可听性与遮挡

> **未协调修订：** 后来的 [PR #66 历史设计](../history/archive-2026-07-14-information/sound-and-lifecycle-information.md)指出：服务端已经选择并发送给客户端的声音，不应再用视觉 DDA 遮挡来丢弃或伪造衰减。该 PR 已关闭且未合并，因此不能静默取代本文的 accepted 规则；当前生产 Provider 也没有实现下面的遮挡推断。在新的听觉实测和 ADR/amendment 完成前，本小节不能直接当作 implementation-ready 规范。

```text
BackendEventEnvelope<ProtocolSoundPayload>
→ session / world / dimension 校验
→ 声音规则查表（类别、基础可听范围、是否位置性声音）
→ volume 修正的最大范围
→ 观察者距离与方向
→ 有界遮挡采样
→ certainty / distance band
→ 去重与时间聚合
→ SoundObservation
```

- 服务端已经决定是否发送声音，但仍应用合理最大距离，防止插件或异常包制造全图听觉。
- v0.1 从耳朵位置到声源做一条粗射线，累计 opaque/partial section；遮挡降低 certainty、扩大 distance band，不把普通墙视为绝对隔音。
- 爆炸、雷声等规则允许更远距离；脚步和交互声范围更短。
- 无位置或全局音乐/天气事件使用 `around/unknown`，不能虚构方向。
- 未加载区块不用于识别沿途方块；声音包本身仍可被听见，但遮挡结果为 unknown 并降低 certainty。

具体范围和衰减是版本化 `SoundPerceptionPolicy`，属于系统校准，不属于人格。

### 12.4 来源关联

声音名称可能暗示来源类别，但不能自动绑定一个具体实体或方块：

- 若同时存在位置、时间均相符的可见 Entity Observation，可建立带 confidence 的 entity 关联。
- 实体被遮挡时，明确物种声音可以创建新的实体候选或更新“可能仍在附近”，但不能读取 raw entity 精确轨迹。
- block break/place 声音可以创建方块区域候选；只有随后视觉或交互证据才能产生具体 Block Observation。
- 脚步可支持“有人/生物在右侧活动”，不能仅凭脚步确定是主要玩家。
- 多个可能来源同时存在时不强行选一个，保留 possible source 为空或较低 confidence。

### 12.5 聚合与失效

- 同类声音在相近方向、距离带和 500 ms 窗口合并，避免脚步/挖掘刷屏。
- 连续窗口最长 3 秒，之后发布更新而非无限延长。
- 爆炸、受击、门/容器和主要玩家相关声音具有较高 salience；音乐和重复 ambient 默认不触发模型。
- 单次 Sound Observation 默认当前有效 2 秒；repeated/continuous 最长 5 秒，之后只能作为最近听闻或 episode 证据。
- 声音停止不产生“声源消失”事实。

### 12.6 身体与交互反馈（不属于三类外界感知）

- 自身受伤、击退、着火、溺水属于 tactile/self-state，可立即进入认知。
- 动作验证可以证明自己挖掉、放置、拾取或交付了什么。
- 打开容器后获得的内容只在交互会话和后续有证据记忆中可用；容器关闭后不是当前视觉。
- 被其他实体碰撞不自动识别对方，除非近身、视觉或伤害来源证据支持。

## 13. 主动观察

```ts
interface PerceptionQuery {
  id: string
  scope:
    | { kind: 'current_viewport' }
    | { kind: 'grounded_referent'; handle: string }
  temporal:
    | { kind: 'once' }
    | { kind: 'while_intent_active'; embodiedIntentId: string }
  maxDurationMs: number
  requestedBy: 'behavior' | 'action' | 'system'
  correlationId: string
}
```

- Perception Query 只决定采样范围和时效，不解释玩家措辞、选择对象或改变身体。
- `grounded_referent` 必须由本轮 Grounding 签发并仍然有效；它可以指向任何有证据的语义对象，接口不按玩家、方块或位置分型。
- 若当前视口不覆盖 referent，Behavior Synthesizer 决定是否转头、转身、扫描、靠近或等待新观察；无法形成合法的信息获取计划时返回 `information_needed`，是否询问由 Companion 决策。Perception 不请求对象专用 gaze skill。
- `while_intent_active` 是与具身意图生命周期绑定的有时限采样，不是 `watch_player`/`track_player` 特例；意图取消、过期或完成时立即停止。
- query 仍受相同 FOV、遮挡、距离和 chunk 边界，不能成为透视 API。
- 失败返回 blocked/out_of_range/unknown，让同伴自然询问或靠近。

## 14. 调度与预算

### 14.1 内部调度

| 通道 | 默认频率/触发 | 输出 |
|---|---|---|
| Control threat | 最多 10 Hz | 仅 Control View，不直接进模型 |
| 主要玩家/当前威胁候选 | 4 Hz | 实体可见性和运动 |
| 普通附近实体 | 2 Hz | 进入/离开/显著变化 |
| 玩家活动窗口 | 2 Hz + 事件 | activity signal |
| 场景 | 最多 1 Hz + 姿态/事件阈值 | SpatialScene |
| 方块变化 | 事件驱动 | 经验证的 change observation |
| 声音 | 事件驱动 + 500 ms 聚合 | SoundObservation |
| 主动观察 | 有界请求 | 临时高密度结果 |

频率是上限；无候选或系统压力高时跳过普通扫描。所有时间值使用单调时钟调度，领域事件记录 wall-clock 时间。

### 14.2 每周期预算

配置至少包括：

- 最大候选实体数。
- 最大实体射线数。
- 最大方块射线数。
- 最大 scene observations。
- 每查询 deadline。
- raw sample ring buffer 大小。
- Event Hub 每类事件速率上限。

优先级：直接危险与主要玩家 > 当前 gaze/活动目标 > 显著变化 > 普通实体 > 环境装饰。预算耗尽产生 telemetry，不把“未检查”标成不可见。

## 15. 聚合、去重与失效

- 高频实体移动保存在内存 tracker，不逐条写 journal。
- visible 状态不变时只更新内存最后确认时间；方向带、距离带、运动或细节变化才发布新观察。
- scene 使用内容哈希；无实质变化不发布。
- 相同 block change 按位置和 state transition 去重。
- 相同声音按量化位置、类别、参数和时间窗去重，再按方向/距离带聚合。
- 相同 player hypothesis 在窗口内合并证据。
- observation 到期后从 Current Observation Projection 移除；需要保留的经历由 Memory System 另行写入。
- 进程重启不恢复“当前可见”；只恢复记忆，连接后重新观察。

## 16. 领域事件

v0.1 注册：

```text
perception.entity.observed
perception.entity.visibility_changed
perception.entity.lost
perception.block.observed
perception.block_change.observed
perception.sound.observed
perception.scene.updated
player.activity.signal_observed
```

事件 payload 引用 ObservationEnvelope 或其持久摘要：

- 普通重复 observation 默认 coalesced。
- 实体首次发现、威胁等级变化、主要玩家显著状态、重要资源/地点发现和 block change 为 journal required。
- scene 周期刷新通常只更新投影；只有显著摘要变化进入 journal。
- raw ray、所有采样点和隐藏 blocking block 只进短期调试附件，不进入模型事件。

事件 visibility 为 `perceived`；聊天、共同确认和明确交互结果可为 `shared`。Protocol-only 数据不发布 perceived 事件。

## 17. Context Package

Context Composer 的 `observations` section 只能读取 Current Observation Projection：

```ts
interface CognitiveObservationSnapshot {
  revision: number
  generatedAt: string
  throughEventSequence: number
  scene?: ObservationEnvelope<SpatialScene>
  entities: Array<ObservationEnvelope<EntityObservation>>
  blocks: Array<ObservationEnvelope<BlockObservation>>
  sounds: Array<ObservationEnvelope<SoundObservation>>
  playerActivity: Array<ObservationEnvelope<PlayerActivitySignal>>
  omissions: Array<{ reason: 'expired' | 'budget' | 'deduplicated' | 'low_salience'; count: number }>
}
```

组装规则：

- 删除已过期观察。
- 当前 focused/nearby 与 `occluded_recently` 分组，禁止混写。
- memory 内容进入 `retrieved_memories`，不进入 observations。
- 精确坐标不注入主模型；规划器可使用合法观察/Epistemic Map 的几何，Controller 可为已授权目标使用局部精确解，两者都不能把 raw tracked 坐标包装成新认知。
- 每条保留 observation ID、modality、observedAt、validUntil、certainty。
- 预算不足先丢低显著装饰，再丢重复普通实体；主要玩家、直接消息相关观察和威胁优先。
- 声音只在未过期且与当前对话、活动、玩家、危险或显著变化相关时注入；音乐和普通 ambient 默认省略。

普通无关 scene revision 不自动增加 Companion revision；触发决策的显著事件由 Attention Router 决定。

认知观察作为 `viewport_information` 与 `sound_information` 接入统一 Catalog/Help/Read；字段发现、读取 envelope、UI 会话和其他客户端信息以[合法信息接口、Help 发现与 UI 会话](./information-access-and-ui.md)为准。本文继续定义观察如何从 raw 协议候选产生，不另建旁路快照。

## 18. 与动作和记忆的边界

### 18.1 动作

- Action Runtime 或 scoped controller 只能为已授权目标和当前控制阶段使用最小 Control View；可以进行实现所需的连续物理、碰撞与局部空间求解，但不能搜索新目标、为高层规划提供隐藏路线，或把精确实现数据回流成认知事实。
- 动作结果只报告实际效果，不把内部路径搜索命中包装成发现。
- gaze、靠近和交互可以创造新的感知机会，结果仍由 Perception Boundary 生成。
- Safety/Reflex 可以因跌落预测、碰撞、受伤或服务端更正释放输入，但不能依据墙后 raw entity 选择躲避方向。防护若需要目标方向，必须来自视觉、声音、伤害方向或其他正常反馈。

### 18.2 记忆

- 只有 `perceived` observation、shared event 或 verified action result 可作为 observed memory evidence。
- entity tracker 的未确认预测不能写成 world fact。
- scene cluster 是派生摘要，写入地点记忆时必须引用底层 observation。
- 维度/世界作用域沿用 ObservationEnvelope，不能跨世界复用当前事实。

## 19. 生命周期与特殊情况

### 19.1 连接和重生

- connect/spawn 后等待 Backend 快照和自身眼睛姿态有效，再启动感知。
- disconnect 立即使所有 current observations 过期。
- respawn 和 dimension change 清空 tracker、scene 和主动 query；旧内容只能作为最近事件或记忆。
- chunk unload 将相关候选标 unknown，不产生“看见消失”。

### 19.2 游戏模式

v0.1 支持 survival/adventure 身体视角。spectator、自由相机或相机附着不是已验证模式：检测到时停止发布普通视觉观察并报告 unsupported mode，避免把穿墙相机当感知。

### 19.3 时间与天气

第一版不模拟完整光照识别距离。黑夜、雨和水下环境可以降低远距离实体/场景 certainty，但不能单独制造不可见。后续以 Paper 场景校准；策略版本记录在 VisibilityProof 中。

## 20. 实现接口与目录

```ts
interface PerceptionEngine {
  start(source: ProtocolObservationSource, signal: AbortSignal): Promise<void>
  snapshot(): CognitiveObservationSnapshot
  query(request: PerceptionQuery, signal: AbortSignal): Promise<PerceptionQueryResult>
  inspect(): PerceptionDebugState
}

interface VisibilityService {
  entity(entity: ProtocolEntityRef, viewport: PerceptionViewport): VisibilityProof
  block(position: BlockPosition, viewport: PerceptionViewport): VisibilityProof
}

interface ActivitySignalDetector {
  accept(observation: ObservationEnvelope<unknown>): void
  evaluate(now: string): PlayerActivitySignal[]
}
```

建议目录：

```text
src/perception/
├── contracts/v1.ts
├── engine.ts
├── viewport.ts
├── visibility/
│   ├── raycast.ts
│   ├── materials.ts
│   └── entity-sampling.ts
├── trackers/entity-tracker.ts
├── blocks/candidate-generator.ts
├── sounds/
│   ├── sound-policy.ts
│   ├── sound-normalizer.ts
│   └── sound-aggregator.ts
├── scene/scene-builder.ts
├── activity/player-activity-detector.ts
├── queries/perception-query.ts
└── projection/current-observations.ts
```

`ProtocolObservationSource` 由 Minecraft driver 提供最小只读接口；它不把完整 Bot 对象交给 PerceptionEngine 消费者。`findBlocks` 如被驱动用于粗候选生成，候选必须经过本文 FOV、光学与时效验证后才能成为 Observation；语义指代还必须经 Grounding 绑定该 Observation，内部交互计划在执行前再由准星验证，raw 命中不得直接传给 Behavior 或 Motor。

## 21. 可观察性

调试状态分两级：

1. Protocol/Control 调试视图：仅本地开发者可见，可用 Prismarine Viewer 展示完整缓存。
2. Cognitive overlay：显示虚拟 FOV、候选、射线结论、tracker 状态、当前 observation 和被裁剪原因。

每轮记录：

- 候选数、FOV 淘汰、raycast 数和耗时。
- visible/blocked/unknown 分布。
- observation 发布、合并、过期和预算遗漏。
- player hypothesis 证据与置信度变化。
- sound 去重、分类、范围/遮挡修正、聚合和来源关联。
- query 与 gaze action 的因果链。
- 任何被边界测试捕获的 raw type 泄漏。

不把隐藏目标内容写入普通同伴日志；完整调试附件明确标为 privileged。

## 22. Paper 1.21.1 验收场景

### 22.1 墙后矿物

在不透明墙后放置钻石矿，确保 chunk 已加载：

- `findBlocks` 可在 Control View 命中。
- Cognitive Observation、Context Package 和 Memory 不出现钻石。
- 移除墙或同伴转入可见位置后才产生 resource observation。

### 22.2 透明与部分遮挡

分别用石头、玻璃、树叶、栅栏和门隔开羊：

- 石墙完全阻断。
- 玻璃允许视觉。
- 树叶降低 exposure/certainty。
- 栅栏/门按 state 与 optical shapes 得到可重复结果。

### 22.3 实体时间连续性

玩家从视野进入、走到墙后、短暂重现、再远离：

- visible → occluded_recently → visible → recently_lost → unknown。
- 遮挡后不继续输出精确位置。
- `entityGone` 不被描述为死亡。

### 22.4 玩家砍树

玩家持斧靠近树、挥动、原木消失并拾取：

- 先产生原始观察。
- 多证据后产生 gathering hypothesis。
- 单次挥手或仅手持斧头不产生高置信 gathering。
- 活动信号不直接修改 SharedActivity。

### 22.5 建造与误判

玩家拿方块走动但不放置，然后实际连续放置：前者只保留低置信替代，后者产生 building；聊天中的玩笑“我在盖房子”只作为 player statement，不伪造视觉证据。

### 22.6 区块与重连

- chunk load 不产生大批“发现”事件。
- unload 不产生看见方块消失。
- 断线重连后旧 current observations 不恢复。
- 重新看见才生成新 observation，旧地点通过 Memory section 表达。

### 22.7 维度和世界隔离

主世界可见实体/地点在进入下界后全部过期；Context observations 无跨维度内容。切换 world ID 时缓存、tracker 和 scene 零复用。

### 22.8 容器

未打开箱子不出现内容；打开后的 verified inventory 只作为交互结果；关闭并离开后不能称为当前所见，之后只能以带时间的记忆表达。

### 22.9 主动观察

玩家说“看右边”：模型理解希望改变视觉注意并引用消息中的“右边”；Grounding 将其绑定为当前身体参考系下的语义方向，Behavior Synthesizer 形成转动/扫描计划，随后 Perception 产生新 scene。若被墙挡住，返回 blocked/unknown，不从缓存猜目标，也不调用 `scan_direction` 之类的话语专用技能。

### 22.10 压力与预算

生成大量实体和 block updates：Threat Supervisor 不受阻塞；普通扫描降级；主要玩家和威胁观察保留；遗漏计数可见且不被标成不存在。

### 22.11 声音方向、聚合与来源边界

在墙后分别触发僵尸叫声、脚步、挖掘、开门和爆炸：

- 生成类别、粗方向、距离带和 certainty，不暴露精确坐标。
- 明确僵尸声可形成“可能有僵尸”的候选，但不绑定 raw entity 或持续墙后跟踪。
- 脚步不自动认定为主要玩家。
- 连续挖掘声在窗口内聚合，不逐包触发模型。
- 视觉发现声源后才建立高置信 entity/block 关联。
- 超过规则范围、错误维度和重复兼容回调不产生重复观察。

## 23. 自动测试

### 单元测试

- FOV 边界、角度归一化和 near-field。
- DDA 对完整/部分/透明 shape。
- 多点实体 AABB 与 partial exposure。
- unknown chunk 不能当透明。
- tracker 所有转换、窗口和 uncertainty 增长。
- block update 模态证明。
- activity 多证据、替代假设与阈值。
- sound registry 分类、范围、方向带、遮挡降置信、双回调去重、时间聚合和来源关联。
- scene 去重、预算和过期。

### 属性与边界测试

- 增加不透明遮挡不能提高 visible exposure。
- 未通过 visibility 的 Protocol candidate 永不出现在 snapshot。
- world/dimension 改变后 snapshot 不含旧 observation。
- Context 类型依赖图中不存在 Mineflayer/Prismarine raw type。
- 同一输入事件和时钟产生确定性 observation 顺序与内容。

### 集成测试

实现 `mcserver/mc.ps1` 驱动的 Paper 固定场景，管理员只负责布置和观测，不把布置动作计入同伴自主行为。测试保存 Protocol 与 Cognitive 双轨迹，失败时可比较泄漏位置。

## 24. 分阶段实现

### P0：防泄漏骨架

- schema、Current Observation Projection 和模块 import 边界。
- 虚拟 viewport、实体/方块基本 raycast。
- opaque/transparent 最小策略。
- SoundObservation、协议声音去重和世界/距离边界。
- 连接、维度和过期处理。
- 墙后矿物、墙后实体和重连测试。

### P1：可用陪伴感知

- 实体多点采样和时间 tracker。
- 主要玩家运动、朝向、装备和手臂动作。
- block change 观察和砍树 activity signal。
- 声音分类、方向/距离带、聚合与实体/方块候选。
- 场景 sector 与资源聚类。
- Context Package 接入。

### P2：主动与自然

- gaze 协调和主动 query。
- partial materials、听觉粗略观察。
- Viewer cognitive overlay。
- 性能预算、压力降级和 Paper 完整验收。

## 25. v0.1 设计完成定义

本设计满足 #24 初步阶段的完成条件：

1. 实体、方块、声音三类基础观察，以及场景、玩家活动两类派生认知具有版本化 schema。
2. 可见性覆盖距离、FOV、实体 AABB、方块 shape、透明/部分遮挡与 unknown chunk。
3. 实体状态机明确当前可见、最近遮挡、丢失和重新发现。
4. 方块候选、变化、聚类和场景生命周期不依赖全量缓存泄漏。
5. 玩家活动以证据窗口、置信度和替代假设表达。
6. 调度频率、预算、聚合、失效和 Context 规则明确。
7. Control View 与 Cognitive Observation 具有代码与测试边界。
8. 领域事件和现有 Companion/Decision/Memory 契约已对齐。
9. 防泄漏和正向 Paper 1.21.1 场景可直接转成集成测试。

具体 FOV、距离、材质衰减和活动阈值仍需在实现阶段用场景数据校准；校准不改变本文的权限边界与事实不变量。
