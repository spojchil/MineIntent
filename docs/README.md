---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-24
---

# MineIntent 文档

主线文档只回答当前真正需要回答的问题：

1. [产品设计](./product-design.md)：项目要成为怎样的 Minecraft 同伴。
2. [当前项目状态](./current-status.md)：代码和实验现在实际到了哪里。
3. [D40 实验](./d40-experiment.md)：当前正在验证什么，以及已经暴露了哪些问题。
4. [架构决策记录](./decisions/README.md)：仍然有效的少量长期决定。
5. [Paper 集成测试](./guides/paper-integration.md)：怎样运行真实服务器验证。

判断事实时遵循一个简单顺序：

- 程序当前做什么，以目标分支的代码、测试和真实运行结果为准。
- 项目长期为何存在，以产品设计为准。
- 实验页只记录候选方向和观察结果，不自动成为最终架构。
- 被删除的旧设计仍可从 Git 历史读取，但不约束当前实现。

接口形状应优先从相邻 schema、类型和测试读取。只有当一个接口已经稳定且人类读者确实需要时，才补独立参考文档。
