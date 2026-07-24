---
status: accepted
authority: normative
implementation: diverged
last_verified: 2026-07-23
scope: roadmap
---

# v0.3：可信具身闭环

> 2026-07-14 因 v0.1 真人联调形成的接受路线图；[milestone](https://github.com/spojchil/mineintent/milestone/2)和追踪 Issue [#41](https://github.com/spojchil/mineintent/issues/41)仍 open。最新分支已经实验性实现 P1 的一部分，但 ADR 0005 仍 proposed，模型—身体接口也被重新打开；“in progress”不等于路线已经稳定。上游：[产品设计](../product-design.md)、[目标系统](../architecture/target-system.md)、[ADR 0005](../decisions/0005-limit-mineflayer-to-protocol-driver.md)。

## 1. 阶段目标

v0.3 不扩展通关、建造或战斗能力。它建立在 v0.2 的合法信息目录、Help/Read、UI Context 和第一人称信息边界上，证明同伴的语言、Grounding、身体控制和结果验证共享同一套事实：同伴只能声称自己合理得知、亲自做过且经过验证的事情，普通规划不能利用角色尚未感知或探索的完整区块地图。

本计划原为 v0.2；2026-07-14 因发现“合法获取信息”是 Grounding 和控制的缺失前置而顺延。第一人称视口、F3/HUD/GUI、状态和字段可发现性由 [v0.2 合法信息与界面接口](./roadmap-v0.2-legal-information-interfaces.md) 先行交付。本计划不再采用逐 tick `BodyInputPlan` 作为既定控制方案，连续控制器在本里程碑重新权衡。

完成后，以下链路必须可信：

```text
玩家表达
→ 当前身体视角与已知空间消解目标
→ 注视和移动取得可交互准星
→ 发出协议动作
→ 区分客户端预测与服务端反馈
→ 验证方块、掉落物和背包结果
→ 基于证据自然回应并形成记忆
```

## 2. v0.1 联调事实

| 现象 | 已确认原因 | 影响 |
|---|---|---|
| “看向我”只回复，头部不转 | 语义意图没有进入 Grounding、行为合成和身体控制；`attention` 不驱动身体 | 语言与身体分离 |
| 玩家在背后仍回答“看得见” | 模型只收到 `trackedPlayers`，没有自身朝向、FOV 或遮挡 | 视觉事实幻觉 |
| 六条 `block_dug` 中只有两块被服务端记账 | `bot.dig` 乐观更新本地世界，应用把 Promise 完成当作成功 | 动作证据失真 |
| 首次真实破坏两块但拾取为零 | 没有掉落物追踪与拾取闭环 | 破坏与采集混淆 |
| “面前/右边的树”可能切换到其他树 | v0.1 对象专用能力只有数量和半径，没有语义 Grounding | 空间指代丢失 |
| 树叶后的原木被直接尝试 | 距离检查没有准星射线和可见方块面 | 交互不符合客户端过程 |
| 半透明方块行为不一致 | `transparentHint` 未消费；光学、交互、碰撞和导航共用形状 | 视觉与行动策略混淆 |
| 能直接找到隐藏树间路线或迷宫出口 | Pathfinder 读取完整 `bot.world` 执行 A* | 普通规划具有上帝视角 |
| 无法从日志复原路径和服务端回滚 | 缺少候选、射线、路径、预测、服务端反馈和拾取阶段事件 | 调试不可证伪 |
| DeepSeek 首轮 JSON 层级无效 | 提示只给协议名，没有合法结构示例或一次修复 | Provider 可靠性不足 |

第二次采木由服务端确认累计破坏从 2 增至 6、拾取从 0 增至 4，证明底层能力可以工作，但成功依赖场景，且当前日志无法可靠解释成功与失败差异。

## 3. 范围

### 本阶段负责

- Mineflayer 协议驱动、Safety Control、Perception、Epistemic Map 和 Motor Controller 的边界；
- 可复原的动作证据链和仅用于测试的 Paper 服务端裁判；
- 消费 v0.2 第一人称 FOV、实体可见性和光学材质信息完成空间 Grounding；
- 从自然语言指代到证据 Grounding、通用行为合成和视觉共同注意的闭环；
- 以准星、服务端方块变化、掉落物和背包为证据的采木闭环；
- 受认知地图约束的普通寻路与未知区域探索；
- 语言事实门控、部分成功和失败报告；
- 固定场景的单元、Paper 集成与真人观察验收。

### 非目标

- 完整像素渲染、截图视觉模型或 OCR；
- 完整通关、建造、成熟战斗和任意工具链；
- 为任务成功保留全知路径作为普通回退；
- 依赖生产服务器 OP、RCON、插件或文件访问；
- 在本阶段替换全部 Mineflayer 协议实现。

## 4. 实施顺序

当前实现进度（2026-07-23 核对）：P0 所需的事件、取消、调试和 Paper 验收骨架已建立；P1 的有源相对坐标、opaque ref、Grounding、有限扫描、渐进注视与结果阶段可见性复查有自动测试和确定性 Paper 场景代码，但复查尚未强制 perception revision 前进。最新实验提交没有关联 Actions/Paper run，场景又只覆盖 `message_referent` 而非 `context_ref`，所以这里不再把“场景已编写”写成“该提交已通过真实 Paper 验收”。P2 及以后尚未接入生产 Behavior 链路；P1 仍需 freshness guard、context-ref 场景和真人观察验收。

### P0：事实与可观测性

1. 定义 `commanded → client_predicted → server_observed → outcome_verified` 证据状态。
2. 记录 embodied intent、Grounding、controller plan/phase、目标候选、射线、路径请求、重规划、协议动作和结果验证。
3. 建立只在本地和 CI 启用的 Paper 测试裁判；测试先以 `scenario_run_id + action_id + player_uuid + target` 注册，再核对服务端方块和物品事件，禁止仅靠时间窗口猜测关联。
4. 在固定低树和叶片遮挡场景重现真假挖掘，先让失败可复原。
5. 从 P0 同步建设 #40 的 fixture、artifact 与清理骨架，验收矩阵不是最后才开始的汇总任务。

P0 完成前不增加新的复杂动作。

### P1：第一人称感知与注视

1. 消费 v0.2 的 Information Read、`PerceptionViewport`、FOV、遮挡和材质证据，不重新读取 raw tracker。
2. 模型输出“希望建立/改变视觉注意”的具身意图和语义指代；Grounding 只用合法 Read、Cognitive Observation、话语角色和有来源记忆生成有时效句柄。
3. 为已 Grounding 的目标设计可取消连续注视 controller；允许内部使用局部精确空间解，但不能搜索目标或把精确坐标回流给 Companion。
4. 使用同一意图路径处理静态点、移动对象与可交互表面；持续注意是意图生命周期，不等于为每个对象类别增加一个主模型命令。
5. 由新 viewport observation 验证是否真正看见，controller 完成转向不能直接生成“看见”。

### P2：挖掘、拾取与目标树闭环

1. 将“面前/右边/这棵树”消解为带 observation evidence 的目标引用。
2. 候选原木必须有合法可见/交互证据；controller 执行前重新验证目标、距离、遮挡和当前可交互性，不得静默换到其他 raw-world 方块。
3. `bot.dig` 可作为 scoped controller 的实现候选，但其本地乐观空气和 Promise 不产生领域成功事件。
4. 跟踪正式客户端可观察的服务端方块变化、掉落物、移动拾取和背包增量；用动作资源锁、目标物品类型和有界因果窗口归因，发生无关背包变化时保留 `unknown/unattributed`。
5. 将树建模为当前可见和交互发现的结构，优先低处原木，有限处理叶片、换站位和失败目标。
6. 支持零成功、部分成功、目标切换与拾取失败的诚实终态。

Paper 裁判只在测试中断言上述正式判定是否正确，不参与 `outcome_verified` 计算。

### P3：认知地图与真人式普通寻路

1. 建立当前观察、探索历史、记忆和未知四种空间状态。
2. 普通 Intentional Planner 只使用 Epistemic Map；原始区块只对 Controller 即将执行的局部控制做硬碰撞、跌落与协议合法性校验，未感知 raw entity 不得驱动定向躲避。
3. 禁止未探索迷宫的全图路线；提供观察、试探岔路和回退行为。
4. 已走过路线可以形成带世界、维度、时间和置信度的空间记忆。
5. 世界方块变化、维度/世界变化、路线碰撞和记忆过期必须使相应路线失效或降级，不能无限信任旧路线。
6. Safety 查询不得批量探测、参与 A* 展开或路线排序，拒绝结果不得写入认知地图；为日常行为关闭无必要跑酷，限制绕路长度，并记录路径来源与已知性。

### P4：语言事实门控与阶段验收

1. 由 [#43](https://github.com/spojchil/mineintent/issues/43) 统一拥有事实门控：“看见、到达、破坏、拾取、完成”分别绑定视觉、位置、服务端方块、背包和动作终态证据。
2. 动作开始前只允许协调语言；结果语言在验证后生成。
3. Model Provider 按 provider capability 配置 structured output/JSON mode，支持严格 schema 和至多一次有超时、取消与 token/cost 上限的结构修复，不把修复失败伪装成理解成功。
4. 运行全部自动测试和真人观察清单，关闭阻断缺陷后再扩展新的身体可供性或行为模式。

## 5. Issue 拆分

| 顺序 | 工作项 | 依赖 |
|---:|---|---|
| 1 | [#32 收紧 Mineflayer 协议驱动与具身控制边界](https://github.com/spojchil/mineintent/issues/32) | 无 |
| 2 | [#33 建立动作证据链与 Paper 服务端测试裁判](https://github.com/spojchil/mineintent/issues/33)：[#44 证据协议](https://github.com/spojchil/mineintent/issues/44)、[#45 测试裁判](https://github.com/spojchil/mineintent/issues/45) | 1 |
| 前置 | v0.2 信息接口；[#34 第一人称视觉](https://github.com/spojchil/mineintent/issues/34)、[#46 视口与实体](https://github.com/spojchil/mineintent/issues/46)、[#47 光学与上下文](https://github.com/spojchil/mineintent/issues/47) | v0.2 里程碑 |
| 3 | [#52 建立具身意图 Grounding 与行为合成边界](https://github.com/spojchil/mineintent/issues/52) | v0.2、1、2 |
| 4 | [#35 实现视觉共同注意与身体反馈闭环](https://github.com/spojchil/mineintent/issues/35) | v0.2、3 |
| 5 | [#36 重构挖掘、方块确认与拾取闭环](https://github.com/spojchil/mineintent/issues/36) | 2、3、4 |
| 6 | [#37 实现空间指代、目标树与结构化采木](https://github.com/spojchil/mineintent/issues/37)：[#48 空间 Grounding](https://github.com/spojchil/mineintent/issues/48)、[#49 目标树策略](https://github.com/spojchil/mineintent/issues/49) | 3、5 |
| 7 | [#38 引入 Epistemic Map 并限制全知寻路](https://github.com/spojchil/mineintent/issues/38)：[#50 地图生命周期](https://github.com/spojchil/mineintent/issues/50)、[#51 规划与 Safety](https://github.com/spojchil/mineintent/issues/51) | v0.2、1、3 |
| 8 | [#39 提高 OpenAI-compatible 结构化决策可靠性](https://github.com/spojchil/mineintent/issues/39) | 2，可并行 |
| 9 | [#43 建立语言与状态的事实证据门控](https://github.com/spojchil/mineintent/issues/43) | 2–8 的领域证据；P0 先建接口，P4 收口 |
| 10 | [#40 建立可信具身 Paper 与真人观察验收矩阵](https://github.com/spojchil/mineintent/issues/40) | P0 开始、贯穿 2–9、最后关闭 |

每个实现 Issue 必须同时交付事件、调试状态和自动测试，不能把可观察性留到最后补。

ADR 0005 保持 `proposed`，直到 #32 的模块依赖矩阵、Safety 非泄漏契约和生产/测试证据边界经评审接受。P0 诊断与测试骨架可以先行，P1 及以后不得在 ADR 未接受时固化新边界。

## 6. 阶段完成定义

1. 有合法方向证据时，“看向我”的具身意图产生自然头部/身体行为，随后由新视觉观察确认是否看见。
2. 身份已知但没有方向证据时，Grounding 以 `partial` 继续；Behavior Synthesizer 不使用 tracked entity 精确坐标直接转准，只合成扫描、等待或利用有证据候选区域的信息获取行为；无可行计划时返回 `information_needed`，由 Companion 决定是否询问，结果可以保持 unknown。
3. 玻璃、树叶、水、植物和实心方块在视觉、交互、碰撞与导航中有分别测试的策略。
4. 叶片遮挡的原木不产生假 `block_break_confirmed`；服务端拒绝会被明确记录。
5. “砍面前这棵树”不会静默切换到其他树；若必须换目标，先说明并等待适当协调。
6. 破坏、掉落和拾取分别验证；背包未增加时不能报告“收集到了”。
7. 首次进入固定迷宫时需要探索；完成探索后重新进入可使用已形成的路线记忆。
8. 任一测试具身行为可以从玩家消息追到 Information Read、semantic referent、Grounding、controller plan/phase、Motor feedback、结果验证、显式关联的服务端裁判和最终语言；生产链路在没有裁判时仍能独立验证结果。
9. 每个阻断级 Paper 场景本地连续运行 10 次全部通过；自托管完整工作流连续 3 次通过；单场景 90 秒内终止，整轮结束 90 秒内清理 Bot、服务器进程和运行世界，且无跨轮状态串扰。
10. 发布前更新版本、README/运行说明与 changelog；release/tag 检查确认生产配置无法启用测试裁判。当前原型事件名、配置和代码没有兼容迁移要求。

## 7. 实施保护

- 聊天、活动、记忆、动作资源锁、取消和 Backend 生命周期按当前契约重新实现；可以复用经审查的算法，不复用旧公共类型或兼容路径。
- 不以更强模型替代感知、动作验证或知识边界。
- 不把模型面对的身体可供性退化为 skill、输入模板或协议事务命令目录，也不为玩家、方块、树等对象类型或“移除对象”等规划结果注册身体动作。
- 不通过增加寻路超时掩盖错误目标和未知地图。
- 不把 Paper 测试裁判输出注入生产同伴认知。
- 不用 Safety Control 的拒绝结果补全认知地图，也不开放批量安全探针。
- ADR 0005 未接受前，除 P0 诊断外不扩展依赖 Mineflayer 高层自动化的新能力。
