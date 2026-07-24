---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_pr: 64
source_commit: a213690
---

# 玩家自身状态、物品与调试信息模块设计

> 原始状态声明（PR 未合并）：Issue #55 实现就绪基线
>
> 日期：2026-07-14
>
> 对应 Issue：[#55](https://github.com/spojchil/mineintent/issues/55)
>
> 上游：[Information Runtime](../../architecture/information-runtime.md)、[合法信息接口与 UI](../../architecture/information-access-and-ui.md)、[#67 UI Context 模块](../../architecture/ui-context.md)、[v0.2 计划](../roadmap-v0.2-legal-information-interfaces.md)
>
> 依赖：[#54 UI Context](https://github.com/spojchil/mineintent/issues/54)、[#63 按 Provider scopeDependencies 绑定分页 Cursor](https://github.com/spojchil/mineintent/issues/63)；与 [#34 第一人称视觉](https://github.com/spojchil/mineintent/issues/34) 共享当前准星观察边界

## 1. 决定与范围

#55 实现七个独立 Information Provider：

- `current_status`：HUD 与玩家可直接感知的自身状态；
- `hotbar_information`：九格快捷栏、当前选择和双手；
- `inventory_information`：玩家自身已同步的物品栏；
- `item_tooltip_information`：由合法 item selector 指定物品的可显示提示；
- `f3_information`：Minecraft 1.21.1 F3 的结构化等价信息；
- `crosshair_information`：当前第一人称准星及其当前命中；
- `client_diagnostics`：只面向 operator 的客户端/运行时诊断。

这些接口描述玩家现在能合法查看或感知的信息，不是任务能力。它们不得加入“找木头”“选择工具”“清理障碍”等任务字段，也不根据自然语言改变可见内容。

本模块采用以下不可妥协边界：

1. Provider 只读最小 source port；不导入 Mineflayer `Bot`、`MinecraftSnapshotV1`、`ProtocolObservationSource`、其他 Provider 或 `InformationRuntime`。
2. 旧 `backend.snapshot()` 不能作为过渡输入；driver 直接从当前协议事件和受控客户端状态更新新投影。
3. 投影先完成显示量化、字段白名单和隐藏值删除，Provider 不能收到 saturation、精确效果 tick、raw NBT、协议实体 ID或未过滤机器信息。
4. Read 不按键、不打开背包、不切换 F3、不移动视角；`structured_ui_equivalent` 只表示玩家此刻可以立即检查。
5. 外部容器、其他玩家背包、未打开末影箱和准星外世界不属于本模块。

## 2. 与 #54、#34 和 #56 的所有权边界

### 2.1 消费 #54，不复制 UI 状态机

#55 直接消费 #67 导出的 `UiContextProjection`/`UiContextProjectionSnapshot`，不重声明其结构、不保留旧 `fieldAvailability` map，也不建立 compatibility adapter：

```ts
function consumeUiContext(
  snapshot: Readonly<UiContextProjectionSnapshot>,
): void {
  const { connectionState, mainSurface } = snapshot.context
  const debugState = snapshot.context.overlays.debugVisible

  if (debugState.availability === 'available') {
    const debugActuallyVisible = debugState.value
    // 可以记录已证明的 overlay 状态；不能用它决定 F3 结构化 Read 是否合法。
    void debugActuallyVisible
  }

  updatePlayerStateEligibility({ connectionState, mainSurface })
}
```

`connectionState` 与 `mainSurface` 是 #67 始终可用的基础值；`inputTarget` 和每个 overlay 都是 `UiFieldState<T>`，只有 `availability === 'available'` 的分支才能读取 `.value`。`baseFieldAcquisition` 与 available state 的 `acquisition` 只描述可信内部 provenance；#55 不用 `baseFieldAcquisition`、`debugVisible.acquisition` 或本次 `uiRevision` 决定 `f3_information` 的结构化等价读取。

- `screenInstanceId` 在 screen 新开或替换时重新生成；`screenRevision` 只随公开身份、槽位拓扑、控件集合或布局结构变化。
- 普通槽位、文本和进度变化不提升 `screenRevision`，由对应内容 Provider 的 `informationRevision` 表达。
- F3、聊天和 Tab 是 overlay，不是 screen；F3 overlay 是否实际显示不决定结构化 F3 Read 是否可用。
- 自身 inventory 的结构化检查不要求打开 inventory screen；外部容器内容由 #56 的 `current_screen_information` 拥有。
- `unknown` screen 是合法的 #67 projection 值。#55 不扩充公共 availability 枚举，也不根据 registry ID/title 猜 screen family；需要世界主表面的 F3/crosshair 在该状态返回 `not_currently_displayed`。

### 2.2 与 #34 共享观察，不做第二次全知 raycast

`crosshair_information` 读取 #34/Perception 发布的当前相机样本投影。它可以为当前命中签发 observation ref，但不能调用 `bot.blockAt`、枚举实体或沿射线查询 raw world。`f3_information` 的 targeted debug 字段也读取同一份经过 F3 可见性转换的投影，不调用 `CrosshairProvider.read()`。

### 2.3 与 #56 的物品边界

- #55 拥有玩家自身 inventory、hotbar 以及这些来源签发的 item ref。
- #56 拥有当前 screen 的可见外部槽位、控件、文本和 screen element ref。
- `item_tooltip_information` 可以接受双方签发的 item ref；本 Issue 先实现 `inventory.item` 与 `hotbar.item`，为 #56 预留 `screen.item` kind，但不读取 #56 的 screen 内容。
- screen item ref 必须 `bindToScreen: true`；自身 inventory/hotbar item ref 不绑定 screen。

早期全信息清单曾把鼠标携带物（cursor stack）列在 `inventory_information`。本模块将其修正为 #56 `current_screen_information` 的可见 screen 内容：carried item 只在具体 screen session 中成立。若放入 inventory，Provider-wide `scopeDependencies` 和 `informationRevision` 会迫使所有普通自身背包 Read、分页 cursor 与 item ref 被无关 screen 开关失效。#55 不保留 `cursor_stack` 占位字段或 `inventory.cursor_item` ref。

### 2.4 #63 是 inventory pagination 的合并前置

当前公共 `InformationCursorStore` 无条件复制签发时非空的 world、dimension、screen instance/revision，没有按 Provider 的 `scopeDependencies` 选择绑定项，也没有表达 `uiRevision`。因此即使 `inventory_information` 正确声明只依赖 `connection, world, dimension`，现实现签发的分页 cursor 仍会被无关 screen open/close 误伤。

#55 不在 Provider 中手工构造 cursor、清除 screen 字段或复制 Runtime 校验。[#63](https://github.com/spojchil/mineintent/issues/63) 必须先于 inventory pagination 实现合并；完成后 Runtime 将密封 Provider definition 的 scope dependencies 传给 CursorStore：

- process session 始终绑定；
- inventory cursor 绑定 connection（含 epoch/state）、world 和 dimension；
- inventory cursor 不绑定 `uiRevision`、`screenInstanceId` 或 `screenRevision`；
- fields、limit、selector、information revision、principal/grant/audience、TTL 和一次性消费规则保持不变；
- item selector ref 继续使用 RefStore 现有 scope/bindToScreen 规则，#63 不改变或放宽 item ref。

## 3. 单一 source kind 与公共读取规则

`ProviderReadResult` 一次 Read 只有一个 `source`。因此每个接口固定一个规范 source kind，接口内所有字段均声明该 kind，禁止在实现时按字段切换来源：

| 接口 | 唯一 `source.kind` | `acquisition` | 投影含义 |
|---|---|---|---|
| `current_status` | `hud_projection` | `immediate_client_state` | 已量化的自身 HUD/身体显示投影 |
| `hotbar_information` | `hud_projection` | `immediate_client_state` | 已量化的快捷栏显示投影 |
| `inventory_information` | `client_state` | `structured_ui_equivalent` | 仅玩家自身、已同步且可检查的 inventory 投影 |
| `item_tooltip_information` | `screen_projection` | `structured_ui_equivalent` 或 `current_screen` | 版本化 tooltip 渲染投影 |
| `f3_information` | `debug_projection` | `structured_ui_equivalent` | 遵守 reduced debug 的 F3 显示投影 |
| `crosshair_information` | `viewport_projection` | `current_perception` | 当前相机/准星认知投影 |
| `client_diagnostics` | `operator_diagnostic` | `operator_only` | 与角色认知物理隔离的诊断投影 |

这保证任意合法多字段请求均有共同 source kind。Provider 不得把 `current_status.health` 标成 `hud_projection`、同时把 `pose` 标成 `client_state`；二者都由 `SelfHudProjection` 先转换后以 `hud_projection` 发布。

#67 的 acquisition 与 `uiRevision` 是 UI Context 内部证据，不是 #55 七个 Provider 的 revision 来源。各玩家状态 projection 订阅 `UiContextProjectionSnapshot` 后，只把会改变本接口公开 value/availability 的事实归约进自己的公开 snapshot：

- acquisition-only 更新可以让 #67 提升 `uiRevision` 并通知 #55，但若本接口公开值/可用性不变，#55 的 `informationRevision`、公开 `sourceRevision` 和 `observedAt` 全部保持；
- `debugVisible` 的 value/acquisition 改变不改变 F3 结构化 Read 的合法性，因此自身不能推动 F3 的三个公开元数据；
- main surface 改为非 world 若使 F3/crosshair 字段从 available 变为 `not_currently_displayed`，这是本 Provider 的公开 availability 变化，才原子提升其公开元数据；
- world/dimension/screen scope 自身由 Runtime envelope 和 ref/cursor scope 约束；若本 Provider value/availability 未变，不能为了“刷新一下”而额外提升 revision。

注册定义总表：

| 接口 | `schemaRevision` | audiences | scope dependencies | overall available 条件 |
|---|---|---|---|---|
| `current_status` | `current-status:1` | companion, controller | connection, world, dimension | play 且 SelfHudProjection ready |
| `hotbar_information` | `hotbar-information:1` | companion, controller | connection, world, dimension | play 且 hotbar projection ready |
| `inventory_information` | `inventory-information:1` | companion, controller | connection, world, dimension | play 且自身 inventory 已同步 |
| `item_tooltip_information` | `item-tooltip:1` | companion, controller | connection, world, dimension | tooltip renderer 可用；具体值仍要求 selector |
| `f3_information` | `f3-information:1` | companion, controller | connection, world, dimension, ui | play；screen 状态下可能 partial |
| `crosshair_information` | `crosshair-information:1` | companion, controller | connection, world, dimension, ui | world main surface 且当前相机样本有效 |
| `client_diagnostics` | `client-diagnostics:1` | operator | connection | 进程诊断投影 ready；连接字段可 partial |

所有接口还遵守：

- `schemaRevision` 只随字段契约变化，首版固定为上表值；
- `informationRevision` 与对模型可见的值或 availability 一起变化；
- `sourceRevision` 也是过滤后投影 revision，不是 raw packet、Bot 或 renderer revision，隐藏 canary 变化不得推动它；
- Provider 只返回请求字段；嵌套对象使用 strict Zod schema，未知属性由 Runtime 清除或拒绝；
- `observedAt` 是产生当前公开 value/availability 的时刻，不是最新内部输入、UI acquisition 或 Help/Read 的包装时刻；
- 无需证据对象的自身状态返回空 `evidenceIds`；crosshair 可返回当前 perception evidence ID，但不能把 raw ID 编进 evidence。

## 4. 值类型与显示量化

以下共享 DTO 是公开值，不是 raw 客户端对象：

```ts
interface DisplayText {
  plain: string                 // 最多 256 Unicode code points；JSON.stringify 后的 UTF-8 最多 768 bytes
  style?: 'common' | 'uncommon' | 'rare' | 'epic' | 'warning' | 'positive' | 'muted'
}

type RegistryNameV1 = string    // namespaced ID；最多 128 code points / JSON string 256 bytes
type DisplayTokenV1 = string    // 已显示的短 token；最多 128 code points / JSON string 384 bytes
type FixedDecimalTextV1 = string // /^-?\d{1,10}(\.\d{1,3})?$/；最多 32 code points / JSON string 48 bytes
type DiagnosticTextV1 = string  // operator-only 净化文本；最多 256 code points / JSON string 768 bytes
type IsoTimestampV1 = string    // RFC 3339 UTC；固定 24 ASCII bytes

interface CappedListV1<T> {
  entries: readonly T[]
  truncated: boolean           // adapter 观察到更多合法公开项时为 true
}

interface ItemSummaryView {
  kind: RegistryNameV1          // 版本化注册名；只表示客户端可识别的物品种类
  displayName: DisplayText
  count: number                // 1..max visible stack count
  durabilityBar?: {
    remainingSteps: number     // 0..13，来自渲染条，不返回原始 damage/maxDamage
    totalSteps: 13
  }
  cooldownOverlay?: {
    remainingSteps: number     // 0..16，来自可见 overlay
    totalSteps: 16
  }
  selector?: InformationSelectorRef
}

type InventorySection =
  | 'hotbar'
  | 'main_storage'
  | 'armor'
  | 'offhand'
  | 'crafting_input'
  | 'crafting_output'

interface InventorySlotView {
  section: InventorySection
  index: number                // section-local、稳定且非协议 slot ID
  empty: boolean
  item?: ItemSummaryView
}
```

`RegistryNameV1/DisplayTokenV1/FixedDecimalTextV1/DiagnosticTextV1/DisplayText.plain/IsoTimestampV1` 各有独立 strict Zod schema；字符串先做 Unicode scalar 校验，禁止孤立 surrogate 和控制字符，再同时检查 code point 与 `Buffer.byteLength(JSON.stringify(value), 'utf8')`。后者把引号、反斜杠和四字节字符的 JSON 编码成本纳入预算。字符串 literal union、schema 字段 ID 和 Runtime 已定义的 opaque ref/cursor 不属于自由文本；除此之外，没有引用上述 bounded type 的裸 `string` 不得进入 projection DTO。`kind` 是版本化物品语义，不包含 components/NBT。显示文本先解析客户端最终可见翻译，再限制长度与样式；不得透传 chat component、hover event、click event 或 translation 参数对象。

量化逻辑由锁定版本 `1.21.1` 的纯函数实现，并建立 golden fixture。若 Mineflayer 数值不足以重现客户端显示，字段返回 `not_supported`，不能退回 raw 数值或猜测默认设置。

## 5. 接口定义

表中的 `requires` 是 Help 可见的稳定说明 token，不是新的公共权限枚举。

### 5.1 `current_status`

定义：audiences `companion, controller`；scope `connection, world, dimension`；无 selector/分页；`maxFieldsPerRead=12`、`maxResultBytes=32 KiB`、`timeoutMs=25`。

所有字段的 `sourceKinds` 均为 `['hud_projection']`。

| 字段 | 值类型与限制 | 精度/单位 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `health` | `{current,max,absorption}`，非负整数 | `displayed`，`health_point`（一格半心） | `connection:play`, `self_hud:ready` | 断线 `not_connected`；HUD 不支持 `not_supported` |
| `armor_display` | `0..20` 整数 | `exactly_displayed`，`armor_point` | 同上 | 同上 |
| `food_display` | `0..20` 整数 | `exactly_displayed`，`food_point` | 生存式 HUD | 非生存模式 `unsupported_game_mode` |
| `air_display` | `{filledBubbles,totalBubbles:10}` | `exactly_displayed`，`bubble` | 空气条当前显示 | 未显示 `not_currently_displayed` |
| `experience` | `{level,bar:{filledSteps,totalSteps:182}}` | `exactly_displayed`，`level/bar_step` | 经验 HUD | 模式不支持 `unsupported_game_mode` |
| `attack_cooldown_display` | `{state:'charging'|'ready',filledSteps,totalSteps:16}` | `displayed`，`overlay_step` | 客户端 attack indicator 当前可显示 | 设置关闭或当前未画出 `not_currently_displayed` |
| `status_effects_display` | `CappedListV1<{kind:RegistryNameV1,displayName:DisplayText,amplifierDisplay?:DisplayText,durationText?:DisplayText,ambientStyle}>`，最多 32 项 | `displayed` | effect icon/panel 可见 | 无效果返回 `{entries:[],truncated:false}`；来源不支持 `not_supported` |
| `pose` | `'standing'|'crouching'|'swimming'|'fall_flying'|'sleeping'|'spin_attack'|'dying'|'unknown'` | `displayed` | 自身姿态可感知 | 无实体 `not_currently_displayed` |
| `on_ground` | boolean | `inferred` | 本地身体接触投影有效 | 无实体 `not_currently_displayed` |
| `mount_state` | `{mounted:boolean,mountKind?:RegistryNameV1}`；无实体/协议 ID | `displayed` | 自身骑乘状态 | 未骑乘仍返回 `{mounted:false}` |
| `fire_state` | `'clear'|'burning'` | `displayed` | 第一人称覆盖层/自身状态 | 不着火返回 `clear` |
| `freeze_state` | `{state:'clear'|'freezing',overlaySteps?:number,totalSteps?:10}` | `displayed` | 冻结覆盖层 | 不冻结返回 `clear` |

健康值只能来自渲染为心格后的整数。`foodSaturation` 不进入 source port；效果只保留实际显示的 icon/文本，`showIcon=false` 的效果和精确 `durationTicks` 均不进入投影。持续时间可返回客户端已格式化的 `durationText`，同一显示文本内的 tick 变化不提升 revision。

### 5.2 `hotbar_information`

定义：audiences `companion, controller`；scope `connection, world, dimension`；无分页；`maxFieldsPerRead=4`、`maxResultBytes=32 KiB`、`timeoutMs=25`。

所有字段的 `sourceKinds` 均为 `['hud_projection']`。

| 字段 | 值类型与限制 | 精度 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `selected_slot` | `0..8` 整数 | `exactly_displayed` | `connection:play`, `hotbar:visible` | 断线 `not_connected`；隐藏 `not_currently_displayed` |
| `slots` | 固定 9 个 `{index,selected,empty,item?}`；item 为 `ItemSummaryView` | `displayed` | 同上 | 同上 |
| `main_hand` | `{slotIndex,item?}` | `displayed` | 自身手部状态有效 | 无实体 `not_currently_displayed` |
| `off_hand` | `{empty,item?}` | `displayed` | 自身 offhand 同步 | 不支持 `not_supported` |

每个非空 item 可签发 `hotbar.item` ref；一次 Read 最多 10 个 ref，不会触发 Runtime 每读签发上限。槽位为空仍是合法公开事实，不通过 unavailable 表示。

### 5.3 `inventory_information`

定义：audiences `companion, controller`；scope `connection, world, dimension`；分页 `defaultLimit=16,maxLimit=24`；`maxFieldsPerRead=3`、`maxResultBytes=48 KiB`、`timeoutMs=35`。自身 inventory 不依赖 `ui` 或 `screen`，打开/关闭任意 screen 或 overlay 不应使普通 Read、分页 cursor 或 item ref 失效。分页代码与 composition 必须等待 #63，definition/projection/非分页 Provider 工作可先进行。

所有字段的 `sourceKinds` 均为 `['client_state']`。

| 字段 | 值类型与限制 | 精度 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `selected_hotbar_slot` | `0..8` | `exactly_displayed` | `self_inventory:synchronized` | 断线 `not_connected` |
| `slots` | `{entries:InventorySlotView[],pageStart,totalSlots}`；固定 section 顺序分页 | `displayed` | `self_inventory:synchronized` | 未同步 `not_currently_displayed` |
| `layout` | 各 section 的公开槽位数，不含协议 slot ID | `exactly_displayed` | `self_inventory:synchronized` | 版本不支持 `not_supported` |

固定页序为 hotbar → main storage → armor → offhand → crafting input → crafting output。每页最多 24 个 slot/ref；#63 完成后 cursor 绑定字段集合、limit、inventory information revision、principal/grant/audience、connection/world/dimension，不绑定 ui/screen。翻页期间任一公开槽位或相关 scope 变化都会令旧 cursor `invalid_page`；单纯打开/关闭 screen、聊天、F3 或 Tab 后仍可续同一页链，不拼接来自两个 revision 的“完整背包”。

`slots` 只含自身 inventory。即使 raw window slots 同时包含箱子/交易/马匹槽位，本投影也必须在写入前按受信任的玩家 inventory layout 白名单切除。鼠标携带物不是自身 inventory section，不得混入 `slots`；它由 #56 当前 screen 投影返回并签发 screen-bound ref。

### 5.4 `item_tooltip_information`

定义：audiences `companion, controller`；scope `connection, world, dimension`；selector 必需，接受 `inventory.item`,`hotbar.item`,`screen.item`；无分页；`maxFieldsPerRead=8`、`maxResultBytes=32 KiB`、`timeoutMs=40`。Provider 不声明 `screen` scope：inventory/hotbar ref 与 screen 无关，不能因无关 screen 开关得到 `scope_changed`；`screen.item` 的竞态由 ref binding 和签发源 revision 复核处理。

所有字段的 `sourceKinds` 均为 `['screen_projection']`。

| 字段 | 值类型与限制 | 精度 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `display_name` | `DisplayText` | `exactly_displayed` | `selector:item`, item 当前可检查 | stale 为请求级 `invalid_selector` |
| `lore_lines` | `CappedListV1<DisplayText>`，最多 64 项 | `exactly_displayed` | tooltip 对应行可见 | 无 lore 返回空 entries |
| `enchantments_display` | `CappedListV1<{displayName:DisplayText,levelText?:DisplayText}>`，最多 64 项 | `exactly_displayed` | tooltip 对应行可见 | 无附魔返回空 entries |
| `attributes_display` | `CappedListV1<{label:DisplayText,valueText:DisplayText,style?}>`，最多 64 项 | `exactly_displayed` | tooltip 对应 section 可见 | 无属性返回空 entries |
| `restrictions_display` | `{canPlaceOn?:CappedListV1<DisplayText>,canBreak?:CappedListV1<DisplayText>}` | `exactly_displayed` | Adventure tooltip 行可见 | 未显示返回空对象 |
| `durability_display` | `{remaining,max}` 正整数 | `exactly_displayed`，`durability_point` | advanced tooltip 已启用且该行显示 | 关闭时 `not_currently_displayed` |
| `registry_id_display` | `RegistryNameV1` | `exactly_displayed` | advanced tooltip 已启用 | 关闭时 `not_currently_displayed` |
| `advanced_summary` | `{componentCount?:number}`；仅列 1.21.1 实际显示的摘要 | `exactly_displayed` | advanced tooltip 已启用且客户端显示 | 关闭 `not_currently_displayed`；无法重现 `not_supported` |

tooltip projection 只保存最终可显示的 allowlist DTO。不得返回 `nbt`、data components map、custom data、服务器 hover/click 事件、隐藏属性 UUID、内部 repair cost 或未渲染 tooltip 行。Provider 不接受槽位号、物品名或坐标作为 selector。

### 5.5 `f3_information`

定义：audiences `companion, controller`；scope `connection, world, dimension, ui`；无 selector/分页；`maxFieldsPerRead=10`、`maxResultBytes=32 KiB`、`timeoutMs=35`。

所有字段的 `sourceKinds` 均为 `['debug_projection']`。

| 字段 | 值类型与限制 | 精度 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `debug_mode` | `{reducedDebugInfo:boolean,visibilityProfile:'full'|'reduced'}` | `exactly_displayed` | `connection:play` | 断线 `not_connected` |
| `position_display` | `{x:FixedDecimalTextV1,y:FixedDecimalTextV1,z:FixedDecimalTextV1}`，各为三位小数的十进制定点字符串 | `exactly_displayed`，`block` | `ui:main_surface.world`, `debug:coordinates` | reduced 时 `blocked_by_reduced_debug` |
| `block_position` | `{x,y,z}` 整数 | `exactly_displayed`，`block` | 同上 | 同上 |
| `chunk_position` | `{chunkX,sectionY,chunkZ,localX,localY,localZ}` 整数 | `exactly_displayed` | `debug:chunk_position` | reduced 时按版本策略阻止 |
| `facing_display` | `{direction:'north'|'south'|'west'|'east',axis:'x'|'z',description:DisplayText}` | `exactly_displayed` | `debug:facing` | 按版本策略阻止 |
| `rotation_display` | `{yaw:FixedDecimalTextV1,pitch:FixedDecimalTextV1}` 一位小数字符串 | `exactly_displayed`，`degree` | `debug:rotation` | 按版本策略阻止 |
| `dimension_display` | `RegistryNameV1` | `exactly_displayed` | `debug:dimension` | 客户端未显示 `not_currently_displayed` |
| `biome_display` | `RegistryNameV1` | `exactly_displayed` | `debug:biome` | reduced 时按版本策略阻止 |
| `light_display` | `{client,sky,block}`，各 `0..15` | `exactly_displayed`，`light_level` | `debug:light` | 未显示/未取样 `not_currently_displayed` |
| `height_display` | `CappedListV1<{label:DisplayTokenV1,value:DisplayTokenV1}>`，最多 8 项 | `exactly_displayed`，`block_y` | `debug:heightmap` | reduced/未加载分别 `blocked_by_reduced_debug`/`not_currently_displayed` |
| `local_difficulty_display` | 仅解析实际显示 token `{difficultyText:DisplayTokenV1,clampedText?:DisplayTokenV1,day?:DisplayTokenV1}` | `exactly_displayed` | `debug:local_difficulty` | reduced/服务端不提供时分别阻止/不可用 |
| `targeted_block_debug` | `{kind:RegistryNameV1,properties:CappedListV1<{name:DisplayTokenV1,value:DisplayTokenV1}>,observationRef?}`；最多 32 properties | `exactly_displayed` | `debug:targeted_block`, current crosshair sample | miss `not_currently_displayed`；reduced 按策略阻止 |
| `targeted_fluid_debug` | 同类白名单 DTO | `exactly_displayed` | `debug:targeted_fluid` | 同上 |
| `targeted_entity_debug` | `{kind:RegistryNameV1,displayName?:DisplayText,observationRef?}`；无协议 ID/UUID/精确距离 | `exactly_displayed` | `debug:targeted_entity`, current observation | 同上 |

`reducedDebugInfo` 本身是 `debug_mode` 的合法公开值。`F3VisibilityPolicy<'1.21.1'>` 为每个字段逐项决定 full/reduced 可见性；不能用“看起来敏感”的通用条件猜测，也不需要扩充公共 availability 枚举。被策略隐藏的字段统一 `blocked_by_reduced_debug`。被隐藏坐标或目标发生变化时，`informationRevision`、`sourceRevision` 和 trace 均不得因此变化。

F3 overlay 实际开关只进入 #67 `context.overlays.debugVisible`。#55 仅在该 state `availability === 'available'` 后读取 `.value`，不能从 unavailable 分支补 `false`，也不能用其 value/acquisition 或 `baseFieldAcquisition` 作为 Read 前置条件。F3 Read 的 acquisition 仍由自身接口固定为 `structured_ui_equivalent` 并记录获取事件；若 main surface 不是 world，则依赖世界表面的字段返回 `not_currently_displayed`，`debug_mode` 仍可读，Read 不自动关闭 screen。

### 5.6 `crosshair_information`

定义：audiences `companion, controller`；scope `connection, world, dimension, ui`；无 selector/分页；`maxFieldsPerRead=7`、`maxResultBytes=12 KiB`、`timeoutMs=25`。

所有字段的 `sourceKinds` 均为 `['viewport_projection']`。

| 字段 | 值类型与限制 | 精度 | `requires` | 当前 unavailable |
|---|---|---|---|---|
| `reticle_display` | `{visible:boolean,mode:'normal'|'attack_indicator'|'hidden'}` | `displayed` | current camera sample | 非世界表面 `not_currently_displayed` |
| `hit_kind` | `'miss'|'block'|'entity'|'fluid'|'unknown'` | `inferred` | current perception sample | 无样本 `not_currently_displayed` |
| `hit_summary` | `{kind:RegistryNameV1,displayName?:DisplayText,distanceBand:'touching'|'near'|'reachable'|'far'|'unknown'}` | `quantized` | current hit | miss `not_currently_displayed` |
| `block_face` | `'down'|'up'|'north'|'south'|'west'|'east'|'unknown'` | `inferred` | current block hit | 非方块命中 `not_currently_displayed` |
| `interaction_reach` | `'within_reach'|'outside_reach'|'unknown'` | `inferred` | versioned reach policy | 缺少设置 `not_supported` |
| `breaking_progress_display` | `{stage:0..9,totalStages:10}` | `exactly_displayed` | 当前 crack overlay 可见 | 未挖掘 `not_currently_displayed` |
| `observation_ref` | `InformationSelectorRef` | `quantized` | #34 当前合法 observation | 无合法观察 `not_currently_displayed` |

crosshair 不返回目标坐标、entity ID、UUID、精确距离或准星外候选。`distanceBand` 由当前相机投影量化；`observation_ref` payload 只含有界 JSON `{observationKey,sampleRevision}`，允许接口由 #34 定义，不能保存 raw entity/block。

### 5.7 `client_diagnostics`

定义：audiences **仅** `operator`；scope `connection`；无 selector/分页；`maxFieldsPerRead=10`、`maxResultBytes=24 KiB`、`timeoutMs=35`。

所有字段的 `sourceKinds` 均为 `['operator_diagnostic']`，acquisition 固定 `operator_only`。

| 字段 | 值类型与限制 | 精度 | 当前 unavailable |
|---|---|---|---|
| `minecraft_version` | `{target:DisplayTokenV1,negotiated?:DisplayTokenV1}` | `exactly_displayed` | 未连接时 negotiated 为字段部分缺省，不猜测 |
| `backend_adapter` | `{name:DisplayTokenV1,revision:DisplayTokenV1,protocolVersion?:DisplayTokenV1}` | `exactly_displayed` | 协议未协商时仅省略 protocolVersion |
| `runtime_memory` | `{heapUsedBytes,heapLimitBytes?,rssBytes}` | `quantized` 到 1 MiB | 运行时无指标 `not_supported` |
| `runtime_cpu` | `{processPercent?,sampleWindowMs}` | `quantized` 到 0.1% | 无采样器 `not_supported` |
| `event_loop_delay` | `{p50Ms,p95Ms,maxMs,sampleWindowMs}` | `quantized` 到 0.1 ms | 无采样器 `not_supported` |
| `protocol_counters` | `{inboundPerSecond,outboundPerSecond,windowMs}` | `quantized` | driver 无计数器 `not_supported` |
| `render_fps` | number | `displayed` | Mineflayer/headless 固定 `not_supported` |
| `gpu_display` | `DisplayText` | `displayed` | Mineflayer/headless 固定 `not_supported` |
| `render_statistics` | `CappedListV1<{label:DiagnosticTextV1,value:DiagnosticTextV1}>`，最多 64 项 | `displayed` | Mineflayer/headless 固定 `not_supported` |

“headless 没有 FPS/GPU”是正常 availability，不得用 tick rate、协议包速率或主机 GPU 冒充。该 Provider 不出现在 companion/controller Catalog，不能进入 Context、Grounding、Memory 或普通 Journal payload；operator grant 也不能放入模型 Tool Session。

## 6. Source port → projection → Provider

### 6.1 端口

```ts
interface ProjectionSnapshot<Values, Field extends string> {
  available: boolean
  informationRevision: number
  publicSourceRevision: number
  publicObservedAt: IsoTimestampV1
  values?: Readonly<Values>
  fieldAvailability: Readonly<Partial<Record<Field, InformationUnavailableReason>>>
}

interface SelfHudSource { capture(): Readonly<ProjectionSnapshot<SelfHudProjectionV1, keyof SelfHudProjectionV1 & string>> }
interface HotbarSource { capture(): Readonly<ProjectionSnapshot<HotbarProjectionV1, keyof HotbarProjectionV1 & string>> }
interface SelfInventorySource { capture(): Readonly<ProjectionSnapshot<SelfInventoryProjectionV1, keyof SelfInventoryProjectionV1 & string>> }
interface TooltipSource {
  render(selector: ResolvedItemSelector): Readonly<TooltipProjectionResultV1>
}
interface DebugScreenSource { capture(): Readonly<ProjectionSnapshot<F3ProjectionV1, keyof F3ProjectionV1 & string>> }
interface CrosshairSource { capture(): Readonly<ProjectionSnapshot<CrosshairProjectionV1, keyof CrosshairProjectionV1 & string>> }
interface ClientDiagnosticsSource { capture(): Readonly<ProjectionSnapshot<ClientDiagnosticsProjectionV1, keyof ClientDiagnosticsProjectionV1 & string>> }
```

`ResolvedItemSelector` 只在 Runtime 成功解析 ref 后进入 Tooltip Provider。它是本模块内部有界 JSON DTO，不是 Mineflayer Item。source port 的返回值必须 `Readonly` 且可 `structuredClone`。

### 6.2 数据流

```text
Mineflayer/protocol events + versioned client settings
  → src/minecraft/driver/player-information-adapter.ts
  → 1.21.1 display quantizers / allowlists
  → projection stores（只保存合法 DTO）
  → source-ports/*.capture()
  → Provider（字段选择、selector、分页）
  → InformationRuntime（权限、schema、scope、限额、envelope）
```

driver adapter 可以接触 raw 对象，但只能调用 projection writer port。它不得把 Bot/Item/Entity 引用、NBT map 或 raw revision写入 store。Provider projection 不使用 `MinecraftSnapshotV1` adapter，也不保留新旧双写。

### 6.3 原子读取

每个 `capture()` 返回一份不可变单 revision 快照。Provider 先 capture 一次，再从同一对象选择全部请求字段，不为每字段重复 capture。inventory 分页 state 只保存下一个公开 ordinal；tooltip render 在一次同步调用内固定已解析 selector 和 renderer 设置 revision，不读取或复制 UI 状态机。超时/Abort 仍由 Provider 在转换大列表前后检查。

每个 projection reducer 可以维护私有 `lastAcceptedInputRevision` 以消费 Driver/#67 更新，但 Provider 只能看到上面的公开 snapshot。只有 `available`、`values` 或 `fieldAvailability` 深比较发生变化时，reducer 才原子替换公开 snapshot、提升 `informationRevision/publicSourceRevision` 并更新 `publicObservedAt`；acquisition-only、`uiRevision`、重复事件和被过滤的隐藏变化只推进私有游标。Provider result 将 `publicSourceRevision` 映射到 `source.sourceRevision`，将 `publicObservedAt` 映射到 `observedAt`，不得在 Read 时临时采用最新 UI 时间。依赖 `ui` scope 的 Read 可能因 acquisition-only `uiRevision` race 被 Runtime 保守要求重试，但这次内部重试本身仍不能改写 Provider 公开 metadata。

## 7. Revision、scope 与失效

| 投影 | `informationRevision` 提升 | 不得提升 |
|---|---|---|
| current status | 任一公开显示值/availability 变化 | saturation、同一显示格内的生命小数、同一 durationText 内 tick、scope/acquisition-only 变化 |
| hotbar | 公开槽位、选择、双手、显示 bar/overlay 改变 | item 隐藏 components、UI scope/acquisition-only 改变但公开摘要不变 |
| inventory | 任一公开自身槽位/layout 或 availability 改变 | screen/carried item、外部 window slot、raw slot ID、未显示 NBT、scope/acquisition-only 变化 |
| tooltip | tooltip 设置或任一公开渲染行改变 | 未渲染 component/NBT、无关 UI scope/acquisition-only 改变 |
| F3 | 任一当前允许字段或 availability 改变；reduced policy 切换 | reduced 隐藏字段的值变化、debug overlay 的 value/acquisition、其他 acquisition-only 变化 |
| crosshair | 当前样本、可见 crack/reticle 或 availability 改变 | 准星外实体/区块、UI acquisition-only 变化 |
| diagnostics | operator-visible metric bucket/availability 改变 | raw 高频采样在同一量化桶内变化、无关 UI/scope 变化 |

所有 companion 接口按各自 Provider `scopeDependencies` 绑定 scope；scope 变化负责丢弃读取竞态和失效 ref/cursor，不自动充当 Provider revision。只有相应 value/availability 确实改变时，projection 才提升自己的三个公开元数据。#63 完成后 inventory cursor 精确绑定 connection/world/dimension，不绑定 ui/screen；重连、新 world 或维度切换使其失效，无关 screen/overlay 变化不使其失效。item ref 仍按 RefStore 原规则绑定，不能从 cursor 修订推导出放宽。死亡不新建 epoch；若 status/inventory/crosshair 的公开值或 availability 随死亡改变，则各自发布新 revision，否则依靠既有 scope/ref 规则而不是伪造 revision。进入 death screen 时，其 screen 语义由 #67 UI Context 负责。

## 8. Item selector 生命周期

### 8.1 签发

```ts
context.refs.issue({
  kind: 'inventory.item',
  payload: {
    projectionItemKey,
    slot: { section, index },
    itemFingerprint,
  },
  allowedInterfaces: ['item_tooltip_information'],
  basedOnInformationRevision: snapshot.informationRevision,
  validUntil: addSeconds(context.now, 30),
  bindToScreen: false,
})
```

- payload 小于 512 bytes，不含 raw item、NBT、协议 window/slot ID。
- `hotbar.item` 同样不绑定 screen。
- `screen.item` 由 #56 签发，也必须 `bindToScreen:true`；没有同时存在的 `screenInstanceId + screenRevision` 时 Runtime 拒绝签发。
- item ref 只允许 tooltip 接口；不能用它查询 inventory、viewport 或行为接口。

### 8.2 使用与失效

Runtime 已在 tooltip Read 前后检查签发源 Provider 当前 `informationRevision`。因此任何公开 inventory/hotbar 变化都会保守地使该来源全部 item ref 失效；v0.2 不增加绕过此规则的“单槽位稳定 ref”。此外，TTL、grant 结束和 epoch/world/dimension 变化都会失效。

对于 `screen.item`，RefStore 在目标 Read 前按 ref 中绑定的 `screenInstanceId + screenRevision` 校验当前 screen；Runtime 又在目标 Read 前后复核签发源 `current_screen_information` 的 Provider-wide `informationRevision`。screen replacement/layout revision 触发 screen invalidation；槽位、文本、进度、title 或读取中的其他公开内容变化若推动 #56 签发源 revision，也会保守地使其全部 ref 失效，即使 #67 的 `screenRevision` 未变。目标结果统一作为请求级 `invalid_selector` 丢弃。因此 TooltipProvider 不需要 Provider-wide `screen` scope，也不会让 inventory/hotbar tooltip 被无关 screen race 误伤。

TooltipSource 仍校验 `projectionItemKey + itemFingerprint + slot` 命中当前投影，防止错误实现把过期 ref 解释为槽位中的新物品。失败作为 `stale_selector`，不回显期望/实际 item 数据。

## 9. reduced debug、设置与 unknown 状态

### 9.1 reduced debug

- driver 捕获服务端下发的 `reducedDebugInfo` 状态，送入版本化 F3 投影；
- visibility policy 对每个 F3 字段作 allow/block，blocked 字段不进入值 DTO；
- Provider 对请求的 blocked 字段返回 `blocked_by_reduced_debug`；
- Help `availability=current` 给出相同原因；
- 不增加新的公共枚举，也不把 blocked 映射为 `permission_required`；
- 切换 reduced 模式提升 F3 information/source revision，并使旧 page/ref（若未来增加）失效。

### 9.2 客户端设置未知

attack indicator、advanced tooltip、GUI scale 等设置必须来自明确 source。未知时相关字段返回 `not_supported`，不能假定 vanilla 默认值。设置变化只影响实际依赖字段的投影与 revision。

### 9.3 unknown screen

unknown screen 仍有受信任的 instance/revision。自身 status/hotbar/inventory 继续按其自身条件读取；F3 中依赖世界表面的字段和 crosshair 因 main surface 不是 world 返回 `not_currently_displayed`；仅持有有效 screen-bound item ref 的 tooltip 可以读取，并自动绑定该 unknown screen。#55 不解释 unknown screen 的控件或槽位。

## 10. 大小、分页和复杂度预算

| 接口 | 最大结果 | 列表硬上限 | ref 上限 | 算法约束 |
|---|---:|---:|---:|---|
| current status | 32 KiB | effects 32；公开值内容预算 24 KiB | 0 | O(请求字段 + effects) |
| hotbar | 32 KiB | slots 9；公开值内容预算 24 KiB | 10 | O(9) |
| inventory | 48 KiB | 每页 slots 24；公开值内容预算 40 KiB | 每页 24 | O(page limit)，不得扫描外部容器 |
| tooltip | 32 KiB | lines/entries 各 64；完整 projection 的固定 JSON string 预算 12 KiB | 0 | O(可见 tooltip 行) |
| F3 | 32 KiB | target properties 32，height rows 8；公开值内容预算 24 KiB | 最多 3 observation refs | O(公开 F3 行) |
| crosshair | 12 KiB | 单一当前命中；公开值内容预算 4 KiB | 1 | O(1)，不得搜索世界 |
| diagnostics | 24 KiB | render stats 64；公开值内容预算 16 KiB | 0 | O(采样指标数) |

非分页列表统一使用 `CappedListV1`；达到项目数或内容字节预算且仍有合法公开项时必须写 `truncated:true`，没有溢出时显式为 false。tooltip 的 12 KiB 是 projection commit 时按固定字段顺序计算的全局预算，与本次 Read 请求了哪些 sibling fields 无关：`display_name` 1 KiB、`lore_lines` 4 KiB、`enchantments_display` 2 KiB、`attributes_display` 2 KiB、`restrictions_display.canPlaceOn/canBreak` 各 1 KiB、其余文本合计 1 KiB；两个 restrictions 列表各最多 64 项。相同 projection revision 下同一字段的 entries/truncated 因而不会随请求字段组合变化。

inventory 等分页列表不使用 truncated 冒充分页：page builder 按项目数与 40 KiB 公开值内容预算中先到者结束本页，并通过 `nextPageState` 让 Runtime 签发 cursor。任何单项若在逐字符串 schema 校验后仍无法装入空页，属于 schema/预算配置错误并由测试阻止发布，不能在生产中退化成请求级 `provider_failed`。

每个 Provider 的 fixture builder 必须计算完整 `InformationReadResult` envelope 的 `JSON.stringify` UTF-8 字节数；上表的公开值内容预算为字段 DTO、ref 与 next-page state 预留空间，`maxResultBytes` 还为 protocol/read/source/unavailable/evidence/cursor envelope 预留至少 8 KiB。四字节字符、引号/反斜杠/控制字符、最大数量 selector ref、最大字段组合和 N/N+1 列表都必须进入边界测试。合法 DTO 不得依赖 Runtime 最终 `provider_failed` 作为普通截断机制。

## 11. 模块文件布局

```text
src/information/
├── source-ports/
│   ├── self-hud.ts
│   ├── hotbar.ts
│   ├── inventory.ts
│   ├── item-tooltip.ts
│   ├── debug-screen.ts
│   ├── crosshair.ts
│   └── diagnostics.ts
├── projections/player-state/
│   ├── projection-store.ts
│   ├── display-quantization-1.21.1.ts
│   ├── f3-visibility-1.21.1.ts
│   ├── tooltip-allowlist-1.21.1.ts
│   └── values.ts
├── providers/
│   ├── current-status.ts
│   ├── hotbar.ts
│   ├── inventory.ts
│   ├── item-tooltip.ts
│   ├── f3.ts
│   ├── crosshair.ts
│   └── client-diagnostics.ts
└── testing/
    ├── player-state-fixtures.ts
    └── player-state-leak-canaries.ts

src/minecraft/driver/
└── player-information-adapter.ts
```

每个 Provider 文件同时导出 values interface、strict Zod schemas、definition 和 Provider；若单文件过大再拆 `providers/<name>/`，不先建立通用“字段 DSL”。composition 只在 App root 将 source 注入 Provider 并注册到 Registry。

## 12. 测试与验收

### 12.1 公共 Provider 契约

七个 Provider 均运行 `information/testing/provider-contract.ts`：

- Catalog/Help 的 schema revision、audience、field ID、type、precision、sourceKinds、requires 一致；
- 任意合法字段组合只返回请求字段，且所有返回字段接受同一个实际 source kind；
- extra nested property、非有限数字、过长文本、非法枚举和错误 source kind 被 Runtime 拒绝；
- disconnected、wrong scope、Abort/deadline 和 Provider throw 转为规定结果；
- companion Catalog 永远没有 `client_diagnostics`。

### 12.2 纯单元与隐藏 canary

至少覆盖：

下列每个“公开结果不变”用例都同时断言 values/unavailable、`informationRevision`、`source.sourceRevision`、`observedAt` 和 trace 不变，不能只比较值对象。

1. health/experience/durability/cooldown 的 1.21.1 量化边界和 golden fixture；
2. saturation 改变、同一显示心格内生命小数改变，不改变输出、information/source revision、observedAt 或 trace；
3. effect tick 在同一 `durationText` 桶内改变不泄漏，隐藏 icon 效果完全缺席；
4. item raw NBT/custom component/协议 slot ID 改变但可见摘要/tooltip 不变时，输出和 revision 不变；
5. 外部容器 slot 中植入唯一 canary 字符串，inventory/tooltip 无法检索到；
6. reduced debug 下用唯一坐标、biome、target canary 反复变化，blocked 字段、revision、sourceRevision、错误和 trace 都不泄漏；
7. 准星外实体/方块使用唯一 canary，crosshair 结果/ref/evidence 不出现；
8. screen item ref 在 replacement/screen revision 或 #56 Provider-wide 内容/title revision 变化后失效；自身 inventory/hotbar ref 及其 tooltip Read 不因 screen open/close 或 F3/chat overlay toggle 失效；
9. inventory cursor 不能换 fields、limit、selector 或跨 information/connection/world/dimension revision 续页；#63 后在 screen/F3/chat/Tab 变化但 inventory revision 不变时必须可以续页；
10. operator 机器名/GPU canary 不进入 companion Catalog、Context、Journal 或模型 trace。
11. 在所有 #55 公开 value/availability 保持不变时，仅切换 #67 的 `baseFieldAcquisition`、`debugVisible.acquisition` 或其他 overlay acquisition，七个 Provider 的 information/source revision、observedAt 和 trace 均保持；需要 `ui` scope 的并发 Read 可以返回 `scope_changed`，重试后的公开 metadata 仍不变。

### 12.3 Paper 集成

在每次复制的干净世界中用服务端命令只负责布景/裁判，生产 Provider 不读取命令结果：

- 造成可控伤害、饥饿、吸收、效果、骑乘、着火/冻结，核对公开显示和隐藏值；
- 给定九格/背包/盔甲/副手物品，切换选中格、消耗耐久、触发 cooldown，核对 revision 与分页；
- 替换同槽物品后旧 item ref 必定失败；
- `/gamerule reducedDebugInfo` 切换前后核对逐字段 availability；
- 传送/维度切换、死亡、重生和重连后旧 read/ref/cursor 不得冒充当前值；
- 将准星对准固定方块/实体和 miss 场景，核对只发布当前命中且不出现服务端裁判坐标。

Paper oracle 只在 test process 做断言，不注入 source port、evidence 或生产 trace。

### 12.4 真人对照验收

同一账号状态或严格同步布景下，由观察者对照原版 Java 1.21.1 客户端：

- HUD 心、甲、饥饿、空气、经验、效果 icon 与 Read 一致；
- 九格、背包空槽/数量、耐久条和 tooltip 标准/advanced 模式一致；
- full/reduced F3 逐字段截图与 Help availability 一致；
- world、inventory screen、unknown screen、聊天输入、F3/Tab overlay 组合下 availability 正确；
- 准星移入/移出目标、开始/停止挖掘时，crosshair revision 和 crack stage 符合画面；
- operator diagnostics 只在本地受权诊断入口可见。

截图仅为验收证据，不进入运行时投影或模型上下文。

## 13. 实施切片与并行边界

### S1：不等待 #54 的首条纵向链

1. 建立共享 DTO、strict Zod schema、1.21.1 量化函数与 source ports。
2. 实现 `current_status` definition/projection/provider/契约测试；接入真实事件流但不调用 snapshot。
3. 实现 hotbar 与 self inventory 的 `selected_hotbar_slot`,`slots`,`layout`；不建立 screen/carried item 占位字段。可先实现第一页结果与 page-state 纯函数，但 CursorStore 接入和分页 PR 合并等待 #63。
4. 实现 operator-only diagnostics 与 Catalog 隔离测试。

完成标准：Backend ready → Catalog/Help → Read health/food/effects → 断线 `not_connected`，hidden canary 全绿。

### S2：接入 #54

1. 直接消费 #67 `UiContextProjectionSnapshot`，按 `UiFieldState.availability` 窄化后读取值，不复制状态机或旧 availability map。
2. 实现 F3 world-surface eligibility、固定 structured acquisition 与 unknown screen 行为；测试 UI acquisition-only/debug overlay 更新不推动 F3 公开 metadata。
3. 实现 crosshair world-surface eligibility；screen/overlay toggle 不使 inventory Read、分页 cursor 或 item ref 失效。

### S3：tooltip 与 reduced debug

1. 实现 `inventory.item`/`hotbar.item` 签发和 TooltipProvider。
2. 固化 standard/advanced tooltip allowlist 与设置 availability。
3. 固化 1.21.1 F3 full/reduced 字段差分，完成 canary/Paper 测试。

### S4：与 #34/#56 集成及验收

1. 消费 #34 当前 perception observation，签发 crosshair observation ref。
2. 接受 #56 `screen.item`，验证 instance/revision race。
3. 完成 Paper、真人对照和 #57 非泄漏矩阵；由 #58 做最终 composition/旧链路删除。

各切片只修改本模块 source port/projection/provider/测试。发现公共 Runtime 缺陷时先提交独立设计问题，不在 #55 分支顺手扩充公共 contract。

## 14. 开放问题与实施前核对

以下问题不阻止 S1，但必须在对应切片前以 fixture/代码评审关闭。另有一项已明确的共享阻塞：#63 必须先于 inventory pagination 合并，不属于可在 #55 内自行裁决的开放问题。

1. Java 1.21.1 的心格舍入、经验条、cooldown overlay 与 crack stage 精确公式需从锁定版本渲染行为生成 golden，不凭 Mineflayer raw 数字猜测。
2. `F3VisibilityPolicy<'1.21.1'>` 的逐字段 full/reduced 差分需用原版客户端和协议设置实测；结论只改模块策略，不扩充公共 availability 枚举。
3. advanced tooltip 的开关来源尚未在 driver 契约中存在；来源未接通前相关字段保持 `not_supported`，不能假定开启或关闭。
4. #34 需确认 observation ref 接受 kind 与 evidence ID 格式；在此之前 crosshair 其他字段可实现，`observation_ref` 返回 `not_supported`。
5. #56 需确认 `screen.item` payload DTO；无论具体字段如何，都必须满足 Runtime JSON/字节限制和 `bindToScreen:true`。
6. headless Mineflayer 不具备真实 renderer。任何无法由协议状态加版本化显示规则可靠重现的 F3、tooltip 或 HUD 字段必须保持 `not_supported`，不能降级为 raw backend 真值。

## 15. 完成定义

1. 七个接口均可 Catalog → Help → Read，字段矩阵、版本、单位、精度、requires 和 availability 与本文一致。
2. 任意合法多字段 Read 都只使用该接口固定 source kind；无 Provider 递归或多来源拼接。
3. current status、hotbar、自身 inventory 与 tooltip 和原版可检查内容一致，隐藏 saturation/tick/NBT/外部容器 canary 不泄漏。
4. F3 严格遵守 1.21.1 reduced debug，crosshair 只表达当前第一人称样本。
5. item ref 按 RefStore 原规则失效；#63 后 inventory cursor 绑定 connection/world/dimension、revision 与 grant，不绑定 ui/screen，并有正反两类续页测试。
6. `client_diagnostics` 仅 operator 可见，headless 缺失项诚实 `not_supported`。
7. 生产实现不导入 `MinecraftSnapshotV1`、raw Mineflayer、Paper oracle 或任务/技能目录；#57 的架构和非泄漏测试通过。
