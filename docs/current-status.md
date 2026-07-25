---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-25
applies_to: main@7e2229d + codex/trustworthy-passive-context@57d438e + experiment/d40-run@f2aada3
---

# 当前项目状态

本文只报告事实，不替项目做尚未完成的架构决定。它刻意把“默认分支”“不同实验
基线”和“已接受设计”分开。

## 基线

| 基线 | 提交 | 含义 |
|---|---|---|
| 默认分支 `main` | [`7e2229d`](https://github.com/spojchil/maineintent/commit/7e2229def2389ac173e15fb19d7a2f07aa3f4755) | GitHub 默认展示、已合并文档治理和现有成功 CI 所在位置 |
| 可信注视架构实验 `codex/trustworthy-passive-context` | [`57d438e`](https://github.com/spojchil/maineintent/commit/57d438e167e7b66408f239369979dad2fd90f975) | 包含 V2 决策、Grounding、Behavior 和可信注视链路；没有被接受或合并 |
| D40 一次性身体实验 `experiment/d40-run` | [`f2aada3`](https://github.com/spojchil/maineintent/commit/f2aada361d314b2fa12896e1bf29ac0d741c1601) | 真实模型与 Paper 上的短动作 Tool Loop：`look_relative`、`move_input`、动作后新视口和最终聊天；是归档证据，不是接受架构 |

按时间最新的是 D40 一次性实验，按完整架构切片则仍有可信注视实验；两者都不是
`main` 或“最新接受架构”。D40 的七次现场运行覆盖分支演进过程，`f2aada3` 是经过
后续几何、取消和说明修订的归档头，不代表七次场景已在该提交上完整重跑。

文档重组已经通过 PR #72 进入 `main`，但其中
`architecture/current-system.md`、`guides/model-interface.md` 和
`proposals/trustworthy-gaze.md` 仍明确描述可信注视实验，不描述后来的 D40 Tool
Loop。它们的 `applies_to` 才是适用范围——**文档描述某个分支，不等于那个分支已被
接受，也不等于它是时间上最新的实验**。

## 当前能力矩阵

下表中的“当前”默认指较完整的可信注视架构实验；D40 例外会明确标为“D40
实验”。`main` 不包含这两条实验分支的运行时代码。

| 能力 | 状态 | 事实边界 |
|---|---|---|
| Mineflayer 连接、生命周期和快照 | 当前 | 可连接 Paper/Minecraft 1.21.1，并提供协议与身体状态底座 |
| 被动 `current_status` | 当前 | 生命、饥饿、氧气、经验和状态效果 |
| 被动背包读取 | 当前 | 当前选择槽和有界物品槽信息 |
| 被动声音读取 | 当前 | 最近声音的简化投影；不等于归档设计中的完整声音/生命周期模块 |
| 被动第一人称视口 | 当前但有限 | 站立方块、准星方块、视锥/遮挡后的实体与方块；一次完整方块读取会遍历 `65 × 65 × 41 = 173,225` 个候选格，再做邻面/遮挡工作，有明确性能风险 |
| Information Runtime | 当前但部分 | 契约声明 17 个 interface ID，生产只注册并固定读取其中 4 个；Registry、Access Policy、Help/Read、Ref/Cursor 与预算主体存在，生产 trace 使用默认 no-op sink |
| model-facing Information tool loop | 不存在 | ToolSession 和适配契约有代码/测试，但可信注视与 D40 都没有把信息读取注册成模型工具 |
| model-facing body Tool Loop | D40 实验 | 模型可逐轮调用 `look_relative` 或短时 `move_input`，Runtime 返回真实效果和新视口；不在 `main` 或可信注视架构实验中 |
| Python Agent Service | 两种实验形态 | 可信注视是一次带 `json_object` 的模型调用；D40 是最多 16 轮的身体工具闭环，再产生最终聊天 |
| `ContextPackageV2` / `CompanionDecisionV2` | 实验实现 | TypeScript schema 是业务权威；严格绑定本轮 context |
| Grounding | 实验实现但有限 | 可绑定本轮 viewport block/entity ref；`message_referent` 只验证表达式出现在消息中，随后一律绑定消息发送者，所以“这棵树/那里”也会错误指向说话者 |
| Behavior Synthesizer | 实验实现但仅一个 operator | 只支持 `self.attention_includes`，即建立视觉共同注意 |
| Controller / 生产 Motor 路径 | 实验实现但仅注视 | 可渐进转向、有限扫描、取消、超时和结果阶段可见性复查；没有强制感知 revision 前进，底层 Motor 虽有 `dig` 但无生产调用方 |
| 移动 | D40 实验接入 | 可信注视的 Behavior 没有 locomotion controller；D40 可短时按住一个真实方向键并释放，不寻路、不跳跃 |
| 挖掘/攻击/使用物品 | 未接入 | 底层 driver 仍有少量原语，例如 `dig`，但没有生产消费者 |
| 选择物品栏 | 未接入 Behavior | 不能把 catalog 描述当作已上线能力 |
| 跟随、采木、Pathfinder | 两条新实验均未保留 | v0.1 曾实现；可信注视与 D40 都没有把它作为当前身体路径 |
| 游戏聊天和发送调度 | 当前但有真实性缺口 | 支持即时、接受后、终止后话术及取消；缺少 Claim Policy，模型仍可把无依据完成断言放进即时文本；终态条件不匹配的队首条目还可能阻塞后续话术 |
| 共同活动和意图状态 | 部分 | 有状态与效果处理，不构成长周期活动规划器 |
| 长期记忆 | 最小原型 | JSON 文件、证据 ID、关键词重合和时间衰减检索；没有冲突、纠正、整合、遗忘或语义关系模型 |
| 主动陪伴 | 未实现 | 契约含 `earliestProactiveAt`，运行时没有 idle/proactive 调度消费者 |
| 危险反射 | 最小停止 | 低生命时取消当前模型/行为、释放输入并警告；不会逃跑或防御 |
| 本地调试接口 | 当前但需视为敏感 | 仅绑定本机且只读；按字段/常见凭证形状脱敏，但供应商错误摘要没有通用 secret scrub |
| Paper 场景 | 两类证据 | 可信注视场景仍只是确定性模型；D40 在真实 Paper、协议客户端和模型上完成七次现场运行，但缺固定世界副本和逐工具视口日志，尚不能完整复现 |

## 两条实验链路

下面第一条是 `57d438e` 的可信注视架构实验，也是
[`current-system.md`](./architecture/current-system.md) 描述的对象：

```text
Minecraft / Mineflayer
  → 可信 source ports
  → 四个固定被动 Information Reads
  → ContextPackageV2
  → Python Agent Service 的一次模型调用
  → TypeScript DecisionProtocolDispatcher
      ├→ speech / activity / intent / memory effects（大多在行为启动前应用）
      └→ embodied_intent
          → Grounding
          → BehaviorSynthesizer
          → VisualAttentionController
          → Motor.look
          → 结果阶段可见性复查与 outcome evidence（未强制新 revision）
          → 依赖终态的 speech
```

后来的 D40 没有沿用这条 Grounding/Behavior 链，而是一次性验证了另一条窄链路：

```text
Minecraft 玩家聊天
  → 固定被动观察（状态、背包、声音、第一人称视口）
  → Python Agent Service 的多轮模型调用
      ├→ look_relative / move_input
      │   → Runtime 执行短时真实输入
      │   → 返回实际效果和动作后的新视口
      │   → 模型继续判断
      └→ 最终结构化聊天
```

第一条链路用代码和确定性测试演示了“信息、语义指代、身体动作和结果证据可以共享
同一事实基础”；第二条链路证明短动作 Tool Loop 能在真实 Paper 和模型之间闭环。
两者都没有证明其自身就是长期正确抽象，也没有让同伴具备通用身体能力。

更详细的模块说明见[当前系统实况](./architecture/current-system.md)。

## 已接受决定与实现漂移

| 决定 | 决策状态 | 实现状态 |
|---|---|---|
| ADR 0001：第一版使用 Mineflayer | accepted | 基本一致 |
| ADR 0002：事件驱动持续同伴 | accepted | 部分实现；主动机会和完整多通道运行时尚缺 |
| ADR 0003：分离心智与 Action Runtime | accepted | **diverged**；可信注视与 D40 都没有保留原独立 Action Runtime |
| ADR 0004：不执行模型任意代码 | accepted | 一致；当前接口比 ADR 描述的 skill 面更窄 |
| ADR 0005：Mineflayer 只作协议驱动 | proposed | 部分实验实现，尚未正式接受 |
| PR #71：原版玩家可信性为长期目标 | accepted | 只完成一个很窄的可信注视切片 |

## 路线图和 GitHub Tracker 的滞后

- v0.1 milestone 是历史上 11/11 完成，不代表后来的实验分支仍保有全部 v0.1 能力。
- v0.2 仍有 10 个 open Issue；五个“实现就绪”设计 PR #64、#65、#66、#68、#69 已于 2026-07-21 关闭并归档。
- v0.3 的 17 个 Issue 仍全部 open；可信注视实验实现了一部分 Grounding 和 gaze，
  D40 又在独立分支验证了不向模型暴露目标 ref 的短动作 Tool Loop。
- Issue #41 要求 ADR 0005 接受后再固化 P1 以后边界；实际实验先发生了。
- Issue #43 的统一语言事实门控仍未实现。
- Issue #63 的 Cursor scope 问题仍 open；若最终取消模型分页，它的范围可能改变，但现在不能假装已经解决。

旧阶段文件保留在 [`roadmap/`](./history/README.md)，但不应直接用作当前排期。

## 验证状态

在 2026-07-23 对 `57d438e` 的本地核验：

- Node 24：TypeScript 检查通过，123/123 测试通过。
- Python 3.12：11/11 测试通过。
- Node 22.23.1（满足项目声明的 `>=22`）：TypeScript 检查通过；测试为 122 pass、1 cancelled，进程退出 1。
- 取消项是 `visual-attention-controller.test.ts` 的 deadline 场景，根因为 `AbortSignal.timeout()` 的 unref timer 在 Node 22 测试进程没有其他活跃 handle 时不会保持事件循环。

该缺陷的修复位于 `codex/docs-information-architecture@437b2b7`，不在 PR #72 中——`src/motor/visual-attention-controller.ts` 在 `main` 上并不存在。修复内容是把控制器超时改用显式 `setTimeout` + `AbortController` 并在 `finally` 中 `clearTimeout`，与 `information/tool-session.ts` 的既有写法一致；ref'd timer 构造性地持有事件循环，不再依赖 unref 行为。Node 24 下复验 123/123 通过，Node 22 待 CI 确认。该修复须随实验分支自身的 PR 合并。

在 2026-07-25 对 D40 归档头 `f2aada3` 的本地核验：

- `pnpm check` 通过；Node 测试 97/97；Python 测试 12/12。
- 七次真实 Paper/模型运行覆盖的是分支演进过程，不是该归档头的一次完整重跑。
- 现场场景没有保留固定世界副本，日志也没有保存每次工具后实际传给模型的视口，
  因此 D40 标为“已决定”但不标为“已验证”。

PR #72 的复验（`main@53ebc57` + 文档改动）：Node 24 本地与 Node 22 CI 均通过；TypeScript 检查、`pnpm test` 87/87、文档检查 47 份 `docs/` 文档的元数据、相对链接和登记表一致。

默认分支最近一次 GitHub CI 成功记录见 [Actions run #29851997169](https://github.com/spojchil/maineintent/actions/runs/29851997169)。

## 尚未做出的根本决定

当前最核心的问题不是“哪份文档更新得最晚”，而是模型—身体接口仍未定：

- 保留当前语义目标 → Grounding → Behavior 分层，还是改成短时身体 Tool Loop？
- Information 采用固定被动读取、主动工具读取，还是混合模式？
- 独立 Action Runtime 是恢复、改名为内部控制面，还是由 controller/dispatcher 完全取代？
- 结果协议如何同时表达终止原因、实际效果和观察事实？
- 哪些 ref 需要保留为内部执行句柄，哪些不应暴露给模型？

这些问题的事实修正和选项编号记录在[具身决策登记册](./proposals/embodiment-decision-register.md)。在明确决策以前，[架构反思](./proposals/embodiment-architecture-reflection.md)不能覆盖现有接受基线。
