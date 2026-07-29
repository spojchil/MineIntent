# MineIntent 来源索引

本页用于找回思想从哪里来，不裁决今天必须怎样做。

## 使用规则

- 早期记录对恢复原始愿景具有高价值，但“初始”不等于“正确”或“仍有效”。
- 恢复方向时优先审计项目初期的 PR 和 Issue：它们最接近原始愿景，且维护者当时参与更集中；这只提高
  取证优先级，不自动提高结论权威。
- 来源能否公开复核，与它是否拥有当前产品决策权是两个维度。非公开维护者澄清只记录最小结论，不复制会话原文。
- GitHub author、merged 状态和多个同源文档不能单独证明维护者确认了产品判断。
- 产品根目的以依确认程序生效后的[产品宪法](../PRODUCT_CONSTITUTION.md)为准；当前实现事实以目标提交的代码和
  可重复运行证据为准，预期行为仍由已确认的产品决定裁决。
- 下表只摘要来源的意义与当前身份。需要细节时直接阅读不可变 commit、Issue 或 PR。

## 当前澄清

| 来源 | 它记录或支持什么 | 当前身份 |
|---|---|---|
| 维护者当前澄清（2026-07-29，本次恢复会话；非公开，不收录原文） | 初始目标已经变化；产品不替 AI 选择 A 或 B、向东或向西，也不为预设玩家安排结果偏向 | 本次宪法起草的直接输入；替代目标的准确文本仍待维护者公开确认并生效 |

## 起点与早期阶段

| 来源 | 它证明什么 | 当前身份 |
|---|---|---|
| [`2ee231c`](https://github.com/spojchil/MineIntent/commit/2ee231c) | 最早 handoff 以“一次长期目标、自主交付结果”为中心，同时提出可取消、验证结果和长期运行问题 | 历史技术前身，不是现行产品目的 |
| [`078f525`](https://github.com/spojchil/MineIntent/commit/078f525) | 早期阶段首次完整表述长期同伴、共同经历、陪伴、可信参与和持续关系；同时把 handoff 降为早期研判 | 初始问题框架的不可变历史证据，不是当前目标；其中任何条款进入宪法都需重新确认 |
| [Issue #12](https://github.com/spojchil/MineIntent/issues/12) / [PR #20](https://github.com/spojchil/MineIntent/pull/20) | 领域事件与事件日志的早期设计 | 历史设计来源；当前接口以代码为准 |
| [Issue #13](https://github.com/spojchil/MineIntent/issues/13) / [PR #21](https://github.com/spojchil/MineIntent/pull/21) | 持续同伴、共同活动和注意模型，也引入第一版“主要玩家” | 注意与共同活动的历史来源；具体关系与活动由 AI 选择，主要玩家假设被后续方向否定 |
| [Issue #10](https://github.com/spojchil/MineIntent/issues/10) / [PR #22](https://github.com/spojchil/MineIntent/pull/22) | 模型输出是提议、世界结果优先、人格不应被结构规则取代 | 其中真实性与非结构化社交判断得到现行来源独立支持；V1 协议及其余工程判断需按当前代码复核 |
| [Issue #4](https://github.com/spojchil/MineIntent/issues/4) / [PR #23](https://github.com/spojchil/MineIntent/pull/23) | 早期长期记忆、档案版本和结构化证据方案 | 历史方案；#109 提议以 AI 编辑的文本记忆取代，是否采纳仍待确认 |
| [Issue #24](https://github.com/spojchil/MineIntent/issues/24) / [PR #25](https://github.com/spojchil/MineIntent/pull/25) | “客户端收到”不等于“同伴知道”的感知边界 | 当前信息边界的早期问题来源；具体感知架构不自动现行 |
| [PR #26](https://github.com/spojchil/MineIntent/pull/26) / [PR #27](https://github.com/spojchil/MineIntent/pull/27) | Mineflayer Backend 生命周期设计与首个真实 Paper 验证 | 当前 Backend 的历史来源 |
| [PR #28](https://github.com/spojchil/MineIntent/pull/28) / [PR #29](https://github.com/spojchil/MineIntent/pull/29) / [PR #30](https://github.com/spojchil/MineIntent/pull/30) | 聊天、可取消执行和可重复 Paper 测试的早期工程闭环 | 早期工程来源；取消与验证机制是否仍存在，以当前代码和运行证据为准 |
| [Issue #17](https://github.com/spojchil/MineIntent/issues/17) / [PR #31](https://github.com/spojchil/MineIntent/pull/31) | 共同采木、打断、真实结果与重启记忆组成过首个纵向同伴切片 | 证明同伴愿景曾指导原型；不是当前能力清单 |

## 后续演变与纠正

| 来源 | 它证明什么 | 当前身份 |
|---|---|---|
| [PR #42](https://github.com/spojchil/MineIntent/pull/42) / [PR #62](https://github.com/spojchil/MineIntent/pull/62) | 合法信息边界与 Information Runtime 的设计、实现来源 | 代码和测试仍承载部分结果；大设计文本不再作为活文档 |
| [Issue #70](https://github.com/spojchil/MineIntent/issues/70) / [PR #71](https://github.com/spojchil/MineIntent/pull/71) | “安静真人/行为图灵测试”从路线与验收代理升格为高层目标 | 历史偏离节点；已由 #108 更正降级 |
| [PR #72](https://github.com/spojchil/MineIntent/pull/72) | 五层文档状态、登记表与大量历史恢复进入主线 | 本恢复分支拟以最小文档体系取代；确认并合并前不是现行规则 |
| [PR #73](https://github.com/spojchil/MineIntent/pull/73) / [PR #78](https://github.com/spojchil/MineIntent/pull/78) | D40 短动作工具循环实验及其有限现场证据 | 实验来源，不是完整可重复验收 |
| [PR #93](https://github.com/spojchil/MineIntent/pull/93) | 当前标准 tool calls、稳定前缀和追加帧模型的主要来源 | 当前实现历史；`primaryPlayer` 触发与当前澄清冲突，#109 只是候选迁移方案 |
| [PR #103](https://github.com/spojchil/MineIntent/pull/103) / [PR #105](https://github.com/spojchil/MineIntent/pull/105) / [PR #106](https://github.com/spojchil/MineIntent/pull/106) / [PR #107](https://github.com/spojchil/MineIntent/pull/107) | 当前 round 宿主、键集移动、能力契约和 `view` 工具的演进 | 当前代码来源，不是产品目的 |
| [Issue #83](https://github.com/spojchil/MineIntent/issues/83) | 指出状态页、架构说明和登记册会腐烂并阻塞决定，提出只留用户文档 | 文档膨胀问题及最小体系方案的历史诊断；具体删减范围未在该 Issue 中定案 |
| [Issue #108 维护者更正](https://github.com/spojchil/MineIntent/issues/108#issuecomment-5100507322) | 公开纠正“安静真人”验收代理的升格，并区分目的、假设、路线、风险策略和验收代理 | 该层级纠正仍有效；其中“初始目标仍是当前目标”的判断已被当前维护者澄清取代 |
| [Issue #109](https://github.com/spojchil/MineIntent/issues/109) | 公开展开无主要玩家、自主关注、公屏历史、文本记忆和单多人统一方案 | 关系与多人机制的工作方向；其中提到的产品外修改只是在说明文件、提示词或源码可以被手工改动，不是产品接口；具体机制仍需确认，代码也尚未迁移 |

## 清理前快照

- [47 份旧文档的完整目录](https://github.com/spojchil/MineIntent/tree/46bcd4d28630421a4199f0857b973818f1569f92/docs)
- [旧文档登记表](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/document-register.md)

删除工作树中的副本不会删除思想历史。若旧结论重新有价值，应回到其原始证据和当前代码重新论证，
而不是把旧正文恢复成现行规范。
