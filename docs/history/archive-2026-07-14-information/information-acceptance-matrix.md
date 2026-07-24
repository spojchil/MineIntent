---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_pr: 69
source_commit: f93cf7e
---

# 合法信息精度、可用性与非泄漏验收矩阵

> 原始状态声明（PR 未合并）：v0.2 实施基线
>
> 对应 Issue：[#57](https://github.com/spojchil/mineintent/issues/57)
>
> 共享 Runtime 修订：[#63 按 Provider scopeDependencies 绑定分页 Cursor](https://github.com/spojchil/mineintent/issues/63)、[PR #66 所含所有 Read 公开 revision 后验](https://github.com/spojchil/mineintent/pull/66)
>
> 上位设计：[合法信息与 UI](../../architecture/information-access-and-ui.md)、[Information Runtime](../../architecture/information-runtime.md)
>
> 已冻结/送审模块基线：[#67 UI Context](../../architecture/ui-context.md)、[PR #64 玩家状态与 envelope 预算](https://github.com/spojchil/mineintent/pull/64)、[PR #65 Screen 与 Overlay](https://github.com/spojchil/mineintent/pull/65)、[PR #66 Runtime/Lifecycle](https://github.com/spojchil/mineintent/pull/66)
>
> 测试环境：[Paper 集成测试](../../guides/paper-integration.md)

## 1. 目标

这套验收证明的不是“协议客户端知道什么”，而是 AI 玩家通过合法信息接口得到的内容与同版本原版玩家此刻可检查的内容一致。

每个 Provider 必须同时证明：

1. **准确**：返回值、显示精度、单位和时效符合客户端可见投影；
2. **诚实**：不能确定或当前不可检查时，按字段契约返回 value-level `unknown` 或明确 unavailable；`unknown` 不是公共 availability reason，不能用内部真值补齐；
3. **有界**：字段数、分页、字节、时间、引用和缓存均受限；
4. **可失效**：连接、世界、维度、screen、revision 或权限改变后，旧 ref/cursor 和旧事实的后续复用确定性拒绝；已经返回的 Read 仍是带时刻/revision 的历史证据，不会被 Runtime 追溯撤回；
5. **不泄漏**：协议真值、测试裁判、operator diagnostics、raw 对象和隐藏精度不进入模型可见结果、错误、引用、日志或 Context。

## 2. 三层证据与裁判隔离

验收同时记录三层数据，但生产依赖只能从上向下单向流动：

| 层 | 内容 | 可见范围 | 用途 |
|---|---|---|---|
| O0 内部裁判 | Paper 命令结果、协议包、Mineflayer raw 状态、确定性 fixture 真值 | 测试 harness/operator | 建立场景与断言，不得进入 Provider/Context |
| O1a 可信内部投影 | source port DTO、逐字段 provenance、内部 revision、UI/screen scope | 对应 reducer/可信消费者 | 验证适配算法、来源证明与内部失效语义 |
| O1b Provider 公开投影 | 模型可见 value/availability、information revision、public source revision/observedAt | 对应 Provider | 验证公开比较器与 metadata 非泄漏 |
| O2 模型结果 | Catalog/Help/Read、opaque ref/cursor、Context 选择结果 | Companion/Controller/Model | 最终产品验收对象 |

O0 可以与 O2 在测试断言中比较，但 O0 不能作为 O1 的生产输入。测试替身必须通过构造合法 source DTO 驱动 reducer/Provider，不能把期望结果直接注入 Runtime。O1a 与 O1b 必须分别断言：内部 provenance 更新不等于公开 Information 变化。

## 3. 矩阵记录格式

每个字段或状态族至少有一条机器可读用例，记录：

| 字段 | 说明 |
|---|---|
| `caseId` | 稳定场景 ID，例如 `status.health.visible` |
| `interfaceId` / `fields` | 请求接口与字段集合 |
| `minecraftVersion` | 锁定客户端/协议版本 |
| `precondition` | 连接、世界、screen、设置、游戏模式与资源状态 |
| `oracle` | O0 真值及其采集方式 |
| `expectedInternalProjection` | O1a 期望 DTO、provenance、scope 与内部 revision 变化 |
| `expectedProviderProjection` | O1b 期望 value/availability、information/public source revision 与 observedAt 变化 |
| `expectedEncoding` | 最大合法 value、完整 Runtime envelope 的 UTF-8 bytes，以及分页/截断恢复语义 |
| `query` | audience、Help/Read、selector、cursor 与 limit |
| `expectedResult` | O2 值或 request/field unavailable |
| `forbidden` | 绝不能出现的值、键、类型、精度或标识符 |
| `invalidation` | 后续事件与旧结果/ref/cursor 的期望行为 |
| `automation` | unit/contract/runtime/Paper/human 覆盖位置 |

矩阵定义是测试数据，不复制 Provider 实现逻辑。字段期望应使用显式值、集合、区间或量化桶，不能调用被测 projection 生成自己的期望。

## 4. 分层自动化

### L0：定义与 schema

所有 Provider 在普通 CI 中检查：

- Definition、Help 和稳定字段 ID 一致；
- Zod schema、`valueType`、unit、precision、`sourceKinds`、requires 和 notes 完整；
- audience、scope dependency、selector kind、分页和硬 limit 合法；
- schema revision 在字段语义变化时同步更新；
- `client_diagnostics` 不能出现在 Companion/Controller Catalog。

### L1：source projection 单元测试

使用冻结的 raw-like 测试 fixture 调用独立 projection，验证：

- 显示精度和量化边界；
- `not_supported`/`not_currently_displayed` 等 availability、value-level `unknown`、未加载与不可见的区别；
- raw ID/坐标/NBT/tick 精度在 DTO 边界前已移除；
- 相同合法投影不提升 revision，被过滤的隐藏变化不提升 revision；
- 对包含 provenance 的模块，`baseFieldAcquisition/UiFieldState.acquisition` 单独改变必须更新 O1a snapshot/revision 并通知可信订阅者；若 value/availability 不变，O1b `informationRevision/publicSourceRevision/publicObservedAt` 必须全部保持；
- `UiFieldState` unavailable 分支没有 value/acquisition；reducer 必须原样传播 `not_supported/not_exposed`，不能补 `false/hidden/none`；
- 聚合、去重、TTL 和预算降级保持确定性。
- 多次读取型 source（如 viewport optical DDA）必须使用 scope/revision-bound token 或等价不可变快照；中途 revision 改变只能 stale/unknown，提交前复核失败时不得混合两个 source revision。
- 所有自由字符串同时验证 code-point 与 `JSON.stringify` 后 UTF-8 byte 上限；四字节字符、引号、反斜杠和控制字符不能绕过预算。
- 非分页硬上限列表在 schema 中有显式 `truncated`/coverage；分页列表按项目与 byte budget 中先到者结束并产生可恢复 continuation，不能静默删项。

L1 fixture 可以模拟协议输入，但 projection 的公共返回类型必须是自有只读 DTO，不能是 `Bot`、Entity、Block、Vec3、Window 或 packet。

### L2：Provider 契约

扩展共享 `assertInformationProviderContract`，对每个 Provider 验证：

- 只返回请求字段，所有请求字段要么有值、要么有明确 unavailable；
- Provider 内部值通过字段 Zod schema；外部结果是否使用解析值重建由 L3 Runtime 测试证明；
- 实际 `source.kind` 属于每个返回字段声明的 `sourceKinds`；
- `observedAt`、`validUntil`、`informationRevision/source.sourceRevision` 和 evidence 合法；
- Provider 的公开 comparator 只比较自身 value/availability；provenance-only、scope-only 和过滤后 raw 变化不得改变公开 information/source revision、observedAt、固定 canonical acquisition 或 trace；
- Provider 协作响应 AbortSignal，内部分页 state 与声明上限一致；timeout、外部 result byte limit 和 cursor envelope 由 L3 Runtime 测试证明；
- Provider 不抛出正常不可用，不自行构造外部 cursor/error envelope。
- 每个 Provider 必须有 maximum-legal fixture：固定列表、字段组合、ref/evidence 和 bounded strings 全部达到契约最大值时，要么完整 value 仍能装入预算，要么在 projection/page builder 层产生 schema 已声明的 deterministic truncation/next page；不能依赖 Runtime 最终 `provider_failed`。
- 同一 projection revision 下，字段的 entries/truncated 不得随本次是否请求 sibling fields 改变；共享总预算必须在 projection commit 时固定，或拆成稳定字段子预算。

### L3：Runtime 安全回归

普通 CI 必须覆盖：

- principal、grant purpose、audience、field allowlist、epoch、world、dimension 和 screen 越权；
- schema 陈旧、未知/重复字段、非法 envelope、错误 selector/cursor 与 scope race；
- selector 在目标 Read 前后匹配签发源 revision，screen-bound ref 必须有具体 screen instance/revision；
- cursor 查询形状和 information revision 绑定且一次性消费；scope 只按密封 Provider definition 的 `scopeDependencies` 绑定，依赖 `ui` 时必须比较 `uiRevision`，不依赖 screen/world 的 cursor 不得被当时恰好非空的 scope 误伤；
- Ref/Cursor payload、page state、每次签发和 principal/interface/global 容量边界；
- Tool Session 调用/Read/字节预算、deadline 和上游取消；
- Runtime 使用 Zod 解析后的 data 重建外部值，嵌套未知键不能从 Provider 原对象残留；
- Runtime 对**所有** Read 在 scope 复核后同步读取同 Provider 当前公开 `informationRevision`；与结果不一致时，普通 Read 返回 `scope_changed`、续页返回 `invalid_page`。测试不得比较内部 source/provenance revision。
- Provider 的额外字段、错误 source、异常、超时、违反 schema/声明预算的超大结果和隐藏对象均转成 sanitized error；合法 maximum fixture 不能走此分支。
- 完整 `InformationReadResult`（含 protocol/read/source/unavailable/evidence/ref/cursor）使用 UTF-8 实际编码计数；maximum-legal envelope 必须 `<= maxResultBytes`，再增加一个非法字节或绕过 projection budget 的 Provider 输出才断言 sanitized `provider_failed`。
- acquisition-only `uiRevision` race 可以保守返回 `scope_changed`；重试后的 values/unavailable 与全部公开 metadata 必须和变化前一致。

### L3.1：大小边界的成对证明

每个接口至少提供两类确定性 fixture，不能只测“越大越拒绝”：

1. `maximum_legal_success`：使用四字节 Unicode、需要 JSON 转义的字符、最大字段组合、最大合法 ref/evidence 数和最长合法 cursor envelope。固定列表按 schema 显式 truncated，分页列表按稳定 byte budget 提前结束；最终 envelope 必须成功并可由 next cursor/flag 恢复语义。
2. `contract_oversize_rejected`：绕过 projection 预算、额外加入一项/一个 byte、返回 schema 外字段或构造超限 page state；Runtime 必须给 sanitized `provider_failed`，错误/trace 不回显 payload。

fixture 必须记录 value bytes、Runtime envelope reserve、最终 encoded bytes 和 `maxResultBytes`。随机 read/ref/cursor ID 使用与生产格式等长的 deterministic test ID，不能用短字符串低估开销。N/N+1 同时覆盖项目上限与字节上限。

### L4：Paper 场景

Paper 场景验证真实连接与客户端事件序列，不在普通 PR CI 中启动服务器。每个场景严格分为：

```text
setup（仅 harness 建立世界真值）
→ companion（只经 Information Runtime 读取）
→ assertion（O0 与 O2 对照）
→ cleanup（停止 Bot/Paper 并清理副本）
```

服务端命令、控制台输出和测试插件数据只允许出现在 setup/assertion recorder；Companion 运行阶段若能导入这些类型、读取 artifact 或调用 Paper oracle，场景直接失败。

### L5：真人可见性

真人验收不是替代自动化，而是确认“结构化等价物”确实对应同版本客户端体验。记录客户端版本、语言、GUI scale、debug 设置、资源包、截图/短视频时间点和 Read trace ID；涉及个人聊天或服务器地址时先脱敏。

## 5. 必测接口族

| 接口族 | 正常场景 | unavailable / value-level unknown 场景 | 主要禁漏项 |
|---|---|---|---|
| `ui_context` | 九个公开字段；world/screen、chat focus、逐项 F3/Tab/HUD overlay 与 0–24/horse classifier fixtures | 连接过渡、unknown/mod screen、逐成员 `not_supported/not_exposed`、异常关闭 | raw window/handler/source identity、伪默认值、逐字段 acquisition 泄漏 |
| `current_status` | 生命、护甲、饥饿、氧气、经验、姿态、效果 | 未连接、不适用的载具/氧气/冷却 | saturation、效果剩余 tick、服务端 attribute 内值 |
| hotbar/self inventory | 选中槽、数量、自身已同步槽位与分页 | 未连接/未同步；screen/overlay 开关不应影响普通 Read、cursor 或 item ref | 私有 NBT、隐藏组件、外部容器、carried item |
| item tooltip/screen item | inventory/hotbar item 的结构化 tooltip；当前 screen carried/slot item | item ref 陈旧；screen item 在 screen 切换/关闭后失效 | 未渲染 tooltip、raw components/NBT、协议 slot ID |
| F3/crosshair | 普通与 reduced debug、当前准星目标 | 禁用字段、未加载/无目标 | 完整区块、实体精确表、机器 diagnostics |
| HUD/chat/Tab | boss bar、字幕、聊天窗口、玩家列表；六 Provider 固定 `structured_ui_equivalent` | overlay state unavailable、未显示、历史超窗、权限不足 | 全聊天史、不可见玩家、服务器内部数据、动态 provenance 聚合 |
| current screen | 箱子、工作台、熔炉、交易、菜单；title/content Provider-wide revision | screen 切换/关闭、未知控件 | 未打开容器、handler 隐藏属性、按 coarse family 猜 role/layout |
| advancement/recipe | 当前客户端可访问条目和分页；嵌入式 recipe panel | 未解锁/不支持/界面不可检查 | 服务端隐藏进度、完整配方注册表、仅凭 `ScreenFamily=recipe_book` 判 current view |
| viewport | 前方、周边、近身、世界遮挡、UI presentation mask、同 revision optical token 与最近观察 | 背后、墙后、UI mask 未证明、unloaded/stale optical read、TTL 到期 | tracked entity 表、可执行坐标、完整 blocks、跨 revision voxel、未定义 `viewportPublication`/UI 下隐藏样本 |
| sound | 方向/距离带、聚合窗口、无位置声音 | TTL 到期、不可感知/不支持 | 声音包精确坐标、raw entity source |
| lifecycle | login/spawn/death/respawn/dimension/disconnect | 连接过渡与事件窗口外 | socket、服务器地址、内部重试状态 |
| diagnostics | operator 明确授权 | companion/controller 任何调用 | token、路径、服务器配置、角色不可见性能真值 |

## 6. 状态与失效矩阵

每个相关接口至少验证以下转换前后的 Help、Read、ref 和 cursor：

| 转换 | 必须变化 | 必须失效 |
|---|---|---|
| disconnected → connecting → configuration → play | connection state；新有效会话提升 epoch | 旧连接的 ref/cursor 与把旧 Read 当作当前事实的复用 |
| world/dimension change | world/dimension scope 与相关 Provider revision | 旧世界/维度观察、分页和目标引用 |
| death → respawn | lifecycle/status/sound/viewport 的相关公开 revision；是否提升 epoch 由 lifecycle 契约固定 | 依赖旧身体/观察状态的普通 Read、ref/cursor 与当前事实复用 |
| screen open/replace/close | screen instance/revision、input target、screen Provider availability | 旧槽位、控件、screen cursor |
| 仅 `baseFieldAcquisition` 或 available `UiFieldState.acquisition` 改变 | O1a `uiRevision`、`UiContextProjectionSnapshot.sourceRevision/observedAt` 与 subscriber notification | 不提升 `ui_context` 或下游 Provider 的公开 revision/metadata；不使 origin ref 失效 |
| input/overlay value 或 availability 改变 | O1a `uiRevision`；受影响公开字段的 Provider revision/metadata | 依赖旧公开 value/availability 的读取；unavailable 不得补默认值 |
| screen 内容变化但布局稳定 | 对应 Provider information revision | 依赖旧内容 revision 的 selector；不应无故替换 screen instance |
| screen title 改变但结构稳定 | `current_screen_information` Provider-wide revision；#54 `screenRevision` 通常不变 | 该 Provider 签发的全部旧 ref 按 origin revision 保守失效 |
| UI presentation mask/provenance 改变 | 受影响 viewport sample 重验；公开 visual/availability 变化时才提升 viewport revision | mask-only provenance 不污染公开 metadata；未证明区域不得继续发布 current visual |
| reduced-debug 设置变化 | F3 字段 availability、F3 information/source revision | 被阻止字段的后续读取；隐藏值变化不能推动公开 revision |
| grant/字段权限变化 | effective Catalog revision 与 authorization | 已不再获准的读取、ref 与 continuation |
| grant revoke/expire | authorization state | 该 grant 签发的全部 ref/cursor |

读取期间发生转换时，Runtime 必须丢弃结果并返回 `scope_changed`、`invalid_selector` 或 `invalid_page`，不能返回转换前的成功值。death/respawn 必须包含无 cursor、无 selector、connection/world/dimension 均未改变的普通 Read：Provider 先返回旧 snapshot，随后公开 revision 提升，PR #66 的后验必须拒绝它；provenance-only 内部变化且公开 revision 不变的对照 Read 仍应成功。

## 7. 非泄漏断言

### 7.1 Canary

测试在 O0 中放入显眼且唯一的 canary，例如隐藏 saturation、小数坐标、私有 NBT 键、未打开容器物品名和 operator-only 标识。对 Catalog、Help、Read、错误、Companion 可达 trace/debug、Journal、Context 和模型请求做递归扫描；任何命中都失败。Privileged operator/O0 artifact 可以合法保留裁判数据，验收重点是它们从 Companion/Controller/Model 不可达且上传前按 artifact 规则脱敏，不能对所有调试文件做无差别字符串禁令。

Canary 只证明已知路径，还必须配合 allowlist schema 和类型边界，不能依赖字符串黑名单作为主要防线。

玩家状态族还必须使用“输出不变”而非只靠字符串扫描验证 side channel：隐藏 saturation、同一显示桶内的 health/food 细值、效果剩余 tick、未渲染 NBT/components、reduced-debug 下坐标/biome/target canary 变化时，公开 values/unavailable、`informationRevision`、`source.sourceRevision`、`observedAt` 和 Companion trace 必须全部保持稳定。

### 7.2 静态依赖扫描

最终迁移门扩展架构测试，至少扫描 `src/companion/`、`src/models/`、`src/memory/`、`src/grounding/` 与 `src/information/`：

- 禁止认知链路导入 raw Minecraft contracts、Mineflayer、Prismarine、Vec3 或 Paper harness；
- 禁止继续消费 `DecisionContext.snapshot`、`availableSkills`、旧 model-facing skill schema；
- 禁止 Companion/Model 为认知用途调用 `backend.snapshot()`；
- 允许 Driver 内部使用 raw 类型，但跨 source port 时必须转换为自有 DTO。

### 7.3 旁路与元数据

错误 message、`evidenceIds`、opaque ref/cursor、source adapter revision、日志和 telemetry 也属于模型可能接触的攻击面。断言它们不包含 raw payload、精确坐标、NBT、文件路径、地址、token 或异常堆栈。

revision/时间戳本身也是侧信道。至少建立以下成对 fixture：

1. 保持 UiContext 基础 value 与九字段 value/availability 完全相同，只把 `baseFieldAcquisition` 从 `structured_ui_equivalent` 改为 `current_screen`；断言 O1a 发布新 snapshot、提升 `uiRevision/UiContextProjectionSnapshot.sourceRevision` 并更新内部 observedAt。
2. 对 available 的 `debugVisible`、`inputTarget` 等只改变 acquisition；断言同样更新 O1a，且 unavailable 分支仍不存在伪 value。
3. 对上述两例重复 `ui_context` Read，并读取受影响但公开结果未变的 #55/#56 Provider；断言 values/unavailable、`informationRevision`、`source.sourceRevision`、`observedAt`、canonical acquisition、evidence 与普通 trace 逐字节不变。
4. 随后真正改变 overlay value 或 availability；断言对应 O1b revision/public source metadata 原子提升，证明测试没有把所有更新都错误过滤。
5. 在 acquisition-only 更新恰好发生于 Read 中间时允许 Runtime 返回 `scope_changed`；重试结果仍满足第 3 条，不能把重试次数或内部 revision 写入模型可达错误/trace。

测试命名必须区分 `internalSourceRevision` 与外部 `source.sourceRevision`，禁止用无前缀的 `sourceRevision` 断言同时代表二者。

## 8. Paper 场景组织

在现有单并发 self-hosted Runner 上按接口族组织小场景，不建立一个包含所有状态的超长剧本：

1. `information-session-lifecycle`：重连、死亡/重生、维度和旧引用；
2. `information-ui-screens`：背包、箱子、工作台、熔炉、交易与快速切换；
3. `information-debug-hud`：F3/reduced debug、HUD、聊天、Tab；
4. `information-player-state`：status、hotbar、self inventory、tooltip、ref/cursor 与 revision side channel；
5. `information-viewport-optics`：前后方、墙、玻璃、树叶、水和未加载边界；
6. `information-sound`：方向、距离带、墙后、无位置声音和聚合；
7. `information-non-leakage`：canary 与 audience/旁路综合检查。

每个场景独立复制基准世界，拥有自己的 timeout 和 artifact 子目录。失败 artifact 至少包含阶段化 JSONL、sanitized Read trace、期望/实际差分和服务端日志；默认不上传模型 prompt、聊天全文或 raw Provider payload。

Paper 能证明协议事件、服务端设置、scope、availability 和失效顺序，不能单独证明 headless 客户端的 HUD/F3/tooltip 像素显示等价、逐字段 acquisition 或 Screen/overlay 的 viewport presentation mask。provenance/revision 分层由 deterministic fixture 验证；显示量化与 mask 由版本化 golden fixture 验证，并用相同布景下的原版客户端真人截图/录像对照。Paper 通过不能替代这些证据。

## 9. CI 与发布门

| 时机 | 必跑 |
|---|---|
| 每个 Provider PR | L0–L3、该 Provider fixture、架构边界、类型检查 |
| 合入主分支前的高风险改动 | 对应 Paper 场景；Runner 不可用时明确阻塞，不以 mock 代替 |
| v0.2 P4 | 全部 Paper 场景、forbidden-import/旁路扫描、真人可见性清单 |
| Minecraft 版本升级 | 字段/材质/界面差分、全部 projection fixture 和受影响 Paper 场景 |

不要求 PR CI 自动下载和启动 Paper。普通 CI 保持快速、确定且无密钥；Paper 工作流继续使用锁定 JAR、模板世界和专用 Runner。

## 10. 所有权与目录

```text
docs/testing/information-acceptance-matrix.md   # 本验收规则
src/information/testing/provider-contract.ts   # 所有 Provider 的共享契约
src/information/testing/leak-assertions.ts      # 递归 allowlist/canary 断言
src/information/testing/fixtures/               # 版本化 O0/O1 小 fixture
src/information/testing/matrix/                 # 机器可读字段/状态用例
src/integration/scenarios/information/          # Paper 场景
```

- #57 拥有共享断言、矩阵格式、旁路扫描和跨接口场景；
- 各功能 Issue 拥有本 Provider 的 fixture、字段用例和最小 Paper 场景；
- #58 在 P4 汇总运行并执行旧认知路径删除门；
- 测试工具不能反向成为生产 Provider 的依赖。

## 11. 功能 PR 审查清单

每个信息功能 PR 合并前必须回答：

1. 这个字段在原版客户端的哪个表面、哪个时刻可检查？
2. source port 是否已经移除 raw 类型与玩家不可见精度？
3. availability、precision、source kind、revision 所有者是否明确？可信内部 provenance revision 与 Provider 公开 value/availability revision 是否使用不同 comparator？
4. 哪些事件使值、ref、cursor 失效？是否有读取中变化测试？
5. 正常、partial、value-level `unknown`、`not_supported`、越权和超限是否都有用例，且没有把 `unknown` 发明成 availability reason？
6. O0 裁判怎样与生产链路隔离？禁漏 canary 是什么？
7. 是否通过共享 Provider contract、Runtime 安全回归和架构扫描？
8. 哪个 Paper 场景与真人步骤证明结构化结果等价于客户端体验？
9. acquisition/provenance-only 变化时，哪些内部 snapshot 必须更新、哪些公开 revision/sourceRevision/observedAt/trace 必须保持？
10. maximum-legal 完整 envelope 是否成功？每个非分页截断/分页提前结束是否在 schema 中可观察且可恢复，真正非法 oversize 才是否被 sanitized 拒绝？

任一问题没有具体答案，模块仍处于设计阶段，不能以“后续补测试”进入集成。

## 12. 实施切片

1. **M1：共享 Runtime 与断言**——先完成 #63 和 PR #66 的所有 Read 公开 revision 后验；再补强 Provider contract、maximum-legal/oversize 成对 fixture、JSON allowlist/canary 和跨目录架构扫描；
2. **M2：矩阵数据**——以 #67/#64/#65 最终契约建立 UI、status、inventory、Screen/overlay 与双层 revision/provenance 用例；
3. **M3：Paper 信息场景**——在现有 runner 上加入 lifecycle/UI 场景和 artifact 差分；
4. **M4：感知矩阵**——随 #34/#59 加入 scope-bound optical token、旧 block event、世界光学、UI presentation mask、声音、TTL 与预算用例；
5. **M5：P4 迁移门**——执行全矩阵、旁路扫描、真人清单并证明旧 snapshot/skill 输入已删除。

## 13. 暂不冻结的参数

FOV、声音距离带、聚合窗口、projection TTL、分页默认值和真人验收设备组合由各模块设计提出，经 fixture/Paper 校准后版本化。本矩阵冻结它们必须被显式记录和测试，不在缺乏实测证据时预先冻结具体数值。
