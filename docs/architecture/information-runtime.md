---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# Information Runtime 模块设计

> 由 [PR #62](https://github.com/spojchil/maineintent/pull/62) 合并并关闭 Issue [#53](https://github.com/spojchil/maineintent/issues/53)。Registry、Access Policy、Read、Ref/Cursor、预算和 trace 主体已实现；完整下游 Provider、Issue #63 修复和 model-facing tool loop 均未完成。上游：[合法信息与 UI](./information-access-and-ui.md)。

## 1. 决定

MineIntent 使用一个统一 `InformationRuntime` 作为所有认知信息的唯一入口：

- Runtime 向受信任的内部消费者提供 Catalog、Help 和 Read；当前不将它们注册为 model-facing 工具；
- `information` 通过 `interfaceId + operation: help | read` 访问所有信息接口；
- 每类信息由一个强类型 Provider 实现，不建立一个包含所有字段的巨大快照；
- Runtime 统一负责注册、audience、schema、字段选择、selector/cursor、限额、错误、证据和 trace；
- Provider 只负责把最小来源端口转换成该接口允许公开的字段；
- Context Composer、Grounding 和后续 controller 都必须经过同一个 Runtime；
- `MinecraftSnapshotV1`、Mineflayer `Bot`、raw entity/block 表和测试 oracle 不得成为旁路输入。

“统一接口”指统一调用和治理协议，不表示所有 Provider 共享同一个值 schema，也不表示把 Minecraft 领域语义抽象成通用环境框架。

## 2. Clean-slate 约束

项目仍处于初期，v0.2 不承担旧内部协议兼容：

- 不实现 `MinecraftSnapshotV1 → InformationReadResult` 兼容适配器；
- 不保留 V1/V2 双模型路径、旧 skill 目录或新旧 Context 并行分发；
- 不为旧字段提供 alias、upcaster、deprecation window 或持久化迁移；
- 新纵向切片可直接删除旧 `DecisionContext.snapshot`、`availableSkills` 和直接 Backend 查询；
- v0.2 尚未交付的新动作可以暂时不可用，不允许为维持原型能力而保留认知旁路；
- 旧测试若验证的是已撤回契约，应删除或重写，而不是迫使新设计兼容。

Git 历史已经保存原型。代码中不再建立“compatibility only”层。

## 3. 总体结构

```text
Mineflayer / Protocol Driver
  ├── SessionScopeSource
  ├── SelfHudSource
  ├── InventorySource
  ├── DebugScreenSource
  ├── ScreenSessionSource
  └── raw observation candidates
             │
             │ narrow, read-only source ports
             ▼
Provider-owned projection / Perception Boundary
  ├── UiContextProvider
  ├── CurrentStatusProvider
  ├── InventoryProvider
  ├── F3Provider
  ├── ScreenProvider
  ├── ViewportProvider
  ├── SoundProvider
  └── LifecycleProvider
             │
             ▼
┌─────────────────────────────────────────────────────┐
│ InformationRuntime                                  │
│ Registry │ AccessPolicy │ RefStore │ CursorStore    │
│ limits   │ validation   │ envelope │ trace          │
└─────────────────────────────────────────────────────┘
       │                    │                    │
       ▼                    ▼                    ▼
Context Composer          Grounding          Scoped Controller
companion grant       evidence-bound refs      bounded grant
       │                    │                    │
       └────────────── Information Read IDs ────┘
                            │
                            ▼
                  Grounding / Evidence / Memory
```

Provider 之间不通过 `InformationRuntime` 互相调用。共享状态由单一投影端口提供，例如 `CurrentScreenProvider` 读取 `UiContextProjection`，而不是递归调用 `ui_context.read()`。

## 4. 统一查询协议

`information_catalog` 和 `information` 是 Runtime 查询协议的薄适配器，用于验证 Catalog/Help/Read 的输入、限额和取消语义。当前生产 Model Gateway 不注册这两个工具，模型只收到 Context Composer 预先选定的完整 Read envelope。如果未来重新开放主动信息获取，必须先单独评审其信息泄漏、成本和决策稳定性，不以兼容路径自动启用。

### 4.1 Catalog 工具

```ts
interface InformationCatalogTool {
  name: 'information_catalog'
  invoke(
    input: InformationCatalogRequest,
    session: InformationToolSessionContext,
    signal: AbortSignal,
  ): Promise<InformationCatalogResult>
}
```

Catalog 只返回当前 caller 可见的接口，不接受 audience 参数。

### 4.2 Information 工具

```ts
type InformationQueryRequest =
  | {
      interfaceId: InformationInterfaceId
      operation: 'help'
      availability?: 'all' | 'current'
      search?: string
      fields?: string[]
    }
  | {
      interfaceId: InformationInterfaceId
      operation: 'read'
      schemaRevision: string
      fields: string[]
      selector?: InformationSelectorRef
      page?: { cursor?: string; limit?: number }
    }

interface InformationTool {
  name: 'information'
  invoke(
    input: InformationQueryRequest,
    session: InformationToolSessionContext,
    signal: AbortSignal,
  ): Promise<InformationToolResult<Record<string, unknown>>>
}
```

非受信输入不能传 `audience`、`principalId`、`grantId`、world、screen、source 或 evidence。适配器从受信任的运行时上下文补齐这些信息。

### 4.3 有界查询 Session

```ts
interface InformationToolSessionContext {
  sessionId: string
  decisionRunId: string
  correlationId: string
  principalId: 'companion-model'
  grantId: string
  budget: {
    maxCalls: number
    maxReadCalls: number
    maxReturnedBytes: number
    deadlineAt: string
  }
}
```

该 Session 为查询适配器保留有界调用、返回字节和 deadline 语义；它不代表当前存在 model tool loop。生产 Context Composer 使用固定且已审查的小型 Read plan，同样合并剩余 deadline 与调用方 `AbortSignal`；已经进入 Provider 的 Read 也必须可取消。

## 5. Runtime 公共接口

```ts
interface TrustedInformationCaller {
  principalId: string
  grantId: string
  purpose: InformationGrant['purpose']
  correlationId: string
  decisionRunId?: string
  controllerLeaseId?: string
}

interface InformationRuntime {
  catalog(
    caller: TrustedInformationCaller,
    request: InformationCatalogRequest,
  ): InformationCatalogResult

  query(
    caller: TrustedInformationCaller,
    request: InformationQueryRequest,
    signal: AbortSignal,
  ): Promise<InformationToolResult<Record<string, unknown>>>

  invalidate(event: InformationInvalidationEvent): void
}

interface InformationRuntimeDiagnostics {
  inspect(caller: TrustedOperatorCaller): InformationRuntimeDebugState
}
```

`TrustedInformationCaller` 只能由 Context Composer、Controller Runtime 或 Operator API 构造。Runtime 要求 caller purpose 与 grant purpose 精确相同，因此 operator grant 不能进入 Companion Context 或 Controller 查询。自然语言、模型 JSON 和 Minecraft 聊天都不能生成 caller 或 grant。`model_tool` purpose 仅为尚未接入生产的适配器契约保留，不会由 Model Gateway 签发。Diagnostics 是单独的 privileged composition port，不从模型、Context 或 Controller 可达。

## 6. Access Policy 与 grant

```ts
interface InformationGrant {
  id: string
  principalId: string
  audience: InformationAudience
  allowedInterfaces: '*' | readonly InformationInterfaceId[]
  allowedFields?: Partial<
    Record<InformationInterfaceId, readonly string[]>
  >
  connectionEpoch?: number
  worldId?: string
  screenInstanceId?: string
  purpose: 'companion_context' | 'model_tool' | 'controller' | 'operator'
  validUntil?: string
}

interface InformationAccessPolicy {
  resolve(grantId: string, principalId: string): InformationGrant | undefined
  authorize(
    grant: InformationGrant,
    provider: InformationProviderDescriptor,
    operation: 'catalog' | 'help' | 'read',
    fields: readonly string[],
    scope: InformationScopeSnapshot,
  ): { allowed: true } | { allowed: false; reason: 'audience_denied' }
}
```

- Companion grant 只覆盖 `audiences` 含 `companion` 的接口。
- Controller grant 由行为运行时签发，绑定 lease、epoch、world、允许接口和可选字段集合。
- Operator grant 由本地管理入口签发，不能进入 Companion Context 或 Controller 查询。
- audience 是 Runtime 权限，不是 Help 字段，也不能由 Provider 根据模型文本推断。

## 7. Provider 定义

### 7.1 字段与描述符

```ts
import type { ZodType } from 'zod'

type FieldId<Values extends object> = Extract<keyof Values, string>

type InformationScopeDependency =
  | 'connection'
  | 'world'
  | 'dimension'
  | 'ui'
  | 'screen'

interface InformationFieldDefinition<Value> {
  description: string
  valueSchema: ZodType<Value>
  valueType: string
  unit?: string
  precision: 'displayed' | 'quantized' | 'exactly_displayed' | 'inferred'
  sourceKinds: readonly InformationSourceKind[]
  requires?: readonly string[]
  notes?: string
}

interface InformationProviderDefinition<Values extends object> {
  id: InformationInterfaceId
  description: string
  schemaRevision: string
  audiences: readonly InformationAudience[]
  scopeDependencies: readonly InformationScopeDependency[]
  fields: {
    readonly [Field in FieldId<Values>]: InformationFieldDefinition<Values[Field]>
  }
  selectors?: {
    required: boolean
    acceptsKinds: readonly string[]
  }
  pagination?: {
    defaultLimit: number
    maxLimit: number
  }
  limits: {
    maxFieldsPerRead: number
    maxResultBytes: number
    timeoutMs: number
  }
}
```

`schemaRevision` 表示字段名、类型、精度、来源或语义版本，不随当前游戏值变化。Registry 在启动时校验字段 ID 唯一、Zod schema 存在、limit 合法且 audience 非空。

### 7.2 Provider 运行接口

```ts
interface ProviderAvailability<Values extends object> {
  overall: 'available' | 'partially_available' | 'unavailable'
  informationRevision: number
  fields: Partial<
    Record<FieldId<Values>, Exclude<InformationAvailability, 'available'>>
  >
}

interface ProviderReadRequest<Values extends object, Selector, PageState> {
  fields: readonly FieldId<Values>[]
  selector?: Selector
  page: { limit: number; state?: PageState }
}

interface ProviderReadResult<Values extends object, PageState> {
  informationRevision: number
  values: Partial<Values>
  unavailable: Array<{
    field: FieldId<Values>
    reason:
      | Exclude<InformationAvailability, 'available'>
      | 'stale_selector'
      | 'wrong_world'
      | 'wrong_screen'
  }>
  source: {
    kind: InformationSourceKind
    adapterRevision: string
    sourceRevision: number
    acquisition: InformationReadResult<Record<string, unknown>>['source']['acquisition']
  }
  observedAt: string
  validUntil?: string
  evidenceIds: string[]
  nextPageState?: PageState
}

interface InformationProviderContext {
  now: string
  scope: InformationScopeSnapshot
  caller: Readonly<{
    audience: InformationAudience
    purpose: InformationGrant['purpose']
  }>
  refs: InformationReferenceIssuer
}

interface InformationProvider<Values extends object, Selector = never, PageState = never> {
  readonly definition: InformationProviderDefinition<Values>

  availability(
    context: InformationProviderContext,
  ): ProviderAvailability<Values>

  read(
    context: InformationProviderContext,
    request: ProviderReadRequest<Values, Selector, PageState>,
    signal: AbortSignal,
  ): Promise<ProviderReadResult<Values, PageState>>
}
```

Provider 返回内部结果，不构造 `InformationReadResult`、`InformationRequestError`、cursor 或 catalog。Runtime 负责外部 envelope。

### 7.3 Provider 不变量

- 只返回请求字段；返回额外字段是程序缺陷。
- 每个值通过对应 Zod schema；Runtime 必须使用 Zod 解析后的 `data` 重建外部结果，不能在校验成功后继续返回 Provider 原始对象。这样嵌套对象中的未知属性也不会越过 schema。
- 每个实际返回字段的 `source.kind` 必须属于该字段声明的 `sourceKinds`；不匹配时整次 Read 返回 sanitized `provider_failed`。
- `informationRevision` 来自 Provider 对外投影；只有可见值或 availability 变化时递增，不能直接复用 raw source revision。
- 不读取网络、文件、模型或测试服务器；Read 只消费内存投影或有界同步 source snapshot。
- 不导入 Mineflayer `Bot`、Paper oracle、Context Composer、Model Provider 或其他 Information Provider。
- 不把 raw 坐标、实体 ID、内部 NBT 或隐藏精度塞进 `evidenceIds`、selector 或错误消息。
- `unavailable` 只描述合法字段当前无法读取；未知字段在进入 Provider 前由 Runtime 拒绝。
- Provider 抛错表示实现缺陷，不是正常 unavailable。

## 8. Registry

```ts
interface InformationProviderDescriptor {
  id: InformationInterfaceId
  description: string
  schemaRevision: string
  audiences: readonly InformationAudience[]
  fieldIds: readonly string[]
}

type ErasedInformationProvider = InformationProvider<
  Record<string, unknown>,
  unknown,
  unknown
>

interface InformationRegistry {
  register<Values extends object, Selector, PageState>(
    provider: InformationProvider<Values, Selector, PageState>,
  ): void
  seal(targetMinecraftVersion: string): void
  provider(id: InformationInterfaceId): ErasedInformationProvider | undefined
  descriptors(): readonly InformationProviderDescriptor[]
  catalogRevision(): string
}
```

`ErasedInformationProvider` 是 Registry 内部的受控类型擦除视图；调用端不能取得它。`register` 在擦除前保留具体 Provider 的字段、selector 和 page-state 类型，Runtime 仍对擦除后的所有输入输出执行 schema 校验。

- 所有 Provider 在 App composition 阶段注册，`seal()` 后不可变。
- 重复 interface ID、同一 Provider 内重复 field ID、空字段集、错误 schema 或无 audience 使启动失败；不同接口可以拥有同名字段。
- Registry base revision 由锁定 Minecraft 版本和排序后的 Provider 描述符计算；返回给 caller 的 effective `catalogRevision` 还哈希其当前 grant 可见的接口、schema 与字段集合。权限收窄会迫使模型刷新 Catalog/Help，当前 availability 仍不改变 revision。
- v0.2 不支持运行时插件热装载。

## 9. Scope 与 revision 所有权

```ts
interface InformationScopeSnapshot {
  processSessionId: string
  connectionState: 'disconnected' | 'connecting' | 'configuration' | 'play'
  connectionEpoch: number
  worldId?: string
  dimension?: string
  uiRevision: number
  screenInstanceId?: string
  screenRevision?: number
  capturedAt: string
}

interface InformationScopeSource {
  capture(): Readonly<InformationScopeSnapshot>
}
```

| Revision | 所有者 | 何时变化 | 不应因何变化 |
|---|---|---|---|
| `catalogRevision` | Registry + Access Policy | Provider、字段描述符、audience、目标版本或 caller 可见字段变化 | 当前游戏值变化 |
| `schemaRevision` | 单个 Provider | 字段名、类型、单位、精度或语义变化 | availability 和当前值变化 |
| `informationRevision` | Provider 对外投影 | 对外可见值或 availability 变化 | raw 状态中被过滤掉的隐藏变化 |
| `connectionEpoch` | Backend lifecycle | 每次新连接尝试进入有效会话 | 重生或普通 screen 变化 |
| `uiRevision` | UI Context projection | main surface、input target 或 overlay 变化 | 普通背包数量变化 |
| `screenRevision` | Screen projection | screen 替换、结构或控件集合变化 | 普通槽位、文本或进度值变化 |
| `sourceRevision` | 来源端口 | Provider 消费的原始投影变化 | Help/Catalog 调用 |

Runtime 在调用 Provider 前后各捕获一次 scope。若 Provider 所声明依赖的 epoch、world 或 screen 已改变，Runtime 丢弃内部结果并返回 `scope_changed`，不能把旧结果包装成新 scope。

不同 Provider 的 Read 默认不保证跨接口原子性。Context Composer 需要组合多个结果时保存各自 `readId`、revision 和 observedAt；v0.2 不增加能一次导出所有状态的 `batch_snapshot`。

## 10. Selector 与 cursor

v0.2 使用有界、进程内、状态型 opaque store，不使用携带 raw payload 的自包含 token。

```ts
interface InformationReferenceIssuer {
  issue<Payload>(input: {
    kind: string
    payload: Payload
    allowedInterfaces: readonly InformationInterfaceId[]
    validUntil?: string
    bindToScreen?: boolean
  }): InformationSelectorRef
}

interface InformationRefStore {
  resolve<Payload>(
    ref: InformationSelectorRef,
    targetInterface: InformationInterfaceId,
    scope: InformationScopeSnapshot,
    grant: InformationGrant,
  ): Payload | undefined
  invalidate(event: InformationInvalidationEvent): void
}
```

- ID 使用不可预测随机值；payload/page state 只保存在进程内 map，且必须是可复制、有字节上限的 JSON DTO，不能保存 raw Minecraft 对象。
- Ref 绑定 principal、audience、epoch、world，并可选绑定 screen instance/revision。
- `bindToScreen` 只有在当前同时存在 screen instance 与 screen revision 时才允许签发，否则拒绝；不能生成名义上绑定、实际上缺少 screen scope 的 ref。
- Provider 可签发 item、screen element、observation 等 ref；目标 Provider 必须在 `allowedInterfaces` 中。
- Runtime 在目标 Read 前后都重新读取签发源 Provider 的 availability；其当前 `informationRevision` 必须等于 ref 的 `basedOnInformationRevision`。读取期间源投影变化时丢弃目标结果，不能让旧物品或旧观察 ref 命中新状态。
- 日志只记录 ref ID、kind、scope 和失效原因，不记录 payload。
- disconnect、world/dimension change、screen replacement、默认 TTL 或 grant 结束会失效。
- Cursor 使用同一原则，额外绑定 interface、字段集合、selector、information revision、limit 和 page state。
- Store 有 payload/page-state 字节上限、每次 Read 的 ref 签发上限，以及每 principal、每 interface 和全局容量上限。清理过期项后仍超限时拒绝新增，不跨主体或接口淘汰尚有效的引用。

## 11. 一次 Query 的完整流程

```text
Context Composer/受信任适配器提交 interfaceId + help/read
→ 从运行时授权构造 TrustedInformationCaller
→ AccessPolicy 解析 grant；失败返回 audience_denied
→ Registry 查 Provider；失败返回 unknown_interface
→ 捕获 scopeBefore
→ 严格解析 envelope；额外/重复/畸形字段返回 invalid_request
→ 校验 caller/grant purpose、audience、interface、schema、字段、selector/cursor、limit 与调用预算
→ help: 合并静态 field definition 与 Provider.current availability
→ read: 解析 opaque ref/page state，调用 Provider
→ 校验 Provider 只返回请求字段且所有值通过 schema
→ 再捕获 scopeAfter；相关 scope 改变则丢弃结果
→ Runtime 分配 readId，采用 Provider informationRevision，签发 nextCursor
→ 包装 source、evidence、partial unavailable
→ 记录不含原始值的 trace
→ 返回模型/Context/Controller
```

正常不可用不抛异常。Abort、deadline、Provider 缺陷和 scope race 由 Runtime 转为结构化错误，完整异常只进入 privileged telemetry。

## 12. Provider 示例：current_status

### 12.1 最小来源端口

```ts
type CurrentStatusSourceSnapshot =
  | { informationRevision: number; available: false }
  | {
      informationRevision: number
      available: true
      sourceRevision: number
      connectionEpoch: number
      observedAt: string
      healthDisplay: number
      maxHealthDisplay: number
      absorptionDisplay: number
      armorDisplay: number
      foodDisplay: number
      airDisplay?: number
      experienceLevel: number
      experienceProgressDisplay: number
      effectsDisplay: Array<{
        id: string
        amplifierDisplay: number
        durationDisplay?: string
      }>
    }

interface CurrentStatusSource {
  capture(): Readonly<CurrentStatusSourceSnapshot>
}
```

这个 source port 是 Provider-owned projection 的读取面：Driver adapter 先过滤并投影，只在对外值或 availability 变化时增加 `informationRevision`。它不包含 saturation、隐藏生命小数、精确效果 tick 或完整 `Bot`。即使 Protocol Driver 拥有这些值，也不应让每个 Provider 重复决定是否过滤。

### 12.2 值与 Provider

```ts
interface CurrentStatusValues {
  health: { current: number; max: number; absorption: number }
  armor_display: number
  food_display: number
  air_display: number
  experience: { level: number; progress: number }
  status_effects_display: Array<{
    id: string
    amplifier: number
    duration?: string
  }>
}

class CurrentStatusProvider
  implements InformationProvider<CurrentStatusValues> {
  readonly definition = currentStatusDefinition

  constructor(private readonly source: CurrentStatusSource) {}

  availability(): ProviderAvailability<CurrentStatusValues> {
    const snapshot = this.source.capture()
    return snapshot.available
      ? {
          overall: 'available',
          informationRevision: snapshot.informationRevision,
          fields: {},
        }
      : {
          overall: 'unavailable',
          informationRevision: snapshot.informationRevision,
          fields: allFields('not_connected'),
        }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<CurrentStatusValues, never, never>,
  ): Promise<ProviderReadResult<CurrentStatusValues, never>> {
    const snapshot = this.source.capture()
    if (!snapshot.available) {
      return unavailableResult(
        request.fields,
        'not_connected',
        snapshot.informationRevision,
      )
    }

    const values: Partial<CurrentStatusValues> = {}
    if (request.fields.includes('health')) {
      values.health = {
        current: snapshot.healthDisplay,
        max: snapshot.maxHealthDisplay,
        absorption: snapshot.absorptionDisplay,
      }
    }
    if (request.fields.includes('food_display')) {
      values.food_display = snapshot.foodDisplay
    }

    return {
      informationRevision: snapshot.informationRevision,
      values,
      unavailable: [],
      source: {
        kind: 'hud_projection',
        adapterRevision: 'current-status-provider.v1',
        sourceRevision: snapshot.sourceRevision,
        acquisition: 'immediate_client_state',
      },
      observedAt: snapshot.observedAt,
      evidenceIds: [],
    }
  }
}
```

示例省略了其他字段分支以及 `allFields`、`unavailableResult` 辅助函数，但不省略 Runtime 校验。Provider 不能依靠 TypeScript 泛型阻止运行时模型输入，所有字段和输出仍通过 Registry schema 校验。

## 13. 统一查询协议样例

以下是 Runtime 与内部适配器的契约样例，不表示这些请求会出现在当前模型上下文或由模型主动发出。

### 13.1 发现接口

请求：

```json
{
  "operation": "list_interfaces"
}
```

响应节选：

```json
{
  "protocol": "mineintent.information-catalog.v1",
  "status": "ok",
  "targetMinecraftVersion": "1.21.1",
  "catalogRevision": "catalog:1.21.1:8d72",
  "interfaces": [
    {
      "id": "current_status",
      "description": "当前 HUD 与自身可检查状态",
      "schemaRevision": "current-status:1",
      "audiences": ["companion", "controller"],
      "availability": "available"
    },
    {
      "id": "current_screen_information",
      "description": "当前 Screen 的可见槽位、控件和文本",
      "schemaRevision": "current-screen:1",
      "audiences": ["companion", "controller"],
      "availability": "unavailable"
    }
  ]
}
```

`client_diagnostics` 不出现在 Companion Catalog 中。

### 13.2 Help

请求：

```json
{
  "interfaceId": "current_status",
  "operation": "help",
  "availability": "current",
  "fields": ["health", "food_display", "status_effects_display"]
}
```

响应：

```json
{
  "protocol": "mineintent.information-help.v1",
  "interfaceId": "current_status",
  "schemaRevision": "current-status:1",
  "availabilityMode": "current",
  "fields": [
    {
      "id": "health",
      "description": "HUD 可表达的当前、最大与吸收生命值",
      "valueType": "object",
      "precision": "displayed",
      "interfaceId": "current_status",
      "sourceKinds": ["hud_projection"],
      "availability": "available"
    },
    {
      "id": "food_display",
      "description": "当前饥饿条显示值",
      "valueType": "number",
      "unit": "half_shank",
      "precision": "exactly_displayed",
      "interfaceId": "current_status",
      "sourceKinds": ["hud_projection"],
      "availability": "available"
    },
    {
      "id": "status_effects_display",
      "description": "当前 HUD 可检查的状态效果",
      "valueType": "array",
      "precision": "displayed",
      "interfaceId": "current_status",
      "sourceKinds": ["hud_projection"],
      "availability": "available"
    }
  ]
}
```

Help 不返回生命值或饥饿值。

### 13.3 Read

请求：

```json
{
  "interfaceId": "current_status",
  "operation": "read",
  "schemaRevision": "current-status:1",
  "fields": ["health", "food_display", "status_effects_display"]
}
```

响应：

```json
{
  "protocol": "mineintent.information-read.v1",
  "readId": "read_01J2YQF5X7",
  "interfaceId": "current_status",
  "schemaRevision": "current-status:1",
  "informationRevision": 184,
  "connectionEpoch": 7,
  "worldId": "survival-main",
  "dimension": "minecraft:overworld",
  "observedAt": "2026-07-14T13:20:31.412Z",
  "source": {
    "kind": "hud_projection",
    "adapterRevision": "current-status-provider.v1",
    "sourceRevision": 491,
    "acquisition": "immediate_client_state"
  },
  "values": {
    "health": { "current": 17, "max": 20, "absorption": 0 },
    "food_display": 14,
    "status_effects_display": []
  },
  "unavailable": [],
  "evidenceIds": []
}
```

### 13.4 当前 Screen 不可用

```json
{
  "protocol": "mineintent.information-read.v1",
  "readId": "read_01J2YQG02A",
  "interfaceId": "current_screen_information",
  "schemaRevision": "current-screen:1",
  "informationRevision": 42,
  "connectionEpoch": 7,
  "observedAt": "2026-07-14T13:20:34.001Z",
  "source": {
    "kind": "screen_projection",
    "adapterRevision": "current-screen-provider.v1",
    "acquisition": "current_screen"
  },
  "values": {},
  "unavailable": [
    { "field": "visible_slots", "reason": "screen_not_open" },
    { "field": "visible_controls", "reason": "screen_not_open" }
  ],
  "evidenceIds": []
}
```

这是合法 partial/unavailable，不是 `provider_failed`。

### 13.5 Selector 跨接口使用

`inventory_information` 返回的每个当前可检查物品可以携带 Runtime 签发的 `inventory.item` selector：

```json
{
  "interfaceId": "item_tooltip_information",
  "operation": "read",
  "schemaRevision": "item-tooltip:1",
  "fields": ["display_name", "lore", "enchantments_display"],
  "selector": {
    "protocol": "mineintent.information-selector-ref.v1",
    "id": "ref_01J2YQH7PA",
    "interfaceId": "inventory_information",
    "connectionEpoch": 7,
    "worldId": "survival-main",
    "basedOnInformationRevision": 91,
    "validUntil": "2026-07-14T13:21:10.000Z"
  }
}
```

Runtime 先确认该 ref 允许 `item_tooltip_information`、仍属于当前 principal/epoch/world 且物品 revision 有效，再把内部 item handle 交给 Provider。模型不能把 `id` 换成槽位号或坐标。

### 13.6 请求级错误

```json
{
  "protocol": "mineintent.information-error.v1",
  "interfaceId": "current_status",
  "code": "stale_schema",
  "message": "The interface schema changed; call help again.",
  "currentSchemaRevision": "current-status:2"
}
```

以下情况是请求级错误而不是 partial unavailable：未知接口/字段、schema 陈旧、selector/cursor 非法、audience 拒绝、deadline、Provider 缺陷和读取期间 scope 改变。

## 14. UI Context 的共享方式

`UiContextProvider` 同时拥有一个只读 `UiContextProjection`：

```ts
interface UiContextProjection {
  snapshot(): Readonly<UiContextV1>
  subscribe(listener: (value: Readonly<UiContextV1>) => void): Unsubscribe
}
```

- `CurrentScreenProvider` 使用 projection 校验 screen instance/revision。
- `ViewportProvider` 使用 main surface 和 overlay 状态决定当前认知视口是否可发布。
- `ItemTooltipProvider` 使用当前 screen/item ref 判断 tooltip 是否可检查。
- `ChatProvider` 使用 `chatMode` 和 input target，但不取得输入文字以外的隐藏 GUI 对象。
- Provider 不调用 `UiContextProvider.read()`，因此不会产生递归 query、重复 readId 或不同步 envelope。

## 15. Context 与模型调用重构

### 15.1 Context Composer

新的 Context 不包含 snapshot：

```ts
interface InformationContextSection {
  catalogRevision: string
  reads: Array<InformationReadResult<Record<string, unknown>>>
  omissions: Array<{
    interfaceId: InformationInterfaceId
    reason: 'budget' | 'low_salience' | 'unavailable' | 'stale'
  }>
}
```

Context Composer 根据触发事件和 Attention 选择少量 Read，例如玩家聊天时附带 `ui_context`、`current_status` 与相关当前观察。它不自动附带全部 17 个接口。

### 15.2 生产决策路径

Model Gateway 每轮只接收一次 Context Composer 的结果，不执行 tool loop：

```text
按触发事件选择固定、有界的 Read plan
→ Information Runtime 校验授权并生成完整 envelope
→ Context Composer 组合事实输入与 omission
→ 模型产生结构化语义决定
→ Grounding 只接受该轮中实际出现的 read/ref
```

Read envelope 是模型事实输入，不是私有思维链。每个 `readId` 进入 Decision trace；模型最终决定若依赖未出现的 Read/ref，Coordinator 拒绝。当前固定 Read plan 是产品边界，不允许模型通过反复分页导出完整历史或全部世界状态。

## 16. 错误与降级

| 情况 | 外部结果 | 内部处理 |
|---|---|---|
| envelope/重复字段不合法 | `invalid_request` | 拒绝，不猜测修复 |
| 字段当前不可见 | partial `unavailable` | 正常统计 |
| 未知字段 | `unknown_field` | 提示重新 Help |
| schema 变化 | `stale_schema` | 返回当前 revision |
| selector 已失效或签发源 revision 已改变 | `invalid_selector` 或字段 `stale_selector` | 删除/拒绝 ref，丢弃读取中的结果 |
| scope 在读取中变化 | `scope_changed` | 丢弃 Provider 结果 |
| Context Read plan 超额 | `budget_exceeded` | 停止本轮额外 Read 并记录 omission |
| Abort/deadline | `deadline_exceeded` | 取消 Provider |
| Provider 抛错/输出越界 | `provider_failed` | privileged telemetry；阻止结果进入认知 |
| Registry 缺失必需 Provider | 启动失败 | 不以空接口继续运行 |

错误消息只包含恢复所需信息，不包含异常堆栈、raw target、服务器地址、token 或 Provider payload。

## 17. 可观察性

Runtime 调试状态至少包含：

- catalog revision、已注册 Provider 和 schema revision；
- 每接口 Help/Read 次数、耗时、结果字节数和 unavailable 分布；
- audience 拒绝、未知字段、陈旧 schema、无效 ref/cursor 和 scope race 数量；
- Ref/Cursor store 当前数量、容量拒绝和失效原因；
- 每个 Provider 当前 information/source revision；
- Context Read plan 的预算使用与 omission；
- `readId → interface/fields/source/evidence/correlation` trace，不记录默认值 payload。

`structured_ui_equivalent` 和显著主动观察可以产生 `information.acquired` 领域事件；普通重复 Read 默认只进入有界 trace，不把生命值轮询写满 Journal。

## 18. 测试设计

### 18.1 所有 Provider 必跑的契约套件

- Definition 字段与 Help 完全一致。
- 只返回请求字段；外部值由字段 Zod 解析结果重建，嵌套未知属性不能残留。
- 每个返回字段的 source kind 都在字段声明的 `sourceKinds` 内。
- 当前不可用返回 partial，不抛异常、不用隐藏值补齐。
- audience、field allowlist 和 grant scope 生效。
- 断线、world/dimension/screen 变化使相关 ref/cursor 失效。
- result 大小、字段数、分页和 deadline 有硬上限。
- Provider 错误、额外字段和 schema 错误不能进入模型结果。
- Help 不返回当前值；Catalog 不返回未授权接口。

### 18.2 Runtime 测试

- 注册顺序不影响 catalog revision。
- 重复 Provider/字段和未密封 Registry 拒绝启动。
- 相同 schema revision 可缓存；改变后返回 `stale_schema`。
- Ref 不能跨 principal、audience、epoch、world、screen 或接口使用；要求 screen binding 时必须存在具体 screen instance/revision。
- Ref 在目标读取前后都必须匹配签发源当前 information revision。
- Cursor 不能修改 fields/limit/selector 后续页。
- Ref/Cursor 的 payload、page state、每次签发数以及 principal/interface/global 容量均有硬上限，超限不能淘汰其他作用域的有效项。
- scope before/after 不同会丢弃结果。
- Context Read plan 达到调用/字节预算后确定性停止并记录 omission，deadline 能中断已经开始的 Provider Read。
- privileged diagnostics 不进入 Companion Catalog、Read、Context 或 Journal payload。

### 18.3 第一条纵向验收

```text
Backend ready
→ UiContextProvider + CurrentStatusProvider 更新投影
→ Companion Catalog 只列合法接口
→ Help current_status
→ Read health/food/effects
→ Context 保存 readId
→ 模型可引用值
→ Decision trace 可复原来源
→ 断线后同一 Read 返回 not_connected，旧 ref 失效
```

这条链完成前不并行实现所有字段。

## 19. 源码目录

```text
src/information/
├── contracts/
│   ├── v1.ts
│   └── schemas.ts
├── registry.ts
├── runtime.ts
├── access-policy.ts
├── scope.ts
├── ref-store.ts
├── cursor-store.ts
├── tool-session.ts
├── trace.ts
├── source-ports/
│   ├── session.ts
│   ├── self-hud.ts
│   ├── inventory.ts
│   ├── debug-screen.ts
│   ├── screen-session.ts
│   └── diagnostics.ts
├── providers/
│   ├── ui-context.ts
│   ├── current-status.ts
│   ├── hotbar.ts
│   ├── inventory.ts
│   ├── item-tooltip.ts
│   ├── f3.ts
│   ├── crosshair.ts
│   ├── hud.ts
│   ├── chat.ts
│   ├── player-list.ts
│   ├── current-screen.ts
│   ├── advancement.ts
│   ├── recipe-book.ts
│   ├── viewport.ts
│   ├── sound.ts
│   ├── lifecycle.ts
│   └── client-diagnostics.ts
├── testing/
│   ├── provider-contract.ts
│   ├── fake-provider.ts
│   └── leak-assertions.ts
└── index.ts
```

Perception 可以保留在 `src/perception/`，但 `ViewportProvider` 和 `SoundProvider` 只读取其合法 Cognitive Projection。Minecraft raw source port 的具体实现位于 `src/minecraft/driver/`，接口定义位于 `src/information/source-ports/`，依赖方向始终指向领域端口。

## 20. Issue 归属与实施顺序

| 阶段 | Issue | 交付 |
|---|---|---|
| P0 | #53 | 本设计、contracts、Registry、Runtime、AccessPolicy、Ref/Cursor Store、Fake Provider、契约测试；不实现真实游戏字段 |
| P1 | #54 | Session/UI projection、UiContextProvider |
| P1 | #55 | CurrentStatusProvider 第一条纵向链，再实现 hotbar/inventory/F3 |
| P2 | #56 | HUD/chat/Tab/current screen/advancement/recipe providers |
| P2 | #34 | Cognitive Projection 与 viewport provider；#46/#47 保持算法子项 |
| P2 | #59 | LifecycleProvider 与 SoundProvider；内部可再拆原生 Sub-issues |
| 全程 | #57 | Provider 契约、泄漏、scope、Paper 和真人可见性矩阵 |

这些 Issue 仍是 #58 的同级原生 Sub-issues。#53 是共享依赖，不是其余功能的父任务。

## 21. 删除旧路径的切换门

第一条 `current_status` 纵向链通过后，由 #58 的 P4 集成收尾直接执行以下替换：

1. Model Gateway 只接收 Context Composer 组合的有源 Read envelope，不再接收 `DecisionContext.snapshot` 且不注册 Information tool loop。
2. Context Composer 只接收 `InformationRuntime`/Information Client。
3. Companion Runtime 删除直接 `backend.snapshot()` 认知读取。
4. 删除 `availableSkills` 和 V1 model-facing skill schema；v0.2 不保留旧动作能力占位。
5. `MinecraftSnapshotV1` 若 Driver 暂时仍需使用，降为 `src/minecraft/driver/` 私有实现类型，不从公共 index 导出。
6. #57 增加自动化 forbidden-import/旁路扫描，禁止 `src/companion/`、`src/models/`、`src/memory/`、`src/grounding/` 导入 raw Minecraft contracts 或继续使用上述旧认知入口。
7. 删除为旧路径服务且没有新契约用途的测试、配置和事件名。

不设置 feature flag、dual write、fallback 或自动回退到 snapshot。新路径失败时明确失败并修复。

## 22. 完成定义

模块设计与 #53 完成必须同时满足：

1. `InformationRuntime` 的 Catalog/Help/Read 入口具有可验证 schema，生产 Model Gateway 没有 Information tool loop。
2. Provider contract 能表达静态字段、当前 availability、partial Read、source、evidence、selector 和分页。
3. Registry、AccessPolicy、scope、revision、Ref/Cursor Store 和 Context Read budget 责任不重叠。
4. Fake Provider 能验证公共 Runtime；`current_status` 样例可由 #55 直接转成第一条真实 Provider 与契约测试。
5. UI/screen、inventory/tooltip、viewport/sound 三种共享关系不需要 Provider 递归调用。
6. Context/Model/Controller 没有 raw Backend 或 snapshot 旁路。
7. 旧 V1 决策、skill 目录和 snapshot 认知输入没有兼容要求。
8. #54/#55/#56/#34/#59 可以只实现自己的 source port、projection 和 Provider，而不重复 Runtime 逻辑。
