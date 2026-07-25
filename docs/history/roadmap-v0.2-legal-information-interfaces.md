---
status: accepted
authority: normative
implementation: stalled
last_verified: 2026-07-23
scope: roadmap
---

# v0.2：合法信息与界面接口

> 2026-07-14 接受的阶段路线图；[milestone](https://github.com/spojchil/mineintent/milestone/3)和追踪 Issue [#58](https://github.com/spojchil/mineintent/issues/58)仍 open。五个下游设计 PR #64/#65/#66/#68/#69 已关闭归档，最新代码采用了更小的被动读取切片，因此本计划当前 **stalled / drifted**。上游：[产品设计](../product-design.md)、[目标系统](../architecture/target-system.md)、[合法信息](../architecture/information-access-and-ui.md)、[Information Runtime](../architecture/information-runtime.md)。

## 1. 阶段目标

v0.2 先回答“同伴现在可以合法知道什么、如何发现和读取”。它不交付复杂 Grounding、挖掘、寻路或任务控制；这些顺延至 v0.3。

完成后，模型可以：

```text
Information Catalog
→ 查看有哪些信息接口
→ 对目标接口调用 Help
→ 得到字段、精度和当前可用条件
→ 只读取需要的字段
→ 依据 revision、来源和 unavailable 原因诚实使用结果
```

## 2. 范围

### 本阶段负责

- Information Catalog、接口注册表和 schema revision；
- 统一 `InformationRuntime` 的 Catalog/Help/Read 协议，由 Context Composer 组合有界被动观察，当前不注册 model-facing Information 工具；
- 强类型 Provider、最小只读 source port、Registry、Access Policy 和有界 Read 预算；
- Runtime 签发并校验的 selector/cursor、分页/大小上限、结构化 source 与请求错误；
- Companion、controller、operator audience 隔离；模型不能自行提权；
- `ui_context`：world/screen 主表面、独立 input target、screen instance/revision 和 overlays；
- 当前状态、快捷栏、自身背包、item tooltip；
- F3、准星、HUD、聊天、Tab、当前 screen、成就和配方书；
- 第一人称 viewport、声音和生命周期接入同一信息目录；
- 显示精度、`reducedDebugInfo`、未打开容器和 raw-world 非泄漏测试；
- Context 只从按需 Information Read 取认知信息，不保留完整 Backend snapshot 兼容入口。

### 非目标

- 固定 Behavior Plan 或连续控制器的最终抽象；
- 逐 tick 模拟全部键鼠；
- Grounding、长期 Epistemic Navigation 和 Safety 路线策略；
- 挖掘、放置、战斗、采木和共同注意闭环；
- 把 Help 变成包含当前世界值的第二观察接口；
- 完整像素渲染、OCR 或所有客户端设置界面自动化。
- 为当前原型保留 V1/V2 双路径、snapshot adapter、旧 skill 目录或旧测试兼容层。

## 3. UI 状态决定

UI 状态单独建模：

- 主界面在 `world` 与一个 `screen` session 之间互斥；
- 断线/过渡期允许 `mainSurface: none`，并与 lifecycle connection state 对齐；
- 输入归属独立为 world、当前 screen、聊天或 none，不能只从主界面推断；
- HUD、F3、聊天、Tab、boss bar、字幕属于可叠加 overlays；
- `ui_context` 只描述状态，不返回 screen 内容；
- `current_screen_information` 使用 `screenInstanceId + screenRevision` 读取可见槽位、控件、文本和进度；
- screen 关闭、切换、布局变化、死亡或重连使旧 selector 失效；普通内容同步只提升信息 revision；
- 多人菜单不默认暂停世界。

## 4. 实施顺序

### P0：字段注册与发现

1. 以 `InformationRuntime` 作为唯一认知入口，建立 Registry、Access Policy、Ref/Cursor Store 与有界 Read plan。
2. 定义 Catalog、Help、Read、字段帮助、结构化 source、部分成功和请求错误 envelope。
3. 建立强类型 Provider 与最小只读 source port；Runtime 统一验证 schema、scope、audience、输出和限额。
4. 建立稳定字段 ID、版本差分、未知字段拒绝和 runtime-signed selector/cursor。
5. Context Composer 的 Read plan 在实现时固定 schema revision 和字段集；schema 变化时必须显式更新，读取和分页具有字段数、字节数与时间上限。

### P1：UI Context 与自身状态

1. 实现 connection state、none/world/screen main surface、screen family/instance/revision、input target 和 overlays。
2. 以 current status 为首个纵向切片，再实现 hotbar、inventory 和 lifecycle；不复用旧 snapshot 契约。
3. 区分显示值与内部值；隐藏 saturation、效果 tick 和不可见精度不暴露。

### P2：F3、HUD 与 Screen

1. 实现 F3 world fields，并遵守 `reducedDebugInfo`。
2. 实现 crosshair、HUD、chat、player list 和 notifications。
3. 以通用 visible slots/properties/controls/text 归一化当前 screen。
4. 覆盖 tooltip、成就和配方书的按需检查。

### P3：第一人称外界信息

1. 将实体、方块、声音、可见文本和生命周期加入 Catalog/Help/Read。
2. 继续执行 FOV、遮挡、材质、声音降精度和 unknown 边界。
3. 禁止 raw tracked entity、loaded blocks 和声音包坐标进入普通 Read。

### P4：模型接入与验收

1. Context 只附带主动读取或 Attention 选择的结果。
2. 测试 Catalog → Help → Read 的正常和错误恢复流程。
3. 测试 screen revision、重连、维度切换、权限变化和不支持字段。
4. #58 负责最终集成切换：删除 `DecisionContext.snapshot`、`availableSkills`、旧 model-facing skill schema，以及 Companion/Model 对 `backend.snapshot()` 的认知读取，不保留双轨兼容。
5. #57 增加自动化 forbidden-import/旁路扫描并完成验收，证明生产链路不通过 Backend snapshot、Safety 或 Paper 裁判获取信息。

## 5. 工作项

| 顺序 | 工作项 | 依赖 |
|---:|---|---|
| 1 | [#58 追踪 v0.2 合法信息与界面接口](https://github.com/spojchil/mineintent/issues/58) | 无；其余工作项使用原生 Sub-issues 管理 |
| 2 | [#53 实现 Information Runtime 并行公共底座](https://github.com/spojchil/mineintent/issues/53) | 无 |
| 3 | [#54 实现 UI Context 与界面会话状态](https://github.com/spojchil/mineintent/issues/54) | #53 |
| 4 | [#55 实现状态、快捷栏、背包与 F3 信息接口](https://github.com/spojchil/mineintent/issues/55) | #53、#54 |
| 5 | [#56 实现 HUD、聊天、Tab 与当前 Screen 信息接口](https://github.com/spojchil/mineintent/issues/56) | #53、#54 |
| 6 | [#34 第一人称视觉、光学遮挡与认知观察](https://github.com/spojchil/mineintent/issues/34) | #53；由原生 Sub-issues #46/#47 拆分 |
| 7 | [#59 实现声音与生命周期信息接口](https://github.com/spojchil/mineintent/issues/59) | #53，可与 #34 并行 |
| 8 | [#57 建立信息精度、可用性与非泄漏验收矩阵](https://github.com/spojchil/mineintent/issues/57) | #53–#56、#34、#59；P0 开始 |

## 6. 完成定义

1. Catalog 能按 runtime audience 列出所有当前支持接口及 schema revision，模型不能自行提权。
2. 每个接口的 Help 能列字段、类型、单位、显示精度、来源、要求和当前不可用原因。
3. Read 只返回请求字段，并带 information revision、epoch、世界作用域、结构化 source 和证据；请求级错误与字段 unavailable 分开。
4. `ui_context` 正确表达断线/过渡、world/screen、独立输入归属、当前 screen 和 overlays。
5. 状态、快捷栏、背包、F3、HUD、聊天、Tab、current screen、视觉、声音和生命周期均可发现。
6. 隐藏 saturation、未打开外部容器、reduced-debug 字段、raw entity/world 和测试裁判无法泄漏。
7. 模型能先 Help，再读取生命/饥饿/效果、F3 和当前 screen；未知字段会刷新 Help 而非猜测。
8. Context/Companion 不再直接消费完整 `MinecraftSnapshotV1` 或 `ProtocolObservationSource`；旧 `DecisionContext.snapshot`、`availableSkills` 和 model-facing skill schema 已删除而非适配。
9. 版本、screen、连接或维度改变后，旧 selector、cursor 和旧值确定性失效；模型不能用坐标、协议 ID 或修改 opaque ref 构造查询。
10. v0.3 可以只基于合法 Read、Cognitive Observation、Grounding 和 scoped controller view 设计控制层。
