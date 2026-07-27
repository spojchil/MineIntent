---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-24
---

# 运行、验证与接口参考

## 怎么跑

- [运行 D40 同伴原型](./companion-prototype.md)
- [Paper 1.21.1 集成测试](./paper-integration.md)
- [Windows Paper 服务端管理](../../mcserver/README.md)

## 接口长什么样

- [旧实验分支模型接口参考](./model-interface.md)：`ContextPackageV2` / `CompanionDecisionV2` 的人类可读视图；文件的 `applies_to` 已明确它不是 D40 接口。

指南只描述怎样运行和验证指定基线，不代表测试场景中的所有动作都是生产同伴能力。接口参考随代码变化，**TypeScript schema 是最终权威**；两者冲突时先修文档。
