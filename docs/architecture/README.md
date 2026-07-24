---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 架构文档

本目录同时保存“当前实况”和“已经接受但可能尚未完全实现的架构契约”。不要仅凭文件位于本目录就推断代码已经存在；必须阅读每篇文档的 `status`、`authority` 和 `implementation`。

## 当前实况

- [当前系统实况](./current-system.md)：从最新实验分支代码反推的运行链路。
- [模型接口参考](../guides/model-interface.md)：当前 V2 schema 的人类可读视图。

## 长期与模块契约

| 文档 | 决策状态 | 实现对齐 |
|---|---|---|
| [目标系统设计](./target-system.md) | accepted target | diverged；不能当作当前模块图 |
| [同伴运行时](./companion-runtime.md) | accepted | diverged；仍引用已删除的 Action Runtime |
| [领域事件与事件日志](./domain-events-and-journal.md) | accepted | partial |
| [决策协议与上下文](./decision-contract-and-context.md) | accepted | partial |
| [Information Runtime](./information-runtime.md) | accepted | partial/current core |
| [合法信息与 UI](./information-access-and-ui.md) | accepted direction | partial |
| [认知感知模型](./cognitive-perception.md) | accepted | partial；声音遮挡仍有未协调意见 |
| [记忆模型与档案版本](./memory-model-and-profile-versioning.md) | accepted | partial；当前仅为最小文件原型 |
| [UI Context](./ui-context.md) | accepted design | planned |
| [Minecraft Backend](./minecraft-backend.md) | accepted v0.1 design | diverged after driver reduction |

尚未接受的替代架构和具身方案位于 [`proposals/`](../proposals/README.md)。
