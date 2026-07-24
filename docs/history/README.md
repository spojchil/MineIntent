---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 项目来路

本分区解释“为什么会走到这里”，不决定下一步必须走哪条路。这里的每份文档在写作时都可能是当前事实，但今天都不是。文中的“当前”“决定”“实现基线”等词描述的是它写作时的情境。

## 演进

- [项目演进史](./project-evolution.md)：v0.1 到最新实验分支的整体脉络。

## 旧里程碑

两份阶段计划都曾通过 PR 成为接受路线图，但 GitHub tracker 没有吸收 2026-07-21 以后的实现。它们现在是待协调的基线，不是可靠排期。

| 计划 | GitHub 状态 | 代码现实 |
|---|---|---|
| [v0.2：合法信息与界面接口](./roadmap-v0.2-legal-information-interfaces.md) | milestone 仍 open | Information Runtime 和四个简化 provider 已存在；五个详细设计 PR 已归档，计划已经停滞/漂移 |
| [v0.3：可信具身闭环](./roadmap-v0.3-trustworthy-embodiment.md) | 17 个 Issue 仍 open | 最新实验已经实现部分 Grounding/gaze，却越过了原计划的 ADR 接受门 |

实际能力见[当前项目状态](../current-status.md)，根本决策见[具身登记册](../proposals/embodiment-decision-register.md)。

## 调研

- [相邻 Agent 与同伴系统调研](./research-system-design.md)
- [Minecraft 认知感知源码调研](./research-cognitive-perception.md)

调研保存事实来源、比较和启发，不自动构成 MineIntent 的产品或架构决定。涉及快速演进的第三方项目时，应在复用结论前重新核对版本。

## 档案

- [早期自主 Agent 交接文档](./early-autonomous-agent-handoff.md)
- [2026-07-14 信息接口设计档案](./archive-2026-07-14-information/README.md)：从五条已关闭、未合并 PR 分支恢复的 3,700 行设计。

**不要直接从档案恢复接口。**重新启用时应先核对当前代码、accepted ADR 和当年的关闭原因，再创建新提案。这些材料中仍然有效的原则已经提炼到[信息接口提案](../proposals/information-interfaces.md)。
