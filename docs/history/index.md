# MineIntent 历史来源与旧路径

> 本页用于寻找思想和旧文件，不裁决产品，也不描述当前实现。产品文字见[《产品》](../产品.md)，当前实现见
> [绑定版本的架构说明](../architecture.md)。

## 使用规则

- 早期记录对恢复思想来源有价值，但“更早”“已合并”或“被反复引用”都不自动赋予当前权威。
- 来源能否公开复核，与它是否拥有产品权威是两个问题。
- 下列摘要只帮助定位；需要判断原文时，直接阅读不可变 commit、Issue 或 PR。
- 旧正文固定在不可变快照中，不复制回活文档，也不因进入本索引而恢复原有状态。

## 产品方向恢复的直接来源

| 来源 | 记录了什么 | 在本索引中的用途 |
|---|---|---|
| 维护者当前澄清（2026-07-29，未复制私人会话原文） | 初始方向发生过变化；产品不替 AI 选择具体社会与行动结果 | 当前 `产品.md` 候选文字的直接输入，仍以逐条确认状态为准 |
| [`2ee231c`](https://github.com/spojchil/MineIntent/commit/2ee231c) | 最早 handoff 以一次长期目标和自主交付结果为中心 | 技术前身 |
| [`078f525`](https://github.com/spojchil/MineIntent/commit/078f525) | 首次完整表述长期同伴、共同经历、陪伴、可信参与和持续关系 | 初始问题框架的历史证据 |
| [Issue #108](https://github.com/spojchil/MineIntent/issues/108) | 追踪“安静真人”从候选路线和验收代理升格为产品目标的偏离与更正 | 产品层级混淆的审计记录 |
| [Issue #109](https://github.com/spojchil/MineIntent/issues/109) | 展开无主要玩家、自主关注、文本记忆和单多人统一方案 | 当前候选文字与实现迁移的来源之一 |

## 起点与早期实现

| 来源 | 记录了什么 | 当前查阅方式 |
|---|---|---|
| [Issue #12](https://github.com/spojchil/MineIntent/issues/12) / [PR #20](https://github.com/spojchil/MineIntent/pull/20) | 领域事件与事件日志的早期设计 | 作为历史设计阅读；当前接口看代码 |
| [Issue #13](https://github.com/spojchil/MineIntent/issues/13) / [PR #21](https://github.com/spojchil/MineIntent/pull/21) | 持续同伴、共同活动、注意模型和第一版主要玩家 | 用于追踪主要玩家假设的来源 |
| [Issue #10](https://github.com/spojchil/MineIntent/issues/10) / [PR #22](https://github.com/spojchil/MineIntent/pull/22) | 模型提议、世界结果优先和早期决策协议 | 设计来源；当前协议看代码 |
| [Issue #4](https://github.com/spojchil/MineIntent/issues/4) / [PR #23](https://github.com/spojchil/MineIntent/pull/23) | 长期记忆、档案版本和结构化证据方案 | 旧记忆方案来源 |
| [Issue #24](https://github.com/spojchil/MineIntent/issues/24) / [PR #25](https://github.com/spojchil/MineIntent/pull/25) | “客户端收到”与“AI 知道”的感知边界 | 感知问题来源 |
| [PR #26](https://github.com/spojchil/MineIntent/pull/26) / [PR #27](https://github.com/spojchil/MineIntent/pull/27) | Mineflayer Backend 生命周期和首个真实 Paper 验证 | 当前 Backend 的历史来源 |
| [PR #28](https://github.com/spojchil/MineIntent/pull/28) / [PR #29](https://github.com/spojchil/MineIntent/pull/29) / [PR #30](https://github.com/spojchil/MineIntent/pull/30) | 聊天、可取消执行和 Paper 测试的早期闭环 | 工程机制来源 |
| [Issue #17](https://github.com/spojchil/MineIntent/issues/17) / [PR #31](https://github.com/spojchil/MineIntent/pull/31) | 采木、打断、真实结果和重启记忆组成的早期纵向切片 | 原型历史 |

## 后续演变

| 来源 | 记录了什么 | 当前查阅方式 |
|---|---|---|
| [PR #42](https://github.com/spojchil/MineIntent/pull/42) / [PR #62](https://github.com/spojchil/MineIntent/pull/62) | 合法信息边界与 Information Runtime | 当前行为看代码，完整设计看历史快照 |
| [Issue #70](https://github.com/spojchil/MineIntent/issues/70) / [PR #71](https://github.com/spojchil/MineIntent/pull/71) | “安静真人/行为图灵测试”升格为高层目标 | 方向偏离节点，后续更正见 #108 |
| [PR #72](https://github.com/spojchil/MineIntent/pull/72) | 五层文档状态、登记表与大量历史恢复进入主线 | 本次文档收敛的前一结构 |
| [PR #73](https://github.com/spojchil/MineIntent/pull/73) / [PR #78](https://github.com/spojchil/MineIntent/pull/78) | D40 短动作工具循环实验 | 实验来源 |
| [PR #93](https://github.com/spojchil/MineIntent/pull/93) | 标准工具调用、稳定前缀和追加帧上下文 | 当前实现的主要来源 |
| [PR #103](https://github.com/spojchil/MineIntent/pull/103) / [PR #105](https://github.com/spojchil/MineIntent/pull/105) / [PR #106](https://github.com/spojchil/MineIntent/pull/106) / [PR #107](https://github.com/spojchil/MineIntent/pull/107) | round 宿主、键集移动、能力契约和 `view` 工具 | 当前代码演进来源 |
| [Issue #83](https://github.com/spojchil/MineIntent/issues/83) | 状态页、架构说明和登记册腐烂问题 | 最小文档结构的诊断来源 |

## 旧路径迁移

本节覆盖 `main@46bcd4d28630421a4199f0857b973818f1569f92` 中后来删除的全部 44 份 `docs/` 文档。
链接指向不可变原文；“当前入口”只说明今天从哪里继续阅读，不继承旧文档的状态。

迁移表不能使已删除的 GitHub URL 自动跳转。需要旧 URL 本身继续可用时，只能在原路径保留薄墓碑文件；当前结构选择
集中索引，因此旧深链仍会返回 404。

### 产品

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/product-design.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/product-design.md) | [产品](../产品.md)；未重新确认的内容只作为历史 |

### 架构、状态、接口和旧 ADR

这些旧文件不能整体并入当前架构页。当前仍成立的实现事实必须从目标代码重新核对；旧目标、旧决定和理由留在快照。

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/architecture/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/README.md) | [当前实现结构](../architecture.md) |
| [`docs/architecture/cognitive-perception.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/cognitive-perception.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/companion-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/companion-runtime.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/current-system.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/current-system.md) | [当前实现结构](../architecture.md) |
| [`docs/architecture/decision-contract-and-context.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/decision-contract-and-context.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/domain-events-and-journal.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/domain-events-and-journal.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/information-access-and-ui.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/information-access-and-ui.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/information-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/information-runtime.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/memory-model-and-profile-versioning.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/memory-model-and-profile-versioning.md) | 当前事实看[架构页](../architecture.md)，候选记忆选择看[产品](../产品.md) |
| [`docs/architecture/minecraft-backend.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/minecraft-backend.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/target-system.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/target-system.md) | 产品看[产品](../产品.md)，实现看[架构页](../architecture.md) |
| [`docs/architecture/ui-context.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/ui-context.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/current-status.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/current-status.md) | [当前实现结构](../architecture.md)与[运行指南](../guides/run.md) |
| [`docs/guides/model-interface.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/guides/model-interface.md) | 模型边界看[架构页](../architecture.md)，配置看[运行指南](../guides/run.md) |
| [`docs/decisions/0001-use-mineflayer-as-initial-backend.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0001-use-mineflayer-as-initial-backend.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0002-event-driven-companion-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0002-event-driven-companion-runtime.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0003-separate-mind-and-action-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0003-separate-mind-and-action-runtime.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0004-no-arbitrary-model-code.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0004-no-arbitrary-model-code.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0005-limit-mineflayer-to-protocol-driver.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0005-limit-mineflayer-to-protocol-driver.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |

### 旧文档治理与目录入口

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/decisions/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/README.md) | [文档入口](../README.md)与[贡献流程](../../CONTRIBUTING.md) |
| [`docs/decisions/template.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/template.md) | 无现行模板；流程见[贡献指南](../../CONTRIBUTING.md) |
| [`docs/document-register.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/document-register.md) | [文档入口](../README.md) |
| [`docs/documentation-policy.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/documentation-policy.md) | [文档入口](../README.md) |
| [`docs/guides/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/guides/README.md) | [文档入口](../README.md)，再分流至运行和验证指南 |

### 历史、研究、路线与提案

以下旧文件的当前入口都是本页；内容只以不可变快照存在。未决工作应回到 GitHub Issue，而不是继承旧提案状态。

| 旧路径（不可变原文） |
|---|
| [`docs/history/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/README.md) |
| [`docs/history/archive-2026-07-14-information/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/README.md) |
| [`docs/history/archive-2026-07-14-information/information-acceptance-matrix.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/information-acceptance-matrix.md) |
| [`docs/history/archive-2026-07-14-information/player-state-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/player-state-information.md) |
| [`docs/history/archive-2026-07-14-information/screen-and-overlay-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/screen-and-overlay-information.md) |
| [`docs/history/archive-2026-07-14-information/sound-and-lifecycle-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/sound-and-lifecycle-information.md) |
| [`docs/history/archive-2026-07-14-information/viewport-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/viewport-information.md) |
| [`docs/history/early-autonomous-agent-handoff.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/early-autonomous-agent-handoff.md) |
| [`docs/history/project-evolution.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/project-evolution.md) |
| [`docs/history/research-cognitive-perception.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/research-cognitive-perception.md) |
| [`docs/history/research-system-design.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/research-system-design.md) |
| [`docs/history/roadmap-v0.2-legal-information-interfaces.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/roadmap-v0.2-legal-information-interfaces.md) |
| [`docs/history/roadmap-v0.3-trustworthy-embodiment.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/roadmap-v0.3-trustworthy-embodiment.md) |
| [`docs/proposals/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/README.md) |
| [`docs/proposals/embodiment-architecture-reflection.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-architecture-reflection.md) |
| [`docs/proposals/embodiment-decision-register.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-decision-register.md) |
| [`docs/proposals/embodiment-interface-inventory.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-interface-inventory.md) |
| [`docs/proposals/information-interfaces.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/information-interfaces.md) |
| [`docs/proposals/trustworthy-gaze.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/trustworthy-gaze.md) |

### 恢复稿到当前结构的改名

| 恢复稿路径 | 当前路径 | 恢复稿不可变原文 |
|---|---|---|
| `PRODUCT_CONSTITUTION.md` | [`产品.md`](../产品.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/PRODUCT_CONSTITUTION.md) |
| `docs/source-index.md` | 本页 | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/source-index.md) |
| `docs/guides/companion-prototype.md` | [`docs/guides/run.md`](../guides/run.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/guides/companion-prototype.md) |
| `docs/guides/paper-integration.md` | [`docs/guides/validation.md`](../guides/validation.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/guides/paper-integration.md) |
| `src/companion/` | [`src/participant/`](../../src/participant/) | [`e2d1f89` 原路径](https://github.com/spojchil/MineIntent/tree/e2d1f89/src/companion) |
| `产品.md`（仓库根） | [`docs/产品.md`](../产品.md) | [`e37ffe7` 原路径](https://github.com/spojchil/MineIntent/blob/e37ffe7/产品.md) |
| `产品待澄清问题.md` | 已移出仓库（转维护者本地 `*.local.*` 文件；2026-07-31 基线整体确认后仅余历史价值） | [`e37ffe7` 原文](https://github.com/spojchil/MineIntent/blob/e37ffe7/产品待澄清问题.md) |
| 施工仓库 `MineIntent-backend-rs`（本地） | [`crates/`](../../crates/) | 完整施工历史（含 60+ 份施工过程文档、逐切片进度日志、决策台账与中期更新）保留在本仓库的 [`archive/rust-port-wip`](https://github.com/spojchil/MineIntent/tree/archive/rust-port-wip) 分支；该分支与 `main` 无共同祖先，只作证据查阅，不合并 |
