---
status: proposed
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 信息接口提案与应保留原则

Information Runtime 的公共底座已经实现并接受；完整 UI、Screen、声音、生命周期、视口和验收体系仍未接受或未实现。2026-07-14 的五份设计虽被关闭，以下证据原则仍值得带入后续方案评审。

## 应保留的证据边界

### 测试裁判与生产认知隔离

测试至少区分：

| 层 | 内容 | 允许用途 |
|---|---|---|
| O0 | Paper 命令、raw 协议状态、fixture 真值 | 只用于建立场景和断言 |
| O1a | 可信内部 source/projection DTO | 验证适配和失效语义 |
| O1b | Provider 公开投影 | 验证模型实际可见值和 metadata |
| O2 | Context、Read、ref/cursor 和模型结果 | 最终产品验收对象 |

O0 可以在测试中和 O2 比较，但绝不能成为 O1/O2 的生产输入。否则测试会用上帝视角证明“没有上帝视角”。

### 声音不是视觉

收到客户端声音事件默认表示角色已经听到。不能复用视觉 DDA，把墙后声音按光学遮挡删除；最多根据版本化听觉模型量化方向、距离或不确定性。视觉可见性和听觉可感知性必须是不同策略。

### revision 不只是计数器

- 内部 provenance 改变不必自动成为模型可见 revision。
- 公开 value/availability 改变必须原子提升对应公开 revision。
- ref/cursor 只能绑定 provider 声明的 scope dependencies，不能被读取时碰巧存在的无关 screen/world scope 误伤。
- Read 完成前必须复核 scope 和公开 information revision，避免拼接两个时刻。

### 不可见也要可验证

隐藏 saturation、精确坐标、NBT、未打开容器内容等 canary 变化时，公开值、revision、时间和 trace 都不应泄漏变化。单纯扫描字符串不够，还要做“隐藏输入变化、公开输出完全不变”的成对测试。

完整原始设计和来源见[信息设计档案](../history/archive-2026-07-14-information/README.md)。这些原则目前是评审输入，不是已接受的新 ADR。
