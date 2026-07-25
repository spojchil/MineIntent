---
status: accepted
authority: normative
implementation: planned
last_verified: 2026-07-23
---

# UI Context 与界面会话模块设计

> 本设计由 [PR #67](https://github.com/spojchil/maineintent/pull/67) 合并；实现 Issue [#54](https://github.com/spojchil/maineintent/issues/54) 仍 open。当前简化 provider 不等于这里定义的 `UiContextProjection`。上游：[合法信息与 UI](./information-access-and-ui.md)、[Information Runtime](./information-runtime.md)、[v0.2 路线图](../history/roadmap-v0.2-legal-information-interfaces.md)。

## 1. 决定

MineIntent 用一个可信、只读的 `UiContextProjection` 表达“当前已建立什么 UI 会话语义，以及输入当前归谁”。它由 Driver 侧 `UiSessionSource` 的有序事件归约产生，`UiContextProvider` 只把该投影转换为 `ui_context` 的合法 Information Read。无头结构化会话与真实渲染画面由 acquisition 明确区分；projection 存在不等于像素画面存在。

UI Context 必须同时表达、但不能混写：

1. 连接阶段；
2. `none | world | screen` 主表面；
3. `world | screen | chat | none` 输入归属；
4. HUD、F3、聊天、Tab、字幕和 boss bar 等 overlay。

Provider、#55 和 #56 都不能直接读取 Mineflayer `Bot`、`currentWindow`、协议 window ID 或旧 `MinecraftSnapshotV1`。共享 UI 状态只能通过投影读取；Provider 之间不得递归调用 `InformationRuntime`。

## 2. 目标与非目标

### 2.1 目标

- 在断线、连接、configuration、进入世界和过渡期间给出确定状态。
- 为每个当前完整 Screen 建立进程内不可猜测的 `screenInstanceId`。
- 区分 Screen 身份/结构变化与普通内容变化。
- 让聊天输入、F3、Tab 等 overlay 不伪装成容器 Screen。
- 为未知或模组 Screen 保留安全身份和 `unknown` 分类，不按标题猜测。
- 原子发布不可变投影，使 Information Runtime 能在 Read 前后检测 UI scope race。
- 给 #55/#56 提供稳定的只读消费契约和失效语义。
- 覆盖单元、Provider 契约、Paper 和真人观察验收。

### 2.2 非目标

- 不读取槽位、按钮、文本框、进度、聊天内容、计分板或玩家列表内容。
- 不执行打开/关闭界面、点击、打字、切换 F3/Tab 或发送聊天等动作。
- 不设计 GUI 自动化、Screen element selector 或 transaction 协议；它们属于 #56/后续控制层。
- 不把任意客户端设置、渲染树、Screen handler 或模组对象变成通用快照。
- 不为旧 snapshot、`backend.snapshot()` 或 `Bot.currentWindow` 建兼容 adapter。
- 不保证不同 Information Provider 之间跨接口原子读取。
- 不把“存在合法结构化等价读取”解释为当前真的打开了对应界面。

## 3. 核心类型

### 3.1 投影值

```ts
type UiConnectionState =
  | 'disconnected'
  | 'connecting'
  | 'configuration'
  | 'play'

type ScreenFamily =
  | 'inventory'
  | 'container'
  | 'crafting'
  | 'processing'
  | 'merchant'
  | 'equipment'
  | 'text'
  | 'advancements'
  | 'recipe_book'
  | 'death'
  | 'sleep'
  | 'menu'
  | 'social'
  | 'resource_pack'
  | 'disconnect'
  | 'unknown'

type UiMainSurface =
  | { kind: 'none'; reason: 'not_connected' | 'transition' }
  | { kind: 'world' }
  | {
      kind: 'screen'
      screenInstanceId: string
      screenRevision: number
      screenFamily: ScreenFamily
      registryId?: string
      title?: string
      pausesWorld: boolean
    }

type UiInputTarget =
  | { kind: 'world' }
  | {
      kind: 'screen'
      screenInstanceId: string
      mode: 'pointer' | 'text' | 'navigation'
    }
  | { kind: 'chat'; mode: 'text' | 'command' }
  | { kind: 'none' }

type UiFieldUnavailableReason = 'not_supported' | 'not_exposed'

type UiFieldAcquisition =
  | 'immediate_client_state'
  | 'structured_ui_equivalent'
  | 'current_screen'

type UiFieldState<T> =
  | {
      availability: 'available'
      value: Readonly<T>
      acquisition: UiFieldAcquisition
    }
  | {
      availability: UiFieldUnavailableReason
    }

interface UiOverlayValues {
  hudVisible: boolean
  debugVisible: boolean
  chatMode: 'hidden' | 'history' | 'input'
  playerListVisible: boolean
  subtitlesEnabled: boolean
  bossBarsVisible: boolean
}

type UiOverlayStates = {
  readonly [K in keyof UiOverlayValues]: UiFieldState<UiOverlayValues[K]>
}

interface UiContextV1 {
  protocol: 'mineintent.ui-context.v1'
  connectionEpoch: number
  uiRevision: number
  connectionState: UiConnectionState
  mainSurface: UiMainSurface
  inputTarget: UiFieldState<UiInputTarget>
  overlays: UiOverlayStates
}
```

`UiFieldState` 是值与可用性的不可分割证据单元。`availability !== 'available'` 的分支在类型上没有 `value` 或 `acquisition`，因此 source、projection、#55/#56 和 Provider 都无法在不检查状态的情况下消费伪造的 `false`、`none` 或空对象。overlay 按成员携带状态：例如协议可证明 boss bar、但不能证明无头客户端是否打开 F3 时，两者可以分别为 `available` 与 `not_supported`。

可用分支中的 `acquisition` 描述该值如何获得，而不是全局运行模式：协议生命周期可以是 `immediate_client_state`；无头 UI 协调器提供的是 `structured_ui_equivalent`；只有真实渲染客户端确认当前画面时才可以标为 `current_screen`。不得仅因值存放在本地 projection 中就升级为“已显示”。

`inputTarget.value.kind: none` 表示来源已经确认当前没有**可信且可供控制层使用**的输入目标；它不同于 `inputTarget.availability: not_supported`。前者不能被解释为“操作系统层绝对没有窗口焦点”，后者则明确表示来源无法判断。若未知 Screen 的输入语义无法可靠分类，Provider 对 `input_target` 返回字段级 `not_supported`，而不是发布内部猜测；见第 11 节。

### 3.2 投影读取面

```ts
type Unsubscribe = () => void

interface UiContextProjectionSnapshot {
  processSessionId: string
  context: Readonly<UiContextV1>
  baseFieldAcquisition: Readonly<{
    connectionState: UiFieldAcquisition
    mainSurface: UiFieldAcquisition
  }>
  sourceRevision: number
  observedAt: string
}

interface UiContextProjection {
  snapshot(): Readonly<UiContextProjectionSnapshot>
  subscribe(
    listener: (value: Readonly<UiContextProjectionSnapshot>) => void,
  ): Unsubscribe
}
```

- `snapshot()` 返回一次原子提交的深只读副本，不暴露 reducer、source 或可变对象。
- `subscribe()` 在新提交完成、scope bridge 已同步后发送完整新值，不发送可乱序拼接的字段 patch。
- listener 失败不能阻止其他 listener，也不能回滚已经提交的状态。
- `connectionState` 与 `mainSurface` 是 projection 的最低保证：连接 source 始终能给出前者，后者在不能证明画面时回到 `none/transition`；其余可能缺失的值只能通过 `UiFieldState` 发布，不另设旁路 availability map。
- v0.2 无头进程不存在可绕过 MineIntent coordinator 的本地 GUI，因此 `mainSurface` 表达完整的结构化控制表面。未来真实客户端 adapter 若不能观察全部主 Screen，不能以 `world` 补缺，也不能注册为 v1 `UiSessionSource`；应先扩展 `main_surface` availability 契约。

## 4. 可信来源端口

### 4.1 来源事件

Driver 内部可以接触 Mineflayer 和协议对象，但跨入 Information 模块的端口只能发送归一化、可复制的 DTO：

```ts
interface UiSourceEventBase {
  processSessionId: string
  connectionEpoch: number
  sourceRevision: number
  observedAt: string
}

type UiSourceInputTarget =
  | { kind: 'world' }
  | {
      kind: 'screen'
      sourceScreenKey: string
      mode: 'pointer' | 'text' | 'navigation'
    }
  | { kind: 'chat'; mode: 'text' | 'command' }
  | { kind: 'none' }

type Java121MenuSourceType =
  | 'minecraft:generic_9x1'
  | 'minecraft:generic_9x2'
  | 'minecraft:generic_9x3'
  | 'minecraft:generic_9x4'
  | 'minecraft:generic_9x5'
  | 'minecraft:generic_9x6'
  | 'minecraft:generic_3x3'
  | 'minecraft:crafter_3x3'
  | 'minecraft:anvil'
  | 'minecraft:beacon'
  | 'minecraft:blast_furnace'
  | 'minecraft:brewing_stand'
  | 'minecraft:crafting'
  | 'minecraft:enchantment'
  | 'minecraft:furnace'
  | 'minecraft:grindstone'
  | 'minecraft:hopper'
  | 'minecraft:lectern'
  | 'minecraft:loom'
  | 'minecraft:merchant'
  | 'minecraft:shulker_box'
  | 'minecraft:smithing'
  | 'minecraft:smoker'
  | 'minecraft:cartography'
  | 'minecraft:stonecutter'

type LocalScreenSourceType =
  | 'inventory'
  | 'book_view'
  | 'book_edit'
  | 'sign_edit'
  | 'advancements'
  | 'recipe_book'
  | 'death'
  | 'sleep'
  | 'game_menu'
  | 'social_interactions'
  | 'resource_pack'
  | 'disconnect'

type UiScreenSourceType =
  | {
      kind: 'java_menu'
      tableVersion: 'minecraft-java:1.21.1/ui-source-types:v1'
      menuType: Java121MenuSourceType
    }
  | { kind: 'java_horse'; minecraftVersion: '1.21.1' }
  | {
      kind: 'local'
      tableVersion: 'mineintent-local-ui:v1'
      localType: LocalScreenSourceType
    }
  | { kind: 'unknown' }

interface UiSessionSourceSnapshot {
  processSessionId: string
  connectionEpoch: number
  sourceRevision: number
  observedAt: string
  connectionState: UiConnectionState
  connectionStateAcquisition: UiFieldAcquisition
  surface:
    | { kind: 'none'; reason: 'not_connected' | 'transition' }
    | { kind: 'world' }
    | {
        kind: 'screen'
        sourceScreenKey: string
        sourceType: UiScreenSourceType
        registryId?: string
        title?: string
        structureKey: string
        pausesWorld: boolean
      }
  surfaceAcquisition: UiFieldAcquisition
  inputTarget: UiFieldState<UiSourceInputTarget>
  overlays: UiOverlayStates
}

type UiOverlaySourceEvent = {
  [K in keyof UiOverlayValues]: UiSourceEventBase & {
    kind: 'overlay_field_changed'
    field: K
    state: UiFieldState<UiOverlayValues[K]>
  }
}[keyof UiOverlayValues]

type UiSessionSourceEvent =
  | (UiSourceEventBase & {
      kind: 'connection_phase_changed'
      state: UiConnectionState
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'world_presented'
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'transition_started'
      cause: 'login' | 'dimension' | 'death' | 'respawn'
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'screen_opened'
      sourceScreenKey: string
      sourceType: UiScreenSourceType
      registryId?: string
      title?: string
      structureKey: string
      inputMode?: 'pointer' | 'text' | 'navigation'
      pausesWorld: boolean
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'screen_structure_changed'
      sourceScreenKey: string
      structureKey: string
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'screen_closed'
      sourceScreenKey: string
      cause: 'client' | 'server' | 'replacement' | 'death' | 'transition'
      acquisition: UiFieldAcquisition
    })
  | (UiSourceEventBase & {
      kind: 'input_target_changed'
      state: UiFieldState<UiSourceInputTarget>
    })
  | UiOverlaySourceEvent

interface UiSessionSource {
  capture(): Readonly<UiSessionSourceSnapshot>
  subscribe(listener: (event: Readonly<UiSessionSourceEvent>) => void): Unsubscribe
}
```

`UiSessionSource` 是多个底层信号合并后的**单一有序端口**。连接生命周期、协议 Screen、客户端本地 Screen、输入协调器和 overlay 追踪器不能各自直接修改投影。Driver 适配器先串行化事件并分配严格递增的 `sourceRevision`，投影只消费这一条序列。

状态变化和能力变化使用同一事件：`available/value/acquisition` 与 `not_supported | not_exposed` 原子替换，不能先发默认值再补 availability。overlay 逐成员发事件，禁止一个不可观测成员把其他已证明成员清空，也禁止用完整布尔对象暗中补默认值。

`connectionState/mainSurface` 虽然是始终可用的基础字段，仍分别保留 acquisition；这是来源证明而不是 availability 旁路。`transition_started`、`screen_closed` 和 disconnect 等 reducer 派生状态继承触发事件/当前 session 的 acquisition，不能一律重标为 `immediate_client_state`。

`sourceType` 是 classifier 唯一可消费的版本化判别值；`registryId` 只是可公开身份，不能兼任分类开关。horse 即使没有 registry identity，仍能由 `kind: java_horse` 明确分类。unknown 不能携带任意类型字符串诱导 classifier；诊断 identity 只走经过清洗的 `registryId/title`。

`sourceScreenKey` 只用于匹配乱序 close/structure/input 事件，留在投影内部；projection 把匹配当前 source key 的输入目标转换成公开 `screenInstanceId`。它不得成为 `screenInstanceId`、selector payload、日志值或 Provider 输出。`structureKey` 是当前公开结构的不可逆指纹，不得包含槽位内容、handler 对象或 raw transaction 状态。

初始化采用“先订阅并暂存事件，再 capture，最后只回放 `sourceRevision` 大于 capture 的事件”的顺序，避免 capture/subscribe 之间漏掉 Screen 转换。Source snapshot 与 event 使用同一 revision 序列；projection 不允许用轮询结果覆盖更晚事件。

store 内部维护 `lastAcceptedSourceRevision` 以拒绝重复/倒退事件；`UiContextProjectionSnapshot.sourceRevision` 则是产生当前可信内部投影的 `publishedSourceRevision`。被接受但没有改变基础值、`baseFieldAcquisition` 或任何完整 `UiFieldState` 的事件只推进内部游标，不替换 projection snapshot，也不通过 Provider metadata 暴露无意义 churn。acquisition-only 变化不属于此类：它必须产生新的内部 snapshot。

### 4.2 Mineflayer 适配边界

Mineflayer 4.37 的 `currentWindow`/`windowOpen`/`windowClose` 只足以覆盖服务端同步的窗口，不能证明客户端本地背包界面、聊天输入、F3、Tab、暂停菜单、成就或资源包提示的完整状态。因此：

- 服务端 Screen 由协议窗口事件适配，不轮询 `currentWindow` 猜会话。
- client-local Screen 和 overlay 必须来自 MineIntent 自己的**结构化 UI/input coordinator**，或未来真实渲染客户端提供的明确事件；两种来源分别标记 `structured_ui_equivalent` 与 `current_screen`，不能混称真实画面。
- 无头模式的 coordinator 只能证明 MineIntent 虚拟输入/界面语义，不能证明一个不存在的渲染客户端“实际显示”了背包、聊天输入、F3、Tab、菜单或字幕。
- 缺少来源能力时对应 `UiFieldState` 使用 unavailable 分支，不能把默认 `false`、`hidden` 或 `none` 当作“已经观察到关闭”。同一 overlay 对象内也不得这样补值。
- Driver 适配器可以导入 Mineflayer；`UiSessionSource`、projection、Provider 和消费者均不得导入。
- 旧 Backend lifecycle 事件可作为连接信号之一，但不得通过 `backend.snapshot()` 补齐 UI。

adapter 必须按下表赋 acquisition，不能由 Provider 根据字段名事后猜测：

| 已证明的 source fact | acquisition | 禁止声称 |
|---|---|---|
| 协议连接阶段、`open_window/close_window`、协议同步的 boss bar | `immediate_client_state` | 像素已经渲染 |
| MineIntent 无头 UI/input coordinator 的虚拟会话 | `structured_ui_equivalent` | 原版客户端实际打开了界面 |
| 真实客户端/渲染 adapter 确认的当前画面与焦点 | `current_screen` | 未观测成员也具有同一状态 |

同一事件只能选择一行。把协议事件和无头 coordinator 组合成 projection，不会自动产生第三行证据。

### 4.3 Screen 分类

分类表以 `minecraft-java:1.21.1/ui-source-types:v1` 为版本标识。服务端 `open_window` 的 `inventoryType` 按项目锁定的 `prismarine-windows@2.10.0` 顺序归一化；数字 ID 只允许在 adapter fixture 内出现，projection 只接收表中的规范 source type。以下 25 项是 Java 1.21.1 `open_window` 的完整冻结表：

| protocol type | 规范 source type | `ScreenFamily` |
|---:|---|---|
| 0–5 | `minecraft:generic_9x1` … `minecraft:generic_9x6` | `container` |
| 6 | `minecraft:generic_3x3` | `container` |
| 7 | `minecraft:crafter_3x3` | `crafting` |
| 8 | `minecraft:anvil` | `equipment` |
| 9 | `minecraft:beacon` | `processing` |
| 10 | `minecraft:blast_furnace` | `processing` |
| 11 | `minecraft:brewing_stand` | `processing` |
| 12 | `minecraft:crafting` | `crafting` |
| 13 | `minecraft:enchantment` | `equipment` |
| 14 | `minecraft:furnace` | `processing` |
| 15 | `minecraft:grindstone` | `equipment` |
| 16 | `minecraft:hopper` | `container` |
| 17 | `minecraft:lectern` | `text` |
| 18 | `minecraft:loom` | `crafting` |
| 19 | `minecraft:merchant` | `merchant` |
| 20 | `minecraft:shulker_box` | `container` |
| 21 | `minecraft:smithing` | `equipment` |
| 22 | `minecraft:smoker` | `processing` |
| 23 | `minecraft:cartography` | `crafting` |
| 24 | `minecraft:stonecutter` | `crafting` |

`open_horse_window` 是独立协议事件，不占上述 ID；它显式映射为 `equipment`，且因为协议没有 menu registry identity，`registryId` 省略，不能伪造 `minecraft:horse`。箱子、木桶、末影箱等可能共享 generic source type；family 分类不宣称知道方块来源。

`prismarine-windows` 内部的 `minecraft:inventory/type=-1` 也不是合法 `open_window` protocol type，不能送入上述表；玩家背包只由下表的 `local:inventory` 会话产生。这样 `-1` 测试既是边界 fixture，也是防止把库内对象误当协议事实的 canary。

client-local source type 由 MineIntent coordinator 显式发出，首版冻结如下；未列类型不得由标题或控件形状补猜：

| coordinator source type | `ScreenFamily` | 适用说明 |
|---|---|---|
| `local:inventory` | `inventory` | 玩家背包主界面 |
| `local:book_view`、`local:book_edit`、`local:sign_edit` | `text` | 文本 UI 会话 |
| `local:advancements` | `advancements` | 成就界面 |
| `local:recipe_book` | `recipe_book` | 仅当 coordinator 把配方书确认为主表面；嵌入面板保持父 Screen family |
| `local:death` | `death` | 死亡界面 |
| `local:sleep` | `sleep` | 睡眠界面 |
| `local:game_menu` | `menu` | 游戏菜单 |
| `local:social_interactions` | `social` | 社交界面 |
| `local:resource_pack` | `resource_pack` | 已确认呈现的资源包提示 |
| `local:disconnect` | `disconnect` | 断开连接界面 |

禁止按本地化标题、槽位内物品、槽位数量或模组类名进行模糊猜测。服务端 protocol type 不在 0–24、client-local source type 未命中上表、或 adapter 的 Minecraft/Prismarine 版本与表版本不符时，一律分类为 `unknown`；仍可保留经过长度限制和文本清洗的合法 `registryId`、可见 `title`。不得返回 handler 类名、协议 window ID、transaction counter 或 raw title component。

测试 fixture 必须逐项覆盖 0–24、独立 horse 事件、全部 client-local source type、边界值 `-1/25` 与任意未知字符串，并断言未列项永远为 `unknown`。实现不得维护第二份 switch；classifier 和测试从同一只读版本表生成。

模组支持通过版本化的显式分类扩展加入，运行中不热装载。扩展改变字段语义时提升 `ui_context` schema revision，而不是静默重分类现有会话。

## 5. 状态机与组合不变量

### 5.1 主状态

```text
disconnected
  └─ connection requested ─→ connecting/none
       └─ protocol configuration ─→ configuration/none
            └─ play ready ─→ play/world

play/world
  ├─ screen opened ─→ play/screen
  ├─ chat input ─→ play/world + inputTarget chat
  ├─ transition ─→ play/none(transition)
  └─ disconnected ─→ disconnected/none(not_connected)

play/screen
  ├─ screen replaced ─→ play/new screen
  ├─ screen closed ─→ play/world
  ├─ transition/death ─→ play/none 或新的 death screen
  └─ disconnected ─→ disconnected/none(not_connected)
```

Overlay 是正交状态，不为每种组合制造主状态。F3、Tab、聊天历史、HUD、字幕和 boss bar 的变化只更新 overlay；聊天输入额外改变 `inputTarget`，但 `mainSurface` 仍为 `world`。

### 5.2 必须始终成立的不变量

1. `connectionState !== 'play'` 时，`mainSurface` 必须为 `none`，`inputTarget` 必须为 `available/none`；任何仍为 available 的 HUD、F3、chat、Tab 和 boss bar 短生命周期状态必须归零，原本 unavailable 的成员保持 unavailable，`subtitlesEnabled` 可以保留为客户端设置。
2. 仅当 `inputTarget.availability === 'available'` 时应用输入组合不变量；unavailable 不能被当作 `none`。
3. `mainSurface: world` 只允许可用的 `inputTarget.value` 为 `world | chat | none`。
4. `inputTarget.value: world` 只允许 `mainSurface: world`；若 `chatMode` 可用，则它不能为 `input`。
5. `inputTarget.value: chat` 只允许 `mainSurface: world`，且 `chatMode` 必须为 `available/input`；若 chat mode unavailable，则 input target 也不能声称可用的 `chat`。
6. `inputTarget.value: screen` 只允许 `mainSurface: screen`，两个 `screenInstanceId` 必须完全相同。
7. 已知 Screen 在来源未给出可靠输入 mode 时不能猜；该字段标记 `not_supported`。
8. `screenInstanceId` 在同一 `processSessionId` 内唯一，不从协议 ID、标题或 registry ID 派生。
9. 同一 Screen 的普通内容更新不改变 `screenInstanceId` 或 `screenRevision`。
10. 任何 connection epoch 改变都先清除 Screen、input target 和短生命周期 overlay，再发布新 epoch 状态。
11. `screen_closed` 只有在 `sourceScreenKey` 匹配当前 Screen 时才生效；旧窗口的迟到 close 不能关闭替代它的新 Screen。
12. `sourceRevision` 相同或倒退的事件拒绝；旧 connection epoch 的迟到事件拒绝。
13. 投影不得产生 Public type 之外的额外字段，也不得保留 raw source 对象。

### 5.3 特殊转换

- **死亡**：使当前 Screen 立即失效。能可靠表示死亡界面时建立新的 `death` Screen；自动重生或无客户端界面时进入 `none/transition`。不能沿用死亡前的 screen instance。
- **重生/维度切换**：进入 `none/transition`，清除输入归属和短生命周期 overlay；世界就绪后回到 `world`。
- **资源包请求**：真实客户端确认提示已呈现，或无头 coordinator 明确建立结构化提示会话时，才建立 `resource_pack` Screen，并分别携带 `current_screen` 或 `structured_ui_equivalent`。仅收到资源包 URL 不等于其中任一种事实。
- **异常关闭/重连**：立即发布 `disconnected/none/not_connected`，清除 Screen；后续重连使用新 epoch，旧事件无法恢复旧界面。
- **断开界面**：v0.2 Mineflayer 适配器在连接关闭后发布 `none/not_connected`，不虚构可见 disconnect Screen。`disconnect` 目前只是上游 union 中的保留值，v1 reducer 不产生；未来若要支持真实断开界面，必须先修订“非 play 只能是 none”的公共不变量。
- **多人菜单**：`pausesWorld` 必须来自客户端会话事实；远程服务器默认 `false`，不能按 `menu` family 猜为 `true`。

## 6. Revision 所有权

| Revision | 唯一所有者 | 提升条件 | 不提升条件 |
|---|---|---|---|
| `connectionEpoch` | Backend lifecycle | 新连接尝试进入新的有效协议会话 | 重生、维度、Screen、overlay |
| `sourceRevision` | `UiSessionSource` | 任一被串行接受的归一化 UI source event | Help/Read |
| `uiRevision` | `UiContextProjection` | `connectionState/mainSurface`、任一 `baseFieldAcquisition` 或任一 `UiFieldState` 的完整判别值改变 | raw 重复事件、被过滤的内部变化、普通 Screen 内容 |
| `screenInstanceId` | `UiContextProjection` | Screen 打开、替换，或关闭后重新打开 | 同一 Screen 内布局或内容改变 |
| `screenRevision` | `UiContextProjection` | 当前 Screen 的公开身份、槽位拓扑、可见控件集合或布局结构改变 | 槽位物品、文本值、进度、滚动值、overlay、input focus |
| `informationRevision` | `UiContextProvider` | 九个公开字段中任一 value 或 availability 改变 | `baseFieldAcquisition` 或 `UiFieldState.acquisition` 单独改变；source 中未改变公开投影的事件 |

`uiRevision`、`screenRevision` 与 `informationRevision` 是三个独立契约，消费者不得比较它们的数值大小。特别是 acquisition-only 变化必须发布新的 UiContext projection snapshot 并提升 `uiRevision`，使 #55/#56 能看到新的 provenance；同一次变化必须被 UiContextProvider 的公开比较器忽略，不能提升 `informationRevision`。

`screenRevision` 从 `1` 开始；新 `screenInstanceId` 的 revision 重新从 `1` 开始。标题改变会推动 `uiRevision`/UiContextProvider `informationRevision`，但标题不参与元素寻址时不推动 `screenRevision`。这只表示 #54 不发送 `screen_changed`，不承诺其他 Provider 签发的 ref 保持有效：#56 若把标题作为公开内容并提升自身 `informationRevision`，现有 RefStore 仍会按签发源 revision 保守失效其 ref。family、registry identity 或公开结构变化推动 `screenRevision`；若来源已明确是替代 Screen，则生成新 instance，而不是只加 revision。

## 7. 失效矩阵

| 事件 | connection epoch | ui rev | screen instance/rev | ui info rev | 对 screen-bound ref/cursor |
|---|---:|---:|---|---:|---|
| 重复 source event，公开值未变 | 不变 | 不变 | 不变 | 不变 | 保持 |
| 仅 base/field acquisition 改变 | 不变 | 提升 | 不变 | 不变 | UI projection 消费者更新 provenance；Provider 公开 revision/ref 不失效 |
| 新连接 epoch | 提升 | 提升 | 清除 | 提升 | 全部旧 epoch 失效 |
| connecting → configuration → play | 不变 | 每次语义变化提升 | 无，直到打开 Screen | 提升 | 原 Screen 不得恢复 |
| world 打开 Screen | 不变 | 提升 | 新 instance，rev=1 | 提升 | 旧 Screen 引用失效 |
| Screen A 被 B 替代 | 不变 | 提升 | 新 instance，rev=1 | 提升 | A 引用失效 |
| Screen 关闭 | 不变 | 提升 | 清除 | 提升 | 当前 Screen 引用失效 |
| 同 Screen 结构改变 | 不变 | 提升 | 同 instance，rev+1 | 提升 | 旧结构引用失效 |
| 槽位/文本/进度内容改变 | 不变 | 不变 | 不变 | 不变 | 由 #56 Provider 自身 revision 决定 |
| Screen title 改变 | 不变 | 提升 | 通常不变 | 提升 | 不触发 screen scope 失效；签发源提升自身 info rev 时其 ref 仍保守失效 |
| input target/focus 改变 | 不变 | 提升 | 不变 | 提升 | 仅 screen scope 引用保持 |
| F3/Tab/HUD/chat overlay 改变 | 不变 | 提升 | 不变 | 提升 | 仅 screen scope 引用保持 |
| 死亡、重生、维度过渡 | 不变 | 提升 | 清除或新 death instance | 提升 | 当前 Screen 引用失效 |
| disconnect/reconnect | 新会话时提升 | 提升 | 清除 | 提升 | 全部旧引用失效 |

Composition-owned `InformationScopeCoordinator` 将 session/world scope 与 UI commit 合成为一个 `InformationScopeSnapshot`；#54 只提供 `uiRevision/screenInstanceId/screenRevision`，不能成为第二个 connection/world 写者。screen commit 触发 `screen_changed` invalidation，connection/world invalidation 仍由对应 session scope 所有者触发。普通 overlay、focus 或 title 更新只改变 `uiRevision`，不能发送伪造的 `screen_changed`。Runtime 仍会根据 Provider 的 `scopeDependencies` 在 Read 前后拒绝 race。

## 8. UiContextProvider 契约

### 8.1 字段

```ts
interface UiContextValues {
  connection_state: UiConnectionState
  main_surface: UiMainSurface
  input_target: UiInputTarget
  hud_visible: boolean
  debug_visible: boolean
  chat_mode: 'hidden' | 'history' | 'input'
  player_list_visible: boolean
  subtitles_enabled: boolean
  boss_bars_visible: boolean
}

type UiPublicFieldState<T> =
  | { availability: 'available'; value: Readonly<T> }
  | { availability: UiFieldUnavailableReason }

interface UiContextProviderProjectionSnapshot {
  informationRevision: number
  publicSourceRevision: number
  publicObservedAt: string
  fields: Readonly<{
    [K in keyof UiContextValues]: UiPublicFieldState<UiContextValues[K]>
  }>
}
```

| 字段 | `valueType` | precision | sourceKinds | availability |
|---|---|---|---|---|
| `connection_state` | `enum(disconnected,connecting,configuration,play)` | `inferred` | `screen_projection` | 始终 `available` |
| `main_surface` | `UiMainSurface` | `inferred` | `screen_projection` | 始终 `available`；断线返回 `none/not_connected` |
| `input_target` | `UiInputTarget` | `inferred` | `screen_projection` | 对应 state 可用时返回；否则 `not_supported/not_exposed` |
| `hud_visible` | `boolean` | `inferred` | `screen_projection` | 逐成员 state 决定 |
| `debug_visible` | `boolean` | `inferred` | `screen_projection` | 逐成员 state 决定 |
| `chat_mode` | `enum(hidden,history,input)` | `inferred` | `screen_projection` | 逐成员 state 决定 |
| `player_list_visible` | `boolean` | `inferred` | `screen_projection` | 逐成员 state 决定 |
| `subtitles_enabled` | `boolean` | `inferred` | `screen_projection` | 逐成员 state 决定 |
| `boss_bars_visible` | `boolean` | `inferred` | `screen_projection` | 逐成员 state 决定 |

所有字段都允许 `screen_projection`，确保任意字段组合能由一次 Provider Read 的唯一 `source.kind` 合法返回。overlay 拆成独立 Information fields，公共 `values` 中不会出现“完整对象里部分布尔值其实不可知”的情况。Provider 只在对应 state 为 available 时写入值；否则省略该字段并在 `unavailable[]` 写入同一 reason。连接生命周期已经先进入统一 UI projection，因此不使用混合的 `lifecycle_event` 结果。

UiContextProvider 订阅内部 projection，并先把它归一化为 `UiContextProviderProjectionSnapshot.fields`：基础字段成为 available/value；`inputTarget` 与 overlay state 复制 availability/value，但**剥离 acquisition**。只有这个九字段对象深比较不等时，Provider 才原子替换公开 snapshot、提升 `informationRevision`，并把该次内部 snapshot 的 source revision/observedAt 记为 `publicSourceRevision/publicObservedAt`。acquisition-only 更新不得改写这三个公开元数据。

v1 对全部 UI Context 字段保守声明 `precision: inferred`：这里的“inferred”表示经过版本化 source adapter 和状态机归一化，并不表示低置信度。真实渲染 adapter 也不能把静态 Help 元数据升级为 `exactly_displayed`。公共 `InformationReadResult.source.acquisition` 是 result-wide，不能表达混合字段来源；逐字段真实来源只保留在可信 projection 的 `UiFieldState/baseFieldAcquisition` 中。若未来要把逐字段渲染证据公开给模型，必须先提升公共契约，而不是用 Help 文案或自定义优先级偷换语义。

建议 definition：

```ts
const uiContextDefinition: InformationProviderDefinition<UiContextValues> = {
  id: 'ui_context',
  description: '当前连接、主表面、输入归属与叠加界面状态',
  schemaRevision: 'ui-context:1',
  audiences: ['companion', 'controller'],
  scopeDependencies: ['connection', 'ui', 'screen'],
  fields: uiContextFields,
  limits: {
    maxFieldsPerRead: 9,
    maxResultBytes: 16_384,
    timeoutMs: 50,
  },
}
```

UI Context 不接受 selector、不分页、不签发 ref。Provider 的 `availability()` 与 `read()` 各读取一次 `UiContextProviderProjectionSnapshot`：整体至少 `partially_available`；连接断开不是接口 unavailable，而是一个可读取的值。`read()` 对请求字段从同一公开 snapshot 构造结果，不能为每个字段重新 capture，也不能在 Read 时临时从内部 snapshot 重算 metadata。#55/#56 则直接消费内部 projection，必须先匹配 `UiFieldState.availability`，不能直接解构 `.value`。

Provider result：

- `informationRevision` 来自 Provider 自身的对外投影 revision；
- `source.kind = 'screen_projection'`；
- `source.adapterRevision = 'ui-context-provider.v1'`；
- `source.sourceRevision` 使用公开 snapshot 的 `publicSourceRevision`，不能使用最新内部 projection source revision；
- `source.acquisition = 'structured_ui_equivalent'`：这是 `ui_context` v1 的固定、保守 canonical acquisition，表示调用者检查了结构化 UI 会话投影，不声称任一字段已渲染。禁止根据所选字段或 adapter 动态改成 `immediate_client_state/current_screen`，因为 result-wide 枚举无法无歧义表达混合来源；
- `observedAt` 使用公开 snapshot 的 `publicObservedAt`；
- 默认不生成 `evidenceIds`，且绝不把协议 window ID 放进证据。

三个必须区分的结果例子：

- 无头 Mineflayer 没有 local UI coordinator：请求 `debug_visible` 时 `values` 不含该字段，`unavailable` 返回 `not_supported`；
- 无头 coordinator 明确进入虚拟 F3 会话：projection state 为 `available/true/structured_ui_equivalent`；
- 真实渲染 adapter 确认 F3 画面：projection state 为 `available/true/current_screen`。

后两者的公共 Read 都保守返回 result-wide `structured_ui_equivalent`；#55/#56 等可信消费者仍可从各自 state 区分来源。三者不能因为最终 boolean 相同而在 projection 中合并为同一种证据。

## 9. 并发与一致性

### 9.1 单写者 reducer

`UiContextProjectionStore` 是唯一写者。所有 source event 进入 FIFO reducer；每次 reduce 按以下顺序提交：

1. 校验 process session、connection epoch 和 source revision；
2. 基于上一不可变状态计算候选状态；
3. 校验组合不变量；
4. 若基础值、`baseFieldAcquisition` 与全部 `UiFieldState` 均未改变，只推进内部 `lastAcceptedSourceRevision`，不替换 projection snapshot、不提升 UI revision；
5. 原子替换 projection snapshot；
6. 把 UI scope slice 同步提交给 composition-owned scope coordinator；
7. 对 connection/world/screen 变化执行 Runtime invalidation；
8. 通知 projection subscribers。

步骤 5–7 由一个 composition-owned commit callback 串行执行。subscriber 不能在 scope 尚未更新时观察到新 UI。connection/world 字段来自 session scope，并在同一 coordinator 合成；projection 不复制所有权。

UiContextProvider 的公开 projection 是下游第二层单写者：它在收到步骤 8 的内部 snapshot 后剥离 acquisition 并比较九字段 public states。相同则整次忽略；不同才提交新的 Provider snapshot。内部 `uiRevision` 提升不等于 Provider `informationRevision` 提升。

### 9.2 Read 一致性

- Provider `availability()` 与 `read()` 各自只 capture 一次。
- `ui_context` 声明 `connection/ui/screen` 依赖；任一相关 scope 在 Read 前后变化时，Runtime 返回 `scope_changed`。
- 不同 Provider 的 Read 不共享事务；调用者以各自 `readId/informationRevision/observedAt` 判断时效。
- #56 的各 projection reducer 订阅 UiContext，把所需 `UiFieldState` 与 `screenInstanceId/screenRevision` 嵌入自身不可变 snapshot；其 Provider 每次 Read **只 capture 自己的一份 projection**，不能先后读取 UI Context 与内容再拼接。Runtime 继续用声明的 `connection/screen` scope 在 Read 前后拒绝 race。
- 不能为追求跨 Provider 原子性重新引入全局 snapshot。

### 9.3 事件竞态

- 迟到旧 epoch 事件直接丢弃。
- 新 `screen_opened` 在当前 Screen 尚未 close 时被视为替代，旧 instance 先失效。
- 迟到的旧 `screen_closed` 通过 `sourceScreenKey` 匹配失败后丢弃。
- 同一个 `structureKey` 的重复结构事件不提升 revision。
- disconnect 是终结屏障；同 epoch 后续非连接事件均拒绝，直到新 epoch 建立。

## 10. 给 #55 与 #56 的稳定消费契约

### 10.1 #55 状态、快捷栏、背包与 F3

- 只注入 `UiContextProjection`，不得注入 `UiContextProvider` 或调用 Information Runtime。
- `connectionState === 'play'` 是自身 HUD/状态类 Provider 可用的必要条件之一；UI Context 不提供生命、物品或 F3 字段。
- 自身背包结构化检查不因 `mainSurface` 不是 inventory Screen 而自动不可用；外部容器内容不属于 #55 的自身背包接口。
- 只有 `overlays.debugVisible.availability === 'available'` 时，#55 才能消费其 `.value`；该值描述相应 adapter 已证明的 F3 overlay 状态，不决定 `f3_information` 的结构化等价读取是否允许。后者仍按 reduced-debug 和 #55 自己的来源契约判断。
- current status/hotbar/inventory 的普通值变化不能推动 `uiRevision` 或 `screenRevision`。
- #55 若只关心连接，不应声明 `ui/screen` scope dependency；避免无关 overlay 切换使 Read 失败。

#55 的合法访问形态为：直接读取始终可用的 `context.connectionState/context.mainSurface`；对 F3 先匹配 `const state = context.overlays.debugVisible`，仅在 `state.availability === 'available'` 分支读取 `state.value`。它不得保留旧 `fieldAvailability` map、建立兼容 adapter，或用 `baseFieldAcquisition` 决定 `f3_information` 是否可结构化读取。

### 10.2 #56 HUD、聊天、Tab 与当前 Screen

- `mainSurface.kind === 'screen'` 时，`screenInstanceId + screenRevision` 是读取当前 Screen 内容的唯一会话 scope；协议 window ID 不能越过 source port。
- `current_screen_information` 的 source port 自己拥有内容 `informationRevision`。槽位、文本和进度变化只提升该 Provider 的 revision，不改变 UI Context screen revision。
- Screen element ref 必须 `bindToScreen: true`；Runtime 同时校验 instance/revision 和签发源 Provider information revision。
- #56 逐成员匹配 `overlays.*.availability` 后才能消费 `.value`；这些值只描述对应 adapter 已证明的显示/输入状态，具体 HUD、聊天、Tab、字幕和 boss bar 内容由 #56 自己的 projection 提供。
- `chatMode: available/input + inputTarget: available/chat` 才表示文本输入会话；任一 unavailable 都必须使依赖该事实的字段 unavailable，收到聊天消息本身不等于进入 chat input。
- 未知 Screen 仍可让 #56 返回已证明可见的通用槽位/文本/控件；不能因为 `unknown` 读取 handler 私有属性。
- current-screen Provider 应声明 `connection/screen` scope dependency。无关 F3/Tab overlay 改变不应使 screen 内容 Read 失效；只有确实消费 input/overlay 的 Provider 才声明 `ui`。

## 11. 未知、模组与能力缺失

### 11.1 未知 Screen

未知不等于 unavailable：

```json
{
  "kind": "screen",
  "screenInstanceId": "screen_01...",
  "screenRevision": 1,
  "screenFamily": "unknown",
  "registryId": "examplemod:machine",
  "title": "Assembler",
  "pausesWorld": false
}
```

这个值只证明一个 Screen session 已建立以及其安全公开身份；是否为真实画面由 acquisition 决定。它不证明槽位语义、按钮动作、输入 mode 或模组业务含义。无法确认输入 mode 时，Provider 省略 `input_target` 并返回 `not_supported`；controller 不得从 `unknown` 推断鼠标或键盘行为。

### 11.2 来源能力缺失

`ui_context` 整体仍在 Catalog 中。能够可靠返回的字段正常返回，缺少 client-local adapter 的字段使用 partial unavailable：

- 不知道 F3/Tab/chat input 的状态时，只把对应 `debugVisible/playerListVisible/chatMode` state 标为 `not_supported`，不能抹掉协议已知的 boss bar 等其他成员；
- 不知道 unknown Screen 的输入语义时，`input_target: not_supported`；
- 断线不是能力缺失，仍返回确定的 connection/main/input 值；
- 不用 `provider_failed` 表示正常的 adapter 能力缺失。

v0.2 现有 availability 枚举没有 `unknown_input_target` 或 `client_ui_unobservable`。首版使用 `not_supported`，并在 Help notes 说明原因；若真人验收发现恢复指引不清晰，再由公共契约 Issue 决定是否扩展枚举，#54 不直接修改 #53 契约。

## 12. 模块与文件布局

```text
src/information/
├── source-ports/
│   └── ui-session.ts                 # 归一化 DTO/event 与只读 source port
├── projections/
│   ├── ui-context.ts                 # 类型、只读 projection port
│   ├── ui-context-store.ts           # 单写者 reducer、revision、不变量
│   └── screen-classifier.ts          # 1.21.1 显式分类表
├── providers/
│   ├── ui-context.ts                 # Provider
│   └── ui-context.definition.ts      # 字段 Zod 与 Help 元数据
├── ui-context-module.ts              # projection/scope/invalidation 组合
└── testing/
    └── ui-context-fixtures.ts

src/minecraft/driver/
├── ui-session-adapter.ts             # 唯一允许接触 Mineflayer UI/protocol 对象
├── local-ui-state.ts                 # MineIntent 本地 Screen/overlay/input 事件
└── ui-session-adapter.test.ts

src/information/projections/
├── ui-context-store.test.ts
└── screen-classifier.test.ts

src/information/providers/
└── ui-context.test.ts
```

`ui-context-module.ts` 在 composition root 中连接 projection、`InformationScopeSource` 和 Runtime invalidation，但不拥有 Registry、AccessPolicy 或 Tool Session。公共 `src/information/index.ts` 最终只导出 source/projection 接口、Provider/factory 和合法值类型，不导出 reducer 可变面或 Driver adapter。

## 13. 测试与验收

### 13.1 Reducer 单元测试

- disconnected → connecting → configuration → play/world 全路径。
- screen open、无 close 替换、close、重复事件和迟到旧 close。
- 新 Screen 的 instance 不可预测且不复用，revision 从 1 开始。
- 同 Screen 结构变化只加 screen revision；槽位/文本/进度事件不进入 UI reducer。
- chat input 改 input target 和 chat mode，但主表面保持 world。
- F3、Tab、HUD、字幕和 boss bar 只改 overlay。
- `baseFieldAcquisition` 或 available state 的 acquisition 单独变化会提升 `uiRevision`、替换 snapshot 并通知订阅者；value/availability 保持不变。
- 死亡、重生、维度切换、异常关闭和重连清除旧 Screen。
- 旧 epoch/倒退 source revision 事件拒绝。
- 每次提交满足组合不变量，返回值深只读且订阅者相互隔离。
- classifier 只消费 `UiScreenSourceType`；registry/title/slot count 改变不能重分类，`unknown` 始终保持 `unknown`。

### 13.2 Provider/Runtime 契约测试

- Catalog/Help 中九个字段、schema 和 `screen_projection` source 完全一致。
- 任意字段组合均只 capture 一个 projection snapshot、只返回请求字段。
- 被过滤的重复 source event 不改变内部 projection snapshot；acquisition-only 事件改变内部 `sourceRevision/observedAt`，但不改变 Provider 的 `informationRevision/publicSourceRevision/publicObservedAt`。
- 断线返回明确值，不返回 `not_connected` 整体错误。
- 未知 Screen 可返回 `main_surface`，未知 input mode 返回 partial `not_supported`。
- Provider information revision 只在九字段公开 value 或 availability 改变时提升；内部 acquisition-only 变化不提升。
- Read 中发生 UI/screen change 时 Runtime 返回 `scope_changed`。
- screen-bound ref 在 open/replace/structure change/close/death/reconnect 后失效。
- output Zod 严格清洗，raw source key、window ID、structure key 和额外 title component 不可泄漏。
- Provider contract、结果大小、deadline 和 source kind 公共套件通过。

### 13.3 Driver adapter 测试

- 锁定 Mineflayer/协议版本的 open/close/replace 包映射为有序 source event；0–24 逐项构造版本化 `java_menu` source type，horse 构造独立判别，`-1/25` 进入 `unknown`。
- `windowOpen` 内容延迟不导致遗漏 Screen open；Screen 内容 readiness 留给 #56。
- 本地 inventory/menu/chat/F3/Tab 事件不能与 server window 混成两个当前主 Screen。
- resource-pack packet 只有在 UI coordinator 确认呈现后才产生 Screen。
- title/registry ID 清洗和长度限制生效；raw handler/window/transaction 不出端口。
- adapter detach 后不再发事件，重连不会保留旧 listener 或旧 source key。

### 13.4 Paper 集成验收

Paper 只作为外部真相裁判，不进入生产 projection：

1. 启动 bot，等待 `play/world`。
2. 打开箱子、工作台等服务端 Screen，确认 instance/family/input target。
3. 服务端替换或关闭 Screen，确认旧 instance/ref 失效并回到 world。
4. 在 Screen 打开时更新槽位内容，确认 UI screen revision 不变，而 #56 内容 revision 变化。
5. 触发死亡/重生、维度切换和断线重连，确认无陈旧 Screen。
6. 使用自定义标题的原版通用容器，确认分类仍依据 registry type、不会按标题猜测；unknown/mod 场景由 adapter fixture 和真人模组客户端验收。

Paper 无法证明本地 F3、Tab、聊天输入或菜单显示状态；这些项目不得写成 Paper 通过即验收。

### 13.5 真人验收

- 通过 operator trace 同时观察 `ui_context` Help/Read 与 bot 的真实/本地 UI coordinator 状态。
- 依次打开/关闭背包、箱子、工作台、聊天输入、F3、Tab 和菜单，逐项核对主表面、输入目标与 overlay。
- 验证聊天消息到达不会自动伪造成 chat input。
- 打开未知 Screen，确认输出 `unknown` 和安全 identity，不猜 family/输入行为。
- 断网、被踢和重连后立即读取，确认旧 Screen 不再出现。
- trace 中检查不到 raw window ID、handler、transaction、URL 或 token。

## 14. 实施切片

### S1：契约与 reducer

- 新增 source/projection DTO、严格 Zod、显式 Screen classifier。
- 完成纯 reducer、不变量、revision 和竞态单元测试。
- 暂不连接 Mineflayer 或 Registry。

### S2：连接与 Scope 纵向切片

- 实现 Driver connection-phase adapter。
- 组合 `UiContextProjection → InformationScopeSource`。
- 实现 UiContextProvider，接入 Registry，完成 disconnected/connecting/play Read。

### S3：服务端 Screen

- 接入协议 Screen open/replace/structure/close。
- 生成 instance/revision，连接 Runtime invalidation。
- 完成箱子/工作台 Paper 测试。

### S4：client-local UI、input 与 overlays

- 接入 MineIntent local UI/input coordinator。
- 覆盖背包、聊天输入、F3、Tab、菜单、死亡和资源包提示。
- 对 adapter 能力缺失返回 partial unavailable。

### S5：下游契约与收尾

- 提供 #55/#56 projection fixture 和 contract test harness。
- 完成未知 Screen、重连、竞态、非泄漏和真人验收。
- 由 #58 composition 接入最终 Provider 集合；不在 #54 建 snapshot fallback。

每个切片独立通过 `pnpm check`、单元测试和 `git diff --check`。S2 之后 #55 可基于连接契约实现；S3 之后 #56 可实现 current screen，无需等待全部 overlay。

## 15. 开放问题与风险

1. **Mineflayer client-local 缺口**：当前 `currentWindow` 不覆盖背包、F3、Tab、聊天输入和菜单。S4 必须有本地 UI/input coordinator；否则相关字段只能 `not_supported`，#54 验收不能声称完成。
2. **unknown input target**：公共 union 没有 `unknown`，availability 也没有专门原因。首版采用字段级 `not_supported`；是否增加 `inputTarget: unknown` 必须另行修改公共契约并评审控制安全性。
3. **configuration 可观察性**：旧 BackendState 没有精确协议 configuration 阶段。Driver 需要从协议状态机产生归一化事件，不能从 `logging_in/spawning` 名称猜测。
4. **Screen structure key**：Mineflayer Window 只覆盖槽位窗口，通用控件树需要未来真实客户端或显式 adapter。无法证明结构时宁可固定基础结构并让 #56 字段 unavailable。
5. **title 与结构**：动态 title 默认只推动 UI/information revision；若某版本 title 实际决定布局，版本适配表必须显式把它计入 structure key。
6. **内容 ref 粒度**：#54 只保证 screen scope；#56 的 Provider-wide information revision 会按现有 Runtime 规则保守失效其 refs。是否需要更细粒度 ref origin 不在 #54 内解决。
7. **单机暂停语义**：当前目标是远程 Paper/多人连接，菜单 `pausesWorld=false`。未来集成单机客户端时必须引入可信 pause source，不能沿用 family 推断。
8. **chat history 可见时窗**：聊天消息淡出与历史 overlay 的精确状态需要 #56 的 chat projection 和客户端设置；#54 只消费其归一化 mode，不自行计时猜测。

## 16. 完成定义

1. `UiSessionSource → UiContextProjection → UiContextProvider → InformationRuntime` 纵向链没有 raw Mineflayer、snapshot adapter 或 Provider 递归。
2. 九个字段可 Help/Read，任意组合具有共同合法 `screen_projection` source；不可用字段不会携带伪值。
3. connection/main surface/input target/overlay 状态机和全部组合不变量通过测试。
4. `uiRevision`、`screenRevision`、Provider `informationRevision` 的所有权和失效矩阵被实现且互不代替。
5. Screen replace、structure change、close、death 和 reconnect 确定性使旧 screen-bound 引用失效。
6. 普通槽位、文本和进度变化不替换 Screen、不改变 screen revision。
7. 未知/模组 Screen 返回 `unknown` 与安全身份，不猜类别、不泄漏 handler。
8. #55/#56 只消费稳定 projection/source port，无需复制 UI 状态机。
9. 单元、Provider contract、Paper 和真人验收分别覆盖其能够证明的事实，测试 oracle 不进入生产信息链。
10. adapter 能力缺失明确降级为 partial unavailable，不以默认值伪造可见状态。
