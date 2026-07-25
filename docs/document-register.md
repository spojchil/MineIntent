---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-23
---

# 文档登记表

本表回答三件事：`docs/` 下除本登记表自身外的每份文档现在放在哪里、应以什么身份阅读、原始材料从哪里来。状态的正式含义见[文档治理规则](./documentation-policy.md)；表中“来源”只提供可追溯性，不表示该材料已经被接受。

## 根目录

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [文档入口](./README.md) | reference / informative | not-applicable | 五分区布局与真相优先级 |
| [产品设计](./product-design.md) | accepted / normative | not-applicable | 原根目录 `PRODUCT_DESIGN.md`；`078f525` 创建，PR #71 / `4718d85` 扩展长期可信目标 |
| [当前项目状态](./current-status.md) | reference / informative | current | 按代码、PR、Issue 和测试核验 |
| [文档治理规则](./documentation-policy.md) | accepted / normative | not-applicable | 经 PR #72 接受；硬规则只有“`main` 只通过合并 PR 变更” |

## 架构（系统现在如何运行？目标架构是什么？）

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [架构入口](./architecture/README.md) | reference / informative | not-applicable | 分区导航 |
| [当前系统实况](./architecture/current-system.md) | reference / informative | current | 从 `57d438e` 代码反推 |
| [目标系统设计](./architecture/target-system.md) | accepted / normative | diverged | 原根目录 `SYSTEM_DESIGN.md`；`078f525` 创建，后经 PR #20/#21/#22/#23/#25/#26/#42 修订 |
| [同伴运行时](./architecture/companion-runtime.md) | accepted / normative | diverged | 原 `docs/design/companion-runtime.md`；PR #21 / `fc1bc3e` |
| [领域事件与事件日志](./architecture/domain-events-and-journal.md) | accepted / normative | partial | 原同名 design；PR #20 / `40f0263` 创建，PR #21/#23/#25 修订 |
| [决策协议与上下文](./architecture/decision-contract-and-context.md) | accepted / normative | partial | 原同名 design；PR #22 / `d7b564a` 创建，PR #42 / `e18f90f` 修订；PR #62 是实现证据 |
| [Information Runtime](./architecture/information-runtime.md) | accepted / normative | partial | 原同名 design；PR #42 / `e18f90f` 定方向，PR #62 / `b13aa58` 接受并实现核心 |
| [合法信息与 UI](./architecture/information-access-and-ui.md) | accepted / normative | partial | 原同名 design；PR #42 / `e18f90f`，后经 PR #62/#67 与实验 `f25c6e4` |
| [认知感知模型](./architecture/cognitive-perception.md) | accepted / normative | partial | 原同名 design；PR #25 / `e053f47`，后经 PR #26/#42 与实验 `e9616c1` |
| [记忆模型与档案版本](./architecture/memory-model-and-profile-versioning.md) | accepted / normative | partial | 原同名 design；PR #23 / `a002f47`，PR #42 修订；当前仅最小文件原型 |
| [UI Context](./architecture/ui-context.md) | accepted / normative | planned | 原同名 design；PR #67 / `2bb710d` 接受，尚无生产实现 |
| [Minecraft Backend](./architecture/minecraft-backend.md) | accepted / normative | diverged | 原同名 design；PR #26 / `e7bba0a`；v0.1 基线后按协议驱动缩减 |

## 架构决策（哪项长期决定被正式接受？）

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [ADR 入口](./decisions/README.md) | reference / informative | not-applicable | 原 `docs/adr/README.md`（`078f525`）；PR #72 新增决策/实现双状态表 |
| [ADR 模板](./decisions/template.md) | reference / informative | not-applicable | 原 `docs/adr/template.md`（`078f525`）；PR #72 新增实现漂移字段 |
| [ADR 0001：首版使用 Mineflayer](./decisions/0001-use-mineflayer-as-initial-backend.md) | accepted / normative | current | 原 `docs/adr/0001-*`；`078f525` 已明确 accepted |
| [ADR 0002：事件驱动持续同伴](./decisions/0002-event-driven-companion-runtime.md) | accepted / normative | partial | 原 `docs/adr/0002-*`；`078f525` 已明确 accepted |
| [ADR 0003：分离 Mind 与 Action Runtime](./decisions/0003-separate-mind-and-action-runtime.md) | accepted / normative | diverged | 原 `docs/adr/0003-*`；`078f525` accepted，Action Runtime 于 `29052bf` 删除 |
| [ADR 0004：不执行模型任意代码](./decisions/0004-no-arbitrary-model-code.md) | accepted / normative | current | 原 `docs/adr/0004-*`；`078f525` 已明确 accepted |
| [ADR 0005：Mineflayer 只作协议驱动](./decisions/0005-limit-mineflayer-to-protocol-driver.md) | proposed / informative | partial | 原 `docs/adr/0005-*`；PR #42 / `e18f90f`，原文即 proposed；方向有实验代码但未接受 |

## 提案与实验（现在还在争什么？）

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [提案入口](./proposals/README.md) | reference / informative | not-applicable | 分区导航；含“怎样接受一个决定” |
| [具身架构反思](./proposals/embodiment-architecture-reflection.md) | proposed / informative | not-applicable | 直接提交 `e50c8f9`，无 PR；最新反思，不是结论 |
| [具身决策登记册](./proposals/embodiment-decision-register.md) | proposed / informative | not-applicable | 直接提交 `57d438e`，无 PR；F01–F20 / B01–B09 / D01–D40 |
| [具身接口清单](./proposals/embodiment-interface-inventory.md) | historical / informative | retired | PR #42 / `e18f90f`；事实审计仍有价值，控制契约已撤回；与具身讨论同处一区以保持阅读单元完整 |
| [信息接口提案](./proposals/information-interfaces.md) | proposed / informative | not-applicable | 从五条已关闭未合并 PR 提炼的耐久原则与待决项，不是实现契约 |
| [可信注视实验](./proposals/trustworthy-gaze.md) | experimental / informative | current | 对 `codex/trustworthy-passive-context@57d438e` 的能力边界说明 |

## 指南与接口（怎么运行？接口什么形状？）

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [指南入口](./guides/README.md) | reference / informative | current | 分区导航 |
| [首个同伴原型](./guides/companion-prototype.md) | reference / informative | current | 原 `docs/testing/companion-prototype.md`；PR #31 / `ff6cd0c`，后经 `0927db4` |
| [Paper 集成](./guides/paper-integration.md) | reference / informative | partial | 原 `docs/testing/paper-integration.md`；PR #30/#31（`f6bd425`/`ff6cd0c`），后经 `0927db4`；最新分支尚无 run |
| [模型接口参考](./guides/model-interface.md) | reference / informative | current | 原根目录 `MODEL_INTERFACE_REFERENCE.md`；直接提交 `9eb3a16`，无 PR；TypeScript schema 优先 |

## 来路（为什么走到今天？）

| 文档 | 状态 / 权威 | 实现 | 来源与用途 |
|---|---|---|---|
| [来路入口](./history/README.md) | reference / informative | not-applicable | 分区导航；吸收原 `archive/README.md` |
| [项目演进史](./history/project-evolution.md) | historical / informative | not-applicable | 结合提交、PR、Issue 撰写 |
| [v0.2 合法信息接口](./history/roadmap-v0.2-legal-information-interfaces.md) | accepted / normative | stalled | 原 `docs/plans/` 同名；PR #42 / `e18f90f`，PR #62 / `b13aa58` 与实验 `f25c6e4`；五个设计 PR 后被关闭 |
| [v0.3 可信具身](./history/roadmap-v0.3-trustworthy-embodiment.md) | accepted / normative | diverged | 原 `docs/plans/v0.3-trustworthy-embodiment.md`（PR #42）；实验越过 ADR 接受门和原 tracker 顺序 |
| [认知感知研究](./history/research-cognitive-perception.md) | reference / informative | not-applicable | 原 `research/COGNITIVE_PERCEPTION_RESEARCH.md`；PR #25 / `e053f47` |
| [系统设计研究](./history/research-system-design.md) | reference / informative | not-applicable | 原 `research/SYSTEM_DESIGN_RESEARCH.md`；直接提交 `078f525` |
| [早期自治 Agent 交接稿](./history/early-autonomous-agent-handoff.md) | historical / informative | retired | 原根目录 `MINEINTENT_HANDOFF.md`；初始提交 `2ee231c` 已含三时间尺度、取消与结果验证思想 |
| [2026-07-14 信息设计档案入口](./history/archive-2026-07-14-information/README.md) | historical / informative | retired | 从五条已关闭、未合并 PR 分支恢复 |
| [玩家状态信息](./history/archive-2026-07-14-information/player-state-information.md) | historical / informative | retired | `a213690`，PR #64 |
| [屏幕与 Overlay 信息](./history/archive-2026-07-14-information/screen-and-overlay-information.md) | historical / informative | retired | `079de7b`，PR #65 |
| [声音与生命周期信息](./history/archive-2026-07-14-information/sound-and-lifecycle-information.md) | historical / informative | retired | `c31a814`，PR #66 |
| [第一人称视口信息](./history/archive-2026-07-14-information/viewport-information.md) | historical / informative | retired | `ccae567`，PR #68 |
| [信息验收矩阵](./history/archive-2026-07-14-information/information-acceptance-matrix.md) | historical / informative | retired | `7b89da6 → a23823b → f93cf7e`，PR #69；保留 O0/O1a/O1b/O2 oracle 隔离原则 |

## 维护方法

- 新增、移动或改变身份时，同一个 PR 同步更新本表。
- 表中的状态应与文件 front matter 完全一致；`pnpm check:docs` 会检查元数据、相对链接和登记完整性。
- Git 提交或 PR 只证明材料的来源；是否 accepted 仍看明确决策证据——[合并的 PR](./documentation-policy.md)。
- 历史材料可以保留原始语气，但入口和 front matter 必须阻止它冒充当前事实。
