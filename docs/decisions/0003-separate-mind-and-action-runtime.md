---
status: accepted
authority: normative
implementation: diverged
last_verified: 2026-07-23
---

# 0003：分离同伴心智与行动运行时

> 接受日期：2026-07-12。
>
> **未协调漂移：** 提交 [`29052bf`](https://github.com/spojchil/maineintent/commit/29052bf09960d6233b16b60528cd4e1efc681bac) 删除了独立 `src/actions/` 和 `src/skills/`；当前只有 `CompanionRuntime` 内联的单行为槽位以及 gaze controller。尚无 ADR amend/supersede 本决定，因此“实现已经删除”不能被解释成“新边界已经接受”。

## 背景

若人格、对话、规划和 Mineflayer 操作集中在同一个 Agent 类中，模型状态、持续动作、取消和恢复会相互耦合；若聊天与游戏 Bot 完全分离，语言又会与真实行为不一致。

## 决定

将 Companion Runtime 与 Action Runtime 作为独立系统边界：

- Companion Runtime 维护关注、对话、共同活动、短期意图和决策。
- Action Runtime 维护技能、身体资源锁、持续执行、取消、超时和验证。
- 双方只通过结构化动作请求和领域事件通信。
- Decision Coordinator 在语言承诺发出前确认相应动作已经通过校验并被接受。

## 理由

- 社交理解可以保持开放，不必硬编码进每个技能。
- 动作执行可以独立保证可靠性和实时性。
- 语言承诺能够与真实动作接受及结果关联。
- 两个边界可以分别测试和演进。

## 后果

### 正面

- 模型供应商和 Minecraft Backend 可以分别替换。
- 本地安全反射可以抢占身体动作而不破坏同伴长期状态。
- 行动失败会成为同伴可以理解和解释的结构化事件。

### 代价与风险

- 需要定义稳定的 Decision Contract、动作协议和因果标识。
- 状态更新必须避免聊天心智和身体状态短暂不一致。
- 同一决策内的多动作需要显式依赖和资源仲裁。

## 备选方案

- 单体 Agent 类：初期代码少，但难以维护并发和恢复。
- 独立聊天 Bot + 自动化 Bot：实现简单，但无法形成可信同伴。

## 后续验证

原型中“我去收集木头”只能在采集动作被接受后发送；动作失败后只能依据真实结果解释和重新协商。
