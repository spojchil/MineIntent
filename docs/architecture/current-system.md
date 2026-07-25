---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-23
applies_to: codex/trustworthy-passive-context@57d438e
---

# 当前系统实况

本文描述最新实验分支的实际代码结构。它不是目标架构，也不把实验模块升级为已接受决定。

## 一眼看懂

```text
Player chat / startup / action result / danger
                         │
                         ▼
                  CompanionRuntime
                         │
       ┌─────────────────┼──────────────────┐
       ▼                 ▼                  ▼
InformationRuntime   FileMemoryStore     JSONL Journal
       │
       ▼
fixed passive reads: current / inventory / sound / viewport
       │
       ▼
ContextPackageV2 → Python Agent Service → raw JSON decision
       │
       ▼
TypeScript validation and effect dispatch
       │
       ├── speech / activity / intent / memory
       │
       └── embodied_intent
              → Grounding
              → BehaviorSynthesizer
              → VisualAttentionController
              → Motor.look
              → current visibility re-check (fresh revision not enforced)
              → terminal evidence and dependent speech
```

## 事实权威边界

| 层 | 可以知道/决定什么 | 不能做什么 |
|---|---|---|
| Mineflayer driver | 协议状态、物理姿态、发送底层动作 | 不能自动成为同伴认知事实 |
| source ports / providers | 把受审查的数据投影成有界信息 | 不能把 raw snapshot、tracked 坐标或 loaded world 旁路给模型 |
| Information Runtime | grant、schema、scope、revision、ref/cursor、预算、trace 接口 | 契约虽列出 17 个 interface ID，生产只注册 4 个，不向模型提供交互式工具循环，生产 trace 也使用默认 no-op sink |
| Context Composer | 按固定计划读取四个被动接口并保留 evidence | 不能动态修复 provider schema 漂移；当前 revision 写死在 read plan 中 |
| Python Agent Service | prompt、HTTP、严格 JSON 传输 | 不拥有业务 schema，不执行工具，不做重试/repair loop |
| TypeScript Decision Protocol | 校验 context binding、效果数量、引用和结构 | 不能证明模型语言语义本身为真 |
| Grounding | 把 viewport block/entity ref，或消息中的表达式，绑定到有时效 handle | 不等于通用世界理解；消息路径当前总是绑定发送者 |
| Behavior | 把支持的 grounded goal 合成短计划 | 当前只认识 `self.attention_includes` |
| Controller / Motor | 生产 Controller 有界、可取消地改变 gaze；底层 Motor 有 `look`、`dig`、`releaseAll` | 生产 Behavior 不会移动、挖掘、交互、战斗或选择物品；结果复查不保证新 revision |
| Journal / Memory | 保存事件和最小证据记忆 | 当前不是完整事件溯源或长期关系记忆系统 |

## 触发与持续性

生产决策只有四类触发：

- `startup`
- `player_chat`
- `action_result`
- `danger`

协议里虽然有下一次主动关注时间，但当前没有 idle timer、机会调度器或 proactive trigger。因此“持续同伴”目前主要表现为进程持续在线，而不是会主动参与生活。

## 信息路径

每次决策固定读取：

1. `current_status`
2. `inventory_information`
3. `sound_information`
4. `viewport_information`

优点是信息面稳定、调用次数有界、很难被模型循环导出全量世界；代价是模型无法主动补问细节，固定读取也会把不相关信息带入每次决策。

接口契约声明了 17 个 interface ID，但生产 Registry 只注册上述 4 个 Provider。其余 ID 是契约占位或测试表面，不能据此推断 UI、F3、聊天、配方、生命周期或诊断 Provider 已上线。

目前的 Context read plan 把 schema revision 写在源码常量中。Provider revision 改变后可能出现 `stale_schema` omission，而不是自动从 Registry 取得版本。这是实现债务，不是期望架构。

`visibleBlocks` 每次完整 Read 都进入 `65 × 65 × 41 = 173,225` 次候选格循环，之后对幸存项继续做邻面和光学遮挡检查。实现会定期让出事件循环并支持取消，但固定被动读取意味着这个成本会进入每次生产决策；目前没有基准测试证明它满足长期预算。

## 模型路径

Agent Service 发起一次 OpenAI-compatible `chat/completions` 请求：

- 使用 `response_format: {type: "json_object"}`；
- 不发送 `tools`、`tool_choice` 或 `functions`；
- 不执行多轮工具调用；
- 不做格式修复重试；
- TypeScript 侧再次用 Zod 验证全部业务字段。

当前人类可读接口见[模型接口参考](../guides/model-interface.md)。

## 身体路径

Capability Catalog 宣称 gaze、locomotion、primary/secondary interaction、inventory selection 和 wait 六类 affordance，但生产 Behavior 只实现 gaze。Catalog 当前混合了“目标身体愿景”和“实际上线能力”，可能诱导模型产生必然被拒的意图。

可信注视的实际过程是：

1. 模型提出语义目标并选择本轮 viewport block/entity ref，或消息中的表达式；
2. Grounding 生成有世界、epoch、decision/effect scope 的 handle；
3. Behavior 只接受 `self.attention_includes(self, subject)`；
4. 已知空间目标直接渐进转向，只有身份但方向未知时做有限连续扫描；
5. Controller 每一步重新检查 scope、deadline 和目标；
6. Controller 在结果阶段再次读取可见性；当前没有比较前后 perception revision，目标若一开始已对准也可能以 0 次 `look` 完成；
7. 当前可见性复查通过后才释放依赖 terminal success 的话术，因此有结果门控，但还不能称为严格的“新观察证明”。

这是一条以可信性为目标的窄 vertical slice，而非已完成验收的通用身体架构。

Grounding 的两条选择路径并不等价：`context_ref` 会使用本轮 Information evidence；`message_referent` 当前只检查表达式确实出现在指定消息中，之后无论表达式是“我”还是“这棵树/那里”，都把目标设为消息发送者身份。这是已知语义错误，不能把后者描述成通用自然语言指代消解。

## 状态、语言和记忆

- `CompanionRuntime` 是当前集中编排点，文件体量已经较大。
- Speech Scheduler 支持节流、压力、依赖动作状态的话术和取消。
- 只有显式依赖 terminal success 的 speech 会等待结果；即时文本没有 Claim Policy。若队首 terminal speech 的条件与实际终态不匹配，该条目当前不会出队，还可能造成后续话术 head-of-line blocking。
- 共同活动、意图、关注和最近事件有最小状态表达。
- Memory Store 保存带 evidence ID 的 JSON 记录，但检索只使用关键词重合与时间衰减。
- 任何近期记忆即使关键词不重合仍有正的时间分，因此可能挤入无关记忆。
- 当前没有统一 Claim Policy 对 speech、activity 和 memory 的自然语言内容做语义事实校验。
- 所有生产 Context 的 `route` 都硬编码为 `new`；协议声明的其他 route 尚无消费者。
- `next_attention` 当前只保存 `waitFor` 和 `focus`，忽略 `earliestProactiveAt`、`expiresAt` 与 `embodiedIntentIds`。

## 当前不存在的模块

以下名字会出现在早期设计或目标架构中，但不能据此认为代码已经存在：

- 独立 `ActionRuntime`
- `src/skills/`
- 全知或普通 Pathfinder
- locomotion controller
- primary/secondary interaction controller
- inventory selection controller
- 完整 UI Context / Screen / Overlay provider 体系
- 认知地图和普通玩家式探索导航
- 记忆冲突、纠正、取代、反思和关系连续性服务
- 主动陪伴调度器
- model-facing Tool Loop

## 当前验证盲区

- Paper companion 场景使用确定性的 `PrototypeScenarioModel`，不经过真实 DeepSeek 请求。
- 该场景用 `message_referent` 让同伴扫描说话者，没有覆盖 `context_ref` 的生产闭环。
- 最新实验提交没有对应 GitHub Actions、commit status 或 Paper run；场景代码存在不等于该提交已经真实运行通过。
- Node 22.23.1 下有一个 visual-attention deadline 测试被取消并使测试进程退出 1；Node 24 下同一套测试通过。

## 与其他架构文档的关系

- [目标系统设计](./target-system.md)保存长期结构设想，不能当成模块清单。
- [同伴运行时](./companion-runtime.md)与[领域事件](./domain-events-and-journal.md)是早期接受设计，但实现仅部分对齐。
- [Information Runtime](./information-runtime.md)的公共底座最接近设计—实现一致。
- [具身架构反思](../proposals/embodiment-architecture-reflection.md)提出替代路线，但仍是 proposal。
- [项目状态](../current-status.md)负责记录分支、能力和验证结果。
