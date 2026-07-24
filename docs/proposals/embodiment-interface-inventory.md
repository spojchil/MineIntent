---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
---

# 具身能力与接口清单

> 本文混合了仍有价值的协议/API 普查和已经撤回的控制契约。逐 tick `BodyInputPlan` 结论已于 2026-07-14 撤回；v0.1 API 清单也不描述最新实验分支。对应 Issue：[#32](https://github.com/spojchil/mineintent/issues/32)、[#52](https://github.com/spojchil/mineintent/issues/52)。相关基线：[ADR 0005](../decisions/0005-limit-mineflayer-to-protocol-driver.md)、[决策契约](../architecture/decision-contract-and-context.md)。

## 1. 目的

本文档保留 MineIntent 对感知、身体控制、协议适配、安全和结果接口的普查证据。合法信息接口的当前规范见[合法信息接口、Help 发现与 UI 会话](../architecture/information-access-and-ui.md)；控制器粒度和行为计划留到 v0.3 重新设计。

核心边界是：

> 系统先提供正常 Minecraft 客户端可合法展示或检查的信息接口。执行层可以使用连续控制器、局部精确解和 Mineflayer 能力，但必须受信息来源、目标授权、作用域、取消和结果证据约束。

此前由本清单推导出的“Behavior 只能逐 tick 输出键鼠样本”过度收紧了执行层，现已撤回。本文保留 packet、输入类别和 v0.1 API 审计，但不再把它们等同于上层唯一合法控制抽象。

### 1.1 版本与调研依据

本次普查以 Java Edition 1.21.1、多人生存模式和当前安装的 `mineflayer@4.37.1` / `minecraft-data@3.111.0` 为基线。证据来源包括：

- Minecraft 官方 [Java 版快捷键清单](https://help.minecraft.net/hc/en-us/articles/360059148111)：核对移动、跳跃、潜行、冲刺、左/中/右键、快捷栏、丢弃、主副手、背包、聊天、玩家列表和视角切换。
- Minecraft 官方 [Java 版辅助功能设置](https://help.minecraft.net/hc/en-us/articles/360061018612-Accessibility-Settings-for-Minecraft-Java-Edition)：确认 Attack/Destroy 和 Use Item/Place Block 是可按住/切换的玩家输入，不是目标专用动作。
- 当前安装的 Mineflayer 源码与 [API 文档](https://github.com/PrismarineJS/mineflayer/blob/master/docs/api.md)：审计 `lookAt`、`dig(block)`、`placeBlock`、`activateEntity`、`navigate` 等高层封装中哪些会代替角色选目标或读取 raw world。
- 当前安装的 1.21.1 [`protocol.json`](https://github.com/PrismarineJS/minecraft-data/blob/master/data/pc/1.21.1/protocol.json)：逐项审计 Play 状态所有 serverbound packet，区分自动协议维护、世界输入派生事务、GUI 事务、游戏模式/权限能力和测试/扩展通道。

包版本和协议版本只是当前证据基线。最终 schema 必须版本化；升级 Minecraft 时重跑本普查，不把 1.21.1 的 packet 细节当成永久不变的产品语义。

本轮完成审计结果：1.21.1 Play serverbound packet `58/58` 已分类；当前 `MinecraftControlsApi` 方法 `7/7`、原型 skill `5/5` 已给出迁移判定；本文引用的本地设计/参考文件均存在。

### 1.2 参考 Agent 项目的反例价值

本地参考项目也证明了为什么要先做接口审计：

- Mindcraft 的 [actions.js](https://github.com/mindcraft-bots/mindcraft/blob/develop/src/agent/commands/actions.js) 直接向模型提供 `followPlayer`、`goToPosition`、`collectBlocks`、`attackPlayer`、`lookAtPlayer` 等命令，并由 [vision_interpreter.js](https://github.com/mindcraft-bots/mindcraft/blob/develop/src/agent/vision/vision_interpreter.js) 读玩家精确坐标后调 `lookAt`。
- Voyager 的 [action.py](https://github.com/MineDojo/Voyager/blob/main/voyager/agents/action.py) 向模型提供 `mineBlock` 等基础 skill，[control_primitives/mineBlock.js](https://github.com/MineDojo/Voyager/blob/main/voyager/control_primitives/mineBlock.js) 再组合区块搜索、CollectBlock 和高层寻路。

这些设计对“尽快完成 benchmark 任务”合理，但会将话语理解、目标选择、隐藏世界查询和长流程固化成命令表。MineIntent 可以参考其错误处理、库存事务或测试场景，但不得把这些 skill 作为 v0.2 接口迁入。

## 2. 分层定义

```text
玩家话语 + 当前情境
        ↓
Companion：理解目的、方法强度和社交含义
        ↓  Semantic Goal Contract（不可执行）
Grounding：将语义引用绑定到有证据的认知对象
        ↓  Grounded Goal Contract（仍不可执行）
Behavior Synthesizer：结合可供性与已知空间临时形成计划
        ↓  Behavior / Controller Contract（v0.3 待定）
Action Runtime / Safety：资源、时效、中断和局部安全校验
        ↓
Motor Controller：鼠标、按键、物品栏和界面输入
        ↓
Protocol Driver：将当前身体输入翻译为合法 Minecraft 协议时序
        ↓
Perception / Motor Feedback / Outcome Verification
```

每层只通过契约交换数据。上层目标不是下层函数名，规划结果也不会因为成功过一次就自动升格为新接口。

## 3. 新增接口的审查门

任何新接口在实现前必须逐项回答：

1. 普通 Java 版客户端是否真的拥有这个输入、感觉或协议事务？
2. 它是一个局部身体因果，还是已经包含目标选择、寻路、工具选择或循环验证的计划？
3. 名称或参数是否编码了玩家措辞、对象类别、任务原因或特定角色？
4. 它是否需要视野外坐标、原始实体表、已加载区块或其他角色不应知道的信息？
5. 如果目的不变，是否存在其他合理方法？如果存在，当前候选通常是计划而不是接口。
6. 如果同一身体过程可服务不同目的，该接口是否把目的错误固化进了名称？
7. 成功是由真实新观察证明，还是只因为 API/Promise 返回？
8. 调用者是否可以借它绕过 Grounding、Epistemic Map、资源锁、停止或安全预检？

第 2、3、4、8 项任一命中，默认拒绝该设计，直到完成拆分。

## 4. 应实现的接口清单

### 4.1 语义与规划边界

| 契约 | 责任 | 不得包含 |
|---|---|---|
| `SemanticGoalContract` | 表达期望状态的组合、语义引用和方法强度 | skill、输入模板、协议事务名、协议 ID、世界坐标、预制任务类型 |
| `GroundingResult` | 产生有证据句柄，区分 `complete/partial/invalid/unavailable` | 使用 raw world 补齐位置、替规划器选方法 |
| `BehaviorSynthesisResult` | 返回有界内部计划引用或明确缺失 | 自行聊天、跳过信息/目标预检、执行任意代码 |

这三项是组件通信契约，不是角色可调用的身体能力目录。

### 4.2 认知感知端口

| 端口 | 合法输出 | 关键限制 |
|---|---|---|
| `ViewportObservationPort` | 当前相机真正可呈现的方块面、流体、实体可见部分、动画、粒子、天气、光照、天空、准星命中和遮挡 | 不输出视野外已加载对象、被遮挡方块内容或 raw entity 表；对象使用观察句柄而非协议 ID |
| `SoundObservationPort` | 听到的声音类别、相对方向、音量、时间和不确定性 | 不把声音包坐标直接当作精确定位 |
| `ProprioceptionPort` | 自身运动/朝向感、着地、姿势、碰撞/受阻感、骑乘状态、生命、饥饿、氧气、冷却和效果 | 默认不输出绝对 XYZ 或精确角度；只有当前 HUD/debug 原本可见时才由对应观察端口提供数值。碰撞不泄露未看见方块的种类 |
| `HudObservationPort` | 快捷栏、主副手、生命/护甲/饥饿/空气/XP、准星、计分板、boss bar、title/action bar、toast、字幕 | 只输出原版界面在当前设置下可呈现内容 |
| `InventoryObservationPort` | 已打开/自身背包可见槽位、选中栏、交易确认 | 未打开容器的内容不可见 |
| `ScreenObservationPort` | 当前 screen revision、可见槽位/按钮/文本框、焦点、鼠标和游标物品 | 不把“制作、交易、重生”等按钮直接提升为高层任务接口 |
| `PlayerListObservationPort` | 按住玩家列表键时原版列表可呈现的身份、显示名和延迟摘要 | 协议已跟踪不等于 Companion 始终注意到列表 |
| `VisibleTextPort` | 当前视口/已打开界面中真正可读的告示牌、书页、地图、物品名与 GUI 文本 | 不读取未打开容器、未翻到书页或遮挡文本 |
| `InteractionFeedbackPort` | 攻击/使用阶段、破坏进度、服务端更新、GUI/transaction 回执 | Promise 完成不等于业务成功 |
| `ChatObservationPort` | 真实聊天、系统消息及来源信息 | 世界文本不获得系统指令权限 |
| `LifecycleObservationPort` | 登录、出生、死亡、重生、维度/世界切换、踢出、资源包请求和断线原因 | 生命周期切换必须提升 epoch；资源包与重连决策由用户/backend 策略处理 |

“找最近的木头”不是感知端口。感知只输出当前合法观察，候选的筛选、排序和选择属于规划。

### 4.3 玩家输入与协议调研（非接受契约）

> 本节 4.3.1–4.3.4 记录 2026-07-13 曾提出的纯输入帧方案，已于 2026-07-14 撤回。它可以用于核对原版输入和协议时序，但不得作为 v0.2 实现要求或 v0.3 控制器结论。

从普通玩家的输入视角看，身体接口很小：移动视角、输入移动方向、跳跃、左键、右键，再加上冲刺、潜行、快捷栏、丢弃、交换主副手、背包和聊天等有限快捷键。

“移动位置”在这里指玩家通过方向输入让客户端物理改变位置，不是直接设置坐标、传送或调用 `navigateTo`。同理，挖掘、放置、攻击、开门、吃东西都不是额外玩家输入，而是当前视角、位置、持有物品、准星命中与左/右键共同产生的游戏结果。

下列客户端输入类别是控制器最终必须能够产生的底层效果，但不是上层能力面的最大边界：

| 输入 | 语义 | 参数边界 |
|---|---|---|
| `applyLookDelta` | 像鼠标一样改变 yaw/pitch | 只接受角度增量/速率和时间，不接受玩家/方块/实体目标 |
| `setMoveAxes` | 像 WASD 一样设置前后与左右输入 | 只接受 forward/strafe 轴或按键状态，不接受坐标、某人/某物或整条路径 |
| `setJumpPressed` | 按下/释放跳跃键 | 不接受“跳上那个方块”等结果目标 |
| `setPrimaryPressed` | 按下/释放左键 | 作用对象只能由当前准星、触达距离和服务端规则决定 |
| `setSecondaryPressed` | 按下/释放右键 | 吃喝、使用、放置、打开等都是当前物品+准星+情境的结果 |
| `pressPickInput` | 点击中键/选取方块键 | 只对当前准星命中生效，物品获取仍受生存/创造规则限制 |
| `setSprintPressed` | 按下/释放冲刺键 | 是输入状态，不是“快速到达”规划 |
| `setSneakPressed` | 按下/释放潜行键 | 是输入状态，不自行选择边缘或隐蔽目标 |
| `selectHotbarSlot` | 选择快捷栏 | 只接受槽位，“选最好斧头”属于规划 |
| `setPlayerListPressed` | 按下/释放玩家列表键 | 只有按住期间才向认知层提供列表观察 |
| `pressUiShortcut` | 背包、聊天、成就、游戏菜单或社交互动的原版按键 edge | 只改变输入焦点/打开对应原版界面，不接受“制作/举报/退出”等后续目标 |
| `pressViewShortcut` | 切换视角、HUD 或 debug 的原版按键 edge | 只改变客户端观察状态，受当前服务器和用户策略限制 |
| `ScreenInputFrame` | 当前界面中的鼠标、滚轮、修饰键、文字和按键输入 | 必须带 screen revision，不接受“制作斧头”“选择交易”等结果目标 |
| `dropInput` | 丢出当前选中物品的一个/一组 | 受背包 revision 和资源锁约束 |
| `swapHandsInput` | 交换主副手 | 只表达客户端输入 |
| `releaseAllInputs` | 立即释放可持续按键并结束当前交互 | 用于停止、取消、失联和安全抢占 |

聊天发送是独立 `SpeechOutputPort`，因为它有自己的长度、频率、时机和承诺真实性契约，不与身体输入混成一个“行动”。

上表仅用于输入/协议普查，不要求每一行成为对外 RPC，也不禁止合法 controller 接收 grounded target、局部空间约束或在内部运行循环。是否允许某种控制参数取决于其信息来源和作用域，而不是它是否长得像物理键码。

#### 4.3.1 World Input Frame（撤回的候选）

世界模式的最大输入契约为：

```ts
interface WorldInputFrameV1 {
  protocol: 'mineintent.motor.world-input.v1'
  sequence: number
  connectionEpoch: number
  leaseId: string
  motorTick: number
  basedOnPoseRevision: number
  lookDelta?: { yaw: number; pitch: number }
  move: { forward: -1 | 0 | 1; strafe: -1 | 0 | 1 }
  held: {
    jump: boolean
    sprint: boolean
    sneak: boolean
    primary: boolean
    secondary: boolean
    playerList: boolean
  }
  edge?:
    | { kind: 'pick' }
    | { kind: 'select_hotbar'; slot: 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 }
    | { kind: 'scroll_hotbar'; steps: number }
    | { kind: 'drop'; wholeStack: boolean }
    | { kind: 'swap_hands' }
    | { kind: 'open_inventory' }
    | { kind: 'open_chat'; initialText?: '/' }
    | { kind: 'open_advancements' }
    | { kind: 'open_game_menu' }
    | { kind: 'open_social_interactions' }
    | { kind: 'cycle_perspective' }
    | { kind: 'toggle_hud' }
    | { kind: 'toggle_debug' }
}
```

- `sequence` 严格递增，`motorTick` 指明本次采样所属的 Motor 控制 tick；过期 frame 不得在新 frame 后重放。持续键是状态，中键、丢弃、换手等是 edge，不得因重试重复执行。有效 lease 若超过 watchdog 时间没有新 frame，Motor 自动 release-all。
- 左/右键的按下、持续和释放都有语义：持续左键可形成挖掘/攻击循环，持续右键可形成进食、拉弓、持盾等，释放必须结束对应协议事务。
- 同一 `move/jump/sneak` 输入在步行、游泳、攀爬、骑乘、创造飞行或鞋翅状态下可由客户端规则产生不同结果。Behavior 不调用 `moveVehicle`、`dismount`、`startFlying` 等目标化捷径；driver 根据当前骑乘/能力状态生成合法 packet。
- `open_game_menu` 在多人世界不会暂停服务器，只切换输入焦点；断开服务器、修改设置和举报等后续控件分别受 supervisor/用户策略约束。`open_social_interactions` 默认只读，任何屏蔽/举报副作用需额外授权。
- `cycle_perspective`/`toggle_debug` 不发生服务端世界动作，但会改变角色可获得的客户端观察，所以不能完全忽略。第三人称需正确实现相机碰撞和自身遮挡；debug 屏幕只能暴露原版在当前服务器 `reducedDebugInfo` 规则下会显示的字段。
- 主手/副手不是 right-click 的上层参数。driver 依原版优先级、持有物和准星决定实际 hand；Behavior 若希望改变条件，只能通过选择快捷栏或交换主副手输入。
- 该 frame 表达的是逻辑控制状态，不是某台电脑的物理键码。键位重映射、鼠标灵敏度和辅助功能的 hold/toggle 模式由客户端适配层处理，不改变 Behavior 的能力面。
- 自动跳跃、视角、HUD/debug 可见性、聊天/命令权限和资源包接受策略属于显式客户端状态或用户策略；它们必须进入 observation/config revision，不能作为规划器暗中可用的动作。v0.2 默认关闭自动跳跃，避免移动结果来自未记录的隐藏输入。

#### 4.3.2 Screen Input Frame（撤回的候选）

当背包、容器、配方书、交易、铁砧、告示牌、书、聊天或死亡屏幕打开时，鼠标不再转动视角，左/右键也不再攻击/使用世界。屏幕模式必须使用独立契约：

```ts
interface ScreenInputFrameV1 {
  protocol: 'mineintent.motor.screen-input.v1'
  sequence: number
  connectionEpoch: number
  leaseId: string
  motorTick: number
  screenRevision: number
  cursorDelta?: { x: number; y: number }
  pointer?: {
    button: 'left' | 'right' | 'middle'
    phase: 'press' | 'release'
  }
  scrollSteps?: number
  modifiers: { shift: boolean; control: boolean }
  keyEdge?:
    | 'escape' | 'enter' | 'tab' | 'backspace' | 'delete' | 'space'
    | 'arrow_up' | 'arrow_down' | 'arrow_left' | 'arrow_right'
    | 'home' | 'end' | 'page_up' | 'page_down'
    | 'hotbar_1' | 'hotbar_2' | 'hotbar_3' | 'hotbar_4' | 'hotbar_5'
    | 'hotbar_6' | 'hotbar_7' | 'hotbar_8' | 'hotbar_9'
  textInput?: string
}
```

- 屏幕输入必须引用当前 `screenRevision`；界面关闭、换页、服务端更正或 transaction 变更后拒绝旧输入。
- pointer press、cursor frame 与 pointer release 的连续时序自然表达拖拽；相同位置的有界重复 press/release 自然表达双击。不得另加 `dragStack`、`doubleClickSlot` 等库存语义捷径。
- 实现可在内部把当前光标命中翻译为 slot/button ID，但 Behavior 不获得 `craftAxe`、`selectTrade`、`respawn`、`wake` 等高层 GUI 命令。重生和离开床铺是点击当前可见屏幕控件后的协议结果。
- 告示牌、书、铁砧重命名与聊天都复用文本输入，但发布到聊天/世界的文本仍经过语言真实性、权限和长度契约。
- 无渲染 GUI 实现若使用结构化槽位/控件句柄作为可访问性适配，句柄必须由 `ScreenObservationPort` 在本 revision 签发，其能力不得超过对同一可见 GUI 的鼠标/键盘输入。

#### 4.3.3 输入接收、抢占与反馈（部分原则保留）

Motor 不是“调用一次函数等待成功”，而是接收有 revision 的输入流。最小回执和反馈契约为：

```ts
interface MotorInputReceiptV1 {
  protocol: 'mineintent.motor.input-receipt.v1'
  connectionEpoch: number
  leaseId: string
  sequence: number
  status: 'accepted' | 'rejected'
  reason?:
    | 'stale_connection'
    | 'stale_pose'
    | 'stale_screen'
    | 'out_of_order'
    | 'input_conflict'
    | 'unsupported_input_surface'
    | 'safety_denied'
    | 'not_ready'
}

type MotorFeedbackEventV1 =
  | { kind: 'input_applied'; connectionEpoch: number; leaseId: string; sequence: number }
  | { kind: 'input_rejected'; connectionEpoch: number; leaseId: string; sequence: number; reason: MotorInputReceiptV1['reason'] }
  | { kind: 'interaction_phase'; input: 'primary' | 'secondary'; phase: 'started' | 'continued' | 'released' | 'cancelled'; evidenceIds: string[] }
  | { kind: 'server_correction'; correction: 'pose' | 'inventory' | 'screen' | 'interaction'; evidenceIds: string[] }
  | { kind: 'screen_changed'; screenRevision: number; evidenceIds: string[] }
  | { kind: 'connection_epoch_changed'; connectionEpoch: number; reason: string }
```

- `accepted` 只表示 Motor 接管了该输入，不表示命中、破坏、放置、伤害、进食、制作或任何上层目标成功。
- 同一时刻只有一个有效的输入 lease。停止、玩家接管、安全反射或新计划抢占时，Runtime 先提升 lease/revision，再释放全部持续输入；旧计划后续 frame 因 revision 失效被拒绝。
- `releaseAllInputs` 必须幂等，释放移动、跳跃、冲刺、潜行、左右键和 Tab，并取消尚未完成的挖掘/使用/GUI pointer phase。普通释放不擅自关闭已打开界面；断线、死亡、维度切换和 screen replacement 另按生命周期清理 transaction。
- `server_correction` 与新 Perception 才是 Outcome Verification 的证据来源。driver 可以在隔离调试日志中记录 packet 目标，但不得把 raw entity ID/方块坐标回流成 Companion 可搜索的世界视图。

#### 4.3.4 Body Input Plan（已撤回）

Behavior 输出短时域输入样本，Action Runtime 为每个样本补齐当前 epoch、lease、sequence、motor tick 与 revision 后才提交 Motor：

```ts
interface BodyInputPlanV1 {
  protocol: 'mineintent.body-input-plan.v1'
  id: string
  intentEffectId: string
  connectionEpoch: number
  basedOnObservationIds: string[]
  leaseId: string
  requiredResources: BodyResource[]
  horizonTicks: number
  samples: BodyInputSampleV1[]
  checkpoints: EvidenceCheckpointV1[]
  completionEvidence: EvidenceConditionV1[]
}

interface BodyInputSampleV1 {
  atTickOffset: number
  input:
    | { surface: 'world'; state: Pick<WorldInputFrameV1, 'lookDelta' | 'move' | 'held' | 'edge'> }
    | { surface: 'screen'; screenRevision: number; state: Pick<ScreenInputFrameV1, 'cursorDelta' | 'pointer' | 'scrollSteps' | 'modifiers' | 'keyEdge' | 'textInput'> }
}

interface EvidenceCheckpointV1 {
  afterTickOffset: number
  evidence: EvidenceConditionV1
  onUnmet: 'stop_and_replan' | 'fail'
}

interface EvidenceConditionV1 {
  protocol: 'mineintent.evidence-condition-ref.v1'
  requirementHandle: string
  intentEffectId: string
  evidenceRevision: number
  validUntilMotorTick: number
}

type EvidenceEvaluationV1 =
  | { status: 'satisfied'; requirementHandle: string; evidenceIds: string[] }
  | { status: 'unsatisfied' | 'unknown' | 'stale'; requirementHandle: string; evidenceIds: string[] }
```

- `horizonTicks` 有严格小上限；计划不能内嵌循环、任意代码、目标选择器或“执行直到任务完成”的 phase。长任务通过“输入若干 tick → 新观察/反馈 → 重新合成”推进。
- `EvidenceConditionV1` 是 Outcome Evidence 服务签发的 opaque requirement ref，不是可执行查询或谓词字符串。任务含义仍属于 Grounded Semantic Goal；Runtime 只能用 `requirementHandle` 得到 `satisfied/unsatisfied/unknown/stale` 和证据 ID，不能提交 `tree_removed`、`wood_collected`、坐标查询或 raw world 表达式。
- `samples` 中没有 world target、grounded handle、路径、配方或对象类型。Behavior 可以利用这些认知信息计算下一段输入，但 Action Runtime/Motor 看见的只有玩家输入状态。
- primary/secondary/pick 即将按下前，checkpoint 必须用最新 `CrosshairObservation` 验证当前准星证据仍与计划引用相符；不符就停止并重规划。该证据检查留在 Runtime，Motor/driver 仍只接收无目标的按键状态。
- plan 中的 `leaseId` 必须与提交出的每个 frame 一致。抢占提升 lease 后，尚未提交的样本全部失效；任何 edge 都至多提交一次。

### 4.4 协议驱动候选端口（v0.3 复审）

下表可用于理解 Minecraft 特有事务，但不再预设上层只能通过准星和逐 tick frame 驱动。v0.3 要逐项判断哪些 Mineflayer 高层能力可以在 scoped control view 内安全复用：

| 内部端口 | 责任 | 上层约束 |
|---|---|---|
| `CrosshairResolver` | 从当前姿态计算准星命中的实体/方块面 | 不接受上层传入的世界目标替代实际准星 |
| `BodyPhysicsStatePort` | 为客户端物理、射线和 packet 编码保有精确自身 pose/velocity | 只在 driver/Motor 内使用；对 Behavior 暴露 pose revision，不把绝对坐标当作额外感知 |
| `ViewPacketDriver` | 维护相机/头部/身体朝向关系并发送合法旋转时序 | v0.3 决定接收 look delta、授权目标的局部姿态解或二者组合；外部观察到的转头与认知视口必须一致 |
| `PrimaryActionStateMachine` | 处理攻击、开始/继续/取消挖掘、冷却与挥手时序 | v0.3 可评审 `attack`/`dig` 在 scoped controller 内复用；调用目标必须来自合法 Grounding 并在执行期重验 |
| `SecondaryActionStateMachine` | 处理主副手优先级、实体/方块交互、放置和物品使用时序 | v0.3 按来源、作用域、取消和证据决定接口粒度，不按对象类别为主模型注册技能 |
| `MovementPacketDriver` | 客户端物理、移动/视角包、冲刺/潜行状态 | 不负责路径选择，不允许上层直接写 position packet |
| `VehicleInputDriver` | 将相同 move/jump/sneak 输入按当前载具翻译为划船、马跳、下载和载具移动时序 | 不对上层暴露特定载具移动技能 |
| `InventoryTransactionDriver` | window revision、click/confirm、回滚与服务端拒绝 | 不负责配方、整理或装备决策 |
| `ScreenTransactionDriver` | 将当前屏幕中的鼠标/键盘/文本输入翻译为配方、交易、铁砧、信标、告示牌、书等事务 | 不对上层暴露配方/职业/容器专用任务命令 |

因此，“挖掘”和“放置”可以是协议驱动中的状态机名称，也可以是结果事件的语义，但不是 Behavior Synthesizer 可直接指定目标坐标的高层接口。

#### 4.4.1 Java 1.21.1 serverbound packet 审计

packet 名只是 driver 实现证据，不是 Motor API。当前 1.21.1 Play 状态的 serverbound packet 全部归类如下：

| 类别 | packet | 处置 |
|---|---|---|
| 自动协议维护 | `configuration_acknowledged`、`chunk_batch_received`、`keep_alive`、`ping_request`、`pong`、`teleport_confirm`、`message_acknowledgement`、`chat_session_update` | driver 根据协议状态自动发送，Behavior 不可见、不可调用 |
| 世界控制派生 | `position`、`look`、`position_look`、`flying`、`entity_action`、`abilities`、`arm_animation`、`block_dig`、`block_place`、`use_entity`、`use_item`、`held_item_slot`、`pick_item` | 由 driver/controller 根据当前物理、持有物、游戏模式和已授权局部目标编码；主模型不能直接填协议 ID/坐标或绕过 Grounding |
| 载具派生 | `vehicle_move`、`steer_boat`、`steer_vehicle` | 由相同移动/跳跃/潜行输入与当前骑乘状态派生 |
| 通用屏幕/GUI | `window_click`、`close_window`、`craft_recipe_request`、`displayed_recipe`、`recipe_book`、`set_slot_state`、`enchant_item`、`select_trade`、`set_beacon_effect`、`name_item`、`edit_book`、`update_sign`、`advancement_tab`、`client_command` | 只从当前有 revision 的可见屏幕输入或合法生命周期 UI 派生；不建立高层“制作/交易/重生”接口 |
| 聊天与文本辅助 | `chat_message`、`chat_command`、`chat_command_signed`、`tab_complete` | 聊天由 Speech 契约调度；命令与补全受独立权限策略限制，不因文本看起来像命令就执行 |
| 客户端声明与响应 | `settings`、`cookie_response` | 客户端设置包由显式偏好 revision 派生；cookie 只响应服务端请求并受 backend 隐私/存储策略约束。二者都不是 Behavior 身体动作 |
| 游戏模式/权限限定 | `set_creative_slot`、`spectate`、`set_difficulty`、`lock_difficulty`、`query_block_nbt`、`query_entity_nbt`、`generate_structure`、`update_command_block`、`update_command_block_minecart`、`update_jigsaw_block`、`update_structure_block`、`debug_sample_subscription` | 不进入默认生存具身契约。若未来支持创造/观战/管理场景，必须使用显式游戏模式与权限端口，不与 Companion 普通身体权限合并 |
| 服务器/模组扩展与资源包 | `custom_payload`、`resource_pack_receive` | 由 backend 兼容性与用户策略管理，不向规划器提供任意 payload 通道 |

这个分类还明确了两个易错边界：

1. `block_dig` 不仅用于挖掘，不同 status 还承担取消挖掘、丢弃物品和释放使用物等协议时序。产品接口不应直接暴露 packet 名；v0.3 再决定用输入、连续控制器还是二者组合表达。
2. `use_entity`/`block_place` 包含实体 ID、方块位置、面和光标点。它们可以是授权 controller 的局部编码数据，但不是主模型可任意填写或从 raw world 搜索的目标参数。

### 4.5 认知规划服务

下列组件可以存在，但它们是规划实现，不是身体原语：

| 服务 | 合法输入 | 输出 |
|---|---|---|
| `EpistemicMap` | 当前观察、亲自探索和带时效记忆 | 角色已知的空间、未知边界和证据 |
| `NavigationPlanner` | 已 Grounding 的空间约束、Epistemic Map、身体状态 | 局部移动/观察计划，而不是直接传送 |
| `BehaviorSynthesizer` | 期望状态契约、方法引导、grounded referent、合法 Information Read、可供性 | 可取消的内部计划引用或明确缺失；具体 controller contract 在 v0.3 决定 |
| `OutcomeVerifier` | 目标契约、服务端来源事件、新认知观察和 Motor feedback | 已验证的状态、部分结果或未证明 |

`NavigationPlanner.plan(...)` 不等于 `navigateToPlayer(...)`。前者是对通用空间约束的可替换规划服务，后者已将对象类别、身份查找、空间求解和移动执行压成一个函数。

### 4.6 安全、调度与证据端口

| 端口 | 责任 | 不得做 |
|---|---|---|
| `BodyResourceArbiter` | 协调 locomotion/gaze/hands/inventory/interaction 占用 | 不选目标或生成方法 |
| `SafetyProbe` | 对已选定的下一局部控制返回 allow/deny/risk | 不批量扫描地图、提供替代路线 |
| `CancellationPort` | 停止、暂停、危险抢占、断线和 revision 变更 | 不留下持续按键或未结束 transaction |
| `MotorFeedbackPort` | 报告输入已提交、客户端预测、服务端观察和失败 | 不把指令完成冒充结果成功 |
| `OutcomeEvidencePort` | 为 grounded semantic state 签发 opaque requirement handle，并将准星、方块更新、实体、掉落物、背包和位置证据评估为 satisfied/unsatisfied/unknown/stale | Runtime 不提交任务谓词或 raw 查询；不使用 Paper 裁判作为生产认知 |

控制抢占优先级固定为：连接/生命周期清理 > 用户停止或人工接管 > 必要安全反射 > 当前行为计划。优先级只决定谁拥有当前控制 lease，不替高层选择目标或路线。

## 5. 功能如何由接口组合产生

| 对外功能 | 可能的组合 | 为什么不是新接口 |
|---|---|---|
| 看向/寻找朋友 | 合法观察 + Grounding + 通用 gaze/scan controller + 新观察 | 目标可以是任何语义对象，不应有 `look_at_player` 这类 model-facing 对象技能 |
| 挖掘方块 | Grounding + 工具/方法选择 + scoped interaction controller + 服务端方块证据 | `dig` 可是内部实现，但不能承担目标搜索、任务规划或成功判定 |
| 放置方块 | Grounding + 物品/依附面选择 + scoped interaction controller + 背包/方块证据 | 放置点和建造目的属于规划，controller 只执行授权局部阶段 |
| 攻击/防御 | 合法实体观察 + Grounding + 移动/交互 controller + 冷却/伤害反馈 | 不为玩家、怪物等对象类别建立 model-facing 专用技能 |
| 吃喝/拉弓/投掷/钓鱼 | 选中物品 + secondary 按下/持续/释放 + 物品/状态反馈 | 都是同一右键时序在不同持有物下的结果 |
| 收集木材 | 理解资源目标 + 观察候选 + 规划来源 + 移动/交互 + 背包验证 | 可以捡掉落物、取已有库存或砍树，不应有 `collect_wood` |
| 清除阻挡 | 理解期望空间状态 + 选方法 + 移动/挖掘/放置组合 + 可通行性验证 | 绕行、破坏、搭桥等均可能，不应有 `remove_object`/`clear_path` |
| 跟随朋友 | 反复观察 + 动态 Grounding + 局部导航 + 移动输入 + 社交距离判断 | 是持续意图和反馈循环，不应有 `follow_player` |
| 打开容器/制作/交易/附魔 | 对准+右键 + Screen Observation + 光标/按键/文本输入 + transaction 反馈 | GUI 对象和配方都是规划所选，不为每种工作方块增加接口 |
| 书写/告示牌/命名 | 右键打开屏幕 + 通用文本输入 + 确认控件 + 世界/transaction 反馈 | 不应有 `write_sign_at`/`rename_item_to` 之类世界目标捷径 |
| 骑乘/划船/下载 | 右键互动 + 相同 move/jump/sneak 输入 + 骑乘反馈 | 载具只改变 driver 如何翻译玩家输入 |
| 睡觉/离床/重生 | 右键交互或当前生命周期屏幕 + 可见 GUI 输入 + 状态反馈 | 不为床或死亡注册任务技能 |
| 逃离危险 | 威胁感知 + 反射优先级 + 视线/移动输入 + 距离/伤害反馈 | 可有安全反射策略，但不是主模型可调用的 `escape_threat` 技能 |
| 等待 | 无新身体输入 + scheduler deadline + 持续感知 | 不需要 `wait` 身体技能；时间调度不等于身体动作 |

### 5.1 支持范围决定

| 范围 | 内容 | 决定 |
|---|---|---|
| v0.2 阻断契约 | Information Catalog、Help/Read、UI Context、状态/F3/背包/HUD/current screen、视觉/声音信息边界 | 以 [information-access-and-ui.md](../architecture/information-access-and-ui.md) 为规范 |
| v0.3 待设计 | Grounding 后的连续控制器、Mineflayer 高层 API 复用、资源/取消/验证和任务接口边界 | 不沿用本节逐 tick Body Input Plan 结论，重新权衡 |
| 默认排除 | 单机暂停、截图、全屏、资源重载、debug crash、创造快捷工具栏保存/载入、社交屏幕中的屏蔽/举报、辅助功能/语言/键位设置修改 | 只影响本地显示/运维、产生平台副作用或不适用于无头产品；必要时走用户/运维策略，不是 Companion 普通身体权限 |
| 需要额外授权 | 服务器命令、创造取物、观战传送、管理/命令方块/结构方块、任意 custom payload | 不属于 Companion 普通身体权限；测试裁判端口与生产认知物理隔离 |

## 6. 现有实现审计

当前代码是 v0.1 原型，下列项不得直接成为 v0.2 契约：

| 现有项 | 判定 | v0.2 处置 |
|---|---|---|
| `MinecraftControlsApi.findNearestBlock` | 使用 raw loaded world 执行认知搜索 | 从 controls 删除；规划只在 Cognitive Observation 候选中选择 |
| `navigateNear(position)` | 把路径规划和身体执行合并，并可读取全知世界 | v0.3 拆分知识来源与 scoped controller；不预设必须拆成逐 tick 移动输入 |
| `navigateToPlayer(username)` | 硬编码对象类别，且用 tracked 坐标 | 删除；使用 Grounding + 通用空间约束 |
| `dig(position)` | 当前调用方可从 raw world 选择坐标，绕过合法信息与目标授权 | v0.3 可在 scoped controller 内复用或替换；阻断点是来源/作用域/验证，不是坐标参数本身 |
| `inventoryCount(names)` | 只读观察被混入 controls | 移入 InventoryObservationPort，保留 revision/evidence |
| `nearestThreat(distance)` | raw entity 威胁分类被混入 controls | 拆到 Perception/反射威胁证据；不进入普通目标选择 |
| `stop()` | 同时停 pathfinder、dig 和所有按键，边界含混 | v0.3 为每个 controller 定义取消/清理并保留总停止入口 |
| `MinecraftBackendApi.controls()` | 将搜索、寻路、威胁分类、挖掘和停止混成一个端口 | 删除公共混合端口；v0.2 建立 Information Runtime，控制面在 v0.3 重新实现 |
| `MinecraftBackendApi.snapshot().trackedPlayers[].position` | 原始 tracked 位置可被 companion/skill 直接读取 | 仅 driver/perception 候选可见；Companion/Behavior 只获得 Cognitive Observation 和 grounded handle |
| `MinecraftSnapshotV1.self.position/yaw/pitch` | backend 必需的精确物理状态与 Companion 认知快照混在一起 | 拆入 driver `BodyPhysicsStatePort`；认知层默认只见 proprioception，坐标/精确角度必须来自当前 HUD/debug 证据 |
| `MinecraftSnapshotV1.inventory.slots` | 未经字段选择、来源和 revision 就被任意上层代码遍历 | 迁入 `inventory_information`；允许结构化检查自身背包，但不延伸到未打开外部容器 |
| `ProtocolObservationSource.listTrackedEntities()` | 一次暴露所有已加载实体及精确位置、协议 ID、装备 | 降为 driver 内部候选源；先经过 FOV、遮挡、距离与注意处理，再由 Viewport 输出观察句柄 |
| `ProtocolEntityEvent` / `ProtocolBlockEvent` | raw tracked/loaded 更新可在视野外发生 | 只驱动渲染候选、已知对象时效和交互反馈；未被正常感知的更新不得自动成为认知事实 |
| `ProtocolSoundPayload.sourcePosition` | 服务端包坐标被直接暴露为精确声源 | Sound Perception 转为可听类别、相对方向、响度与不确定性；精确坐标仅留在 driver trace |
| `ProtocolObservationSource.readBlock(position)` | 必要的 driver 读取原语，但可被滥用为地图查询 | 只允许 Perception 沿当前视线采样或 Safety 检查下一个局部输入；架构测试禁止 Behavior/Navigation 导入 |
| `MinecraftBackendApi.sendChat(message)` | 直接发送文本，没有社交时机、承诺依赖和统一节流 | 仅由 `SpeechOutputPort` 适配调用；Behavior/Motor 不发送聊天 |
| `src/minecraft/` 中的 Mineflayer imports | 当前 factory/backend/controls 仍共享同一目录，边界靠约定 | v0.2 收拢到 `src/minecraft/driver/`，其余模块只依赖归一化 DTO；integration 测试客户端单独豁免 |
| `ActionRequest.skill` / `SkillDefinition` | v0.1 将任务执行目录暴露为决策协议 | 删除；不提供 compatibility adapter，v0.3 按新边界实现 Behavior 与 controller |
| `SkillDefinition.preconditions/expectedEffects` | `primary_player_visible`、`wood_block_removed` 等字符串把任务语义和执行注册项绑定 | v0.2 由 Grounded Semantic Goal、通用 evidence condition 与 Outcome Verifier 承担；不迁移字符串目录 |
| v0.1 `BodyResource` (`locomotion/gaze/hands/...`) | 当前粒度可能不能表达 controller 的真实并发关系 | v0.3 随连续控制层一并复审，不预设必须改成键鼠资源 |
| `follow_player` | 对象专用长流程，混合目标选择、知识和控制 | 删除；v0.3 从持续意图、Grounding、Behavior 与受约束 controller 重新形成 |
| `collect_wood` | 资源选择、寻路、挖掘、拾取和循环验证被编成一个 skill | 删除；不迁移为 v0.2 接口 |
| `return_to_anchor` | 目标与导航方法合并 | 活动锚点是记忆/引用，返回是规划结果 |
| `escape_threat` | 威胁搜索、目标生成和寻路合并 | v0.3 拆分合法威胁信息、反射策略、Safety 抢占和 controller |
| `wait` | 无输入时间段被包装成 skill | 迁入 scheduler/plan deadline，期间感知不停止 |
| Paper/CI 中的直接 `bot.dig`/命令 | 测试布景和裁判需要的特权快捷方式 | 只存在 integration/test process，不进入生产 Backend、Perception、Behavior 或 OutcomeVerifier |

这些原型项直接删除，不得被新模块依赖、复制或包进兼容层。

## 7. 当前有效结论

1. 1.21.1 Play serverbound packet `58/58` 分类和 Mineflayer/v0.1 API 普查仍有效。
2. raw tracked entity、loaded block、精确声音坐标和测试裁判不能直接成为 Companion 知识。
3. 客户端状态、F3、HUD、背包和 screen 信息迁入 Catalog/Help/Read，并按显示精度与可用条件读取。
4. Mineflayer 高层 API 的风险来自全知来源、目标越权、乐观成功和不可取消组合；是否复用必须逐 controller 评审。
5. 逐 tick `BodyInputPlan`、Motor 不得消费 grounded handle、坐标参数一律非法等结论已撤回。
6. v0.2 实施和完成定义以 [合法信息接口、Help 发现与 UI 会话](../architecture/information-access-and-ui.md) 为准；v0.3 再制定动作完成定义。
