---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_pr: 68
source_commit: ccae567
---

# 第一人称视口与视觉信息模块设计

> 原始状态声明（PR 未合并）：v0.2 实现基线，参数待 Paper/真人校准
>
> 对应 Issue：[#34](https://github.com/spojchil/mineintent/issues/34)、[#46](https://github.com/spojchil/mineintent/issues/46)、[#47](https://github.com/spojchil/mineintent/issues/47)
>
> 公共契约：[Information Runtime](../../architecture/information-runtime.md)、[合法信息与 UI](../../architecture/information-access-and-ui.md)
>
> 上游认知边界：[认知感知模型](../../architecture/cognitive-perception.md)
>
> 验收基线：[#57 合法信息验收矩阵](https://github.com/spojchil/mineintent/issues/57)
>
> 共享基础设施前置：[#63 Cursor scope](https://github.com/spojchil/mineintent/issues/63)、[PR #66 普通 Read 公开 revision 后验](https://github.com/spojchil/mineintent/pull/66)

## 1. 决定

v0.2 将第一人称实体/方块视觉实现为一条有界、可失效且不能反查 raw 世界的单向流水线：

```text
Mineflayer / protocol state                 UI coordinator / renderer
        │  仅 Driver 可见                              │
        ▼                                               ▼
ProtocolObservationSource                UiContextProjection + ViewportPresentationSource
  pose/candidates/optical voxels            UI identity/revision + proven sample regions
        └────────────────────────┬──────────────────────┘
                                 ▼
Viewport Scheduler
  scope → presentation gate → range → FOV/near-field eligibility → sampling budget
        ▼
Visibility Engine
  independent DDA + versioned optical material policy
        ▼
Temporal Tracker
  current visible → recent visual → lost/expired
        ▼
ViewportProjectionEngine
  safe Cognitive Projection + internal spatial records
        ▼
ViewportInformationSource
        ▼
ViewportProvider → InformationRuntime → Context / Model / Grounding
```

核心决定：

1. `identity`、`tracked`、`visible` 与 `spatially-known` 是四个独立维度。协议正在跟踪一个对象，不代表角色看见、认识或知道其当前位置。
2. `ProtocolObservationSource` 可以为几何计算提供精确候选，但它不是认知接口。只有通过当前 scope、FOV、DDA、材质、时效与预算的内容能进入 Cognitive Projection。
3. 模型只读取相对方向、距离带、可见程度、时间状态和 opaque observation ref；不读取协议 entity ID、完整 tracked 表、完整 blocks、绝对/可执行坐标或内部操作能力。
4. observation ref 的进程内 payload 只含 observation 标识、种类和投影代次，不含坐标、raw ID、NBT、对象引用或动作。签发源 `informationRevision` 改变后，旧 ref 必须失效。
5. 内部空间记录可为已经 Grounding 的单个观察提供短时 scoped resolution；它不能搜索新对象，也不能把精确实现值回流为模型事实、事件或记忆。
6. 声音由 `sound_information` 和 #59 独立拥有。视口不读取声音坐标、不用声音发现实体/方块，也不重复发布 auditory observation。
7. 每次 Provider Read 只返回一个 `source.kind = viewport_projection`。跨模态或跨接口组合由 Context/认知层基于已发布 evidence/ref 完成，Provider 不互相 Query。

### 1.1 设计权威关系

- [Information Runtime](../../architecture/information-runtime.md) 是 Catalog/Help/Read、schema、scope、source kind、Ref/Cursor Store、授权与错误 envelope 的权威；本文不另建第二套 token 或工具协议。
- 本文是 v0.2 `viewport_information`、视口几何、视觉材质、实体/方块安全投影和 observation ref 语义的实现权威。
- [认知感知模型](../../architecture/cognitive-perception.md) 继续拥有三类外界感知、场景/活动派生和认知事件语义。其旧草案中向模型暴露 `position`、在普通观察中保留精确位置或让声音参与视觉候选的部分，不适用于本文 v0.2 边界。
- 并行中的 [#59 声音与生命周期设计](https://github.com/spojchil/mineintent/issues/59) 拥有声音与 lifecycle。两模块只共享只读 session scope 与事件时观察者姿态；其设计合入后以 `sound-and-lifecycle-information.md` 为对应实现权威。

## 2. 范围与非目标

### 2.1 本模块负责

- 眼睛位置、yaw/pitch、focused/peripheral FOV 和近身候选资格；
- entity AABB 多点采样、光学可见性与时间状态机；
- UI/Screen/overlay 对世界视口的有界 presentation gate 与保守采样区域；
- block ray fan、关注区域和已发生 block change 的视觉复核；
- 独立 DDA、state-aware optical shape 与 1.21.1 版本化材质策略；
- 当前/最近实体和方块视觉观察的安全投影、聚合、TTL 与 revision；
- `viewport_information` 的 Help/Read、pagination、selector、availability、source 与 limit；
- 供 Grounding、Attention 和 scoped Controller 使用的窄接口；
- CPU、射线、内存、刷新、输出容量与压力降级策略；
- 单元、属性、Provider 契约、Paper、真人可见性和非泄漏验收。

### 2.2 本模块不负责

- 声音、字幕、音频遮挡、声源身份或声音到实体/方块的直接绑定；
- 屏幕像素、OCR、粒子、天气、天空、光照渲染、shader 或完整 Minecraft renderer；
- Screen/HUD/overlay 内容与控件语义；本模块只消费有界、归一化且已证明的视口遮挡区域，不读取 UI 文本或控件树；
- F3 targeted block/entity、准星交互距离与挖掘进度；这些属于 `f3_information` / `crosshair_information`；
- 通过全量 `findBlocks`、pathfinder、collision map 或 tracked table 建立认知世界；
- 从观察直接生成“看玩家”“砍树”“移除对象”“跟随”等对象/任务专用技能；
- 决定玩家话语含义、方法偏好、最终行动计划或成功条件；
- 让 Controller 用观察端口搜索替代目标、展开全知路线或读取其他对象位置；
- 把当前观察自动写成长时记忆。Memory 只能另行保存有来源、已降精度的经历。

## 3. 不变量

1. `loaded ≠ tracked ≠ identified ≠ visible ≠ spatially-known ≠ reachable`。
2. 当前视觉事实必须有本轮或仍在 TTL 内的视觉证明；raw candidate、packet presence、碰撞命中和 pathfinder 知识都不是视觉证明。
3. 未加载 chunk/section 为 `unknown`。射线必须停止，不能把它解释为空气、透明、不可见或不存在。
4. 背后对象不进入当前视觉。近身 360° 只提高候选调度优先级；除非仍通过 visual FOV/光学证明，否则其身份、种类和位置不能由 `viewport_information` 发布。
5. 墙后对象不因其被协议跟踪而发布。若此前看见过，只能在短 TTL 内表达“最近看见/现在被遮挡”，且不能继续使用 raw tracked 位置更新方向和运动。
6. 部分可见不等于完全可见。实体细节、方块名称和 identity 的可发布程度随 focused/peripheral、暴露比例和距离受 policy 限制。
7. 未知材质默认 `opaque`；collision shape、`block.transparent` 或 Mineflayer raycast 结果不能单独决定光学语义。
8. 被过滤、墙后、背后或预算前未验证的 raw candidate 不得通过 count、revision、`truncated`、错误、evidence 或普通日志形成侧信道。
9. `informationRevision` 只因安全投影值或 availability 改变而递增；raw tracked 移动、隐藏方块变化、缓存加载和被拒绝候选不递增。
10. connection/world/dimension/death/respawn transition 立即清空 current projection；进程重启不恢复“当前看见”。
11. Provider 只 capture 安全 source snapshot，不直接订阅 Driver、不运行 DDA、不等待新帧、不调用其他 Provider。
12. 调度不得阻塞协议处理、Threat Supervisor、动作取消或控制 lease；预算耗尽意味着“未检查/过期”，不能写成“不可见”。
13. UI main surface 或 overlay 本身不是视觉遮挡证明。没有可信 presentation mask 时，受影响视口为 unavailable/unknown；不能假定 Screen 后的世界仍可见，也不能把 UI 元素遮住的世界样本发布为 current visual。

## 4. 四种状态必须分开

| 维度 | 含义 | 可用证据 | 是否可进模型 |
|---|---|---|---|
| identity | 这是哪个语义对象/显示身份 | 当前可见 nameplate/外观证据，或独立已授权 identity evidence | 仅带证据、按 policy 降精度后 |
| tracked | 协议当前为 Driver 维护该对象 | raw entity lifecycle / entity ID | 否 |
| visible | 当前视口中至少一个样本通过 FOV 与光学证明 | visibility proof | 是 |
| spatially-known | 对当前/最近位置知道到什么精度 | 当前视觉几何、短时最近视觉、合法记忆 | 只返回 `current_relative/recent_relative/unknown`；精确值不进模型 |

典型组合：

- 玩家在背后：可以 tracked、identity known，但 `visible = false`，当前 `spatially-known = unknown`；不能输出“他在我背后”。
- 玩家刚绕到墙后：可以 tracked、identity known、`visible = false`，仅保留 `recent_relative` 与增长的不确定性；raw tracked 新位置不得更新观察。
- 玻璃后的玩家：若采样射线通过版本化策略，可 `visible = true`，暴露和 certainty 由累计衰减决定。
- 墙后僵尸从未被看见：即便 tracked，也没有 viewport observation/ref。
- 已看见但无法到达的方块：可以 visible/spatially-known；reachable 仍为未知，由后续行为/控制边界处理。

Identity 不从 tracking key 自动推导。玩家 candidate 可以携带 Driver 内部的 `displayIdentityCandidate`，但只有当前 nameplate/外观符合 1.21.1 显示规则并通过视觉证明时，projection 才发布 `displayIdentity`。聊天发送者、Tab 列表成员与一个视觉身体的关联属于 Grounding/identity evidence，不允许仅凭相同 username + tracked 坐标完成空间绑定。

## 5. Driver 边界与候选 DTO

`Bot`、Prismarine `World`、Entity、Block、Vec3、chunk、packet 和 numeric entity ID 只能存在于 `src/minecraft/driver/`。跨入 `src/perception/viewport/` 前转换为自有只读 DTO。

```ts
type ViewportProtocolRevision = 'mineintent.viewport-candidate.v1'

interface ViewportFrameCandidateV1 {
  protocol: ViewportProtocolRevision
  frameId: string
  processSessionId: string
  connectionEpoch: number
  worldId: string
  dimension: string
  capturedAt: string
  sourceRevision: number
  opticalSourceRevision: number
  observer: {
    eye: { x: number; y: number; z: number }
    yawRadians: number
    pitchRadians: number
    pose: 'standing' | 'sneaking' | 'swimming' | 'fall_flying' | 'other'
  }
  entities: readonly EntityVisualCandidateV1[]
}

interface EntityVisualCandidateV1 {
  trackingKey: string
  category: 'player' | 'hostile' | 'passive' | 'item' | 'projectile' | 'vehicle' | 'other'
  bounds: { minX: number; minY: number; minZ: number; maxX: number; maxY: number; maxZ: number }
  velocity?: { x: number; y: number; z: number }
  displayIdentityCandidate?: { kind: 'nameplate' | 'appearance'; text: string }
  displayDetails?: {
    pose?: string
    heldItemDisplayName?: string
    equipmentSilhouette?: string[]
  }
}

interface BlockChangeCandidateV1 {
  candidateId: string
  processSessionId: string
  connectionEpoch: number
  worldId: string
  dimension: string
  sourceRevision: number
  opticalSourceRevision: number
  position: { x: number; y: number; z: number }
  previousState?: OpticalBlockStateV1
  currentState?: OpticalBlockStateV1
  occurredAt: string
  cause: 'protocol_update' | 'verified_self_interaction'
}

interface OpticalBlockStateV1 {
  registryName: string
  stateKey: string
  collisionShapes: readonly AabbV1[]
  opticalPolicyKey?: string
}

interface AabbV1 {
  minX: number; minY: number; minZ: number
  maxX: number; maxY: number; maxZ: number
}

interface OpticalReadTokenV1 {
  tokenId: string
  processSessionId: string
  connectionEpoch: number
  worldId: string
  dimension: string
  opticalSourceRevision: number
}

type OpticalVoxelReadV1 =
  | { state: 'loaded'; block: OpticalBlockStateV1 }
  | { state: 'unloaded' }
  | { state: 'stale' }

interface ProtocolObservationSource {
  captureFrame(): Readonly<ViewportFrameCandidateV1> | undefined
  beginOpticalRead(
    expected: Readonly<{
      processSessionId: string
      connectionEpoch: number
      worldId: string
      dimension: string
      opticalSourceRevision: number
    }>,
  ): Readonly<OpticalReadTokenV1> | undefined
  opticalVoxelAt(
    token: Readonly<OpticalReadTokenV1>,
    position: Readonly<{ x: number; y: number; z: number }>,
  ): Readonly<OpticalVoxelReadV1>
  validateOpticalRead(token: Readonly<OpticalReadTokenV1>): boolean
  subscribeBlockChanges(listener: (candidate: Readonly<BlockChangeCandidateV1>) => void): Unsubscribe
}
```

边界规则：

- `trackingKey` 只在 perception 内关联连续 candidate，不进入 projection、ref、evidence、日志或错误。
- Driver 在 DTO 边界拒绝非有限数、退序 `sourceRevision/opticalSourceRevision`、错误 session/scope、非法 AABB 和超长显示文本。frame `sourceRevision` 表示候选帧发布序列，`opticalSourceRevision` 只随 voxel/chunk 光学读视图改变；实体移动不得无故使 optical token/cache 失效。block-change event 必须携带产生它的完整 scope、event source revision 和更新后的 optical revision；reducer 先拒绝旧 session/world，再把位置作为待重验候选，不能把 event 自带 state 直接发布为视觉事实。
- `beginOpticalRead` 只在 expected scope/revision 仍等于当前 optical source 时签发进程内 token；否则返回 undefined。token 不可序列化到安全 projection/ref/evidence/trace，也不能由模型构造。
- `opticalVoxelAt` 只能由预算化 DDA 持有效 token 调用。实现必须固定该 revision 的不可变读视图，或在任一读前/后发现 generation 改变时返回 `stale`；同一 token 绝不能悄悄读取新 revision。它不是通用 block query，不导出到 Grounding、Behavior、Controller、Context 或测试生产替身。
- 完成一条 proof 后、提交 scene/projection 前都调用 `validateOpticalRead`。token scope/optical revision 已变化时整条 proof 丢弃为 unknown，不能把旧 proof 包装进新 frame。
- Driver 不替 visibility engine 做 collision matcher/raycast。它只返回一个坐标处的光学输入 DTO；DDA、shape 交点和 attenuation 属于本模块。
- `subscribeBlockChanges` 产生的 candidate 只是待复核事件；`captureFrame` 不重复携带它们。不可见更新只改变 Protocol State，不改变公开 revision。
- 测试 fixture 可以构造这些 DTO，但 Paper oracle 不能实现生产 `ProtocolObservationSource`。

### 5.1 UI 呈现与视口遮挡端口

#54 的 `UiContextProjection` 只证明 UI 会话、输入归属与逐项 overlay state，不拥有“哪些世界像素仍可见”的结论。因此 #34 不读取不存在的 `viewportPublication`，也不从 `mainSurface/screenFamily/title` 自行推断背景世界是否可发布。

#34 另拥有一个窄、只读的呈现端口：

```ts
interface NormalizedViewportRegionV1 {
  left: number    // 0..1
  top: number     // 0..1
  right: number  // 0..1，且 right > left
  bottom: number // 0..1，且 bottom > top
}

type ViewportPresentationStateV1 =
  | {
      availability: 'available'
      mode: 'world_primary' | 'world_behind_screen'
      publishableSampleRegions: readonly NormalizedViewportRegionV1[]
      maskPolicyRevision: string
      acquisition: UiFieldAcquisition
    }
  | {
      availability: 'not_currently_displayed' | 'not_supported' | 'not_exposed'
    }

interface ViewportPresentationSnapshotV1 {
  processSessionId: string
  connectionEpoch: number
  uiRevision: number
  screenInstanceId?: string
  screenRevision?: number
  sourceRevision: number
  observedAt: string
  state: ViewportPresentationStateV1
}

interface ViewportPresentationSource {
  capture(): Readonly<ViewportPresentationSnapshotV1>
  subscribe(
    listener: (snapshot: Readonly<ViewportPresentationSnapshotV1>) => void,
  ): Unsubscribe
}
```

端口由真实渲染 adapter 或锁定 1.21.1 的 MineIntent 无头呈现策略实现；它可以消费 #54 的 identity/revision 进行绑定，但不能修改 #54。`publishableSampleRegions` 是**已经证明没有被当前 Screen/overlay 遮住**的归一化采样区域，不是 UI element tree，也不进入模型结果/ref/evidence。visibility engine 把实体/方块样本投影到归一化视口后，只对命中这些区域的样本继续 FOV/DDA；落在区域外表示“本轮没有视觉证明”，不是对象不存在或被世界方块遮挡。

regions 使用 strict finite schema，最多 32 个，按 top/left 排序并在 adapter 内合并重叠；available 分支必须至少有一个正面积区域，空集合改为 `not_currently_displayed`。区域数量、mask 复杂度和被遮挡 sample 数只进 privileged telemetry，不得通过 coverage/count/revision 形成 UI 内容侧信道。

首版 fail-closed 规则：

- `mainSurface=world` 且无头 coordinator 已明确全部相关 overlay 状态时，可以由版本化策略发布完整或扣除已知 mask 后的区域，acquisition 为 `structured_ui_equivalent`；
- 真实客户端确认当前 framebuffer/GUI mask 时使用 `current_screen`；
- `mainSurface=screen` 但没有该精确 source identity 的版本化 mask、或任一会影响视口的 `UiFieldState` unavailable 时，不按 coarse family/title/slot 猜测，presentation state 为 `not_supported/not_exposed`；
- 已证明 Screen 完全遮住世界时为 `not_currently_displayed`；部分遮挡只有已证明区域可以继续发布；
- chat/F3/Tab 等 overlay 不因“通常透明”而自动忽略。缺少 mask 能力时保守 unavailable，不用空 mask 冒充无阻挡。

adapter 可以接收与 #54 classifier 同版本的内部 `UiScreenSourceType`，但该 identity 不越过此端口成为模型事实。协议 window ID、raw Window、标题和控件树不得用于 mask 猜测。presentation source、#54 projection 与 protocol frame 由 viewport reducer 按 `processSessionId/epoch/uiRevision/screen instance` 对齐后原子归约；Provider Read 不再二次 capture UI。

## 6. 视口、FOV 与 near-field

```ts
interface ViewportOpticalPolicyV1 {
  policyRevision: string
  focusedFov: { horizontalDeg: number; verticalDeg: number }
  peripheralFov: { horizontalDeg: number; verticalDeg: number }
  nearFieldRadiusBlocks: number
  maxEntityRangeBlocks: number
  maxBlockDetailRangeBlocks: number
  maxSceneRangeBlocks: number
  directionBandBoundariesDeg: readonly number[]
  distanceBandBoundariesBlocks: readonly number[]
}
```

处理顺序固定为：

```text
scope/finite validation
→ distance hard bound
→ near-field scheduling priority
→ focused/peripheral FOV classification
→ sample point generation
→ optical DDA
→ detail/identity publication policy
```

- 眼睛位置由当前 pose 计算，不能用固定身高。
- FOV 用 observer yaw/pitch 与 candidate sample vector 计算；边界采用固定 epsilon，fixture 必须锁定。
- focused 区可发布通过证明的类别、身份与显示细节；peripheral 区默认只发布类别、方向、距离和显著运动，不能发布手持物、精确姿态或 nameplate 文本。
- near-field 不绕过 FOV 和 DDA。它只让近身候选更早被检查、增加实体 AABB 采样，并允许当前可见的近身表面使用 `touching/near` 距离带。
- 碰撞、受伤或脚步带来的 360° 身体/听觉事实分别属于自身反馈或声音，不得借 near-field 包装成视觉。
- FOV、距离与 band 边界是版本化 system policy，不属于初始性格或可由模型修改的提示词。

初始实现沿用认知感知草案的校准种子（focused 90°×70°、peripheral 160°×110°、near-field 2.5 blocks、实体 64 blocks、方块细节 24 blocks、场景 64 blocks），但它们不是 v0.2 冻结产品常量。进入发布基线前必须由 Paper fixture 与真人对照确定 `policyRevision`。

## 7. 独立 DDA 与视觉材质策略

### 7.1 DDA 契约

实现使用确定性的 voxel traversal（Amanatides–Woo 风格），不调用 Mineflayer collision matcher 或“找到第一个非空气块”的通用 raycast：

```ts
interface VisibilityRayRequestV1 {
  opticalRead: Readonly<OpticalReadTokenV1>
  origin: { x: number; y: number; z: number }
  target: { x: number; y: number; z: number }
  maxDistanceBlocks: number
  maxVoxelVisits: number
}

interface InternalVisibilityProofV1 {
  result: 'visible' | 'partially_visible' | 'blocked' | 'unknown'
  sampledPoints: number
  visiblePoints: number
  attenuationBand: 'none' | 'low' | 'medium' | 'high'
  stoppedBy: 'target' | 'opaque_shape' | 'attenuation' | 'unloaded' | 'range' | 'budget'
  checkedAt: string
}
```

算法约束：

1. 使用 segment 与每个 block 的 optical shapes 求交，不把整个 voxel 当成实心。
2. 起点所在 voxel 只处理眼睛实际落在 optical shape 内的异常情况，避免把自身周围方块误作遮挡。
3. 到达 target sample 前遇到 `opaque` shape 立即 blocked；`partial` 累积 attenuation；`transparent` 可记录表面但继续。
4. 遇到 unloaded/stale、token scope/optical source revision 改变、非有限值、距离或 voxel visit budget，返回 unknown 并停止。绝不能从未加载边界后继续，也不能以新 token 续跑半条旧射线。
5. target 自身 shape 与遮挡 shape 分开判断；可见目标表面不因“射线命中目标”被当作遮挡。
6. 多点实体只要达到 policy 的最小可见样本数即可 visible；可见比例决定 clear/partial。未检查点不能当 blocked 点。
7. proof 的阻挡 block 名/位置只进入 privileged diagnostic，不进入安全 projection/evidence。

### 7.2 版本化材质策略

```ts
interface VisionMaterialRuleV1 {
  key: string
  registryNames: readonly string[]
  mode: 'opaque' | 'partial' | 'transparent'
  attenuation: number
  shapeSource: 'full_cube' | 'collision_shapes' | 'policy_shapes'
  policyShapesByState?: Readonly<Record<string, readonly AabbV1[]>>
  visibleSurface: boolean
}
```

1.21.1 policy 至少覆盖：

| 类别 | 初始光学语义 | 必测点 |
|---|---|---|
| 普通实心块 | opaque/full cube | 墙后实体/矿物不出现 |
| glass / stained glass | transparent，表面自身可观察 | 后方对象仍需独立射线证明 |
| leaves | partial | 多层累计，不能一层无限透视 |
| water / lava | partial | 液体表面、浸没状态与距离校准 |
| tall grass / flowers / fire | transparent，表面可观察 | 不使用 full voxel 遮挡 |
| fence / pane / bars | opaque 或 partial + optical shapes | 空隙射线可通过，杆体可遮挡 |
| door / trapdoor / gate | state-aware policy shapes | 开关/朝向变化立即影响 proof |
| slab / stairs | collision/policy shapes | 空余空间可通过 |
| unknown/mod block | opaque/full cube | fail closed，不因缺表透视 |

`block.transparent`、碰撞 shape 与注册名只作为 policy 生成/校验输入，不能绕过版本化表。策略变更会改变观察语义，必须提升 policy 与 `viewport_information` schema/adapter revision，并重跑材质差分测试。

## 8. 候选生成与方块边界

### 8.1 实体候选

- Driver 只提供当前 scope 的 bounded candidate batch；超过内部 batch cap 时按距离与协议到达顺序稳定裁剪，但裁剪详情只进 privileged telemetry。
- Scheduler 对当前已可见/主要社交对象的 tracker、当前 gaze 区和显著移动优先；这个优先级只影响检查顺序，不提供额外权限。
- focused 实体默认采样头/胸/下部中心；大实体增加左右边缘；peripheral 与压力降级可减少样本，但不能把未采样当 blocked。
- identity/details 只从实际通过的 sample 和对应显示 policy 派生。一个脚点可见不等于 nameplate、手持物或脸部可见。

### 8.2 方块候选

方块视觉只来自以下入口：

1. 固定、版本化 ray fan 的第一批可见表面；
2. 当前准星/注意区域的加密 ray fan；
3. 当前视口内的 `blockChanges` 位置复核；
4. 已有且仍有效的 observation ref 的有界重验。

禁止遍历所有 loaded blocks 后按方块名挑选结果。即使 Driver 内部为性能产生候选索引，每个候选也必须独立经过当前 FOV、DDA 与预算，且索引命中数量不能进入公开 projection。

安全方块观察只描述被证明的表面：block category、允许显示的 block name、可见面、相对方向/距离、partial exposure，以及由可见表面组成的粗聚类。它不扩展到后方同类方块、不从一块原木推断整棵树、不根据碰撞图宣称通路，也不包含绝对位置。

`blockChanges` 只有在变化前/后表面当前可见，或它是同伴刚执行且结果已验证的局部交互时，才可形成视觉/interaction evidence。仅收到墙后 block update 不发布 observation、revision 或 count。

## 9. 实体时间状态机

```text
absent
  → candidate_internal
  → current_focused | current_peripheral | current_partial
  → recent_occluded | recent_out_of_view
  → recently_lost
  → expired

recent_* / recently_lost
  → current_*       新视觉证明
  → expired         TTL 到期
```

```ts
interface EntityVisualTrackV1 {
  observationId: string
  trackingKey: string
  state:
    | 'candidate_internal'
    | 'current_focused'
    | 'current_peripheral'
    | 'current_partial'
    | 'recent_occluded'
    | 'recent_out_of_view'
    | 'recently_lost'
  lastVisualGeometry?: InternalSpatialRecordV1
  firstVisuallyConfirmedAt?: string
  lastVisuallyConfirmedAt?: string
  validUntil: string
  uncertaintyBand: 'none' | 'low' | 'medium' | 'high'
  lostReason?: 'occluded' | 'out_of_view' | 'out_of_range' | 'protocol_removed' | 'scope_changed' | 'unknown'
}
```

- current → recent 转换由新的 FOV/visibility proof 驱动，不从 raw candidate 缺失直接断言“离开”。
- recent 状态冻结最后视觉相对位置；只能按时间扩大 uncertainty。raw tracked 位置/速度不再更新它。
- `protocol_removed` 只表示跟踪结束。没有可见死亡/离开证据时，模型只能得到 recently_lost/expired，不能得到“死亡”“离线”或新位置。
- `recent_occluded` 只能由最后一次视觉几何或连续消失过程的射线证明产生；不能对 raw tracked 的新坐标做墙后射线后再宣称“仍在那里”。没有该证据时使用 `recent_out_of_view/recently_lost`。
- 重新发现必须生成新 proof；可以复用 observation identity continuity，但 revision 与 evidence 必须更新。
- 普通实体与主要社交对象可有不同 recent TTL，但该差异来自 attention policy，不改变可见性权限。Threat Supervisor 的内部威胁持续性是独立 Control View，不能延长模型视觉事实。

方块 current surface 同样有短 TTL。视角变化、材质状态变化或 chunk unload 后，未重验表面到期；不会因为 world cache 仍有 block 就永久保持 current。

## 10. 安全 Cognitive Projection

### 10.1 模型可见 DTO

```ts
type RelativeDirectionV1 =
  | 'front' | 'front_left' | 'left' | 'back_left'
  | 'back' | 'back_right' | 'right' | 'front_right' | 'unknown'

interface RelativeVisualPositionV1 {
  direction: RelativeDirectionV1
  vertical: 'above' | 'level' | 'below' | 'unknown'
  distance: 'touching' | 'near' | 'medium' | 'far' | 'unknown'
}

interface SafeVisualObservationBaseV1 {
  observationRef: InformationSelectorRef
  temporal: 'current_visual' | 'recent_visual'
  visibility:
    | 'focused' | 'peripheral' | 'partial'
    | 'recently_occluded' | 'recently_out_of_view' | 'recently_lost'
  relative: RelativeVisualPositionV1
  exposure: 'clear' | 'partial' | 'unknown'
  spatialKnowledge: 'current_relative' | 'recent_relative' | 'unknown'
  certainty: 'high' | 'medium' | 'low'
  observedAt: string
  lastVisuallyConfirmedAt: string
  validUntil: string
}

interface SafeEntityVisualObservationV1 extends SafeVisualObservationBaseV1 {
  kind: 'entity'
  category: 'player' | 'hostile' | 'passive' | 'item' | 'projectile' | 'vehicle' | 'other'
  displayIdentity?: { kind: 'nameplate' | 'appearance'; value: string }
  motion?: 'still' | 'approaching' | 'leaving' | 'crossing' | 'unknown'
  facing?: 'toward_observer' | 'away' | 'sideways' | 'unknown'
  displayedPose?: string
  displayedHeldItem?: string
}

interface SafeBlockVisualObservationV1 extends SafeVisualObservationBaseV1 {
  kind: 'block'
  observationType: 'surface' | 'visible_change' | 'visible_cluster'
  blockCategory: string
  displayedBlockName?: string
  visibleFaces?: Array<'top' | 'bottom' | 'north' | 'south' | 'west' | 'east'>
  change?: { fromCategory: string; toCategory: string; occurredAt: string }
  cluster?: {
    estimatedCount: 'one' | 'few' | 'several' | 'many'
    extent: 'small' | 'medium' | 'large'
  }
}

type SafeViewportObservationV1 =
  | SafeEntityVisualObservationV1
  | SafeBlockVisualObservationV1

interface SafeViewportSceneV1 {
  generatedAt: string
  visibleRegions: Array<{
    direction: Exclude<RelativeDirectionV1, 'unknown'>
    distance: RelativeVisualPositionV1['distance']
    surfaceCategories: string[]
    openness: 'open_in_view' | 'surface_dominated' | 'unknown'
  }>
  uncertaintyNotes: Array<'bounded_sampling' | 'unloaded_boundary' | 'partial_occlusion'>
}

interface ViewportCoverageV1 {
  policyRevision: string
  window: { startedAt: string; endedAt: string }
  sampling: 'bounded_first_person'
  safeObservationSetTruncated: boolean
  unloadedDirections: RelativeDirectionV1[]
}
```

Projection 在 Provider 签发 caller-specific ref 之前使用独立内部 DTO：

```ts
type ProjectedViewportObservationV1 =
  | (Omit<SafeEntityVisualObservationV1, 'observationRef'> & {
    observationId: string
    projectionGeneration: number
    evidenceIds: string[]
  })
  | (Omit<SafeBlockVisualObservationV1, 'observationRef'> & {
    observationId: string
    projectionGeneration: number
    evidenceIds: string[]
  })
```

`observationId` 是 projection 自己生成的随机标识，不是 trackingKey、协议 ID 或坐标；它只在安全 source port、Attention 通知和 ref payload 内流转。`evidenceIds` 也是无法反查 raw payload 的安全 ID。Provider 分页后为本页每个 observationId 签发 caller-specific ref，聚合本页 evidenceIds 到外部 envelope，再构造模型可见 `SafeViewportObservationV1`。因此共享 projection 不保存 principal/grant 相关 ref，模型也永远看不到 observationId。

发布规则：

- projected DTO 与模型 DTO 分别通过 strict Zod schema 重建；任何额外嵌套键、坐标型键、numeric ID、raw handle、NBT 或非有限值使 projection commit/Provider Read 失败并进入 privileged telemetry。
- `displayIdentity`、`displayedPose`、`displayedHeldItem` 只在字段对应视觉区域通过 policy 时存在；不可见细节使用省略，不用 raw 值补齐。
- `motion` 只由连续视觉确认计算。recent 状态不继续从 raw velocity 推断运动。
- `scene` 只压缩同一安全 projection 内的观察，不读取 world/collision/pathfinder。`open_in_view` 只描述当前采样射线未遇到可见表面，不等于可通行或已探索。
- `safeObservationSetTruncated` 只表示已经形成安全观察后，因公开 list/output cap 被裁剪。raw candidate 超限、墙后对象或 DDA 前预算不足不得改变它。
- `unloadedDirections` 只来自当前视口射线实际遇到未加载边界的粗方向；不含 chunk 坐标或距离。

### 10.2 安全 source port

```ts
type ViewportProjectionSnapshotV1 =
  | {
      available: false
      reason: 'not_connected' | 'not_currently_displayed' | 'not_supported' | 'not_exposed'
      informationRevision: number
      sourceRevision: number
      observedAt: string
    }
  | {
      available: true
      informationRevision: number
      sourceRevision: number
      observedAt: string
      validUntil: string
      observations: readonly Readonly<ProjectedViewportObservationV1>[]
      scene: Readonly<SafeViewportSceneV1>
      sceneEvidenceIds: readonly string[]
      coverage: Readonly<ViewportCoverageV1>
    }

interface ViewportInformationSource {
  capture(): Readonly<ViewportProjectionSnapshotV1>
}
```

Projection reducer 同时消费 protocol frame、#54 UI identity 和 `ViewportPresentationSnapshotV1`，先按 epoch/ui/screen scope 对齐，再构造完整 immutable snapshot 并原子替换。`sourceRevision/observedAt/informationRevision` 只随模型可见 value 或 availability 改变，不复用 protocol、UI 或 presentation raw revision。仅 `UiFieldAcquisition`、mask provenance 或内部 source revision 改变而安全输出不变时，允许更新私有游标/证据缓存，但不得替换此公开 source snapshot。Provider 每次 availability/read 只 capture 一次；不同 Read 不承诺跨接口原子性。

### 10.3 内部空间记录

```ts
interface InternalSpatialRecordV1 {
  observationId: string
  projectionGeneration: number
  connectionEpoch: number
  worldId: string
  dimension: string
  target:
    | { kind: 'entity'; trackingKey: string; bounds: AabbV1 }
    | { kind: 'block_surface'; block: { x: number; y: number; z: number }; face?: string }
  visuallyConfirmedAt: string
  validUntil: string
}
```

该类型只存在于 `src/perception/viewport/internal/`，不从包入口导出。它不是 Cognitive Projection，也不得被序列化到 Context、Journal、Memory、ref payload、Provider error 或普通 trace。

## 11. Observation ref、revision 与空间解析

Provider 为每个 safe observation 通过公共 `InformationReferenceIssuer` 签发：

```ts
interface ViewportObservationRefPayloadV1 {
  observationId: string
  observationKind: 'entity' | 'block'
  projectionGeneration: number
}
```

签发参数：

- `kind = viewport_observation`；
- `payload` 只使用上述 strict JSON DTO；
- `allowedInterfaces = ['viewport_information']`；未来若 `crosshair_information` 明确接纳该 kind，必须通过双方 schema revision 扩展，不能预先放宽；
- 不绑定 screen；绑定 principal、grant、epoch、world 和签发源 `informationRevision` 由 Runtime 完成；
- `validUntil` 不晚于 observation 与 projection 的最早 TTL；
- 单次 Read 签发数和 store 容量使用 Runtime 公共 limit。

`viewport_information` 接受可选 selector kind `viewport_observation`。无 selector 时读取当前 bounded projection；带 selector 时只允许读取 `observations`，且只返回同一安全 schema 的对应一项，不增加精度、扩大 FOV、搜索相邻对象或刷新过期事实。

Runtime 在目标 Read 前后校验签发源当前 `informationRevision`。projection 任一公开变化后旧 ref 保守失效；调用者重新 Read 获取新 ref。此策略牺牲长寿命引用，换取 v0.2 明确的当前性与竞态安全。

Grounding/Controller 不得反序列化 ref payload：

1. Grounding 先验证 ref 来自本轮合法 Information Read/Context、scope/grant/revision 仍有效，再产生用途受限 grounded handle。
2. grounded handle 只表达 observation identity、证据和 `spatialKnowledge`，不包含协议 ID/坐标或计划谓词。
3. 已授权 Controller 在持有 active lease、grounded handle 和相同 scope 时，才可向内部 `ViewportSpatialResolver` 请求该单个观察的当前实现解。
4. resolver 只查 observationId 对应的 `InternalSpatialRecordV1`，不接受类别、名称、坐标范围或“最近目标”等搜索条件；过期、recent-only、revision/scope 不同返回 stale/unknown。
5. 精确解只能进入当前局部控制阶段，不能写回 Companion、模型、Epistemic Map、Journal 或 Memory 作为新观察。控制后的事实必须由新的合法视觉/交互结果确认。

`ViewportSpatialResolver` 的具体 lease/handle schema 属于 #52/v0.3 Controller 设计；本文冻结其权限边界和“单个已选观察、不可搜索、不可回流”约束，不提前冻结行为计划。

## 12. `viewport_information` Provider 契约

```ts
interface ViewportInformationValuesV1 {
  observations: SafeViewportObservationV1[]
  scene_summary: SafeViewportSceneV1
  coverage: ViewportCoverageV1
}
```

Provider 定义：

| 属性 | 值 |
|---|---|
| `id` | `viewport_information` |
| `schemaRevision` | `viewport-information:1` |
| `audiences` | `companion`, `controller` |
| `scopeDependencies` | `connection`, `world`, `dimension`, `ui` |
| selectors | optional；只接受 `viewport_observation` |
| pagination | observations default 12，max 24；稳定顺序分页 |
| source kind | `viewport_projection` |
| acquisition | `current_perception` |
| max fields / bytes / timeout | 3 / 32 KiB / 50 ms |

共享前置依赖：[#63 Cursor 按 Provider scopeDependencies 绑定](https://github.com/spojchil/mineintent/issues/63) 必须在 viewport pagination 合并前完成。当前 Runtime 不能正确表达本文声明的 `ui` cursor dependency；在 #63 合入前不得以省略 `uiRevision`、额外绑定 screen 或关闭分页来临时兼容。

[PR #66](https://github.com/spojchil/mineintent/pull/66) 的 Runtime 公开 revision 后验必须在 viewport 生产 Provider 前进入目标基线：所有 Read 在 scope 复核后同步读取同 Provider 当前 `availability().informationRevision`；普通 Read 不一致返回 `scope_changed`，续页返回 `invalid_page`。该检查只比较公开 revision，不把 optical/presentation 内部 revision 暴露成模型侧信道。

成功 Read 的 `adapterRevision` 使用锁定 Minecraft/optical policy 的安全适配版本（初始命名 `viewport-projection:1.21.1:1`），`sourceRevision` 取本次唯一 capture 的 projection source revision。请求 `observations` 时聚合本页 observation evidence；请求 `scene_summary` 时聚合 snapshot 的 bounded `sceneEvidenceIds`。这些 ID 都不能解析为 raw packet/block/entity payload。

Help 字段：

| 字段 | valueType | precision | sourceKinds | availability 与说明 |
|---|---|---|---|---|
| `observations` | `SafeViewportObservationV1[]` | `quantized` | `viewport_projection` | 当前/短时最近的实体和方块视觉；空数组表示本次有界投影无观察，不断言世界为空 |
| `scene_summary` | `SafeViewportSceneV1` | `inferred` | `viewport_projection` | 只由同一安全观察集合压缩；不表示可达性或隐藏空间 |
| `coverage` | `ViewportCoverageV1` | `inferred` | `viewport_projection` | policy、窗口、公开集合裁剪与可见的未加载边界；不含 raw candidate/ray/drop 计数 |

所有 schema 使用 strict object/finite/bounded string/array。Runtime 必须使用 Zod parsed data 重建结果，并校验本次单一 `source.kind` 在每个实际返回字段的 `sourceKinds` 内。

### 12.1 Availability

| 状态 | overall / field availability | 说明 |
|---|---|---|
| disconnected | unavailable / `not_connected` | 不保留旧视觉 |
| connecting/configuration/initial spawn | unavailable / `not_currently_displayed` | 姿态、世界或 chunk source 尚不可信 |
| death/respawn/world transition | unavailable / `not_currently_displayed` | 先清空，ready 后从空投影重建 |
| play + observer pose/source + presentation ready | available | 即使 observations 为空也合法；只含 proven publishable regions 内的样本 |
| presentation 证明世界视口完全被遮住 | unavailable / `not_currently_displayed` | #34 presentation source 的结论，不是 #54 ScreenFamily 推断 |
| presentation mask/source capability 缺失 | unavailable / `not_supported/not_exposed` | 不返回遮挡区域下的旧 current visual，不补全屏可见默认值 |
| 部分 ray 遇到 unloaded | available | 观察仍可返回；coverage 以粗方向标注 unknown |
| 具体 selector 过期 | request `invalid_selector` 或字段 `stale_selector` | 不回退到 tracked/world 搜索 |

#54 只提供 `UiContextProjection` 的 identity/revision 与 nested state，#34 的 `ViewportPresentationSource` 独立提供经过证明的 publishable sample regions；不存在 `viewportPublication` 字段。两者在 viewport reducer 内原子归约，Provider 不 Query `ui_context`、不二次 capture。因声明 `ui` dependency，Read 中 UI/presentation scope 改变由 Runtime 保守丢弃为 `scope_changed`；若重试后公开 visual value/availability 未变，viewport `informationRevision/sourceRevision/observedAt` 仍保持。

### 12.2 Pagination 与排序

`observations` 在签发 ref 前使用稳定顺序：current before recent → salience band → focused/partial/peripheral → last visual time descending → internal random observationId。salience 只能来自视觉变化、当前 attention 关联和安全 policy，不来自任务名称或对象类别专用技能。

- 第一页必须优先保留当前 focused、主要社交视觉变化和显著威胁视觉，但“威胁”只作调度/Attention hint，不授予动作。
- `scene_summary` 与 `coverage` 若被请求，每页返回同一 projection revision 的相同值；只有 observations list 参与游标切片。
- cursor 的 bounded JSON page state 只保存 offset；稳定集合由 information revision 绑定，不复制 observations、spatial record 或坐标。
- #63 完成后，Runtime 根据 Provider 声明的 `scopeDependencies` 绑定 cursor；本接口因此绑定 connection epoch、world、dimension、`uiRevision`，不绑定未声明且无关的 screen instance/revision。
- cursor 还绑定 principal、grant、interface、字段、selector、limit 与 `informationRevision`，并一次性消费。
- projection 改变后续页返回 `invalid_page`，不尝试拼接新旧观察。
- public projection 在分页前已有硬 cap。cursor 不能导出 tracked 历史或将当前接口变成长时订阅。

## 13. Attention、Grounding、Behavior 与 Controller 边界

### 13.1 Attention

Projection 可以发布不含坐标的内部通知：

```ts
interface ViewportProjectionChangedV1 {
  informationRevision: number
  changedObservationIds: string[]
  changeKinds: Array<'appeared' | 'visibility_changed' | 'detail_changed' | 'lost' | 'scene_changed'>
  salience: 'low' | 'medium' | 'high'
  occurredAt: string
}
```

Attention Router 用它决定是否调度一次 Context Read/模型回合，不直接获得 trackingKey、坐标或 raw candidate。通知只在安全 projection 实质改变时产生；raw hidden motion 不触发。Attention 可以选择读取 `observations/scene_summary/coverage` 或忽略，不能借通知直接形成世界事实。

### 13.2 Grounding

Grounding 只消费：

- 本轮合法 `InformationReadResult` 与其中的 observation ref；
- 当前消息中的语义 referent；
- 有来源记忆、当前活动和安全 Epistemic Map。

它区分：identity 已绑定但空间 unknown、current relative、recent relative 和完全 stale。它不导入 `ProtocolObservationSource`、internal tracker、spatial record 或 raw entity table，也不按“玩家/树/方块”注册专用 grounding 函数。

“看向我”中若说话者身份已知、但未在当前/最近视觉中定位，Grounding 返回 partial + spatial unknown。后续 Behavior 可以合成通用转动/扫描/等待等信息获取过程，不能用 tracked 坐标一步转准。“这棵树”只有在语义引用绑定到合法 visual observation 时才形成 grounded handle。

### 13.3 Behavior 与 Controller

- Behavior Synthesizer 看到 grounded semantic goal、空间知识等级、观察 ref/证据、身体状态与通用可供性；不看到原始玩家话语、tracked 表或 `look_at_player/chop_tree` 技能目录。
- 本 Provider 所谓“身体可供性摘要”仅由 `temporal + visibility + exposure + spatialKnowledge + visibleFaces` 表达当前证据能否支持注意/Grounding。它不声明 reachable、interactable、recommended action、鼠标输入或技能；真实身体/控制可供性来自独立自身状态与 controller port。
- 本模块可接受通用的有界 `refresh current viewport` 或 `revalidate selected observation` 调度请求；请求只改变采样优先级/期限，不移动身体、不选择新对象、不解释任务。
- 转头、移动、扫描、等待、挖掘、放置等方法由后续行为合成与 controller 契约决定。视口模块不输出“应当转头/砍掉/靠近”的计划提示。
- Controller 只有在目标已经 Grounding、lease 有效且局部阶段需要时，才能解析该 handle 的当前实现位置；解析失败返回 stale/unknown，由行为层重新感知或处理信息缺口。
- Controller 的控制结果不会自动证明“看见”“到达”或“移除完成”；必须由新视觉、合法状态或交互结果验证。

## 14. 调度、预算与降级

### 14.1 版本化预算

```ts
interface ViewportBudgetPolicyV1 {
  policyRevision: string
  maxFrameCandidates: number
  maxEntitySamplePointsPerCycle: number
  maxBlockRaysPerCycle: number
  maxVoxelVisitsPerRay: number
  maxVoxelVisitsPerCycle: number
  maxProjectionObservations: number
  maxEntityTracks: number
  maxRecentBlockObservations: number
  maxEvidenceRecords: number
  maxCycleCpuMs: number
  refreshHz: {
    focused: number
    attended: number
    ordinaryEntity: number
    scene: number
  }
  ttlMs: {
    currentObservation: number
    ordinaryRecentEntity: number
    attendedRecentEntity: number
    blockSurface: number
  }
}
```

初始 benchmark seed（不是发布冻结值）：

| 预算 | seed |
|---|---:|
| frame entity candidates | 128 |
| entity sample points / cycle | 96 |
| block rays / cycle | 128 |
| voxel visits / ray | 128 |
| voxel visits / cycle | 4096 |
| public observations | 48 |
| internal entity tracks | 256 |
| recent block observations | 128 |
| safe evidence records | 256 |
| synchronous CPU slice | 4 ms，达到即 yield |
| focused/attended/ordinary/scene | 4 / 4 / 2 / 1 Hz |

任何主动重验也受同一个 cycle/ray/voxel hard cap；它只能提高队列优先级，不能建立第二套无限预算。

### 14.2 降级顺序

压力升高时按固定顺序降级：

1. 跳过装饰性 scene block rays；
2. 降低 peripheral 实体采样点与普通实体刷新；
3. 延后非 focused block change 复核；
4. 停止 scene 重算，只让旧 scene 按 TTL 过期；
5. 对普通/主动请求返回 partial 当前投影或 deadline，不阻塞事件循环。

始终保留 scope invalidation、取消检查和已选 focused observation 的最小重验机会，但视觉模块没有权力挤占 Threat Supervisor 的独立预算。每个 CPU slice 后检查 `AbortSignal` 并让出事件循环。

预算前未验证的 candidate 不进入 public `truncated`/revision。公开 observation cap 裁剪发生在安全投影之后，才可设置 `safeObservationSetTruncated`。内部 telemetry 可记录 candidate/ray/voxel/drop 数和耗时，但使用 privileged 类型与独立 sink。

### 14.3 缓存与内存

- DDA 只允许按 `(processSessionId, connectionEpoch, worldId, dimension, optical source revision, block position, policyRevision)` 短时缓存 optical rule/shape；token/world/source revision 变化立即清除。
- visibility proof cache 必须包含 observer pose bucket、target sample、optical source revision 和 policy revision；不得跨移动、转头或 block change 复用。
- tracker 使用 bounded LRU/priority eviction，但不能因淘汰发布“对象消失”；只让观察过期。当前 focused/attended 项优先保留。
- safe projection、internal spatial record 和 evidence 分别有 cap；过期项先清理。内存超限只丢低优先级未发布/最近项，不能跨 world 恢复。
- 普通 trace 不记录缓存 key、坐标或 trackingKey。

## 15. 生命周期、revision 与竞态

| 事件 | projection | information revision | ref/cursor |
|---|---|---:|---|
| 新连接尝试 | 清空，unavailable | 可见 availability 改变则 +1 | 旧 epoch 全失效 |
| initial spawn ready | 从空投影开始 | +1 | 旧 world 不复用 |
| observer yaw/pitch/position 改变 | 重新调度；只提交安全差异 | 安全结果变化才 +1 | 源 revision 改变后旧 ref 失效 |
| hidden tracked movement | 不变 | 不变 | 不变 |
| 当前可见/细节/relative band 改变 | 原子提交 | +1 | 旧 ref/cursor 失效 |
| raw block update 在墙后 | 不变 | 不变 | 不变 |
| visible material/block change | 重验并提交 | +1 | 旧 ref/cursor 失效 |
| UI/presentation mask 改变 | 只重验受影响 sample regions；缺证据时 unavailable | 公开 value/availability 改变才 +1 | UI scope race 可重试；仅 provenance 改变不失效 ref/cursor |
| chunk unload 命中当前视口 | 对应观察到期/unknown boundary | 安全投影变化才 +1 | 旧 ref/cursor 失效 |
| death/respawn transition | 立即清空并 unavailable | +1 | 旧身体观察失效，epoch 不必改变 |
| dimension/world change | 先清空，再在新 scope 重建 | +1 | 旧 scope 全失效 |
| disconnect/process stop | 清空并 unavailable | +1 | 全失效；重启不恢复 current |
| policy/schema 改变 | 重建 projection | catalog/schema/adapter revision 更新 | 旧 schema/ref/cursor 失效 |

Projection、Scope 与 Runtime invalidation 由 composition transition barrier 的同一提交序列驱动。Read 前后 scope、selector source revision 或目标 Provider 的公开 `informationRevision` 改变时，Runtime 丢弃结果；Provider 不自行把旧 snapshot 包装到新 scope。普通无 cursor/selector Read 的最后一项依赖 PR #66 引入的共享后验，在该 Runtime 修订进入目标基线前不得合并 viewport 生产 Provider。

## 16. 文件布局与依赖方向

```text
src/
├── minecraft/driver/
│   ├── viewport-candidate-adapter.ts
│   ├── optical-voxel-adapter.ts
│   ├── viewport-presentation-adapter.ts
│   └── viewport-candidate-adapter.test.ts
├── perception/viewport/
│   ├── candidate-contracts.ts
│   ├── policy.ts
│   ├── fov.ts
│   ├── sample-points.ts
│   ├── dda.ts
│   ├── material-policy.ts
│   ├── entity-tracker.ts
│   ├── block-observer.ts
│   ├── scene-builder.ts
│   ├── scheduler.ts
│   ├── presentation-gate.ts
│   ├── safe-contracts.ts
│   ├── projection.ts
│   ├── spatial-resolver.ts
│   ├── internal/
│   │   └── spatial-record.ts
│   └── *.test.ts
└── information/
    ├── source-ports/
    │   └── viewport.ts
    └── providers/
        ├── viewport.ts
        └── viewport.test.ts
```

依赖方向：

```text
information/providers/viewport → information/source-ports/viewport
perception/viewport projection implements ViewportInformationSource
minecraft/driver adapters implement ProtocolObservationSource
local UI/renderer adapters implement ViewportPresentationSource
app composition wires UI identity + presentation + protocol source → engine → projection → provider
```

禁止：

- `information/providers/viewport` 导入 `perception/viewport/internal`、minecraft、Mineflayer、Prismarine、其他 Provider 或 Context；
- `grounding/companion/context/models/memory` 导入 candidate、tracker、spatial record、Bot/World/Entity/Block/Vec3；
- 创建 `MinecraftSnapshotV1 → ViewportProvider` adapter、snapshot fallback、dual write 或 trackedPlayers compatibility field；
- 从 `findBlocks`、pathfinder 或 collision matcher 直接签发 observation/grounded handle；
- 将 privileged diagnostic DTO 复用为 Provider source/values。

## 17. 测试设计

### 17.1 单元测试

- yaw/pitch、focused/peripheral 边界、vertical/direction/distance band 与 epsilon；
- 眼睛高度随 standing/sneaking/swimming/fall-flying 改变；
- entity AABB 多点采样、partial exposure 和 detail/nameplate 发布阈值；
- DDA 正/负轴、轴对齐、角点/边界、起始 voxel、目标 voxel、最大距离/step/budget；
- `beginOpticalRead` 的 expected scope/revision 不匹配不签 token；同 token 的全部 voxel read 固定同一 revision，任一中途 block/source change 返回 stale/unknown，提交前 validation 失败时整条 proof 丢弃；
- 旧 session/world 的 block-change event、倒退 source revision 和 world transition 后迟到 event 被拒绝；合法 current event 只触发当前 token 下的重新观察；
- solid、glass、leaves、water、plants、fence/pane/bars、door/trapdoor/gate、slab/stairs 和 unknown block；
- door 等 state-aware optical shape 变化；
- unloaded voxel 立即 unknown 且不继续读下一个 voxel；
- current → recent_occluded/out_of_view/lost → expired → rediscovered；
- protocol_removed 不生成死亡事实，recent 不跟随 raw movement；
- wall/back/hidden block change 不改变安全 revision；
- full/partial/empty publishable sample regions、Screen/overlay mask、unavailable `UiFieldState` 与 source identity mismatch 全部 fail closed；
- 仅 presentation acquisition/provenance 改变时内部游标可变，但安全 projection value/availability/revision/sourceRevision/observedAt 均不变；
- scene 只引用安全 observations，稳定排序/去重/TTL；
- safe DTO/ref/error/普通 trace 不含坐标、trackingKey、raw ID、NBT 或 hidden canary。

### 17.2 属性与变形测试

- 增加一个与所有有效射线相交的 opaque shape，不能提高可见点数/exposure/certainty。
- 将目标旋转到 FOV 外，不能保留 current visual；旋转 observer 与场景同角度，relative bands 保持等价。
- 将任一中间 loaded voxel 改为 unloaded，结果只能从 visible/partial 变 unknown，不能继续 visible。
- 添加任意数量墙后/背后 raw candidate，不改变安全 projection、revision、公开 truncation 或普通 trace。
- recent 状态下任意改变 raw tracked position，模型可见 relative/motion 不变，只随时钟扩大 uncertainty/过期。
- 相同 frame、policy、scope 与单调时钟产生相同 observation 顺序、revision 和 JSON。
- ref payload 任意生成都符合 strict allowlist，递归扫描不存在 `x/y/z/position/coordinates/entityId/trackingKey`。

### 17.3 Provider 契约与 Runtime 回归

`ViewportProvider` 必跑共享 `assertInformationProviderContract`，并验证：

- Catalog/Help 可发现三个字段、precision、sourceKinds、availability、selector 和 limits；
- 只返回请求字段，Zod parsed data 去掉/拒绝嵌套未知键；
- 三字段一次 Read 使用同一 source snapshot/kind/revision；
- empty observations 是合法空数组，不生成“周围没有实体/方块”断言；
- selector 只返回原安全观察，不增加精度；wrong kind/world/revision/expired 拒绝；
- ref payload 无坐标，projection revision 在目标 Read 前后变化返回 invalid_selector；
- cursor 绑定查询形状/revision 并一次性消费；分页期间 revision 改变返回 invalid_page；
- connection/world/dimension/ui race 返回 scope_changed；
- 无 cursor/selector 的普通 viewport Read 中发生 death/respawn 或 projection commit，PR #66 后验拒绝旧 result；仅内部 optical/presentation revision 改变但公开 projection 不变时 Read 仍成功且公开 metadata 稳定；
- Provider 不读取 #54 的不存在字段、不二次 capture UI；presentation unavailable 原样映射为 `not_supported/not_exposed/not_currently_displayed`；
- Abort、timeout、result byte/page/ref issuance limit 生效；
- Provider extra field、错误 source kind、raw object、超大结果和异常转为 sanitized provider_failed；
- architecture scan 阻止 Context/Model/Grounding 从 snapshot/tracked/driver 旁路。

### 17.4 Paper 1.21.1 场景

| 场景 | 布置/动作 | O2 断言 |
|---|---|---|
| 前方/周边/背后 | 玩家以固定半径绕 Bot | focused/peripheral 正确；背后无 current visual/ref |
| 实心墙 | 玩家/僵尸/矿物分别置于墙后 | 从未看见者完全不出现；recent 只在 TTL 内且不跟随 |
| 玻璃 | 玩家、方块在 glass/stained glass 后 | 按 policy 可见，玻璃表面与后方证据分开 |
| 树叶 | 1 层与多层 leaves | attenuation/exposure 单调，不能无限穿透 |
| 水/植物 | 浸水、隔水、草/花前后 | 材质语义与真人客户端对照，无 full-voxel 误遮挡 |
| 栅栏/铁栏/玻璃板 | 射线穿空隙与命中杆体 | optical shape 正确 |
| 门/活板门 | 开关、朝向变化 | 当前 visual revision 改变，旧 ref 失效 |
| 半砖/楼梯 | 空余空间与实体部分暴露 | partial/visible 与 shape 一致 |
| unknown chunk | 视距边缘/fixture unload | 粗方向 unknown，不穿透、不暴露 chunk 坐标 |
| 转身与移动遮挡 | 看见玩家后转身/玩家入墙后 | current → recent → expired，raw tracked motion 不更新 |
| block change | 可见/墙后各修改方块 | 只有可见变化进入 projection |
| world/dimension/death | transition 时持有 ref/cursor | projection 清空，旧引用确定失效 |
| 压力 | 大量实体、block updates 与 active control | budgets 有界；取消/协议循环不被阻塞，无 raw count 侧信道 |

Paper 坐标、服务端命令、tracked table 和 protocol trace 只存在于 O0 setup/assertion recorder。Companion 阶段只经 Information Runtime 读取 O2；生产 source 不能导入 Paper oracle。

Paper 不能证明 inventory/chat/F3/Tab 的 framebuffer mask 或 Screen 后哪些像素实际可见；这些只用版本化 presentation fixture 和真人客户端验收。Paper 场景不得以“服务端知道对象仍在前方”替代 UI 遮挡证明。

### 17.5 真人验收

固定 Minecraft 1.21.1、FOV、GUI、资源包和视距，真人客户端与 Bot 同姿态录制：

- focused/peripheral/背后边界和转头后的出现时机；
- 实体头/身体局部露出、蹲伏/nameplate、手持物与远距离细节；
- 实心墙、普通/染色玻璃、一层/多层树叶、水、植物、栅栏、门、半砖与楼梯；
- 视距边缘、chunk 尚未加载和快速转身时的 unknown/TTL；
- world、inventory/container、chat、F3 与 Tab 下的 full/partial/empty publishable regions；被 UI 遮住的样本不出现，关闭 UI 后从新视觉帧重建；
- 高实体密度下主观上是否仍保留朋友与 focused 目标，且没有冻结控制；
- 模型 Read/Context 中不存在 tracked 表、坐标、完整 blocks、操作器名或墙后 canary。

记录录像时间点、observer pose、policy revision、Read trace ID 与 O0/O2 差分；个人显示名和服务器信息先脱敏。真人验收只校准 policy/threshold，不可放宽非泄漏不变量。

### 17.6 性能基准

独立 benchmark 固定候选、AABB、材质和 ray fan，记录 p50/p95/p99：candidate 数、entity sample 数、ray 数、voxel visit、cycle CPU、projection bytes、tracker/evidence/cache entries 与 event-loop lag。

发布门不是“平均很快”，而是：

- hard cap 在最坏 fixture 下始终生效；
- Abort 与 scope invalidation 在一个 slice 内可见；
- 压力降级顺序确定；
- 被跳过内容不会错误变为 invisible/absent；
- hidden candidate 数变化不影响模型可见 metadata/revision。

## 18. 可观察性

普通 trace 只记录：

- interface/read ID、information/source/policy revision、返回 observation 数、公开 bytes、page/selector 状态；
- current/recent/entity/block 的安全数量带；
- scope race、stale ref/cursor、deadline、公开 output truncation 和 contract failure；
- 不含值的 evidence IDs 与 correlation ID。

Privileged diagnostics 可以记录：

- raw candidate、sample、ray、voxel visit、材质命中、DDA stop reason 与耗时；
- trackingKey、内部 spatial record、cache hit、budget drop 与 projection rejection；
- 精确 observer/target/block 坐标，仅在本地受控调试 sink 中。

两类必须使用不同类型、不同 logger method 和不同 sink。普通 trace/error/Context/Journal 禁止精确坐标、raw registry/numeric ID、trackingKey、NBT、blocking block、完整异常或服务器信息。

## 19. 实施切片

### S0：共享视觉契约与 fixture

1. 定义 candidate/safe DTO、strict schema、policy 与 test fixture builder。
2. 定义 `ProtocolObservationSource`、scope-bound `OpticalReadTokenV1`、`ViewportPresentationSource`、`ViewportInformationSource` 和内部 spatial record 边界。
3. 加 architecture scan：raw Minecraft 类型不得越过 Driver；internal spatial record 不得进入认知目录。
4. 固定 1.21.1 最小材质 fixture 与 benchmark harness。

### S1：#46 视口与实体状态机

1. 实现 pose/FOV/range/near-field 调度与 AABB sample points。
2. 先接最小 opaque/transparent visibility port，完成 entity current/recent/lost tracker。
3. 实现安全 entity projection、revision/TTL、Attention notification 和隐藏 candidate 不变性测试。
4. 加前方/周边/背后/墙后/重连 Paper 最小场景。

### S2：#47 DDA、材质与方块观察

1. 实现独立 DDA、token-bound optical reads、提交前 token validation、optical shapes 与 unloaded/stale/budget unknown。
2. 实现 solid/glass/leaves/water/plants/fence/door/slab/stairs/unknown policy 与属性测试。
3. 实现 ray fan、block change 复核、surface/cluster 与安全 scene builder。
4. 用正式 DDA 替换 S1 最小 visibility port；不保留 dual path。

S1 与 S2 在 S0 contract 合并后可用不同文件所有权并行；S1 不复制临时 DDA 实现，使用明确测试 port。

### S3：Provider、ref 与下游接线

1. 先合入 #63 与 PR #66；用 viewport fixture 证明 cursor 按 `connection/world/dimension/ui` 绑定且普通 Read 会拒绝公开 projection race，不绑定无关 screen、不在本模块复制 Runtime 修复。
2. 实现 `ViewportProvider`、Help/Read/pagination/selector 和公共契约测试。
3. Observation ref 接公共 Ref Store；验证 payload 无坐标与源 revision 前后校验。
4. 接 Attention 安全通知、Grounding evidence adapter 与 scoped resolver stub；不实现 v0.3 行为计划。
5. #58 composition 注册 Provider，并删除 trackedPlayers/snapshot 到模型的旧入口；不做 fallback。
6. 将 #54 UI identity/revision、presentation mask 与 protocol frame 原子归约；禁止 Provider Read 二次 capture UI。

### S4：验收与校准

1. 跑 `information-viewport-optics`、non-leakage canary、竞态和压力场景。
2. 真人对照 FOV、partial materials、identity/details 与 TTL。
3. 根据数据冻结 `ViewportOpticalPolicyV1`、`ViewportBudgetPolicyV1` revision 与阈值。
4. #57 汇总 Provider contract、架构扫描和 v0.2 P4 迁移门。

## 20. 开放校准参数与实现前 spike

以下参数必须显式版本化，但在实测前不冻结：

1. focused/peripheral 水平与垂直 FOV、边界 epsilon 和 Minecraft 客户端 FOV 设置的映射；
2. near-field 半径、实体/方块/scene 最大距离和方向/距离 bands；
3. 各实体 AABB sample 点、partial exposure、远距离 detail/nameplate 阈值；
4. glass/leaves/water/fence/door 等 attenuation、optical shapes 与累计阈值；
5. ray fan 分辨率、attention/gaze 加密区域和 scene 聚类阈值；
6. focused/ordinary/scene 刷新率、current/recent TTL、publish debounce；
7. candidate/sample/ray/voxel/CPU/memory/output hard caps 与降级触发；
8. “主要社交对象”的 Attention 关联怎样只引用安全 identity/evidence，而不读取 tracked 坐标；
9. 原版 nameplate 在蹲伏、隐身、队伍规则、距离和遮挡下的实际显示语义；未完成 fixture 前默认不发布有争议 identity；
10. Mineflayer 1.21.1 block state/collision shape 是否足以生成门、栅栏、楼梯等 optical shapes；不足时维护锁定版本 policy override，不能退回 full world renderer。
11. 1.21.1 inventory/container/chat/F3/Tab 的保守 presentation masks；未完成 fixture 的 Screen/overlay 默认 unavailable，不能假设背景全可见。

这些开放项只允许调整 adapter、policy、预算与精度，不允许改变“未验证不发布、unknown fail closed、ref 无坐标、Controller 不搜索、模型无 raw 世界”的权限边界。

## 21. 完成定义

1. `viewport_information` 可通过 Catalog → Help → Read 发现 observations、scene_summary、coverage 的类型、precision、sourceKinds、availability、selector、pagination 与 limits。
2. Driver raw 类型止于 candidate DTO；Provider 只读取安全 `ViewportInformationSource`，每次 Read 只有 `viewport_projection` 单一 source。
3. identity/tracked/visible/spatially-known 在类型、状态机、日志和测试中独立，背后/墙后 tracked 对象不会成为 current fact。
4. DDA、optical shapes、opaque/partial/transparent 与 unknown chunk 有确定性实现和材质 fixture。
5. current/recent/lost/expired、重发现、世界/维度/death/重连清理与 revision/ref/cursor 失效全部可测试。
6. observation ref payload 不含坐标/raw ID，签发源 revision 在 Read 前后校验；selector 不能扩大视野或精度。
7. 模型结果、Context、普通 trace、Journal 和 Memory 不含 tracked 表、完整 blocks、可执行坐标、操作器/技能目录或隐藏 canary。
8. Grounding 只验证有来源观察，Controller 只解析已选 handle 的局部实现解且不能搜索/回流；模块没有对象/任务专用规划接口。
9. CPU、射线、voxel、内存、刷新、分页、字节与 ref 均有 hard cap；压力下降级不阻塞协议、安全或取消，也不把未检查写成不可见。
10. 单元、属性、Provider 契约、Runtime 竞态、Paper optics、non-leakage、性能和真人验收全部通过，校准 policy revision 已冻结。
11. #54 只提供 UI identity/revision；#34 独立 presentation source 对 Screen/overlay 遮挡 fail closed，且不存在未定义的 `viewportPublication` 或按 coarse family/title 猜背景可见性的路径。
