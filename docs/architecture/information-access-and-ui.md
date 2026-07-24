---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# 合法信息接口、Help 发现与 UI 会话

> v0.2 接受设计基线，2026-07-14。当前实现了 Information Runtime 和四个简化被动接口，但完整 Catalog/Help/UI/Screen 字段体系未完成，生产模型也不主动调用 Information tools。上游：[产品设计](../product-design.md)、[目标系统](./target-system.md)、[认知感知](./cognitive-perception.md)。

## 1. 决定

MineIntent 先定义角色可以合法获得哪些信息、如何发现字段、以什么精度读取，以及这些信息对不同消费者有什么用途。Grounding、Behavior、Motor 和复杂任务必须建立在该边界之上，不能先获得完整客户端状态再事后过滤。

v0.2 优先交付全信息目录与首批读取实现；原“可信具身闭环”顺延至 v0.3。此前提出的“Behavior 必须逐 tick 输出全部玩家输入”不是已接受决定，v0.2 不固定动作计划或连续控制器的最终形态。

信息接口是结构化无障碍读取能力，不要求无头客户端真实渲染文字再 OCR；但其结果不得超过相同版本、设置、游戏模式和当前情境下原版客户端能向玩家展示或让玩家立即检查的信息。

## 2. “合法信息”不是一份全局快照

相同 raw 数据对不同消费者有不同权限：

| 层 | 可以拥有 | 不得产生的效果 |
|---|---|---|
| Protocol Driver | 协议实体 ID、加载区块、精确物理状态、screen handler 和 transaction | 直接成为 Companion 事实或普通规划地图 |
| Information Adapter | 将 raw 状态转换为 HUD、F3、GUI、视觉、声音和身体信息 | 通过转换保留界面没有显示的精度 |
| Companion / Context | 当前读取结果、合法观察、有来源记忆和 unknown | 搜索 raw world、读取未打开外部容器 |
| Grounding / Planner | 认知信息、带证据引用和 Epistemic Map | 把 protocol tracked 当作已看见或已知道 |
| Scoped Controller | 已选对象在当前控制行为所需的局部空间解 | 搜索其他目标、把精确实现数据写回认知 |
| Safety | 校验下一局部控制所需的最小 allow/deny/risk | 返回隐藏地形、替代路线或未感知实体身份 |
| Test Judge | Paper/服务端真相 | 进入生产认知、规划或语言 |

信息是否合法由来源、消费者、用途、精度、时效和世界作用域共同决定，不能只靠字段名判断。

## 3. 接口发现：Catalog → Help → Read

### 3.1 总目录

主模型不需要在系统提示词里永久携带全部字段。它先读取轻量目录：

```ts
type InformationInterfaceId =
  | 'ui_context'
  | 'current_status'
  | 'hotbar_information'
  | 'inventory_information'
  | 'item_tooltip_information'
  | 'f3_information'
  | 'crosshair_information'
  | 'hud_information'
  | 'chat_information'
  | 'player_list_information'
  | 'current_screen_information'
  | 'advancement_information'
  | 'recipe_book_information'
  | 'viewport_information'
  | 'sound_information'
  | 'lifecycle_information'
  | 'client_diagnostics'

type InformationAudience = 'companion' | 'controller' | 'operator'

interface InformationCatalogRequest {
  operation: 'list_interfaces'
  knownCatalogRevision?: string
}

type InformationCatalogResult =
  | {
      protocol: 'mineintent.information-catalog.v1'
      status: 'ok'
      targetMinecraftVersion: string
      negotiatedMinecraftVersion?: string
      catalogRevision: string
      interfaces: Array<{
        id: InformationInterfaceId
        description: string
        schemaRevision: string
        audiences: InformationAudience[]
        availability: 'available' | 'partially_available' | 'unavailable'
      }>
    }
  | {
      protocol: 'mineintent.information-catalog.v1'
      status: 'not_modified'
      catalogRevision: string
    }
```

Catalog 只说明接口存在和当前大类可用性，不返回游戏值，也不列出未打开容器的内容。Runtime 根据调用者身份过滤 `audiences`；模型不能在请求里把自己改成 `controller` 或 `operator`，Companion Catalog 默认不包含 `client_diagnostics`。

### 3.2 统一 Information 查询支持 Help 与 Read

```ts
interface InformationPageRequest {
  cursor?: string
  limit?: number
}

type InformationQueryRequest<Field extends string = string, Selector = InformationSelectorRef> =
  | {
      interfaceId: InformationInterfaceId
      operation: 'help'
      availability?: 'all' | 'current'
      search?: string
      fields?: Field[]
    }
  | {
      interfaceId: InformationInterfaceId
      operation: 'read'
      schemaRevision: string
      fields: Field[]
      selector?: Selector
      page?: InformationPageRequest
    }
```

17 个逻辑信息接口统一使用 `interfaceId + operation: help | read` 协议，不注册为 17 个 model-facing 工具。`help/all` 返回当前版本理论支持的字段；`help/current` 同时给出此刻是否可读和不可读原因。Help 是接口元数据，不是世界观察，不能借此泄露当前值。当前生产模型不直接调用 Catalog/Help/Read；Context Composer 从受信任 Runtime 预先组合有界的被动观察。具体模块边界见 [Information Runtime 模块设计](./information-runtime.md)。

```ts
type InformationSourceKind =
  | 'client_state'
  | 'hud_projection'
  | 'debug_projection'
  | 'screen_projection'
  | 'viewport_projection'
  | 'sound_projection'
  | 'lifecycle_event'
  | 'operator_diagnostic'

type InformationAvailability =
  | 'available'
  | 'not_connected'
  | 'screen_not_open'
  | 'not_currently_displayed'
  | 'blocked_by_reduced_debug'
  | 'unsupported_game_mode'
  | 'permission_required'
  | 'not_supported'
  | 'not_exposed'

interface InformationFieldHelp {
  id: string
  description: string
  valueType: string
  unit?: string
  precision: 'displayed' | 'quantized' | 'exactly_displayed' | 'inferred'
  interfaceId: InformationInterfaceId
  sourceKinds: InformationSourceKind[]
  availability: InformationAvailability
  requires?: string[]
  notes?: string
}

interface InformationHelpResult {
  protocol: 'mineintent.information-help.v1'
  interfaceId: InformationInterfaceId
  schemaRevision: string
  availabilityMode: 'all' | 'current'
  fields: InformationFieldHelp[]
}
```

首次读取某接口或 schema revision 改变后，模型先 Help，再把 `schemaRevision` 带回 Read。相同 revision 可以缓存，不强制每次重复 Help。未知字段必须拒绝并提示重新 Help，不能模糊匹配到相近的内部属性。

### 3.3 读取结果

```ts
interface InformationReadResult<T> {
  protocol: 'mineintent.information-read.v1'
  readId: string
  interfaceId: InformationInterfaceId
  schemaRevision: string
  informationRevision: number
  connectionEpoch: number
  worldId?: string
  dimension?: string
  observedAt: string
  validUntil?: string
  source: {
    kind: InformationSourceKind
    adapterRevision: string
    sourceRevision: number
    acquisition:
      | 'immediate_client_state'
      | 'structured_ui_equivalent'
      | 'current_screen'
      | 'current_perception'
      | 'operator_only'
  }
  values: Partial<T>
  unavailable: Array<{
    field: string
    reason:
      | Exclude<InformationAvailability, 'available'>
      | 'stale_selector'
      | 'wrong_world'
      | 'wrong_screen'
  }>
  evidenceIds: string[]
  nextCursor?: string
}

interface InformationRequestError {
  protocol: 'mineintent.information-error.v1'
  interfaceId?: InformationInterfaceId
  code:
    | 'invalid_request'
    | 'unknown_interface'
    | 'stale_schema'
    | 'unknown_field'
    | 'invalid_selector'
    | 'invalid_page'
    | 'audience_denied'
    | 'scope_changed'
    | 'budget_exceeded'
    | 'deadline_exceeded'
    | 'provider_failed'
  message: string
  currentCatalogRevision?: string
  currentSchemaRevision?: string
  rejectedFields?: string[]
}

type InformationToolResult<T> =
  | InformationHelpResult
  | InformationReadResult<T>
  | InformationRequestError
```

Read 允许部分成功。某字段不可用不应使其他合法字段消失，也不得用内部值静默补齐。整个请求结构无效、schema 陈旧、字段未知或 audience 越权时返回 `InformationRequestError`，不产生半猜测结果。`invalid_request` 专门表示 envelope、额外字段、重复字段或基本限额不合法，不用 `provider_failed` 掩盖调用方错误。`source` 证明本次值经过哪类适配器取得；`evidenceIds` 连接到具体协议/观察证据，但不能让调用者反查 raw world。

### 3.4 Selector、cursor 与大小边界

任何 selector 都是信息接口签发的 opaque ref，不是模型自行填写的实体 ID、方块坐标、槽位地址或任意查询表达式：

```ts
interface InformationSelectorRef {
  protocol: 'mineintent.information-selector-ref.v1'
  id: string
  interfaceId: InformationInterfaceId
  connectionEpoch: number
  worldId?: string
  screenInstanceId?: string
  basedOnInformationRevision: number
  validUntil?: string
}
```

- selector 只能回传给签发它的接口或 Help 明确声明的兼容接口；Runtime 校验 audience、epoch、world、screen instance、有效期，以及签发源 Provider 当前的 information revision。目标 Read 前后任一次不匹配都拒绝结果。
- 要求 screen binding 的 selector 只有在 screen instance 与 screen revision 均存在时才能签发；selector 内部 payload 是有字节上限的 JSON DTO，不能保存 raw Minecraft 对象。
- cursor 同样 opaque，并绑定接口、字段集合、selector、revision 与调用者；不能修改、跨接口复用或在 revision 改变后继续翻页。
- 每次 Read 的字段数、`limit`、返回字节数、ref 签发数和耗时有硬上限；Ref/Cursor Store 还有 payload/page-state、principal、interface 和全局容量上限。清理过期项后仍超限就拒绝新增，不淘汰其他作用域的有效项。
- 无 selector 的 `viewport_information` 和 `sound_information` 只返回当前有界投影；selector 只能细看已返回观察，不能扩大 FOV、距离或时间窗口。

## 4. UI 状态需要独立接口

需要单独设计，但应保持很小。当前是否处于 UI、是什么 UI，决定鼠标/键盘含义、可读取内容、transaction revision 和后续可用接口。

Minecraft 客户端同时存在三类相关但不能混写的界面状态：

1. **主表面会话**：世界画面或一个完整 screen，两者互斥；
2. **输入归属**：此刻世界、当前 screen、聊天输入或没有任何一方接收玩家输入；
3. **叠加层**：HUD、F3、聊天历史、Tab 列表、boss bar、字幕等，可在世界或部分 screen 上叠加。

不能用单个 `isInUi: boolean` 表达三者。尤其是打开聊天输入时，世界仍是背景主表面，但移动键和文本已经归聊天；F3 和 Tab 通常只是 overlay，不是容器 screen。

projection 内部以判别状态绑定值和可用性，不能把 availability 放在旁路 map：

```ts
type UiFieldState<T> =
  | {
      availability: 'available'
      value: Readonly<T>
      acquisition:
        | 'immediate_client_state'
        | 'structured_ui_equivalent'
        | 'current_screen'
    }
  | { availability: 'not_supported' | 'not_exposed' }

interface UiContextV1 {
  protocol: 'mineintent.ui-context.v1'
  connectionEpoch: number
  uiRevision: number
  connectionState: 'disconnected' | 'connecting' | 'configuration' | 'play'
  mainSurface:
    | { kind: 'none'; reason: 'not_connected' | 'transition' }
    | { kind: 'world' }
    | {
        kind: 'screen'
        screenInstanceId: string
        screenRevision: number
        screenFamily:
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
        registryId?: string
        title?: string
        pausesWorld: boolean
      }
  inputTarget: UiFieldState<
    | { kind: 'world' }
    | {
        kind: 'screen'
        screenInstanceId: string
        mode: 'pointer' | 'text' | 'navigation'
      }
    | { kind: 'chat'; mode: 'text' | 'command' }
    | { kind: 'none' }
  >
  overlays: {
    hudVisible: UiFieldState<boolean>
    debugVisible: UiFieldState<boolean>
    chatMode: UiFieldState<'hidden' | 'history' | 'input'>
    playerListVisible: UiFieldState<boolean>
    subtitlesEnabled: UiFieldState<boolean>
    bossBarsVisible: UiFieldState<boolean>
  }
}

interface UiContextReadValues {
  connection_state: UiContextV1['connectionState']
  main_surface: UiContextV1['mainSurface']
  input_target: Extract<UiContextV1['inputTarget'], { availability: 'available' }>['value']
  hud_visible: boolean
  debug_visible: boolean
  chat_mode: 'hidden' | 'history' | 'input'
  player_list_visible: boolean
  subtitles_enabled: boolean
  boss_bars_visible: boolean
}
```

`UiContextReadValues` 仍通过 `InformationReadResult.values: Partial<T>` 返回。Provider 仅把 available state 的 `.value` 写入对应字段；unavailable state 只进入 `unavailable[]`。overlay 必须拆为独立读取字段，不能为了返回一个完整对象而填入虚假的 `false/hidden`。

无头 Mineflayer 的结构化 UI/input coordinator 只能在内部 field state 标记 `structured_ui_equivalent`，不能证明一个不存在的渲染客户端实际显示了 F3、Tab、背包或菜单；只有真实渲染来源才能在内部标记 `current_screen`。公共 v1 Read 的 acquisition 是 result-wide，无法表达混合来源，因此 `ui_context` 固定保守返回 `structured_ui_equivalent`，Help 对这些归一化字段统一使用 `precision: inferred`。公开逐字段 provenance 需要未来提升公共契约。

内部 UI projection 与公开 Information projection 使用不同 revision：基础 acquisition 或 field acquisition 单独改变时，内部 `uiRevision` 必须提升并通知可信消费者；公开九字段的 value/availability 未变，因此 `informationRevision`、公开 `sourceRevision` 和 `observedAt` 必须保持。Provider 不能把内部 revision 直接复用为公开 revision。

规则：

- `ui_context` 只描述界面状态，不返回槽位、文字或控件内容；内容由对应信息接口读取。
- 可用的 `inputTarget.value` 必须引用当前 screen instance；不能只从 `mainSurface` 推断键鼠含义，unavailable 也不能被解释为 `none`。
- `inputTarget.value: world` 只允许与 `mainSurface: world` 同时出现；`inputTarget.value: screen` 的 instance 必须与主 screen 相同；`inputTarget.value: chat` 必须对应可用的 `chatMode.value: input`；断线和过渡期使用可用的 `none`。
- screen 打开、关闭、替换、布局/控件集合变化或死亡/重生会提升 `screenRevision`；旧控件和槽位 selector 失效。普通槽位、文本或进度更新只提升对应 `informationRevision`，不让稳定 selector 每 tick 失效。
- 多人游戏中的菜单通常不暂停世界，必须按真实 `pausesWorld` 表达。
- F3、HUD 和 Tab 是 overlay，不应被错误当作容器 screen。
- `unknown` screen 仍可通过通用可见控件读取；不允许因无法分类而暴露 handler 内部对象。

## 5. 全信息接口目录

| 接口 | 主要信息 | 默认获取方式 |
|---|---|---|
| `ui_context` | 主表面、screen family/revision、输入归属、逐项 overlays | 接口始终存在；字段可 partial unavailable |
| `current_status` | 生命、吸收、护甲、饥饿条、空气、经验、效果、姿势、骑乘 | HUD/自身可检查状态 |
| `hotbar_information` | 9 个槽位、选择槽、主副手、可见数量/耐久条、冷却 | HUD |
| `inventory_information` | 自身主物品栏、盔甲、副手、合成格、cursor stack | 结构化检查自身背包 |
| `item_tooltip_information` | 名称、lore、附魔、属性和当前可显示的高级提示 | 指定可见 item handle |
| `f3_information` | 坐标、朝向、维度、生物群系、光照、本地难度和准星 debug | 结构化等价于查看 F3，遵守 reduced debug |
| `crosshair_information` | 当前命中类别、观察引用、方块面、交互可达、可见进度 | 当前第一人称准星 |
| `hud_information` | 计分板、boss bar、title/action bar、字幕、toast | 当前 HUD/overlay |
| `chat_information` | 可见聊天、系统消息、输入焦点和补全 | 当前聊天界面与历史 |
| `player_list_information` | 显示名、队伍样式、spectator、ping 图标、header/footer | 结构化等价于查看 Tab |
| `current_screen_information` | 当前 screen 的可见槽位、控件、文本和进度 | 仅当前 screen instance |
| `advancement_information` | 当前可见 tab、节点、说明和显示进度 | 成就界面/结构化检查 |
| `recipe_book_information` | 已解锁、当前可见和可筛选的配方信息 | 配方书/结构化检查 |
| `viewport_information` | 当前相机可见的方块面、实体、粒子、天气和遮挡 | 第一人称感知边界 |
| `sound_information` | 可听类别、粗方向、距离带、响度和不确定性 | 听觉转换，不返回包坐标 |
| `lifecycle_information` | 登录、出生、死亡、维度切换、资源包请求、断线 | 客户端生命周期 |
| `client_diagnostics` | FPS、内存、CPU、GPU、渲染统计 | 运维/调试，默认不进角色认知 |

接口目录是客户端信息来源清单，不是模型每轮上下文。模型按需 Help/Read；Attention/Context 可以主动附带少量与当前事件直接相关的读取结果。

### 5.1 可用条件、selector 与边界

| 接口 | Companion 可读条件 | selector / 分页 | 必须阻止的泄漏 |
|---|---|---|---|
| `ui_context` | 始终；断线时返回 `none`/`none` | 无 | screen handler 内部对象 |
| `current_status` | `play` | 无 | 隐藏 saturation、不可见小数和内部 tick |
| `hotbar_information` | `play` | 可按槽位字段读取 | 背包其余未请求内容 |
| `inventory_information` | 自身已同步背包；不要求真的打开背包 UI | 槽位页或已签发 item ref | 外部容器、其他玩家背包 |
| `item_tooltip_information` | item ref 当前对玩家可检查 | 必须有 item selector | 未显示组件、服务端私有 NBT |
| `f3_information` | `play`；遵守版本、权限和 reduced debug | 字段子集 | 被隐藏的坐标、目标或服务器数据 |
| `crosshair_information` | `play` 且姿态有效 | 当前命中或已签发 observation ref | 准星外实体/方块搜索 |
| `hud_information` | 对应 overlay 当前可展示 | 类型/页 cursor | 隐藏 scoreboard/team 数据 |
| `chat_information` | 当前会话合法接收的聊天历史 | 时间/消息 cursor，有长度上限 | 服务端未发送消息、其他会话历史 |
| `player_list_information` | 结构化等价于当前可查看 Tab | 有界分页 | 未在列表中展示的 tracked player |
| `current_screen_information` | 当前 screen instance 打开 | screen/element selector | 未打开容器和 handler 隐藏属性 |
| `advancement_information` | 当前角色可检查的成就数据 | tab/page cursor | 未同步或隐藏进度 |
| `recipe_book_information` | 当前角色已解锁且可检查 | filter/page cursor | 未解锁配方和服务器私有规则 |
| `viewport_information` | 当前有界认知投影 | observation selector | FOV/遮挡外 raw entity/block |
| `sound_information` | 当前未过期听觉投影 | observation/time cursor | 声音包精确坐标和 raw source ID |
| `lifecycle_information` | 当前连接 epoch | event cursor | 旧 epoch 冒充当前状态 |
| `client_diagnostics` | 仅 operator audience | 诊断字段子集 | 进入角色认知或行为 Grounding |

## 6. 第一批字段清单

### 6.1 `current_status`

- `health`、`max_health`、`absorption`；
- `armor_display`；
- `food_display`；
- `air_display`；
- `experience_level`、`experience_progress`；
- `attack_cooldown_display`；
- `status_effects_display`；
- `pose`、`on_ground`、`mount_state`；
- `fire_state`、`freeze_state`。

精确隐藏 saturation、界面无法分辨的生命小数和效果剩余 tick 标记为 `not_exposed`。内部物理可以拥有这些值，但不能从当前状态工具返回。

### 6.2 `hotbar_information` 与 `inventory_information`

- 槽位、物品注册名/显示名、数量；
- 选择槽、主手、副手；
- 盔甲槽、2×2 合成格和输出；
- 当前 cursor stack；
- 可见耐久条；
- 当前界面真实展示的 tooltip 信息。

自身背包可以结构化检查；未打开的外部容器、其他玩家背包和未打开末影箱不可读取。

### 6.3 `f3_information`

- 坐标、方块/区块坐标；
- facing、yaw、pitch；
- 维度、生物群系；
- 客户端实际显示的光照、高度和本地难度；
- 当前 targeted block/fluid/entity 的 debug 信息；
- 当前 F3 权限状态与 `reducedDebugInfo`。

Java/Minecraft 版本、FPS、内存、CPU、GPU、显示设备和渲染统计归入 `client_diagnostics`，不与世界认知混合。

### 6.4 `current_screen_information`

通用字段：

- screen family、registry ID、title 和 revision；
- 可见槽位、cursor stack；
- 可见按钮、启用状态和选择状态；
- 文本、文本框、焦点；
- 进度条、燃料条和界面显示属性；
- 当前页、滚动位置和可见列表项。

首批 screen family 覆盖自身背包、通用容器、工作台/Crafter、熔炉类、酿造台、附魔、铁砧/砂轮/锻造、切石/织布/制图、信标、交易、马匹物品栏、书/告示牌、死亡/睡眠、资源包和断线界面。

## 7. 获取动作与信息读取的关系

信息接口允许结构化直接读取，不要求 OCR，但读取仍有获取语义：

- HUD、当前状态、快捷栏和 `ui_context` 可以立即读取；
- F3、Tab、背包、成就和配方书可作为“查看相应界面”的结构化等价操作，记录 acquisition event；
- 外部容器内容只有当前合法 screen 打开后才能读取；
- tooltip 需要当前可见 item handle；
- viewport/sound 只能来自当前感知，不因 Help 或 Read 主动搜索 raw world；
- 读取不会自动生成长期记忆。是否注意、相信和记住仍由 Companion/Memory 决定。

`read` 本身是无世界副作用的信息操作：不会自动按键、打开/关闭界面、切换 F3、移动视角或占用身体 controller。`structured_ui_equivalent` 表示“玩家此刻本可立即检查”，并记录一次 acquisition/attention event；如果信息必须先打开外部容器或改变视角，当前 Read 返回 unavailable，由后续 Companion/Behavior 决定是否产生独立具身意图。

后续可以为 acquisition 建模注意成本和与世界控制的并发关系，但不能因此把界面字段藏回完整 Backend snapshot。

## 8. 版本、缓存与失效

- Catalog、每个接口 schema、信息值和 UI screen 分别有 revision。
- Minecraft 版本变化时重新生成字段清单并做差分测试。
- `connectionEpoch`、world、dimension、screen instance 或签发源 information revision 改变会使相关 selector 失效。
- cursor 还绑定字段集合、selector、information revision 和 audience；任一项变化都拒绝续页。
- `help` 可以缓存到 schema revision；`read` 值不能跨 information revision 冒充当前事实。
- v0.2 首次交付前可直接修订字段；正式发布后若需重命名，再通过版本化 alias/deprecation 演进，不做自然语言近似匹配。当前原型不建立 alias 兼容层。
- 当前实现缺失的合法字段返回 `not_supported`，不得退回 raw Mineflayer 对象。

## 9. v0.2 实施顺序

1. 定义 Information Catalog、Help/Read envelope、字段注册表和 schema revision。
2. 实现 `ui_context`，区分主表面会话、输入归属与 overlays。
3. 以新 Provider/source-port 契约实现 `current_status`、hotbar、inventory、chat 和 lifecycle；删除旧认知 DTO 路径。
4. 实现 F3、crosshair、HUD、player list 和当前 screen 的字段适配。
5. 将现有实体、方块、声音设计接入同一 Catalog/Help/Read 发现方式，保持 Perception 权限边界。
6. Context 只注入主动读取或 Attention 选择的信息结果，不再发送完整 Backend snapshot。
7. 建立字段精度、screen revision、reduced debug、未打开容器和 raw-world 非泄漏测试。

## 10. 参考基线

- [Minecraft Java Edition Controls](https://help.minecraft.net/hc/en-us/articles/360059148111) 用于核对玩家可主动查看的快捷键、HUD、聊天、玩家列表、背包和视角入口。
- [Accessibility Settings for Minecraft: Java Edition](https://help.minecraft.net/hc/en-us/articles/360061018612-Accessibility-Settings-for-Minecraft-Java-Edition) 用于核对按住/切换输入等客户端设置对信息与控制状态的影响。
- 具体字段和 screen handler 必须按项目锁定的 Minecraft 版本生成并做差分测试；参考资料不能代替运行时版本、服务器设置和 `reducedDebugInfo` 检查。

## 11. v0.2 完成定义

1. 模型可以通过 Catalog → Help → Read 发现并读取合法字段，不需要提示词内置完整清单。
2. `ui_context` 正确区分断线/过渡、世界、当前 screen、输入归属和可叠加 overlays。
3. 状态、F3、快捷栏、背包、HUD、聊天、Tab、当前 screen、视觉和声音均在目录中有明确 schema/availability。
4. `reducedDebugInfo`、隐藏 saturation、未打开容器、raw entity/world 和测试裁判不会通过接口泄漏。
5. 所有结果带版本、revision、epoch、结构化 source、证据与不可用原因；旧 selector/cursor 可确定性拒绝。
6. Companion/Context 不再依赖完整 `MinecraftSnapshotV1` 或 `ProtocolObservationSource` 作为认知输入。
7. 至少一个模型场景能先 Help，再按需读取生命/饥饿/效果、F3 和当前 screen，而不请求不存在字段。
8. 后续 Grounding、Behavior 和 Motor 只能依赖这些信息端口或用途受限的 controller view，不能重新引入全局快照旁路。
