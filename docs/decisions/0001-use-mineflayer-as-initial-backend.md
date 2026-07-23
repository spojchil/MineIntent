---
status: accepted
authority: normative
implementation: current
last_verified: 2026-07-24
---

# 0001：使用 Mineflayer 作为第一版 Minecraft Backend

## 背景

MineIntent 需要以普通玩家身份连接 Minecraft Java Edition。项目当前价值在同伴体验，不在从数据包开始重写完整客户端。

## 决定

第一版使用 Mineflayer 作为 Minecraft Backend，并由项目自己的代码决定哪些状态和操作可以进入模型边界。

这是一项当前实现选择，不排除未来增加完整客户端或其他后端，也不决定模型最终看到什么工具。

## 理由

- 已验证 Mineflayer 能连接当前 Paper / Minecraft 1.21.1 测试环境。
- TypeScript/Node.js 便于连接模型、运行时和集成测试。
- 可以先验证语言、观察和身体行为是否形成同伴体验。

## 风险

- 协议客户端收到的数据不自动等于角色能够知道的信息。
- 高层插件可能产生过强或不像真人的行为，不能不经审查直接暴露给模型。
- 支持协议版本不等于每项身体行为可靠，仍需真实服务器测试。

## 复审条件

D40 及后续实验应验证 Mineflayer 能否提供可信观察、短时真实输入、取消和动作后反馈。若这些基础能力无法可靠实现，再重新评估 Backend。
