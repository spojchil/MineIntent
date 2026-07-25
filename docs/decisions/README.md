---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 架构决策记录

ADR（Architecture Decision Record）记录影响长期实现的单项决定。讨论过程保留在 GitHub Issue，ADR 保存结论、取舍以及实现是否仍然对齐。

## 状态

- `proposed`：仍在讨论。
- `accepted`：当前有效。
- `superseded`：已被后续 ADR 取代。
- `rejected`：经过讨论但未采用。

## 列表

| ADR | 决策状态 | 实现状态 | 说明 |
|---|---|---|---|
| [0001：使用 Mineflayer](./0001-use-mineflayer-as-initial-backend.md) | accepted | current | 第一版 Backend 选择仍有效 |
| [0002：事件驱动持续同伴](./0002-event-driven-companion-runtime.md) | accepted | partial | 主动机会、多通道和完整恢复尚缺 |
| [0003：分离心智与行动运行时](./0003-separate-mind-and-action-runtime.md) | accepted | **diverged** | 最新实验删除了独立 Action Runtime，ADR 尚未 amend/supersede |
| [0004：不执行模型任意代码](./0004-no-arbitrary-model-code.md) | accepted | current | 当前 V2 接口比原始 skill 示例更窄 |
| [0005：Mineflayer 仅作协议驱动](./0005-limit-mineflayer-to-protocol-driver.md) | proposed | partial experiment | Issue #32 仍 open；代码先实现了部分边界 |

实现漂移不会自动把 accepted ADR 变成 superseded。必须通过新 ADR 或明确 amendment 记录替代边界、迁移后果和验证结果。

新 ADR 从 [模板](./template.md) 创建，编号递增。若新决定替代旧决定，应在两份 ADR 中互相链接。
