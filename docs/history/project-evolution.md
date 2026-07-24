---
status: historical
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 项目演进史

MineIntent 的摇摆并不是毫无积累地反复推倒。每次转向都发现了前一代原型无法回答的新问题。这里保存这些转折，避免旧文档被误删，也避免它们继续冒充现行方案。

## 1. 起点：自主任务 Agent

最早的自包含交接文档把目标描述成：玩家给出“获得鞘翅”之类长期目标，AI 自己规划并完成。它已经包含若干后来仍然重要的思想：

- 程序提供可靠、通用、可组合的玩家能力；
- 模型不应直接生成任意代码；
- 动作应可取消、超时并返回结构化结果；
- 把 tick 级本地控制、数秒到数分钟的可验证短流程、事件驱动模型规划分成三种时间尺度；
- 协议客户端缓存不能自动成为角色知识；
- 用户决定意图和成功条件，系统保留路线自由，不把固定攻略硬编码成唯一答案；
- 真实图像可以成为未来可选传感器，而非当前阻塞项；
- 调试与人工接管必须被记录，不能冒充自主成功。

它同时把 skill、Flow 和“完成长期目标”放在产品中心，后来已不再代表 MineIntent 的产品定义。原文完整保存在[早期自主 Agent 交接文档](./early-autonomous-agent-handoff.md)。

## 2. 2026-07-12：从任务工具转向长期同伴

项目建立新的[产品设计](../product-design.md)：复杂能力只是共同生活的基础，而不是替玩家交付结果的终点。陪伴、共同经历、可信参与和持续关系成为北极星。

同一天形成 ADR 0001–0004 和大批详细设计，建立 Mineflayer、事件驱动持续运行时、心智—动作边界以及禁止模型任意代码的基础。

这一阶段的长文档价值在于把聊天、活动、动作、事件、记忆和恢复放进同一个持续同伴系统；问题是它们大多在实现前一次性展开，容易让“契约已经写完”看起来像“能力已经存在”。

## 3. 2026-07-12 至 07-13：v0.1 能力优先原型

[PR #27](https://github.com/spojchil/maineintent/pull/27) 到 [PR #31](https://github.com/spojchil/maineintent/pull/31) 快速接通：

- Mineflayer Backend；
- 游戏聊天和语言调度；
- Action Runtime 的取消、资源锁和验证；
- Paper 1.21.1 测试框架；
- 跟随、等待、寻找/采集木头、返回玩家；
- 最小跨重启记忆。

v0.1 milestone 在历史上 11/11 完成。它证明了一个 AI 可以在真实 Paper 世界中完成“聊天—活动—行动—结果—记忆”闭环。

但它依赖高层 skills、`findBlock`、Pathfinder 和 Mineflayer 的乐观动作语义。这些能力后来从最新实验分支删除。因此 v0.1 应被视作**成功的历史验证原型**，不是当前能力清单。

## 4. 2026-07-13 至 07-14：真人联调带来的可信性转折

真实游玩暴露了关键区别：

- tracked entity 不等于当前看见；
- `findBlock` 可以读取视野外已加载区块；
- Pathfinder 可以利用角色尚未探索的地形；
- `bot.dig` 的本地完成不等于服务端已经接受；
- 面向模型的对象级 skill 会把共同经历压缩成任务调用。

[PR #42](https://github.com/spojchil/maineintent/pull/42) 因此接受阶段重排：v0.2 先解决合法信息和 UI，v0.3 再解决可信具身。[PR #62](https://github.com/spojchil/maineintent/pull/62) 实现 Information Runtime 公共底座；[PR #67](https://github.com/spojchil/maineintent/pull/67) 接受 UI Context 设计；[PR #71](https://github.com/spojchil/maineintent/pull/71) 把经许可、无作弊的“原版玩家可信性”确立为长期压力测试目标。

这是项目从“先会做事”转向“语言、知识和身体必须共享合法事实”的根本转折。

## 5. 2026-07-14 至 07-21：设计栈过度展开

PR #64、#65、#66、#68、#69 分别详细设计玩家状态、Screen/Overlay、声音/生命周期、视口和验收矩阵。五份文档都很认真，也包含大量可复用边界：

- source/projection/provider 的单向所有权；
- UI session 与像素画面的区别；
- 声音不可复用视觉 DDA 遮挡；
- ref/cursor 必须绑定正确 revision/scope；
- Paper 证据与真人可见性裁判必须分开。

然而这些 PR 共同依赖未实现的 Issue #63，又继续生成交叉契约。2026-07-21，它们以相同理由关闭：停止堆叠纯设计，优先玩家可以看见的功能。

它们没有被“证明错误”，也没有成为接受基线。原文已从各保留分支恢复到[2026-07-14 信息设计档案](./archive-2026-07-14-information/README.md)。

## 6. 2026-07-21：默认分支直接简化实现

九个直接提交把 Python Agent Service 和四个被动信息接口带入 `main`，随后修复区块加载、脚下方块、yaw/pitch、视锥和遮挡问题。这些提交提供了玩家可观察进展，但没有 PR 评审，也没有同步 Issue 状态。

这里形成新的治理教训：**代码可以比 tracker 新，tracker 也可以比 accepted decision 更像一份计划；三者必须分别记录。**

## 7. 2026-07-22：可信注视实验

最新实验分支依次加入：

- 被动观察的更严格边界；
- JSON/Python 传输验证；
- 删除 V1 Action Runtime 和 skills；
- Decision V2；
- viewport refs、Grounding、Behavior Synthesizer；
- scoped Motor primitives 和视觉 controller；
- “看向我”的 Paper 场景；
- 模型接口说明。

这一代实现并用确定性测试演示了一个很窄的注视闭环，却把可玩能力收缩到近乎只有“转头 + 说话”。后来审计又发现结果阶段没有强制感知 revision 前进，因此事件名 `outcome_verified` 仍不能等同于 fresh observation proof。Capability Catalog、主动性、记忆和身体能力之间的不匹配也随之变得明显。

## 8. 2026-07-23：架构重新开放

[具身架构反思](../proposals/embodiment-architecture-reflection.md)提出直接的短动作/信息 Tool Loop，质疑当前 Grounding/ref/Behavior 层是否过度复杂。紧接着，[决策登记册](../proposals/embodiment-decision-register.md)纠正反思中的事实错误，列出 F01–F20、B01–B09 和 D01–D40。

项目在这里尚未完成第二次迁移。准确结论是：

> 已经确认当前系统的若干根本问题，也有候选替代方向，但新的模型—身体接口尚未被接受。

## 保留下来的稳定思想

跨越所有阶段，以下思想反复出现，值得继续作为设计判断基线：

- 产品中心是共同经历和长期关系，不是任务吞吐量；
- 模型表达开放意图，确定性运行时控制副作用、安全和证据；
- 协议事实、角色知识、语言主张和动作结果必须分层；
- 失败应如实成为下一轮观察，而不是伪装成功或静默消失；
- 身体动作应短、有界、可打断，并由新观察验证；
- 早期方案应保留来源和失败原因，但不能永久占据“当前”位置。
