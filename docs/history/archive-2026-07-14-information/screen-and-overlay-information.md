---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_pr: 65
source_commit: 079de7b
---

# Screen 与 Overlay 信息模块设计

> 原始状态声明（PR 未合并）：#56 实现就绪设计
>
> 日期：2026-07-14
>
> 对应 Issue：[#56](https://github.com/spojchil/mineintent/issues/56)
>
> 上游：[合法信息接口、Help 发现与 UI 会话](../../architecture/information-access-and-ui.md)、[Information Runtime 模块设计](../../architecture/information-runtime.md)、[UI Context 与界面会话模块设计](../../architecture/ui-context.md)、[#63 按 Provider scopeDependencies 绑定分页 Cursor](https://github.com/spojchil/mineintent/issues/63)、[PR #66 所含普通 Read 公开 revision 后验](https://github.com/spojchil/mineintent/pull/66)
>
> 下游：#57 信息验收矩阵、#58 最终 composition 与旧链路删除

集成顺序要求：#54 的 UI Context 设计必须先合入，以上相对链接和本设计消费的 `UiContextProjection` 契约才能闭合；#63 必须在本设计任何分页 Provider 实现合并前完成。设计和不含分页的 projection/字段 schema 可以继续并行。

## 1. 决定

MineIntent 用六个彼此独立的 Information Provider 暴露客户端可展示的界面内容：

- `hud_information`：计分板、boss bar、title/action bar、字幕和 toast；
- `chat_information`：当前会话可显示的聊天历史、输入状态和可见补全；
- `player_list_information`：结构化等价于当前客户端可查看的 Tab；
- `current_screen_information`：当前 Screen 会话中可见的槽位、控件、文本、列表项和进度；
- `advancement_information`：当前角色已同步、已揭示且可在成就界面检查的内容；
- `recipe_book_information`：当前角色已解锁且可在配方书检查的内容。

每个接口都采用独立的 `source port → immutable projection → Provider` 纵向链。Provider 只读取自己的一份原子 projection snapshot，不读取 Mineflayer `Bot`、协议对象、其他 Provider 或 `InformationRuntime`。`UiContextProjection` 只在 projection reducer/组合阶段提供显示状态和 screen scope，不在 Provider Read 内被二次拼接。

这保证一个关键不变量：**一次 Provider Read 只有一个 `source`，请求该接口的任意字段组合都能由同一 snapshot、同一 `source.kind` 和同一 `sourceRevision` 证明。**

## 2. 目标与非目标

### 2.1 目标

- 为六个接口给出稳定字段、严格类型、precision、source kind、availability 与限额。
- 只公开同版本客户端本可呈现或立即结构化检查的信息，不把“客户端收到”误当作“玩家可见”。
- 绑定 #54 的 `screenInstanceId/screenRevision/inputTarget/overlays`；消费 nested `UiFieldState` 时先判断 availability，再读取 value，并让内容 revision 与 screen revision 各自拥有明确职责。
- 为聊天、玩家列表、Screen 元素、成就节点和配方结果提供有界分页。
- 为当前 Screen 元素与物品签发不透明 selector，严格绑定 screen 与签发源 revision。
- 未知/模组 Screen 只返回已证明的通用呈现信息，安全降级为 partial/unavailable。
- 明确 Mineflayer、Paper 和真人客户端各自能证明什么。

### 2.2 非目标

- 不打开/关闭界面，不按 Tab、聊天键或配方按钮，不滚动、点击、悬停、输入或发送消息。
- 不设计“购买”“合成”“接受交易”“选择配方”等语义动作或 GUI 宏。
- 不公开 raw window/slot/entity ID、Screen handler、NBT、聊天签名、内部 scoreboard/team 表或未解锁配方。
- 不保证不同 Provider 的 Read 跨接口原子一致。
- 不从标题、材质或模组类名猜控件语义。
- 不为 Mineflayer 缺失的 client-local 状态伪造默认值。

## 3. 总体结构与单 source 证明

```text
Mineflayer/protocol adapter     MineIntent local UI adapter
        └──────────────┬──────────────┘
                       │ normalized, bounded domain DTOs
                       ▼
               six presentation source ports
                       │
UiContextProjection ───┤ display/screen gate only
        (#54)          │
        ┌──────────┬───┴───┬──────────┬────────────┬─────────────┐
        ▼          ▼       ▼          ▼            ▼             ▼
       HUD        Chat   PlayerList  Screen      Advancement    RecipeBook
    Projection Projection Projection Projection   Projection     Projection
        │          │       │          │            │             │
        ▼          ▼       ▼          ▼            ▼             ▼
       HUD        Chat   PlayerList  Screen      Advancement    RecipeBook
    Provider   Provider   Provider   Provider      Provider       Provider
        └──────────┴───────┴──────────┴────────────┴─────────────┘
                           │
                  InformationRuntime
```

`UiContextProjection` 的订阅事件进入各 projection 的单写者 reducer；reducer 将所需 UI slice 与本接口内容一起原子提交。Provider 不在 Read 时分别 capture UI 与内容，否则无法证明组合结果属于同一时刻。

| Provider | 唯一 projection | 所有字段允许的唯一 `source.kind` | acquisition |
|---|---|---|---|
| `hud_information` | `HudProjection` | `hud_projection` | `structured_ui_equivalent` |
| `chat_information` | `ChatProjection` | `hud_projection` | `structured_ui_equivalent` |
| `player_list_information` | `PlayerListProjection` | `hud_projection` | `structured_ui_equivalent` |
| `current_screen_information` | `ScreenContentProjection` | `screen_projection` | `structured_ui_equivalent` |
| `advancement_information` | `AdvancementProjection` | `screen_projection` | `structured_ui_equivalent` |
| `recipe_book_information` | `RecipeBookProjection` | `screen_projection` | `structured_ui_equivalent` |

同一个 Provider 的字段定义不得增加第二种 source kind。若未来一个字段只能由另一来源证明，应新增独立接口或先在该 Provider 所属 projection 中完成可信归一化，不能在 Read 内拼两个 source。

六个 Provider 的公共 `InformationReadResult.source.acquisition` 在 v0.2 固定使用保守的 `structured_ui_equivalent`。公共 acquisition 是 result-wide，无法无歧义表达一个 Read 内混合的协议即时状态、无头结构化会话和真实渲染证据；因此禁止根据请求字段或 adapter 动态升级为 `immediate_client_state/current_screen`。真实逐字段 provenance 仍保留在可信 projection 中，供 reducer、#57 验收与未来契约演进使用，但不越过当前公共 envelope。

Provider 之间禁止递归。共享原始事件可以在 Driver 层扇出到多个 source port；共享 UI 身份只通过 #54 的只读 projection 进入 reducer。

## 4. 共同呈现类型

所有字符串先转为客户端实际渲染的纯文本和有限样式 DTO；不返回原始 text component、click/hover event、URL、命令或翻译参数。

```ts
interface DisplayText {
  plain: string                 // 最多 512 Unicode code points
  color?: string                // 规范化命名色或 #RRGGBB
  bold?: boolean
  italic?: boolean
  underlined?: boolean
  strikethrough?: boolean
  obfuscated?: boolean
}

interface DisplayItemStack {
  registryName?: string         // 仅客户端已知且本次呈现可检查
  displayName: DisplayText
  count?: number
  durabilityBar?: { filledPixels: number; totalPixels: number }
  glint: boolean
  itemRef?: InformationSelectorRef
}

interface NormalizedRect {
  left: number                  // 0..1024 的显示量化值
  top: number
  width: number
  height: number
}
```

- `registryName` 不是服务端私有 NBT；来源无法证明玩家可检查时省略。
- `itemRef` 首版只由 `current_screen_information` 为当前槽位或 `carried_item` 签发；HUD、Tab、advancement 和 recipe 的装饰/目录物品保持省略。
- `NormalizedRect` 只在真实客户端/版本锁定 renderer 能证明布局时返回；Mineflayer 根据 slot index 猜坐标不合规。
- `obfuscated` 表示呈现样式，不返回被混淆前的底层文本。
- 数组保持客户端显示顺序；内部 map 顺序、协议顺序不得直接冒充显示顺序。

## 5. 可信来源端口与 projection

### 5.1 来源边界

Driver adapter 可以消费客户端收到的协议事件和 MineIntent 本地 UI 事件，但跨入 Information 模块的 DTO 必须已经：

1. 绑定 `processSessionId/connectionEpoch/sourceRevision/observedAt`；
2. 丢弃 raw 对象、handler、packet、UUID、签名和未显示组件；
3. 将文本、样式、数值和布局限制为可复制的严格 JSON；
4. 区分“收到但未呈现”“可结构化检查”“正在显示”；
5. 带上 adapter 对每类内容的 `complete | partial | unsupported` 能力证明。
6. Screen 内容携带与 #54 同版本的内部 source identity 供显式 role/layout 表使用；该 identity 不进入 Provider values、ref、trace 或日志。

协议可作为来源，但包本身不是可见性证明。例如收到全部 team、recipe 或 advancement 数据，不代表这些内容都能从对应 UI 展示；projection 必须再次执行显示槽位、解锁、揭示、过滤和布局规则。

### 5.2 最小端口

```ts
interface ProjectionSourceSnapshot<T, Field extends string = string> {
  scope: Readonly<{
    processSessionId: string
    connectionEpoch: number
    worldScopeRevision: number
    worldId?: string
    dimension?: string
  }>
  sourceRevision: number
  observedAt: string
  value: Readonly<T>
  fieldProvenance: Readonly<Partial<Record<Field, UiFieldAcquisition>>>
}

interface PresentationSource<T> {
  capture(): Readonly<ProjectionSourceSnapshot<T>>
  subscribe(listener: (snapshot: Readonly<ProjectionSourceSnapshot<T>>) => void): () => void
}

type HudPresentationSource = PresentationSource<HudSourceValue>
type ChatPresentationSource = PresentationSource<ChatSourceValue>
type PlayerListPresentationSource = PresentationSource<PlayerListSourceValue>
type ScreenPresentationSource = PresentationSource<ScreenSourceValue>
type AdvancementPresentationSource = PresentationSource<AdvancementSourceValue>
type RecipeBookPresentationSource = PresentationSource<RecipeBookSourceValue>
```

端口按内容域分开，避免一个巨大 `ClientUiSnapshot` 重新成为认知旁路。source DTO 的具体 packet 映射由锁定 Minecraft 版本的 adapter 拥有。

### 5.3 projection 公共形状

```ts
type ContentFieldState<T> =
  | {
      availability: 'available'
      value: Readonly<T>
      provenance: readonly UiFieldAcquisition[]
    }
  | {
      availability: InformationUnavailableReason
    }

type ContentProjectionFields<Values extends object> = {
  readonly [Field in keyof Values]: Readonly<ContentFieldState<Values[Field]>>
}

interface ContentProjectionSnapshot<Values extends object, UiSlice> {
  processSessionId: string
  connectionEpoch: number
  worldId?: string
  dimension?: string
  informationRevision: number
  publicSourceRevision: number
  publicObservedAt: string
  internalSourceRevision: number
  internalObservedAt: string
  fields: ContentProjectionFields<Values>
  ui: Readonly<UiSlice & { uiRevision: number }>
}
```

`UiSlice` 由各 reducer 按需定义，只能包含本接口实际消费的 `connectionState/mainSurface`、nested `UiFieldState` 和 screen instance/revision。reducer 必须先匹配 `UiFieldState.availability`：available 分支才可读取 `.value/.acquisition`；`not_supported/not_exposed` 原样写入对应 `ContentFieldState` unavailable 分支。该分支在类型和 JSON 中都没有 `value/provenance`，不能补 `false/hidden/none`，也不能保留旧值。available 分支的 provenance 合并所消费 UI state acquisition 与内容 source provenance。

每个 projection store 是单写者：source adapter 从 composition-owned scope coordinator 原子取得 `scope` identity 后才发布内容。store 提交前同时验证 process/epoch、`worldScopeRevision/worldId/dimension` 与 source revision，再用同一 commit barrier 下的一份 `UiContextProjection` snapshot 门控并构造候选字段，最后将所需 UI slice 与 `fields` 原子替换。world/dimension transition 开始时 coordinator 先提升内部 `worldScopeRevision` 并清空相关 store；旧 identity 的迟到 snapshot 永久丢弃，即使之后回到同名维度也不能复活。Provider 的 `availability()`/`read()` 只 capture 自己的这份 projection，不能再读取 UiContext、scope source 或其他 projection 拼接结果。

只有把 `fields` 映射成逐字段 `{availability,value?}` 后的公开比较结果改变，才提升该 Provider 的 `informationRevision` 并更新 `publicSourceRevision/publicObservedAt`。provenance-only 或未公开 raw 变化可以更新内部不可变 snapshot、`internalSourceRevision/internalObservedAt` 和 available state 的可信 provenance，但在固定 canonical acquisition 下不得改写 Provider 公共 metadata 或制造虚假 information revision。Provider result 只使用 `publicSourceRevision/publicObservedAt`。

## 6. `hud_information`

### 6.1 值类型

```ts
interface HudInformationValues {
  scoreboard_sidebar: null | {
    title: DisplayText
    lines: Array<{ text: DisplayText; displayedScore?: DisplayText }>
  }
  boss_bars: Array<{
    title: DisplayText
    filledPixels: number
    totalPixels: number
    color: string
    overlay: 'progress' | 'notched_6' | 'notched_10' | 'notched_12' | 'notched_20'
  }>
  title_overlay: null | { title?: DisplayText; subtitle?: DisplayText; phase: 'fade_in' | 'hold' | 'fade_out' }
  action_bar: null | DisplayText
  subtitles: Array<{ text: DisplayText; direction: 'left' | 'center' | 'right'; phase: 'fresh' | 'fading' }>
  toasts: Array<{ kind: 'advancement' | 'recipe' | 'tutorial' | 'system' | 'unknown'; title: DisplayText; detail?: DisplayText; phase: 'entering' | 'shown' | 'leaving' }>
}
```

### 6.2 字段定义与来源

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `scoreboard_sidebar` | nullable `ScoreboardSidebar` | `displayed` | `hud_projection` | `play` 且 HUD 可展示；无 sidebar 返回 `null` |
| `boss_bars` | `BossBarDisplay[]` | `displayed` | `hud_projection` | `play` 且 HUD 可展示；无 bar 返回 `[]` |
| `title_overlay` | nullable `TitleOverlay` | `displayed` | `hud_projection` | `play` 且 HUD 可展示；无 title 返回 `null` |
| `action_bar` | nullable `DisplayText` | `exactly_displayed` | `hud_projection` | `play` 且 HUD 可展示；无内容返回 `null` |
| `subtitles` | `SubtitleDisplay[]` | `displayed` | `hud_projection` | HUD 可展示且字幕 adapter 可证明；功能关闭时 `not_currently_displayed` |
| `toasts` | `ToastDisplay[]` | `displayed` | `hud_projection` | HUD 可展示且 toast adapter 可证明；无 toast 返回 `[]` |

断线返回 `not_connected`。F1/隐藏 HUD 时内容字段返回 `not_currently_displayed`，不能继续暴露后台缓存。仅收到 scoreboard/team 数据不构成 sidebar 可见性；只投影当前 display slot 真正渲染的行。boss bar 只返回当前呈现进度的像素量化，不返回 packet 中更高精度浮点。

Definition 建议：audiences `companion, controller`；scope dependencies `connection, world, dimension, ui`；不接受 selector，不分页；`maxFieldsPerRead=6`、`maxResultBytes=32 KiB`、`timeoutMs=50`。

## 7. `chat_information`

### 7.1 值类型

```ts
interface ChatInformationValues {
  display_state: {
    mode: 'hidden' | 'history' | 'input'
    scroll: 'bottom' | 'scrolled'
    unreadBelow: boolean
  }
  messages: Array<{
    sequence: number
    kind: 'player' | 'system' | 'game' | 'emote' | 'unknown'
    rendered: DisplayText
    displayedSender?: DisplayText
    visibility: 'active' | 'fading' | 'history'
  }>
  draft: { mode: 'text' | 'command'; renderedText: string; cursorCodePoint: number; selection?: [number, number] }
  completions: Array<{ rendered: DisplayText; selected: boolean }>
}
```

### 7.2 字段定义

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `display_state` | `ChatDisplayState` | `exactly_displayed` | `hud_projection` | `play` 且 local UI coordinator 可证明；否则 `not_supported` |
| `messages` | paged `ChatMessageDisplay[]` | `exactly_displayed` | `hud_projection` | 当前设置允许查看本会话历史；隐藏聊天为 `not_currently_displayed` |
| `draft` | `ChatDraft` | `exactly_displayed` | `hud_projection` | chatMode/inputTarget 均 available 且为 input/chat；已证明其他值时 `not_currently_displayed`，state unavailable 原样传播 |
| `completions` | `ChatCompletionDisplay[]` | `exactly_displayed` | `hud_projection` | 输入会话中 suggestion popup 可见；无候选返回 `[]`，能力缺失为 `not_supported` |

`messages` 以最新在前分页，默认 20、最大 50。projection 最多保留当前 connection epoch 内、客户端仍能从聊天 HUD 调出的最近 100 条且不超过 10 分钟的呈现结果；两个界限取较小集合。cursor 绑定字段集合、information revision 和页位置，新消息或显示设置变化后旧 cursor 失效，调用者重新从首页读取。

消息正文每条最多 512 code points，单页正文总计最多 16 KiB。`sequence` 只表示当前 projection 内顺序，不是协议 index。不得返回服务器未发送给本客户端的消息、其他连接历史、过滤前文本、签名、公钥、salt、click/hover payload 或举报元数据。`draft` 只来自 MineIntent 自己管理的游戏内文本输入，不读取系统剪贴板、输入法候选或其他应用键盘事件。

Definition 建议：audiences `companion`；scope dependencies `connection, world, dimension, ui`；pagination `20/50`；`maxFieldsPerRead=4`、`maxResultBytes=48 KiB`、`timeoutMs=50`。

## 8. `player_list_information`

### 8.1 值类型

```ts
interface PlayerListInformationValues {
  display_state: { currentlyShown: boolean; layout: 'single' | 'columns' | 'unknown' }
  header: null | DisplayText
  footer: null | DisplayText
  entries: Array<{
    displayOrder: number
    profileName: string
    displayName: DisplayText
    spectator: boolean
    pingIcon: 'unknown' | 'no_connection' | 'one' | 'two' | 'three' | 'four' | 'five'
    displayedScore?: DisplayText
  }>
}
```

### 8.2 字段定义

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `display_state` | `PlayerListDisplayState` | `exactly_displayed` | `hud_projection` | local UI coordinator 可证明 Tab 显示状态；否则 `not_supported` |
| `header` | nullable `DisplayText` | `exactly_displayed` | `hud_projection` | `play` 且当前客户端可检查 Tab；无 header 返回 `null` |
| `footer` | nullable `DisplayText` | `exactly_displayed` | `hud_projection` | 同上；无 footer 返回 `null` |
| `entries` | paged `PlayerListEntry[]` | `displayed` | `hud_projection` | 客户端 player-list projection ready；否则 `not_supported` |

Tab 是结构化等价读取：即使用户当前没有按住 Tab，`header/footer/entries` 仍可读取；`display_state.currentlyShown` 只陈述实际 overlay 状态。entries 使用真实客户端显示排序规则，默认 32、最大 80；它是当前快照，没有历史时窗。任何 entry/order/header/footer 可见变化提升 revision 并使 cursor 失效。

只返回 Tab 可呈现的用户名、显示名、spectator 样式、score 和延迟图标档位。不得返回精确 ping 毫秒、UUID、IP、skin URL、游戏模式（spectator 除外）、未进入 Tab 的 tracked entity、隐藏 team membership 或服务端内部排序键。无法证明实际字体、窗口宽度和截断时，entry 内容仍可按结构化等价返回，但 `display_state.layout='unknown'`，不能猜当前屏幕能同时画出多少行。

Definition 建议：audiences `companion, controller`；scope dependencies `connection, world, dimension, ui`，`display_state` 的 UI 显示状态已预先归入自身 projection，Provider 本身不额外 capture UI；pagination `32/80`；`maxFieldsPerRead=4`、`maxResultBytes=48 KiB`、`timeoutMs=50`。

## 9. `current_screen_information`

### 9.1 通用元素

```ts
type ScreenElement =
  | {
      kind: 'slot'
      elementRef: InformationSelectorRef
      role: 'player_inventory' | 'container' | 'input' | 'output' | 'equipment' | 'trade' | 'unknown'
      label?: DisplayText
      item: null | DisplayItemStack
      selected: boolean
      bounds?: NormalizedRect
    }
  | {
      kind: 'control'
      elementRef: InformationSelectorRef
      role: 'button' | 'toggle' | 'tab' | 'slider' | 'text_field' | 'list_item' | 'unknown'
      label?: DisplayText
      enabled: boolean
      selected?: boolean
      focused?: boolean
      bounds?: NormalizedRect
    }
  | {
      kind: 'text'
      elementRef: InformationSelectorRef
      role: 'title' | 'label' | 'body' | 'value' | 'tooltip' | 'unknown'
      text: DisplayText
      bounds?: NormalizedRect
    }
  | {
      kind: 'progress'
      elementRef: InformationSelectorRef
      role: 'progress' | 'fuel' | 'experience_cost' | 'cooldown' | 'unknown'
      label?: DisplayText
      filledPixels: number
      totalPixels: number
      bounds?: NormalizedRect
    }

interface CurrentScreenInformationValues {
  screen: {
    screenInstanceId: string
    screenRevision: number
    family: ScreenFamily
    registryId?: string
    title?: DisplayText
  }
  content_capabilities: Record<'slots' | 'controls' | 'texts' | 'progress' | 'layout', 'complete' | 'partial' | 'unsupported'>
  elements: ScreenElement[]
  carried_item: null | DisplayItemStack
  view_state: { page?: number; pageCount?: number; scroll?: 'top' | 'middle' | 'bottom'; focusedElementRef?: InformationSelectorRef }
}
```

槽位、控件、文本和进度统一放入按视觉/导航顺序排列的 `elements`，避免一个 cursor 同时为多组数组维护不同页状态。`family` 原样复用 #54 `UiMainSurface.screenFamily`，#56 不得二次分类。语义明确的原版 Screen 可以给出有限 `role`，但 role/layout 映射必须使用与 #54 同版本的精确、内部 source identity（`minecraft-java:1.21.1/ui-source-types:v1`）及显式表，不能只按粗粒度 `ScreenFamily`、标题或 slot index 推断。horse 等没有公开 `registryId` 的 Screen 仍由可信 adapter 的精确 source identity 映射，不能伪造公开 identity；未知/模组元素一律 `unknown`。

role/layout 表是仓库内受评审的版本化 fixture，不是 adapter 内散落的条件分支：

```ts
type KnownUiScreenSourceType = Exclude<UiScreenSourceType, { kind: 'unknown' }>

interface ScreenElementMappingFixtureBaseV1 {
  policyRevision: 'minecraft-java:1.21.1/screen-elements:v1'
  sourceType: KnownUiScreenSourceType  // 复用 #54 的精确内部 identity；禁止 unknown
  orderGroup: number
  orderWithinGroup: number
  boundsPolicy: 'renderer_only' | 'versioned_gui_layout' | 'omit'
}

type ScreenElementMappingFixtureV1 = ScreenElementMappingFixtureBaseV1 & (
  | {
      sourceElementKey: { kind: 'protocol_slot'; index: number }
      publicKind: 'slot'
      publicRole: 'player_inventory' | 'container' | 'input' | 'output' | 'equipment' | 'trade' | 'unknown'
    }
  | {
      sourceElementKey: { kind: 'local_control'; index: number }
      publicKind: 'control'
      publicRole: 'button' | 'toggle' | 'tab' | 'slider' | 'text_field' | 'list_item' | 'unknown'
    }
  | {
      sourceElementKey: { kind: 'local_text'; index: number }
      publicKind: 'text'
      publicRole: 'title' | 'label' | 'body' | 'value' | 'tooltip' | 'unknown'
    }
  | {
      sourceElementKey: { kind: 'local_progress'; index: number }
      publicKind: 'progress'
      publicRole: 'progress' | 'fuel' | 'experience_cost' | 'cooldown' | 'unknown'
    }
)
```

唯一 artifact 为 `src/information/projections/screen/fixtures/screen-elements-1.21.1.json`，由 screen projection adapter 拥有。每条记录以 `sourceType + sourceElementKey` 唯一；fixture schema 拒绝 `sourceType.kind=unknown`、重复键、kind/key/role 不匹配、负数顺序和不受支持的 bounds policy。协议 slot/control index 只允许作为精确 source identity 下的查表键，不能脱离版本表推断角色。未列项一律 `role=unknown`、省略 `bounds`，并在 `content_capabilities` 中体现 partial/unsupported；禁止用相邻索引、标题或同 family 记录补齐。

v1 fixture 的评审覆盖门至少包含 #54 classifier 表中的 inventory、generic_9x1..9x6、generic_3x3、crafter_3x3、crafting、furnace、blast_furnace、smoker、brewing_stand、enchantment、anvil、grindstone、smithing、stonecutter、loom、cartography、beacon、merchant、shulker_box、hopper、lectern 与 horse source type。某 source type 尚未校准不阻止安全 adapter 存在，但其具体元素只能 unknown/omit；该 source type 不能被宣称为实现完成。

### 9.2 字段定义

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `screen` | `ScreenIdentity` | `displayed` | `screen_projection` | 当前 `mainSurface=screen`；否则 `screen_not_open` |
| `content_capabilities` | `ScreenContentCapabilities` | `inferred` | `screen_projection` | Screen 打开时始终 available |
| `elements` | paged `ScreenElement[]` | `displayed` | `screen_projection` | Screen 打开且至少一种元素已证明；完全无法读取时 `not_supported` |
| `carried_item` | nullable `DisplayItemStack` | `displayed` | `screen_projection` | Screen 打开且能证明鼠标当前携带物；没有携带物返回 `null`，能力缺失为 `not_supported` |
| `view_state` | `ScreenViewState` | `displayed` | `screen_projection` | 当前页/滚动/焦点可证明；否则 `not_supported` |

Definition 建议：audiences `companion, controller`；scope dependencies `connection, world, dimension, screen`，无关 F3/Tab/chat overlay 不导致 Read race；selector 可选，接受 `screen.element`；pagination `8/12`；`maxFieldsPerRead=5`、`maxResultBytes=48 KiB`、`timeoutMs=75`。

### 9.3 selector 与 revision

每个元素签发一个 `screen.element` ref：

```ts
context.refs.issue({
  kind: 'screen.element',
  payload: { elementKey },
  allowedInterfaces: ['current_screen_information'],
  basedOnInformationRevision: snapshot.informationRevision,
  bindToScreen: true,
})
```

槽位中存在玩家此刻可检查的物品时，另签发 `screen.item` ref，放在 `DisplayItemStack.itemRef` 中，允许 `item_tooltip_information` 使用。两类 payload 只含 projection 内部不透明 key，不含协议 slot/window ID、坐标或 raw item。

`carried_item` 同样可以签发 `screen.item`，并必须 `bindToScreen: true`。它由 #56 独占：鼠标携带物属于当前 Screen 的瞬时可见内容，不属于 #55 的 self `inventory_information`。Screen 未打开时整个字段为 `screen_not_open`，不能从 Mineflayer 的残留 cursor 状态、普通背包 projection 或旧 Screen 缓存返回；这样普通背包的 cursor/ref 不会被 screen scope/revision 污染。

带 `screen.element` selector 读取时，`elements` 只返回该元素的当前呈现；未请求 `elements` 时 selector 不产生隐式字段。Runtime 同时验证：

- principal/grant/epoch/world；
- `screenInstanceId + screenRevision`；
- ref 允许目标接口；
- 签发源 `current_screen_information.informationRevision` 在 Read 前后仍相同。

因此 screen 替换、结构变化、关闭以及任意公开内容变化都会保守地使旧元素/物品 ref 失效。普通内容变化不提升 #54 `screenRevision`，但会提升本 Provider revision；二者不能互相替代。特别是 title 是 `screen` 字段的公开内容：title 改变通常不提升 #54 `screenRevision`，却必须提升 `current_screen_information.informationRevision`，所以所有由该 Provider 签发的 ref 仍按 origin revision 保守失效。首版接受 Provider-wide revision 的这种粒度，不承诺“只依赖结构”的 ref 在 title 或其他内容改变后继续有效，也不建立按槽位细分的隐藏 revision。

### 9.4 未知/模组 Screen

未知 Screen 至少可以返回 `screen` 与 `content_capabilities`。只有 adapter 实际证明的通用槽位、文字或控件才能进入 `elements`：

- Mineflayer `currentWindow.slots` 只能证明协议容器槽位内容；其 carried item 只有与当前 #54 Screen session 对齐时才可进入 `carried_item`。它不能证明控件树、屏幕坐标、hover、焦点或视觉顺序；
- 有槽位但无法证明角色时返回 `role=unknown`；
- 无真实 renderer 时省略 `bounds`；
- 无法识别控件时 `controls=unsupported`，不从 handler 方法或模组类反射；
- 已证明部分元素时返回部分 `elements` 并通过 `content_capabilities` 声明 coverage；完全没有安全内容时字段级 `not_supported`。

## 10. `advancement_information`

### 10.1 值与字段

```ts
interface AdvancementInformationValues {
  current_view: { screenInstanceId: string; selectedTabRef?: InformationSelectorRef; scroll?: 'top' | 'middle' | 'bottom' }
  tabs: Array<{ title: DisplayText; icon: DisplayItemStack; selected: boolean; tabRef: InformationSelectorRef }>
  nodes: Array<{
    title: DisplayText
    description: DisplayText
    icon: DisplayItemStack
    frame: 'task' | 'goal' | 'challenge'
    state: 'locked' | 'in_progress' | 'completed'
    displayedProgress?: DisplayText
  }>
  selected_node: null | { title: DisplayText; description: DisplayText; displayedProgress?: DisplayText }
}
```

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `current_view` | `AdvancementCurrentView` | `displayed` | `screen_projection` | 当前 advancement Screen 打开；否则 `not_currently_displayed` |
| `tabs` | paged `AdvancementTab[]` | `displayed` | `screen_projection` | `play` 且已同步可检查树；未支持 adapter 为 `not_supported` |
| `nodes` | paged `AdvancementNode[]` | `displayed` | `screen_projection` | 同上；只含已揭示节点，可用 tab selector 限定 |
| `selected_node` | nullable `AdvancementNodeDetail` | `exactly_displayed` | `screen_projection` | 节点详情当前可见；否则 `not_currently_displayed` |

`tabs/nodes` 是结构化等价读取，不自动打开成就 Screen。只有 tab 签发 `advancement.tab` ref，用于限定 `nodes`；它绑定 epoch/world 与本 Provider revision，但不默认 `bindToScreen`。节点列表不逐项签发 ref，避免把呈现目录变成对象枚举和突破每 Read ref 预算。从 `current_screen_information` 返回的实际 tab 控件 ref 仍严格 screen-bound，两类 ref 不可互换。

tabs/nodes 共用一次 query 的 page state；调用者宜分别请求，组合请求时 Provider 分别保存两个 offset。默认 16、单字段最大 24，按 tab 和节点的 UI 展示顺序分页；一页签发的 tab ref 不超过 24。隐藏且尚未揭示的节点、未同步 criteria 名称、服务端内部触发条件和不可见精确进度不返回。完成日期只有在客户端当前详情实际展示时才可加入未来字段，首版不从协议时间戳直接暴露。

Definition 建议：audiences `companion`；scope dependencies `connection, world, dimension, screen`；selector 可选，接受 `advancement.tab`；pagination `16/24`；`maxFieldsPerRead=4`、`maxResultBytes=64 KiB`、`timeoutMs=75`。

## 11. `recipe_book_information`

### 11.1 值与字段

```ts
interface RecipeBookInformationValues {
  current_view: {
    screenInstanceId: string
    categoryRef?: InformationSelectorRef
    filter: 'all' | 'craftable'
    searchText?: string
    page: number
  }
  categories: Array<{ title: DisplayText; selected: boolean; categoryRef: InformationSelectorRef }>
  recipes: Array<{
    result: DisplayItemStack
    craftableDisplay: 'yes' | 'no' | 'unknown'
    highlighted: boolean
  }>
  selected_recipe: null | {
    result: DisplayItemStack
    ingredients: Array<Array<DisplayItemStack>>
    craftableDisplay: 'yes' | 'no' | 'unknown'
  }
}
```

| 字段 | valueType | precision | sourceKinds | 当前 availability |
|---|---|---|---|---|
| `current_view` | `RecipeBookCurrentView` | `displayed` | `screen_projection` | 当前配方书实际打开；否则 `not_currently_displayed` |
| `categories` | paged `RecipeCategory[]` | `displayed` | `screen_projection` | `play` 且已同步 recipe book；否则 `not_supported` |
| `recipes` | paged `RecipeDisplay[]` | `displayed` | `screen_projection` | 只含已解锁且当前 category/filter 可检查的配方 |
| `selected_recipe` | nullable `RecipeDetail` | `displayed` | `screen_projection` | 当前 UI 已选择并显示详情；否则 `not_currently_displayed` |

`categories/recipes` 是结构化等价读取，不自动打开配方书、改变 category/filter/search 或选中配方。只有 category 签发 `recipe.category` ref，用于限定 recipes；它绑定 epoch/world 与 Provider revision。recipe 列表和装饰性 result/ingredient 不逐项签发 ref；当前实际 Screen 中可悬停的结果/原料由 `current_screen_information` 作为 screen-bound `screen.item` 拥有。

categories/recipes 共用一次 query 的 page state；调用者宜分别请求，组合请求时分别保存 offset。默认 16、单字段最大 24，一页签发的 category ref 不超过 24。projection 最多保留当前同步 revision 的已解锁集合，不保留旧世界/旧连接历史；cursor 绑定 category selector、当前 filter、字段集合和 information revision。新解锁、库存导致 craftable 显示改变、搜索/分类改变都会使旧 cursor 失效。搜索文本只在 MineIntent 自己管理的游戏内配方搜索框中读取，不读取系统输入法或剪贴板。

不返回未解锁 recipe、服务端私有配方条件、raw recipe ID（除非版本 UI 实际显示）、隐藏 ingredient tags 或从库存之外推导的合成计划。轮换 ingredient 图标只能返回当前 UI 可检查的候选集合；adapter 无法证明完整集合时 `selected_recipe` 为 `not_supported`，而不是猜 tag 展开。

Definition 建议：audiences `companion`；scope dependencies `connection, world, dimension, screen`；selector 可选，接受 `recipe.category`；pagination `16/24`；`maxFieldsPerRead=4`、`maxResultBytes=64 KiB`、`timeoutMs=75`。

`advancement_information` 与 `recipe_book_information` 在 v0.2 刻意接受 Provider-wide `screen` 绑定：两者同时包含结构化目录和 actual-current-view 字段，而公共 Runtime 尚无字段级 scope。结构化 `advancement.tab/recipe.category` ref 自身仍不 `bindToScreen`，但任何 Read/cursor 都会随无关 Screen instance/revision 变化保守返回 `scope_changed/invalid_page`，调用者从首页重试。不得再承诺结构化分页跨 Screen 保持；若实测重试成本不可接受，后续版本必须拆分 Provider 或先升级密封的字段级 scope contract，不能由 Provider 私自裁剪 dependency。

## 12. UiContext、input、overlay 与 screen 的关系

| UiContext 事实 | 消费方 | 约束 |
|---|---|---|
| `context.connectionState` | 全部 projection | 非 `play` 时内容清空或字段 `not_connected`；旧 epoch 事件拒绝 |
| `context.overlays.hudVisible: UiFieldState<boolean>` | HUD/Chat projection | available/false 才证明 HUD 隐藏；unavailable 原样传播，不能补 false |
| `context.overlays.subtitlesEnabled/bossBarsVisible` | HUD projection | 逐成员先判 availability；只门控依赖该事实的字段，不抹掉其他已证明 HUD 内容 |
| `context.overlays.chatMode` + `context.inputTarget` | Chat projection | 两者都 available，且分别为 input/chat，才证明 draft/completion 正在显示 |
| `context.overlays.playerListVisible` | PlayerList projection | available value 只决定 `display_state`；unavailable 不影响已独立证明的结构化 Tab entries |
| `context.mainSurface.kind=screen` | Screen projection | 建立内容 session；不是 screen 时 `screen_not_open` |
| `screenInstanceId/screenRevision` | Screen projection/ref | 与内容原子嵌入；替换、结构改变或关闭立即失效 |
| `context.inputTarget` 的 available/screen | Screen projection | 可公开 focus；unavailable 不得补 none，不授予点击或输入权限 |
| `screenFamily=advancements` | Advancement projection | 可证明 advancement current view；不决定已同步结构化内容是否存在 |
| RecipeBook projection 的 panel state | Recipe projection | 自己证明 open/filter/page 并绑定当前 screen instance；`screenFamily=recipe_book` 只覆盖独立主表面 |

#56 直接消费 #54 的内部 nested projection，而不是调用九字段 `ui_context` Provider。每个 reducer 只对自己实际依赖的 `UiFieldState` 做判别：available 分支读取 value/acquisition；`not_supported/not_exposed` 对依赖该事实的 #56 字段原样传播。无论断线、无头还是 adapter 缺失，都禁止用 `false/hidden/none` 填补 unavailable。

配方书通常作为 inventory/crafting/processing Screen 的嵌入面板，此时 #54 有意保留父 `ScreenFamily`。因此 `recipe_book_information.current_view` 只能由 RecipeBookProjection/local coordinator 的面板 state 证明，并与当前 `screenInstanceId` 原子绑定；不能把 `screenFamily === recipe_book` 作为唯一判据，也不能因父 family 不是 recipe_book 返回错误的 `not_currently_displayed`。

HUD/Chat/Tab overlay 的公开 value/availability 变化推动对应 Provider revision；provenance-only 变化只更新可信内部 snapshot。它们不推动 `screenRevision`。当前 Screen 的槽位、控件、文本、title 和进度变化推动 `current_screen_information.informationRevision`，不推动 #54 `screenRevision`。Screen 结构变化先由 #54 提升 `screenRevision`，再由 Screen projection 建立新结构 snapshot，并使旧 ref/cursor 失效。

`read` 永远无世界副作用。`structured_ui_equivalent` 只表示当前角色无需获得新世界信息即可检查已有客户端状态；它不发送按键、打开界面或占用 controller lease。

## 13. 并发、分页、预算和失效

### 13.1 原子 Read

- 每个 Provider 的 `availability()` 和 `read()` 各只 capture 一次自己的 projection。
- 一次 Read 的所有字段、page 和 selector 从该 snapshot 计算。
- Provider 返回一个 source，acquisition 固定为 `structured_ui_equivalent`，并使用 snapshot 的 public source revision/observedAt；不能把内部逐字段 provenance 动态折叠进公共 metadata。
- Runtime 负责 scope before/after、Zod 清洗、sourceKinds、deadline、结果字节和 cursor/ref 校验。
- projection 在 Read 中改变时，Runtime 根据声明 scope 拒绝 UI/screen race。world A→B→A 等 before/after 字符串相同的 ABA 由 Provider 单调且不重置的公开 `informationRevision` 后验复核拒绝；该共享实现由 PR #66 引入，在它合并前本模块的生产 Provider 不得合并。后验只比较公开 revision，不比较 `worldScopeRevision` 或 provenance，因此不会形成内部变化侧信道。

### 13.2 Cursor 与时窗

当前 Runtime 的 CursorStore 会无条件绑定签发时存在的 world/dimension/screen，同时没有绑定 `uiRevision`，与 Provider 声明的 scope 不一致。该共享缺口由 [#63](https://github.com/spojchil/mineintent/issues/63) 修复；在 #63 合并前，本节只作为目标契约，chat/player-list/current-screen/advancement/recipe 的分页实现不得合并。

#63 完成后，Runtime 在签发 cursor 时传入 Registry 中已经密封的目标 Provider `scopeDependencies`，CursorStore 按以下规则记录和续页校验：

- process session 始终绑定；
- `connection/world/dimension/ui/screen` 只在目标 Provider 声明对应 dependency 时绑定；
- `ui` 精确绑定 `uiRevision`，`screen` 精确绑定 instance/revision；
- principal、grant、audience、interface、fields、limit、page state、information revision、TTL、容量和一次性消费规则保持不变；
- Provider 不自行构造 cursor，也不能手工增删 scope。

带 selector 的分页仍先由 RefStore 解析 selector。selector 自己的 principal/epoch/world/screen/allowed-interface/签发源 revision 前后校验继续完整保留；cursor 按 Provider dependency 绑定不能复制、替代或放宽 selector 约束。于是一个不依赖 screen 的 chat/player-list cursor 不会因无关 Screen 开关失效，而 `current_screen_information` cursor 仍会随 instance/revision 立即失效。

| 接口/字段 | 默认/最大页 | 保留范围 | cursor 额外绑定 |
|---|---:|---|---|
| chat `messages` | 20/50 | 当前 epoch；最多 100 条且最多 10 分钟 | newest-first 页位置 |
| player list `entries` | 32/80 | 当前 projection，无历史 | 当前排序 revision |
| screen `elements` | 8/12 | 当前 screen instance/revision | element ordering + screen scope |
| advancement `tabs/nodes` | 16/24 | 当前 world 已同步且已揭示内容 | per-field offset + tab selector + Provider-wide screen scope |
| recipe `categories/recipes` | 16/24 | 当前同步的已解锁内容 | per-field offset + category selector + 当前 filter + Provider-wide screen scope |

Runtime cursor 是 opaque、一次性、有 TTL 的进程内状态；模型不能构造页位置。information revision 改变后续页返回 `invalid_page`，调用者从第一页重读。字段组合包含非分页字段时，这些字段可在每页重复，但仍计入字节预算。

### 13.3 数据和会话预算

- 采用第 6–11 节的 Provider `maxResultBytes`，并受 Tool Session 总返回字节预算再次限制。
- 单字段字符串、数组项数、样式段数和嵌套深度在 projection 边界截断；截断必须可观察为 capability/notes，不能静默冒充完整。
- ref/cursor 使用 Runtime 的每 Read 签发数、每 principal、每 interface、payload byte 与 TTL 上限。
- Provider Read 必须响应 `AbortSignal`；不得在 Read 内网络请求、磁盘扫描、协议查询或等待下一帧。
- projection retention 是界面注意窗口，不是长期记忆；Memory 只能保存模型实际读取并选择记忆的结果。

### 13.4 失效矩阵

| 事件 | HUD/Chat/Tab | Current Screen | Advancement/Recipe | ref/cursor |
|---|---|---|---|---|
| disconnect/reconnect | 清空，新 epoch | 清空 | 清空并等待新同步 | 旧 epoch 全失效 |
| world/dimension change | 清空世界内容 | Screen 清除 | 清空/重新同步 | world-bound 全失效 |
| HUD/chat/Tab 显示切换 | 对应 revision 变化 | 不变 | 不变 | 对应 cursor 失效 |
| Screen 替换/关闭 | overlay 独立 | 新 instance/清空 | current-view 变化 | screen-bound ref/cursor 失效 |
| Screen 结构变化 | 不变 | 同 instance，新 screen revision | current-view 可变化 | screen-bound ref/cursor 失效 |
| 槽位/控件/文本变化 | 不变 | content information revision 变化 | 若其 own projection 可见则变化 | 签发源 revision ref/cursor 失效 |
| 新聊天/玩家条目变化 | 对应 revision 变化 | 不变 | 不变 | 对应页 cursor 失效 |
| 新 advancement/recipe 同步 | 不变 | 若当前 Screen 可见则内容变化 | own revision 变化 | 对应 selector/cursor 失效 |
| grant 结束/deadline | 值不必改变 | 值不必改变 | 值不必改变 | Runtime 拒绝/清理 |

## 14. 隐私与非泄漏规则

- 聊天只含本连接客户端最终可显示文本；不含过滤前正文、签名链、公钥、举报上下文或其他会话历史。
- player list 不等于 entity tracker；不含 UUID、IP、精确 ping、skin URL 或未显示的 team/member 数据。
- scoreboard 只投影当前显示槽位，不允许通过 Help/分页枚举隐藏 objective/team。
- Screen 不返回 handler、window ID、state/transaction ID、raw slot index、NBT、hover/click payload 或控件回调。
- advancement 只含已同步且 UI 已揭示内容；隐藏节点和 criteria 内部名不可因已收到 packet 而泄漏。
- recipe 只含已解锁且可检查内容；不能枚举未解锁 recipe 或服务器私有条件。
- 错误、trace、evidence 只记录 interface/read/ref ID、revision、source kind 和失效原因，不记录聊天正文、玩家列表、物品或 Screen 文本。
- operator diagnostics 若需要 raw adapter 状态，必须走 `client_diagnostics`/privileged telemetry，不能给这六个 Provider 增加 operator-only 隐藏字段。

## 15. Mineflayer 与 Paper 的证明边界

### 15.1 Mineflayer 可以作为来源候选的内容

- 服务端发送且客户端维护的 scoreboard display slot、boss bar、title/action bar 和 player-info 数据；
- 当前协议容器的槽位内容与 cursor item；
- 本连接收到的聊天呈现候选；
- 已同步的 advancement/recipe 数据（具体覆盖取决于锁定版本 adapter）。

这些仍必须经过 presentation projection；raw 候选不能直接进入 Provider。

### 15.2 Mineflayer 无法单独证明的 client-local 缺口

- F1/HUD 真实隐藏状态、GUI scale、窗口尺寸、字体换行、裁剪和实际像素布局；
- 聊天框是否打开、焦点、滚动、opacity、淡出、draft、光标、选择区和补全 popup；
- Tab 键是否按下、实际列布局、当前屏幕可画出的行数和精确 ping 图标；
- 字幕方向/淡出、toast 生命周期、title 的实际渲染 phase；
- 客户端自身背包/菜单、通用控件树、hover、焦点、滚动、文本框和模组 widget；
- advancement/recipe 当前 tab、搜索、filter、页、选中项和真实 UI 展示；
- 任意模组 Screen 的语义、布局或 renderer 状态。

这些能力在 MineIntent local UI adapter 或真实客户端桥接完成前必须返回 `not_supported/not_currently_displayed`，不能用计时、packet 或已知原版布局猜测后标为 `exactly_displayed`。

### 15.3 Paper 能与不能证明的事实

Paper 可作为测试 oracle 驱动：发送消息、设置 scoreboard/boss bar、改变 player list、打开/更新/关闭服务端容器、授予 advancement/recipe、死亡/重生和维度/重连。Paper 能证明“服务端向该客户端提供了什么”和 screen 生命周期，不能证明 F1、Tab 键、聊天输入、本地菜单、像素布局、字幕、toast、GUI focus 或模组 renderer。后者必须用 adapter fixture 与真人客户端验收。

## 16. 模块与文件布局

```text
src/information/
├── source-ports/
│   ├── hud-presentation.ts
│   ├── chat-presentation.ts
│   ├── player-list-presentation.ts
│   ├── screen-presentation.ts
│   ├── advancement-presentation.ts
│   └── recipe-book-presentation.ts
├── projections/
│   ├── hud.ts
│   ├── chat.ts
│   ├── player-list.ts
│   ├── screen-content.ts
│   ├── advancement.ts
│   └── recipe-book.ts
├── providers/
│   ├── hud.ts / hud.definition.ts
│   ├── chat.ts / chat.definition.ts
│   ├── player-list.ts / player-list.definition.ts
│   ├── current-screen.ts / current-screen.definition.ts
│   ├── advancement.ts / advancement.definition.ts
│   └── recipe-book.ts / recipe-book.definition.ts
├── screen-information-module.ts
└── testing/screen-information-fixtures.ts

src/minecraft/driver/
├── hud-presentation-adapter.ts
├── chat-presentation-adapter.ts
├── player-list-presentation-adapter.ts
├── screen-presentation-adapter.ts
├── advancement-presentation-adapter.ts
└── recipe-book-presentation-adapter.ts
```

每个 projection/store/Provider/definition 都有相邻测试。`screen-information-module.ts` 只连接 source、#54 projection、六个 projection store 和 Provider factory；Registry 注册与 grant 仍由 #58 composition root 负责。公共导出不包含可变 store、raw source value 或 Driver adapter。

## 17. 测试与验收

### 17.1 契约测试

- Catalog/Help 可发现六个接口及本文全部字段、precision、sourceKinds、availability notes。
- 每个接口穷举单字段与多字段组合，断言 Provider 只 capture 一次、只返回请求字段且只有本文指定的 source kind。
- 六个 Provider 的所有字段组合都返回 canonical `structured_ui_equivalent`；无头/真实渲染差异只留在可信 fixture provenance，不动态改公共 acquisition。
- `UiFieldState` unavailable 原样进入依赖字段；projection 的 unavailable 判别分支在类型和序列化结果中都没有 value/provenance，测试明确禁止补 `false/hidden/none` 或旧值。
- Zod 严格清洗所有嵌套对象；额外 raw key、text event、window/slot ID 和 NBT 不可泄漏。
- `not_connected/screen_not_open/not_currently_displayed/not_supported` 是正常 partial result，不变成 `provider_failed`。
- page limit、result bytes、deadline、abort、ref issuance 和 stale cursor 公共套件通过。

### 17.2 projection/reducer 单元测试

- 旧 epoch、倒退 source revision、重复事件和 detach 后事件拒绝。
- world/dimension transition 提升 `worldScopeRevision` 后，旧 identity 的迟到 source snapshot 即使 revision 更高或之后返回同名维度也被拒绝。
- packet 已收到但不在 display slot/未解锁/未揭示时不会进入公开投影。
- UI gate 与内容在一个不可变 commit 中发布，不出现新 screen 身份配旧元素。
- raw 变化未改变公开值时 information revision 不提升。
- provenance-only 变化更新内部 snapshot，但不提升 Provider information revision、不改 public source revision/observedAt。
- HUD hide、chat mode、Tab display、Screen replace/structure/content change 的 revision 所有权符合本文矩阵。
- 嵌入 inventory/crafting/processing 的 recipe book 可产生绑定当前 screen instance 的 current view，且不要求父 family 为 `recipe_book`。
- role fixture schema 拒绝 unknown source type、重复/非法键与 kind/key/role 交叉组合；原版元素映射逐项覆盖精确 `minecraft-java:1.21.1/ui-source-types:v1` identity，未列项 unknown/omit；同 family 的 furnace/beacon/brewing 或 anvil/smithing/horse 不得共享错误布局。
- 字符串、数组、样式、retention 与 coverage 截断确定且有测试。

### 17.3 selector/cursor 测试

- screen element/item ref 同时绑定 instance、screen revision、签发源 information revision、principal、epoch 和 world。
- `carried_item` 只在 Screen 打开且来源可证明时返回，其 `screen.item` 可被 tooltip 接口接受；关闭 Screen 后立即失效，self inventory 不提供同一状态。
- Screen 替换/关闭/结构变化或普通内容变化后旧 ref 均拒绝。
- title 改变不要求 #54 screen revision 变化，但必须提升 current-screen Provider revision 并使其全部旧 ref 失效。
- ref 不能改 interface、kind 或 payload；未知 ID 返回 sanitized `invalid_selector`。
- chat/player/screen/advancement/recipe 的 continuation 在字段、selector、limit、revision 或 grant 改变后拒绝。
- advancement/recipe 的结构化 selector 本身不 screen-bound，但其 v0.2 Provider cursor 对任意 Screen instance/revision 变化保守失效；测试不得期待跨 Screen 续页。
- 异步 Read 中 world A→B→A 且最终字符串 identity 相同，仍因单调 Provider 公开 revision 后验返回 `scope_changed`；仅内部 provenance/world source churn 且公开 projection 不变时正常成功。
- 分页不会重复/跳过同一稳定 revision 内的项；新事件发生时要求重读第一页。

### 17.4 Paper 集成

1. 设置/清除 sidebar、boss bar、title/action bar，核对只公开当前呈现槽位。
2. 向 bot 发送 player/system chat，核对本 epoch 顺序、分页与重连清空。
3. 增删 Tab 玩家、header/footer、score 和延迟，核对显示档位而非精确 ping。
4. 打开箱子、熔炉、交易并更新槽位；关闭/替换后旧 selector 失效。
5. 授予 advancement/recipe，核对已揭示/已解锁集合与 cursor revision。
6. 死亡、维度切换、踢出和重连后，旧 screen/world/epoch 内容不可读。

测试只从 operator oracle 对照生产 Read，不把 Paper 状态注入 projection。

### 17.5 真人客户端验收

- 切换 F1、聊天 hidden/history/input、滚动与补全，逐项核对实际可见状态。
- 按下/松开 Tab，在不同 GUI scale/窗口尺寸核对 display state、排序与安全降级。
- 观察字幕、toast、title fade、boss bar 和 action bar 生命周期。
- 打开原版 inventory/container/processing/trade/text/menu/advancement/recipe Screen，核对槽位、控件、焦点、页和进度。
- 打开至少一个未知模组 Screen，确认 family/role 不猜测、无 renderer 时无 bounds、coverage 为 partial/unsupported。
- 检查 operator trace 不含聊天正文、玩家隐私、raw handler/window/slot/NBT。

## 18. 实施切片

### S1：公共呈现 DTO 与 contract harness

- 定义 DisplayText/DisplayItemStack/ScreenElement 严格 schema 与长度上限。
- 建六个 Provider definition、nested `UiFieldState` gate fixture、canonical acquisition 与任意字段组合单-source contract test。
- 不接 Mineflayer，不修改 Runtime 公共契约。
- 可以与 #63 并行，但任何真实 pagination/cursor 纵向切片不得在 #63 前合并；任何生产 Provider 不得在 PR #66 的普通 Read 公开 revision 后验进入目标基线前合并。

### S2：服务端可驱动 Overlay

- 前置：#54 UI Context 设计和 #63 Cursor scope 修订已合入目标基线。
- 实现 scoreboard、boss bar、title/action bar、chat 和 player list source/projection。
- 接入 HUD/Chat/PlayerList Provider 与分页。
- 完成可由 Paper 证明的集成测试；client-local 字段保持 `not_supported`。

### S3：当前协议 Screen 纵向切片

- 消费 #54 screen session，完成容器槽位、cursor stack、content revision。
- 先提交并评审 `screen-elements-1.21.1.json` 及 schema/重复键/unknown fallback 测试，再按精确版本化 source identity 实现通用 elements、screen-bound element/item ref 和 cursor；不按粗 family 猜布局。
- 覆盖箱子、熔炉、交易 open/update/replace/close。

### S4：Advancement 与 Recipe Book

- 建立已同步/已揭示/已解锁 projection 与 selector/page。
- 区分结构化等价字段和只在实际 Screen 可用的 current-view 字段。
- 完成授权、隐藏内容和 world/epoch 失效测试。

### S5：client-local UI 与未知 Screen

- 接入 MineIntent local UI adapter：HUD hidden、chat input、Tab visible、字幕/toast、控件/焦点/滚动、advancement/recipe current view。
- 覆盖嵌入父 Screen 的 recipe-book panel state，并与 screen instance 原子绑定。
- 对版本锁定原版 Screen 添加显式 adapter；未知/模组 Screen 只提供通用证明。
- 完成真人验收与非泄漏审计。

### S6：集成收尾

- 由 #58 在 composition root 注册六个 Provider 和 grants。
- 由 #57 固化精度、availability、selector/cursor、Paper/真人验收矩阵与 forbidden-import scan。
- 删除旧 snapshot/chat/GUI 认知旁路；不保留 fallback。

## 19. 开放问题与风险

1. **client-local adapter 载体**：Mineflayer 无 renderer。S5 是扩展 MineIntent 自有 UI 状态机、接真实客户端桥接，还是在 v0.2 明确缩小字段完成范围，需要实现前决定；无论选择哪种都不得伪造 available。
2. **chat retention**：本文采用 100 条/10 分钟上限。若锁定客户端可见历史与此不同，应以更小的已证明集合为准并更新 schema notes，不扩大为永久日志。
3. **Tab 结构化等价**：服务端可有极大 player list，而真实 GUI 会按窗口裁剪/分列。首版 entries 表达可检查列表，不声称每行此刻在像素中出现；若产品要求严格当前像素可见，需要真实 renderer。
4. **Screen role fixture 覆盖**：schema、唯一所有者、键和 fail-closed 语义已由 9.1 冻结；实现阶段仍需填充并评审 1.21.1 artifact。未覆盖 source type 保持 unknown/omit，不是允许实现者自行推断的开放问题。
5. **Provider-wide ref 失效**：任一 Screen 内容变化会使全部 element/item ref 失效，安全但保守。除非真实交互证明重试成本不可接受，v0.2 不扩展 Runtime 为细粒度 origin revision。
6. **Advancement/recipe selector 与 Provider scope**：结构化 selector 自身绑定 world/revision 而非 screen，实际控件 selector 由 `current_screen_information` 另行签发；但 v0.2 Provider/cursor 因混合 current-view 字段而保守绑定 screen。实现必须保持 ref kind 不可互换，也不能把“不 screen-bound 的 ref”误写成“Read/cursor 不受 screen 影响”。
7. **文本交互内容**：hover/click、书页链接、聊天 URL 与命令在首版全部省略。未来若公开，必须作为显式“当前可检查详情”字段设计，不能把 raw component 塞回 DisplayText。
8. **多语言与资源包**：服务端 translation key、客户端语言和资源包会影响最终显示文本。只有已应用资源包/locale 的 renderer 结果可标 `exactly_displayed`；仅有 key 时字段 `not_supported` 或降级为明确的 `displayed` 结果，不能泄漏未渲染参数。

## 20. 完成定义

1. 六个接口的 Help/Read、字段、precision、availability、limits 和分页均按本文实现。
2. 每个 Provider 任意字段组合只 capture 一个 projection snapshot，并返回唯一合法 source kind。
3. 六个 Provider 公共 acquisition 固定为保守的 `structured_ui_equivalent`；真实逐字段 provenance 留在可信 projection，provenance-only 变化不污染公共 revision/metadata。
4. Provider 之间无递归，任何 Provider/Context 都不能读取 Mineflayer Bot、raw packet 或旧 snapshot。
5. HUD/chat/Tab 与 Screen 内容逐成员匹配 #54 nested `UiFieldState`，unavailable 原样传播，并遵守 input、screen instance/revision 语义。
6. current Screen element/item ref 同时受 screen scope 与签发源 information revision 约束；title 等任一公开内容变化均按 Provider-wide revision 保守失效。
7. 聊天、玩家列表、Screen、成就和配方分页有界，旧 cursor 在相关 revision 变化后拒绝。
8. recipe book current view 支持嵌入父 Screen，不以 `ScreenFamily=recipe_book` 为唯一判据。
9. 未知/模组 Screen 只返回已证明的通用元素与 coverage，不猜语义、不泄漏 handler。
10. 所有 source snapshot 带 composition-owned world scope identity；dimension/world transition 后迟到内容不能复活，六个 Provider 的 scopeDependencies 与清理承诺一致。
11. projection 每字段使用判别联合；unavailable 分支没有 value/provenance，role/layout fixture 未列项一律 unknown/omit。
12. 隐藏 scoreboard/team、其他会话聊天、精确 ping、未解锁 recipe/advancement 和 raw GUI 数据均有非泄漏测试。
13. Paper 测试只证明服务端可观察事实；Mineflayer 缺失的 client-local 能力由真人客户端验收或明确 unavailable。
14. 读取无世界副作用，不自动打开界面，不生成动作宏，不占用 controller lease。
15. #54 已先行合入；#63 已使 cursor 只绑定目标 Provider 声明的 scope，且 selector 的独立 RefStore 约束没有被放宽。
