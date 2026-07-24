---
status: accepted
authority: normative
implementation: current
last_verified: 2026-07-23
---

# 0001：使用 Mineflayer 作为第一版 Minecraft Backend

> 接受日期：2026-07-12。当前仍使用 Mineflayer；其职责边界受 proposed ADR 0005 的部分实验影响，但 Backend 选择本身未被取代。

## 背景

MineIntent 需要一个能够以真实玩家身份连接 Minecraft Java Edition 1.21.1，并提供世界、实体、背包、容器、物理与玩家操作的无界面客户端。项目目标是研究 AI 同伴，而不是先从数据包开始重写完整 Minecraft 客户端。

## 决定

第一版使用 Mineflayer 作为 Minecraft Backend，并通过内部边界隔离其事件和操作接口。

这是一项第一版实现决定，不排除未来增加完整客户端或其他协议后端。

## 理由

- Mineflayer 已提供成熟的协议连接和高层玩家状态。
- TypeScript/Node.js 便于接入模型、事件、调试接口和结构化协议。
- 本地 Paper 1.21.1 冒烟测试已经验证 Mineflayer 4.37.1 能够连接、生成角色并读取基础状态。
- 替代方案需要显著增加与产品价值无关的客户端基础工作。

## 后果

### 正面

- 可以优先实现同伴语言、活动、行动和记忆闭环。
- 可以利用 PrismarineJS 生态中的世界数据与基础插件。

### 代价与风险

- “支持协议版本”不代表全部行为可靠，关键能力仍需集成测试。
- 协议客户端天然获得的信息需要经过认知可见性过滤。
- Mineflayer 插件可能包含不符合 MineIntent 产品边界的策略，需要包装而非直接暴露。

## 备选方案

- MCProtocolLib：协议层稳健，但需要自行补齐大量玩家能力。
- Azalea：Rust 性能和并发优秀，但第一版生态与既有验证少于 Mineflayer。
- Fabric/完整客户端：更接近原版客户端，但部署、资源和控制复杂度更高。

## 后续验证

首个纵向原型必须验证连接、重连、移动、跟随、采集、取消、背包结果和 1.21.1 边缘行为。若底层无法可靠承载这些能力，应重新评估本决定。
