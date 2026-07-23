---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-24
---

# 架构决策记录

本目录只保留当前仍然有效、范围足够清楚的长期决定。

| ADR | 状态 | 当前含义 |
|---|---|---|
| [0001：使用 Mineflayer](./0001-use-mineflayer-as-initial-backend.md) | accepted | 第一版 Backend 选择仍有效，不决定最终身体接口 |
| [0002：事件驱动持续同伴](./0002-event-driven-companion-runtime.md) | accepted / partial | 玩家消息、动作结果和危险可以触发运行；完整主动性与恢复尚未实现 |
| [0004：不执行模型任意代码](./0004-no-arbitrary-model-code.md) | accepted | 模型只能调用实现方注册的受限接口 |

已删除的 ADR、提案和完整设计仍存在于 Git 历史，但不再具有当前权威。新实验不必先写 ADR；只有在实验结果支持一个需要长期维持的边界时，才新增或修订记录。
