---
status: accepted
authority: normative
implementation: diverged
last_verified: 2026-07-23
scope: target
---

# MineIntent 目标系统设计

> 这是一份已接受的目标系统设计，不是当前模块清单。最新实验分支已删除图中的独立 Action Runtime，记忆、主动性、导航和大部分 controller 仍未实现。实际运行链路见[当前系统实况](./current-system.md)。原始日期：2026-07-12；上游：[产品设计](../product-design.md)；依据：[系统设计调研](../history/research-system-design.md)。

## 1. 设计目标

MineIntent 要承载的是一个持续存在于 Minecraft 世界中的 AI 同伴，而不是一个收到任务后运行到结束的 Bot。

系统必须同时满足：

- 自然语言、非语言行为和实际游戏行动属于同一个连续情境。
- AI 能在没有明确任务时陪伴，也能参与长期复杂活动。
- 玩家随时可以交谈、改变计划、要求等待或打断行动。
- 实时危险反应不依赖大模型延迟。
- 所有动作由真实游戏状态执行和验证。
- 人格、关系和共同经历能跨会话持续。
- 记忆区分背景设定、事实、经历、推测和行为经验。
- 系统可以解释一次语言或行动用了什么上下文、产生了什么结果。
- 死亡、断线、模型超时和进程重启是可恢复事件。

## 2. 非目标

本设计当前不追求：

- 让模型逐 tick 或逐按键控制玩家。
- 用固定任务树定义全部游戏行为。
- 执行模型生成的任意 JavaScript。
- 一开始支持多个 Minecraft 后端、多个同伴或任意模组。
- 用微服务拆分尚未验证的模块。
- 模拟完整人类心理或宣称 AI 具有人类意识。
- 把所有协议客户端可见信息直接交给同伴心智。

## 3. 架构原则

### 3.1 同伴心智与行动运行时解耦

同伴心智决定如何理解情境、说什么、想参与什么；Minecraft 运行时负责怎样可靠地完成动作。两者不能混成一个 Agent 类，也不能各自独立到语言与行动互不知情。

所有联系通过有因果关系的事件、意图和动作结果表达。

### 3.2 事件驱动，而非模型轮询世界

游戏状态持续变化，但大多数变化不值得模型思考。底层先将协议事件整理为稳定、可解释的领域事件，只有具有社交、活动或策略意义的事件才触发同伴决策。

### 3.3 一个持续同伴，多种运行通道

第一版只有一个逻辑上的同伴心智。可以使用不同模型完成主决策、记忆整理或复杂规划，但这些是同一同伴的内部工作，不表现为多个互相争论的角色。

### 3.4 真实状态优先

模型提出的意图、解释和记忆候选都不是事实。Minecraft 当前状态、已记录事件和动作验证结果具有更高事实权威，但协议真相不会自动成为角色知道的事实；Companion 只能通过合法信息接口、观察、共享信息和有来源记忆获得知识。

### 3.5 决策可以自然，执行必须受约束

人格与社交判断主要由自然语言同伴档案、关系记忆和模型完成；信息先经过版本化、可发现的合法读取接口，世界副作用再经过 Grounding、受约束控制、安全与结果验证。

模型表达意图和期望结果，不直接读取 Mineflayer 全局状态，也不选择协议目标或执行任意代码。连续控制器可以在已合法选定的对象和用途范围内使用精确实现数据、循环和 Mineflayer 能力；关键约束是信息来源、目标授权、作用域、取消和结果证据，而不是强迫 Behavior 逐 tick 模拟键鼠。任务宏和对象专用命令是否合理必须在 v0.3 控制设计中另行判断，v0.2 不预先固化。

### 3.6 从可恢复的单体开始

第一版采用一个 Node.js 进程中的模块化单体。模块边界清晰，但不引入网络分布式一致性。持久数据与 Minecraft 连接生命周期独立。

## 4. 总体架构

```text
Minecraft Server
      ↕ protocol
┌───────────────────────────────────────────────────────────┐
│ MineIntent Process                                        │
│                                                           │
│  Minecraft Adapter                                        │
│  ├── protocol state / raw events                          │
│  ├── atomic controls                                      │
│  ├── world truth queries (driver-only)                    │
│  ├── information adapters / UI context                    │
│  └── information catalog / help / read                    │
│             ↓                                             │
│  Perception & Event Layer ───────────────┐                 │
│  ├── visibility / knowledge boundary    │                 │
│  ├── event normalization                │                 │
│  └── salience / aggregation             │                 │
│             ↓                            │                 │
│  Event Journal → State Projections ←─────┤                 │
│             ↓                            │                 │
│  Companion Runtime                      │                 │
│  ├── attention & trigger routing        │                 │
│  ├── conversation state                 │                 │
│  ├── shared activity                    │                 │
│  ├── intent and action coordination     │                 │
│  └── proactive opportunity scheduler    │                 │
│       ↓              ↓                   │                 │
│  Context Composer   Grounding                              │
│       ↓              ↓                                     │
│  Model Gateway     Behavior Synthesizer                    │
│       ↓              ↓                                     │
│  Decision Contract → Action Runtime ─────┘                 │
│                      ├── bounded controller plans           │
│                      ├── resource locks                     │
│                      └── cancellation / verification        │
│       ↓                                                   │
│  Speech / Activity / Action / Memory coordination          │
│                                                           │
│  Memory System          Journal / Telemetry / Debug API     │
└───────────────────────────────────────────────────────────┘
```

## 5. 四种时间尺度

### 5.1 Tick 与亚秒级：本地控制和反射

不调用大模型：

- 物理移动和碰撞。
- 攻击冷却与基础战斗控制。
- 防止明显跌落、溺水或灼烧。
- 低血量逃生触发。
- 服务端位置修正。
- 动作取消和资源释放。

本层可以中断普通动作，但必须产生事件，让同伴之后知道自己为何停下或逃跑。

本层包含持续运行的 Threat Supervisor。它不等到生命值很低才接管，而是根据预计受伤时间、可能伤害、当前生命余量、威胁移动趋势、当前动作对躲避的限制以及可用防护评估危险：

| 等级 | 含义 | 处理 |
|---|---|---|
| 注意 | 存在潜在威胁，尚无迫近伤害 | 更新认知观察，必要时触发模型决策 |
| 防护 | 短时间内可能受伤 | 本地调整距离、朝向、防护或当前动作 |
| 紧急 | 即将遭受重大伤害或死亡 | 抢占身体动作，立即逃生或防御 |
| 恢复 | 迫近危险已经解除 | 产生结果事件，将控制交回共同活动与模型决策 |

具体阈值属于安全与战斗详细设计。无论模型是否正在推理，Threat Supervisor 都持续负责迫近危险，不能出现“模型思考时身体无人控制”的窗口。

### 5.2 秒到分钟级：具身行为与内部计划

执行导航、跟随、采集、制作、建造或战斗等连续行为。动作运行时持续观察进度、前置条件和失败，不需要模型逐步遥控。

### 5.3 事件级：同伴决策

在玩家聊天、共同活动变化、重要发现、动作结束、危险解除、计划需要调整或自然主动机会出现时调用模型。

### 5.4 跨会话：记忆整理和长期计划

在低优先级后台运行：

- 情景记忆提取。
- 事实核对与去重。
- 关系记忆整理。
- 旧记忆衰减、合并和反思。
- 长期活动检查点保存。

后台工作不能阻塞实时聊天、动作取消或危险反射。

## 6. 事件模型

领域事件、事件日志、投影和重放的实现级契约见 [领域事件与事件日志协议](./domain-events-and-journal.md)。本节保留系统级概览。

### 6.1 事件是系统连接点

所有对同伴连续性有意义的输入和输出都表示为事件。事件至少包含：

```ts
interface DomainEvent<T = unknown> {
  id: string
  type: string
  occurredAt: string
  recordedAt: string
  source: 'minecraft' | 'player' | 'companion' | 'action' | 'system'
  sessionId: string
  worldId: string
  correlationId?: string
  causationId?: string
  visibility: 'internal' | 'perceived' | 'shared'
  payload: T
}
```

- `correlationId` 连接同一次对话—决策—行动—结果。
- `causationId` 指明这个事件由哪个事件直接造成。
- `visibility` 区分程序知道、同伴合理感知到和双方明确共享的信息。

### 6.2 主要事件族

```text
Lifecycle
  connected / spawned / disconnected / respawned / dimension_changed

Player interaction
  chat_received / player_joined / player_left / player_gesture
  item_given / player_started_activity / player_in_danger

World
  time_phase_changed / weather_changed / entity_appeared
  important_place_observed / resource_observed / threat_changed
  threat_protective_action / threat_emergency_takeover / threat_recovered

Companion
  attention_changed / decision_started / decision_finished
  speech_requested / speech_sent / activity_changed / intent_changed
  activity_alignment_uncertain / activity_alignment_restored

Action
  action_requested / accepted / started / progressed
  suspended / cancelled / completed / failed

Memory
  candidate_created / memory_written / corrected / superseded / deleted
  reflection_created
```

高频原始包和每 tick 状态不会全部写入长期事件日志。它们先在 Minecraft Adapter 中更新即时状态；只有状态边界、重要变化和可复现动作轨迹进入领域事件。

### 6.3 日志与投影

事件日志是行为历史和因果审计来源；当前状态由投影得到，例如：

- 当前连接和角色状态。
- 最近聊天与对话参与者。
- 当前共同活动。
- 当前动作和占用资源。
- 最近威胁与重要观察。
- 尚未处理的承诺和记忆候选。

Minecraft 在线状态始终是当前世界事实的权威来源。重启后投影先从持久记录恢复，再与服务器实际状态重新核对。

## 7. Companion Runtime

持续状态、消息打断、共同活动、活动对齐和恢复的实现级契约见 [同伴运行时状态机与活动对齐](./companion-runtime.md)。本节保留系统级概览。

Companion Runtime 是系统协调核心，但不直接操作 Mineflayer，也不保存全部世界数据。

### 7.1 持续状态

```text
Presence
  offline | connecting | active | recovering | stopping

Attention
  当前关注对象、事件或活动
  是否处于高压力状态
  最近为何转移注意力

Conversation
  当前参与者
  最近话题和未回答问题
  玩家消息是否明确对同伴说
  是否正在等待合适的回复时机

Shared Activity
  我们在做什么
  当前阶段与状态
  双方目前是否仍与活动对齐
  玩家和同伴各自的分工
  已协商约束和相关地点
  尚未解决的问题

Intent
  同伴当前短期打算
  意图来源与适用条件
  是否仍与共同活动一致

Action
  正在执行、暂停或等待验证的动作
```

### 7.2 共同活动模型

共同活动是一等领域对象，不等同于任务队列：

```ts
interface SharedActivity {
  id: string
  summary: string
  status: 'proposed' | 'active' | 'paused' | 'completed' | 'abandoned'
  alignment: 'aligned' | 'uncertain' | 'diverged'
  participants: Array<{
    playerId: string
    role?: string
    currentContribution?: string
  }>
  agreedFacts: string[]
  openQuestions: string[]
  referencedPlaces: string[]
  startedByEventId: string
  lastUpdatedByEventId: string
}
```

自然语言摘要保留活动的开放性；结构化字段支持恢复、检索和行动协调。模型可以提出更新，运行时根据真实事件与现有状态应用。

### 7.3 意图不是固定任务树

意图描述同伴当前准备做什么以及为什么，例如“与玩家一起回到刚才砍树的位置”，而不是预定义攻略节点。

复杂活动可以分解为短期步骤，但步骤随情境变化，不要求一次规划完整路线。

## 8. 主循环

MineIntent 没有一个让模型永久自我提示的无限循环。持续性来自事件、动作进展和自然机会。

```text
1. 接收原始 Minecraft、玩家、动作或定时事件
2. 规范化并写入领域事件
3. 更新当前状态投影
4. 运行确定性即时处理
   ├── 安全反射
   ├── 明确停止
   └── 动作生命周期更新
5. Attention Router 判断：
   ├── 无需模型，只记录
   ├── 合并到当前待处理情境
   ├── 调整或打断正在运行的决策
   └── 创建新的同伴决策运行
6. Context Composer 生成有预算、有来源的上下文快照
7. 模型返回 Decision Contract
8. 校验决策是否仍适用于当前世界版本
9. 协调语言、共同活动更新、动作和记忆候选
10. 后续真实结果再次成为事件
```

### 8.1 决策运行串行化

同一同伴同一时刻只有一个主要决策运行可以提交状态变更。新事件可以：

- `interrupt`：停止旧推理并用新情境重新决策。
- `steer`：补充到仍在运行的推理；若模型接口不支持，则废弃旧结果后重跑。
- `collect`：短时间聚合多条相关事件。
- `follow_up`：当前运行结束后立即处理。
- `observe`：只更新状态，不触发推理。

第一版的模型接口不假设支持真正的运行中上下文注入。因此当前执行策略中，`steer` 与 `interrupt` 都会取消或废弃旧推理结果，再使用新增事件重新组装上下文。保留 `steer` 这一语义分类，是为了表达“玩家在调整同一件事”并为未来支持增量转向的模型接口预留边界，而不是承诺当前能够无重跑地修改推理。

### 8.2 快照版本与过期决策

每次模型运行绑定一个 `contextRevision`。提交决策前运行时重新检查：

- 目标玩家是否仍在线和可见。
- 当前活动是否已变化。
- 动作前置条件是否仍成立。
- 是否出现更高优先级事件。

过期决策不能直接执行。可安全保留的语言回复可以重新评估；动作必须重新验证或重新决策。

## 9. Attention Router 与主动行为

### 9.1 Attention Router

Attention Router 不负责完整理解，只负责控制模型调用和响应时机。它使用：

- 事件类型和紧急等级。
- 当前动作是否可中断。
- 玩家是否直接呼叫同伴。
- 当前共同活动。
- 最近是否已有相同事件。
- 当前模型运行状态。
- 同伴档案和近期互动对主动性的影响。

明确停止、死亡和严重危险使用确定性优先路径；复杂社交含义交给主模型判断。

### 9.2 活动对齐监测

Activity Alignment Monitor 以低频、低成本方式判断当前共同活动是否可能陈旧。它观察：

- 玩家是否持续远离活动区域或同伴。
- 玩家是否较长时间进行与当前分工无关的行为。
- 玩家是否停止了原本的贡献。
- 原活动是否已经事实完成、受阻或失去前置条件。
- 偏离是短暂插曲还是逐渐形成新的活动方向。

监测器不直接判定玩家意图，也不自动结束活动。证据达到一定程度时，它发布 `companion.activity.alignment_became_uncertain` 事件，由 Reducer 将 `alignment` 改为 `uncertain`。主模型可以选择继续观察、等待、跟随、暂停自己的动作、自然询问或更新活动。

询问需要冷却和去重。玩家短暂追逐动物、捡拾物品或整理背包不应反复触发确认；如果玩家恢复相关行为，监测器产生 `companion.activity.alignment_restored`。

### 9.3 主动机会而非定时发言

Proactive Scheduler 只产生“可能适合主动参与”的机会事件，不直接生成聊天：

- 长途同行一段时间且没有高压力事件。
- 完成一段共同工作。
- 回到有共同意义的地点。
- 发现与旧经历有关的事物。
- 天黑、资源不足或计划自然转折。
- 玩家长时间无明确活动且同伴有合理建议。

主模型可以选择说话、做一个非语言动作、提出建议或保持沉默。系统记录玩家的明确反馈和长期互动效果，用于以后构造情境，但不把主动程度简化成一个固定数值。

## 10. 上下文组装

上下文包、来源信任、预算截断和确定性组装的实现级契约见 [同伴决策协议与上下文包](./decision-contract-and-context.md)。

### 10.1 上下文层次

每次模型运行按优先级组装：

1. 产品底层约束。
2. 可编辑同伴档案。
3. 当前玩家档案和关系核心摘要。
4. 当前共同活动、意图和动作状态。
5. 新触发事件及近期互动。
6. 当前情境所需的认知观察。
7. 按需检索的相关长期记忆。
8. 当前身体可供性、限制、证据要求和可靠性摘要。
9. 仅在需要时加载的游戏知识和经验证行为经验；不包含可调用执行目录。

### 10.2 上下文包

Context Composer 输出的不只是文本，还保留来源清单：

```ts
interface ContextPackage {
  revision: number
  triggerEventIds: string[]
  sections: Array<{
    kind: string
    content: string
    sourceIds: string[]
    trust: 'authoritative' | 'observed' | 'remembered' | 'inferred' | 'profile'
    tokenEstimate: number
    truncated: boolean
  }>
}
```

调试界面可以回答“这次为什么想起旧矿洞”“这句话是否来自同伴档案”以及“哪些信息因预算没有进入上下文”。

### 10.3 预算策略

- 产品约束和同伴档案稳定、紧凑并优先保留。
- 当前共同活动和刚发生的玩家消息不可被旧聊天挤出。
- 原始世界数据先压缩为认知观察。
- 旧聊天通过情景记忆检索，不无限累积。
- 详细游戏知识与经验证行为经验按需加载；输入模板或协议事务名不进入模型上下文。
- 压缩前先产生记忆候选，避免重要内容只存在于即将丢弃的上下文中。

## 11. 决策接口

完整的版本化 schema、具身意图 Grounding、内部计划预检、过期拒绝、输出修复和效果提交语义见 [同伴决策协议与上下文包](./decision-contract-and-context.md)。本节保留系统级概览。

模型不直接调用 Mineflayer，也不输出可执行代码。一次主决策返回一个受版本控制的结构化结果：

```ts
interface CompanionDecisionV2 {
  protocol: 'mineintent.decision.v2'
  runId: string
  context: DecisionContextRef
  summary: string
  effects: DecisionEffectV2[]
}

type DecisionEffectV2 =
  | SpeechEffectV2
  | ActivityEffect
  | IntentEffect
  | EmbodiedIntentEffect
  | MemoryCandidateEffect
  | AttentionEffectV2

interface SpeechEffectV2 {
  kind: 'speech'
  timing: 'now' | 'after_intent_accepted' | 'after_intent_terminal'
  dependsOn?: string[]
  text: string
}

interface AttentionEffectV2 {
  kind: 'next_attention'
  waitFor: Array<'player_message' | 'embodied_progress' | 'embodied_terminal' | 'world_event' | 'natural_opportunity'>
  embodiedIntentIds?: string[]
}

type SemanticGoalExpressionV1 =
  | {
      kind: 'state'
      state: {
        id: string
        concept: string
        description: string
        arguments: Record<string, unknown>
      }
    }
  | { kind: 'all' | 'any'; goals: SemanticGoalExpressionV1[] }

interface EmbodiedIntentEffect {
  kind: 'embodied_intent'
  id: string
  summary: string
  desiredOutcome: string
  semanticGoal: {
    schema: 'mineintent.semantic-goal.v1'
    objective: SemanticGoalExpressionV1
    methodGuidance: Array<{
      description: string
      referentRoles: string[]
      strength: 'required' | 'preferred' | 'suggested'
    }>
  }
  referents: Array<{
    role: string
    selection:
      | { kind: 'context_ref'; ref: string }
      | { kind: 'message_referent'; eventId: string; expression: string }
  }>
}
```

这是系统通信协议，不是人格规则。模型仍以自然语言理解和表达情境；结构只约束哪些状态变化和动作可以进入系统。

V2 schema 没有选择技能、输入模板或协议事务的字段，也不接受世界坐标、协议实体 ID、Mineflayer 对象或伪造引用。`context_ref` 必须由 Context Composer 在本轮签发；`message_referent` 必须引用实际消息中的表达。两者都只是语义选择，尚不是可执行目标。`desiredOutcome` 仅用于人类可读的解释与调试；Behavior Synthesizer 只消费经验证的通用 `semanticGoal` 契约。设计只约定目标组合、语义引用、方法约束和拒绝结果，不枚举“视觉注意、砍树、跟随”等任务谓词，也不规定状态关系与输入序列的映射。即使自然语言碰巧出现某个内部名称，也不会因此获得执行权限，更不能因为词面相同就拒绝正常语言。

系统不要求或持久保存模型的私有推理过程，只保存用于产品解释的简短意图、输入来源和最终决定。

### 11.1 同一决策中的动作仲裁

模型决策中的具身意图先经过如下边界：

```text
自然语言理解与具身意图
→ Grounding：把语义指代绑定为有来源、有时效的 grounded referent
→ Behavior Synthesizer：根据可供性、Epistemic Map 和当前身体状态形成内部计划
→ Action Runtime：校验计划作用域、资源、取消与安全约束
→ Controller / Protocol Driver：执行有界连续控制与协议事务
→ Perception / Outcome Verification：验证实际结果
```

Grounding 不用固定词表替代语言理解，也不允许模型伪造目标句柄。它验证本轮引用、话语角色、合法 Information Read、Cognitive Observation、当前活动目标和有来源记忆，并将 `semanticGoal` 中的角色引用改写为有时效的 grounded handle。身份已确定但空间位置未知时，Grounding 以 `partial` 结果继续；身份歧义、证据无效或当前具身目标已知不可用时才在 Grounding 停止。

Behavior Synthesizer 可以由确定性策略、模型辅助规划或两者组合实现；无论实现方式，它只能看到 grounded intent、角色已知空间和身体可供性，输出都必须通过相同的计划作用域、资源、安全与证据预检。连续控制器可以获得完成已选行为所需的 scoped control view，但不能搜索新目标或把精确实现数据回流成角色知识。

Behavior Synthesizer 不接收玩家原话或 `desiredOutcome`，也不得以任何自然语言中是否包含“看、砍、跟随”等词做固定命令枚举匹配。它只消费结构化目标契约、grounded handle、方法约束和可供性，自行合成可验证计划。计划和控制契约的最终层级留给 v0.3 设计；当前只确定它们不能绕过合法信息与证据边界。

例如收到朋友发来的“看向我”时，模型依语境理解“我”指说话者，并表达“自己的视觉注意与说话者建立关系”这一期望状态。Grounding 将 `speaker` 绑定到有证据的实体；如果身份已知而方向未知，则以 `partial` 进入行为合成。Synthesizer 可合成扫描、移向已知候选区域或等待新观察，不能读取墙后/身后的协议精确坐标直接转向。如果无法通过身体行为补足信息，Synthesizer 只返回 `information_needed`，由 Companion 决定是否自然询问、等待或放弃。持续注视表示同一具身意图仍有效，由行为控制循环继续满足，不是另一个 `track_player` 技能。

同样，“家里木材不够，去砍这棵树”的上层目标是获得可用木材，树是指定来源或方法引导；“这棵树挡路，去砍了它”的上层目标是移除指定阻挡物并恢复可通行性，斧头砍只是候选实现。两者可以合成相同的局部身体计划，但完成证据和后续决策不同。系统只提供通用契约和受约束的身体接口，不注册 `chop_tree_for_wood`/`chop_tree_for_path` 之类任务技能。

## 12. 语言与行动一致性

### 12.1 协调提交

模型说“我去拿木头”时，动作不能在语言发送后才发现参数无效。Decision Coordinator 按顺序：

1. 校验结构和上下文版本。
2. 校验语义引用并执行 Grounding。
3. 合成有界行为计划，检查信息/目标 revision、资源、取消和安全约束。
4. 接受、拒绝，或因歧义返回结构化 `needs_clarification`。
5. 只有具身意图与内部计划被接受后，发送带承诺的语言。
6. 启动计划并记录从话语、grounded referent 到身体结果的因果关系。

如果动作未被接受，运行时不发送虚假承诺，而是触发一个结构化拒绝、`needs_clarification` 或 `information_needed` 结果，供同伴决定重新表达、自然询问、等待或放弃；运行时不自动生成固定问句。

### 12.2 行动结果驱动后续语言

“完成了”“没找到”“我被挡住了”等陈述只能基于动作终止事件和世界验证生成。模型可以决定怎样自然表达，但不能决定事实结果。

### 12.3 非语言行为

看向目标、停下等待、保持距离、递交物品和跟随也属于动作系统。它们应和聊天一样受意图及资源协调，而不是作为无记录的装饰动画。

## 13. Action Runtime

v0.2 先固定[合法信息接口、Help 发现与 UI 会话](./information-access-and-ui.md)，并以[统一 Information Runtime](./information-runtime.md)作为唯一实现入口。具身控制的调研清单保留在[具身能力与接口清单](../proposals/embodiment-interface-inventory.md)，但其中曾提出的逐 tick `BodyInputPlan` 不再是接受的系统契约。

### 13.1 v0.3 待设计的控制边界

当前只确定以下不变量：

- 模型不执行任意代码，也不直接读取或调用 Mineflayer raw 对象；
- 行为目标必须来自合法 Information Read、Cognitive Observation、Grounding 或有来源记忆；
- Controller 可以在已选目标与用途范围内使用 scoped spatial resolution、循环、物理和协议能力；
- Controller 不能搜索新目标、使用全知地图替普通规划，也不能把精确实现数据回流成认知事实；
- 每个运行行为可取消、有资源协调、超时、部分副作用和结果验证；
- `chop_tree_for_wood` 等将目的、对象和方法全部固化的命令不能因为实现方便就成为主模型词表。

v0.3 需要在“逐 tick 键鼠模拟”和“高层任务宏”之间设计合适的连续控制层。Mineflayer 的 `lookAt`、`dig`、Pathfinder 等既不默认禁止，也不默认可信；是否使用由信息来源、目标授权、控制作用域和结果证据共同决定。

### 13.2 身体资源与长期活动

同伴只有一个身体。资源锁仍负责阻止视角、移动、交互、物品栏和 screen 控制冲突，但资源命名和控制器粒度在 v0.3 重新确定，不由 v0.2 的信息接口倒推动作实现。

聊天通常可以与移动并行；精确操作或高压力战斗时，Speech Scheduler 可以延迟非紧急发言。长期活动由 Companion Runtime 维护，不能退化成运行数小时、无法交谈和取消的单个 Flow。

### 13.3 本地反射

反射层拥有临时优先权，例如停止走向悬崖或低血量撤退。反射结束后返回：

- 触发原因。
- 采取的动作。
- 当前结果。
- 被中断的原动作是否可以恢复。

Companion Runtime 再决定解释、恢复、改变计划或向玩家求助。

### 13.4 行为可靠性

系统按 Minecraft 版本、环境、具身目标特征和输入计划模式统计成功率、常见失败和平均耗时。可靠性数据只影响规划启发与身体可供性摘要，不能把失败静默隐藏，也不能自动生成新接口、任务 AST 叶子或对象专用 skill。

学习到的经验先作为带来源、可失效的规划知识候选，经离线或受控环境验证后才可影响启发式；即使反复成功，也不能升格为“砍树”“移除对象”等可调用宏命令。

## 14. 感知与认知边界

所有客户端可读信息先进入 [统一 Information Runtime](./information-runtime.md)：强类型 Provider 经最小只读 source port 生成各接口投影，Runtime 统一执行 Registry、audience、schema、scope、selector/cursor、预算、错误、证据和 trace。当前生产决策路径不向模型注册 `information_catalog` 或 `information` 工具；Context Composer 按触发事件确定性读取有界的被动观察集，并将完整的 Read envelope 作为有来源 Context 注入。模型可用观察帧内的量化 `[right, up, forward]` 相对坐标理解空间关系，但只能用同项签发的 opaque ref 选择目标，不能提交坐标、协议 ID 或查询表达式。Context 和模型不得直接读取 Backend snapshot，也不设兼容旁路。字段全集与 UI 语义见[合法信息接口、Help 发现与 UI 会话](./information-access-and-ui.md)；实体、方块、声音、局部空间、玩家行为、可见性状态机和防信息泄漏测试的实现级契约见 [实体、方块、声音与玩家行为的认知感知模型](./cognitive-perception.md)，源码依据见 [Minecraft 认知感知源码调研](../history/research-cognitive-perception.md)。

当前 UI 由独立 `ui_context` 表达：断线/过渡期主表面为 none，play 中主表面在 world 与一个 screen session 之间互斥；输入归属另行表达为 world、当前 screen、聊天或 none；HUD、F3、聊天、Tab、boss bar 和字幕作为可叠加 overlays。screen 内容通过当前 instance/revision 的信息接口读取，不把 `isInUi`、输入焦点和完整 screen 内容混成一个快照。

### 14.1 三层世界表示

```text
Protocol State
  Mineflayer 已收到的完整即时数据
        ├──→ Safety Control View
        │      下一输入的碰撞、跌落和协议合法性；不能选择隐藏路线或读取未感知实体
        └──→ Perception Boundary
                 ↓
             Cognitive Observation
               一个合理玩家当前可以感知的信息
                 ↓
             Epistemic Map
               当前观察、亲自探索和仍然记得的空间
                 ↓
             Intentional Planner
               普通行动只能使用角色已知空间
```

同伴模型默认只接收 Cognitive Observation 和有来源的记忆。原始已加载区块只能用于硬安全和协议校验，不能让普通寻路直接利用未感知迷宫、遮挡后通道或隐藏资源。普通路线由 Epistemic Map 规划；未知区域必须通过观察和探索转为已知。Mineflayer 的长期角色边界见 [ADR 0005](../decisions/0005-limit-mineflayer-to-protocol-driver.md)。

Safety Control 只校验 Motor Controller 已选定且即将执行的一个局部动作，并只返回最小的允许、拒绝或风险结果。Intentional Planner 不得用它批量探测远端、展开 A*、排序路线或预判可达性；安全拒绝也不向 Epistemic Map、语言或记忆写入隐藏地形知识。若需要知道“为什么过不去”，同伴必须转头、靠近或以其他正常感知取得证据。

### 14.2 观察类型

- 自身：生命、饥饿、装备、背包摘要、状态效果。
- 空间：可见方块、地形、出入口、相对方向和遮挡。
- 实体：当前可见、近身感知或最近失去视野的玩家、生物、掉落物和威胁。
- 声音：客户端真实收到并经过距离、方向、类别、遮挡和聚合处理的声音观察。
- 社交：聊天、朝向、距离、递交物品、跟随和活动变化。
- 事件：受伤、死亡、昼夜、天气、维度和重要发现。
- 记忆提示：已知地点、最近经过路线和与当前情境相关的旧经历。

### 14.3 注意与聚合

Perception Layer 会：

- 将高频位置更新聚合为“玩家开始离开”“已经跟随一段距离”。
- 将威胁变化聚合为出现、升级、解除。
- 对重复普通事件去重。
- 保留事件来源和可见性判断。
- 允许当前 grounded intent 请求更详细的局部观察，而不扩大模型的全局知识。

非语言行为通常是含歧义的观察，不能直接变成确定的玩家意图。

## 15. 记忆系统

记忆记录、同伴档案版本、候选验证、检索排序和重启冲突协调的实现级契约见 [记忆模型、档案版本与冲突协调](./memory-model-and-profile-versioning.md)。本节保留系统级概览。

### 15.1 记忆类型

```text
Profile
  用户主动编辑的同伴档案与初始背景

Working State
  当前对话、共同活动、意图、动作和未解决问题

Episodic Memory
  有时间、地点、参与者和证据的共同经历

World Knowledge
  已观察并验证的地点、设施、容器和世界事实

Social Memory
  玩家明确偏好、约定、共同称呼及关系理解

Commitments
  有状态、条件和完成期限的暂时约定

Procedural Experience
  行为计划成功、失败、环境条件和可复用经验

Raw Journal
  可回溯的原始领域事件与动作轨迹
```

### 15.2 记忆记录

```ts
interface MemoryRecord {
  id: string
  kind: string
  content: string
  source: 'profile' | 'observed' | 'player_stated' | 'derived'
  evidenceIds: string[]
  confidence: number
  validFrom: string
  validUntil?: string
  status: 'active' | 'superseded' | 'disputed' | 'deleted'
  subjects: string[]
  placeIds: string[]
  activityId?: string
  profileVersion?: string
  derivedFromProfileVersion?: string
}
```

初始背景可影响人格和关系表达，但涉及当前世界的事实只有经过观察后才能标记为 verified world knowledge。

### 15.3 写入流程

```text
真实事件或模型提出候选
→ 来源与证据校验
→ 分类并去重
→ 写入情景记忆或事实记录
→ 必要时更新当前活动/承诺
→ 后台整理形成长期摘要或关系反思
```

- 明确玩家陈述可以保存为“玩家说过”，不自动等同于永久偏好。
- 模型推断的关系理解标记为 `derived`，不能覆盖事实。
- 纠正生成新记录并 supersede 旧记录，不悄悄重写历史。
- 删除对正常检索立即生效；审计保留策略在产品隐私设计中另行确定。

同伴档案每次编辑产生带生效时间的新版本。情景记忆继续描述当时真实发生的事情，不因新档案而重写；依赖旧档案形成、并声称同伴“当前是什么样”的 `derived` 记忆进入待复核状态。当前行为优先使用最新档案，历史回忆仍可说明以前的行为方式。撤销档案编辑会形成新的有效版本，而不是删除中间历史。

### 15.4 检索

检索综合：

- 语义相关性。
- 时间和近期访问。
- 事件重要性。
- 当前玩家与其他参与者。
- 地点和维度。
- 当前共同活动。
- 来源可信度和事实状态。
- 玩家明确固定或纠正的内容。

检索先命中细粒度片段，再回到完整记忆及原始证据。Context Composer 决定最终注入哪些内容。

### 15.5 反思与关系连续性

后台可以从多条共同经历形成高层关系理解，例如“玩家通常喜欢自己完成建筑外观”。该内容必须：

- 标为推断。
- 引用支持它的经历。
- 在出现反例或玩家纠正时更新。
- 不把情绪化的一次事件永久固化为玩家人格。

反思还要记录它使用的同伴档案版本。档案变化后，只重新评估与当前人格、风险倾向或表达方式有关的派生判断；关于玩家、世界和真实共同经历的记忆不能因人格编辑而被反向改写。

## 16. 模型边界

### 16.1 主同伴模型

负责：

- 社交情境理解。
- 自然语言交流。
- 共同活动与分工判断。
- 选择短期意图和调用能力。
- 处理动作结果与失败。
- 提出记忆候选。

### 16.2 可选辅助工作

以下工作可以使用同一模型的独立低优先级调用，未来也可换成更小模型：

- 记忆提取与整理。
- 是否需要主动参与的复杂判断。
- 长期复杂活动的方案分析。
- 大段日志和历史压缩。

第一版不建立多个有独立人格的规划、批评和对话 Agent。这样可以减少人格不一致、状态同步和额外延迟。

### 16.3 确定性程序负责

- 协议状态和物理控制。
- 输入结构校验。
- 动作资源锁、超时和取消。
- 背包、方块、位置和效果验证。
- 明确停止与安全终止。
- 事件日志和恢复。
- 信息可见性边界。

## 17. 并发、打断与优先级

### 17.1 运行通道

```text
Reflex lane       实时，本地，可抢占身体动作
Action lane       有界控制器、观察检查点与结果验证，按身体资源协调
Deliberation lane 单同伴串行模型决策
Speech lane       聊天发送、节流、分段和时机控制
Background lane   记忆整理、索引、统计和压缩
```

Reflex lane 的关键路径只更新本地内存状态并把事件放入有界异步队列，不等待 SQLite、模型、日志文件或调试接口。持久化单写者随后按顺序落盘；失败时告警和重试，但不能反向阻塞实时控制。极端进程崩溃可能丢失队列末端的少量调试事件，这一取舍优先保证身体反应时延。

### 17.2 优先级

从高到低：

1. 进程安全终止和玩家明确停止。
2. 立即生存危险与死亡。
3. 连接、重生和维度切换等生命周期变化。
4. 玩家对当前行动的明确纠正或紧急警告。
5. 当前动作终止结果和共同活动关键变化。
6. 普通直接聊天。
7. 普通世界观察。
8. 主动机会和后台整理。

这个顺序只解决运行时抢占，不替代模型的社交判断。例如玩家一句玩笑不能仅凭关键词“停”触发硬停止；明确控制表达和专用安全入口需要区分。

### 17.3 取消语义

取消不是简单丢弃 Promise：

- 发出 `AbortSignal`。
- 使旧输入 lease 失效并执行幂等 `releaseAllInputs`。
- 取消未完成的 pointer/协议 phase 并清理临时监听器；普通取消不擅自关闭仍有效的界面。
- 释放身体资源锁。
- 捕获已经发生的副作用。
- 验证最终世界状态。
- 产生 `action_cancelled` 事件。

## 18. 持久化与恢复

### 18.1 第一版存储

建议：

- 同伴档案：可编辑 Markdown。
- 运行数据：本地 SQLite。
- 大型原始调试附件：按会话存放的文件。
- 配置：版本化配置文件和环境变量中的密钥。

SQLite 保存事件日志、投影检查点、记忆、活动、模型运行、动作和内部行为统计。采用单写者事务，避免第一版引入外部数据库运维。

### 18.2 恢复流程

```text
进程启动
→ 加载同伴档案和数据库
→ 恢复最后投影与未完成活动
→ 将未终止动作标记为 interrupted_by_restart
→ 连接 Minecraft
→ 获取实际位置、维度、背包、生命和附近状态
→ 对比并修正过期投影，同时产生事实失效事件
→ 生成恢复情境事件
→ 同伴决定是否继续、解释或放弃旧活动
```

断线时动作立即失去执行权，但共同活动不会自动完成或删除。重连后必须重新验证前置条件。

恢复时不能用当前状态静默改写历史记忆。协调器区分：

- 当前状态投影错误：直接按服务器状态修正。
- 带观察时间的历史事实：继续保留，例如“昨天箱子里有二十个铁锭”。
- 声称当前仍成立的世界事实：标记为 stale 或由新事实 supersede。
- 来源或解释发生实质冲突的派生判断：标记为 disputed，等待后续核对。

协调器产生 `world_fact_stale`、`world_fact_superseded` 或 `world_fact_disputed` 事件。只有差异影响当前共同活动或玩家询问时，才需要由同伴自然解释。

### 18.3 世界身份

记忆必须绑定稳定的 `worldId`，避免把一个服务器的基地和容器事实带到另一个世界。关系记忆可以跨世界存在，但具体世界事实不可以无条件迁移。

## 19. 可观察性

### 19.1 结构化轨迹

每次有意义的闭环记录：

```text
触发事件
→ 上下文来源和版本
→ 模型与调用参数
→ 结构化决策
→ 被接受或拒绝的部分
→ 语言发送
→ 动作生命周期
→ 世界验证结果
→ 记忆变更
```

### 19.2 调试状态

只读调试接口至少展示：

- 连接、位置、生命和背包摘要。
- 当前关注、共同活动和短期意图。
- 当前动作、占用资源、进度和超时。
- 待处理事件和模型运行状态。
- 最近语言、动作结果和失败。
- 本轮上下文各部分的来源和大小。
- 被检索与写入的记忆。

玩家正常游玩不需要看到内部推理或复杂面板。

### 19.3 指标

- 聊天响应延迟与主动发言频率。
- 决策调用次数、token 和成本。
- 动作成功率、取消率、超时与平均持续时间。
- 卡死和断线恢复时间。
- 记忆召回命中、纠正和冲突数量。
- 语言承诺后动作未启动或未完成的比例。
- 人工干预次数。

## 20. 外部接口与模块边界

### 20.1 第一版进程内接口

Mineflayer Backend 的生命周期、状态快照、原始协议 DTO、重连和清理契约见 [Mineflayer Backend 生命周期与状态快照](./minecraft-backend.md)。

```text
MinecraftBackend
  connect / disconnect / snapshot / subscribe / atomic controls

EventBus
  publish / subscribe / persist domain events

CompanionRuntime
  start / stop / handle events / inspect state

ContextComposer
  compose decision context with provenance and budget

ModelProvider
  run structured companion decision

ActionRuntime
  submit / cancel / inspect actions

MemoryStore
  capture / search / correct / consolidate

TelemetrySink
  record model, action, event and metric data
```

接口为测试和未来替换实现服务，不在没有第二个实现时建立复杂插件市场。

### 20.2 对外入口

第一版需要：

- CLI：启动、停止、选择档案和连接世界。
- 游戏聊天：主要产品交互。
- 只读本地调试 API：状态和事件查看。
- 本地管理入口：强制暂停、继续和安全终止。

未来语音、Web UI、MCP 或其他消息渠道通过输入/输出适配器接入，不改变 Companion Runtime 的核心事件模型。

## 21. 第一版系统切片

第一版不是先实现所有架构模块的空壳，而是完成一个纵向闭环：

### 21.1 场景

1. 同伴读取自然语言档案并加入世界。
2. 识别主要玩家并自然打招呼。
3. 玩家说“一起收集些木头吧”。
4. 同伴理解为共同活动，回复并参与。
5. 玩家途中聊天、说“等一下”或改变计划，动作能及时调整。
6. 天黑、受伤或找不到木头时，同伴能行动并自然说明。
7. 玩家说“够了，我们回刚才那里”，同伴能使用当前情境和地点记忆。
8. 活动结束后保存真实共同经历。
9. 重启后同伴能正确回答上次发生了什么。

### 21.2 最小能力

- 连接、重连和基础状态快照。
- 游戏聊天收发与主要玩家识别。
- 跟随、等待、导航、寻找可见方块和采集。
- 停止、取消、超时和真实背包验证。
- 危险事件和最小本地自保。
- 当前共同活动、意图和动作状态。
- 同伴档案、事件日志、情景记忆和检索。
- 一个主模型适配器。
- 结构化决策与完整因果轨迹。

### 21.3 暂缓

- 完整生存技能树。
- 任意建筑生成。
- 战斗策略完善。
- 自动学习可执行代码。
- 多 AI 同伴和复杂多人社会关系。
- 语音、视觉模型和完整调试 UI。
- 完整 Flow 文本 DSL。

## 22. 建议的代码结构

```text
src/
├── minecraft/       # driver adapters; raw Mineflayer/Prismarine types stop here
│   └── driver/      # protocol state, observation/motor/safety port implementations
├── perception/      # visibility and observations from read-only protocol DTOs
├── information/     # unified runtime, registry, policy, providers and legal client information
├── grounding/       # bind semantic referents to evidence-backed runtime handles
├── behavior/        # synthesize bounded plans from grounded embodied intent
├── motor/           # scoped continuous controllers, cancellation and feedback ports
├── navigation/      # epistemic planning and single-step safety contract
├── events/          # domain events, bus, journal, projections
├── companion/       # runtime, attention, conversation, activity, intent
├── context/         # context composition, provenance, budgets
├── models/          # provider adapters and decision protocol
├── actions/         # action runtime, locks, cancellation, verification
├── memory/          # capture, retrieval, consolidation, correction
├── speech/          # chat channel, timing, chunking, rate limiting
├── persistence/     # SQLite and profile files
├── telemetry/       # traces, metrics and debug state
├── app/             # composition root and lifecycle
└── cli/             # user-facing process control
```

目录是边界提示，不要求每个目录立即成为独立包。

`src/information/`、`src/perception/`、`src/grounding/`、`src/behavior/`、`src/motor/`、`src/navigation/` 和 `src/actions/` 均不得导入 Mineflayer/Prismarine 原始类型；唯一适配入口见 ADR 0005。Information Runtime 的模块契约见 [统一 Information Runtime](./information-runtime.md)。项目不保留原型 `src/skills/` 兼容目录。

## 23. 关键设计决定

当前确定：

1. 系统以持续同伴和共同活动为中心，不以任务队列为中心。
2. 采用事件驱动运行时与多时间尺度控制。
3. 同伴心智和 Minecraft 动作解耦，通过可追踪事件闭环统一。
4. 主模型一次决策可以同时产生语言、活动更新、普通意图和具身意图，但不选择执行技能。
5. Companion 只能通过合法信息接口、观察和有来源记忆形成世界知识；具身意图再经过 Grounding、Behavior Synthesis、受约束控制和验证，不运行任意模型代码。
6. 语言承诺必须和动作接受、世界验证协调。
7. 记忆保留来源与证据，事实和推断分开。
8. 第一版使用单同伴、单主决策通道和模块化单体。
9. Minecraft 实时反射与模型决策分层。
10. 从第一个产品切片开始记录完整因果轨迹并支持恢复。
11. v0.2 先固定 Information Catalog、Help/Read、UI Context 和信息精度/可用性；连续控制器的最终抽象在 v0.3 决定。

## 24. 后续详细设计

系统设计通过后，按以下顺序进入详细设计：

1. Information Catalog、Help/Read envelope、字段版本与 UI Context。
2. 当前状态、快捷栏、背包、F3、HUD、聊天、Tab 和 current screen 信息接口。
3. 第一人称视觉、声音和生命周期信息接入统一目录，并建立非泄漏测试。
4. Context 只消费合法 Information Read、认知观察和有来源记忆。
5. v0.3 再确定 Grounding、Behavior Synthesizer、Action Runtime、连续控制器和身体资源契约。
6. 记忆 schema、档案版本一致性、恢复冲突联动、写入验证和检索排序。
7. 每一阶段对应的纵向场景和真人验收设计。

每项详细设计都应能追溯到产品原则，并避免为尚未验证的未来能力提前建立复杂扩展系统。
