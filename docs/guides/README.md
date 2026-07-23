---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-24
---

# 运行与验证指南

- [Paper 1.21.1 集成测试](./paper-integration.md)
- [D40 实验说明](../d40-experiment.md)

项目启动方式见仓库根目录 [README](../../README.md)。模型服务与工具循环的当前边界直接由 [agent-service/server.py](../../agent-service/server.py) 及相邻测试定义。

接口的最终事实位于代码中的 schema、类型和测试。实验接口仍会快速变化，因此当前不维护一份与源码重复的大型模型接口手册。
