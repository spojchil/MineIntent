---
status: accepted
authority: normative
implementation: diverged
last_verified: 2026-07-23
---

# 同伴运行时状态机与活动对齐

> 版本 1，2026-07-12；关联 Issue [#13](https://github.com/spojchil/maineintent/issues/13)。依据 [ADR 0002](../decisions/0002-event-driven-companion-runtime.md)、[ADR 0003](../decisions/0003-separate-mind-and-action-runtime.md)和[领域事件协议](./domain-events-and-journal.md)。
>
> **实现漂移：** 最新实验分支仍有集中式 `CompanionRuntime`、活动/意图/注意和决策触发，但独立 Action Runtime 已在 `29052bf` 删除，主动机会调度和本文多项状态机也未完整实现。本文仍是接受设计；在 amend/supersede 前不能把漂移误写成新决定。

## 1. 目标

Companion Runtime 负责维持一个 AI 同伴在 Minecraft 世界中的持续社会与活动状态。本设计定义：

- 一名同伴与一名主要玩家的第一版状态聚合。
- Presence、Attention、Conversation、SharedActivity、Intent 和 Deliberation 状态机。
- 领域事件如何触发观察、聚合、打断、重新决策或后续处理。
- 玩家聊天到达时与模型推理、身体动作和语言发送的并发关系。
- 共同活动的对齐、不确定、恢复和偏离语义。
- 如何避免把短暂偏离误判为活动结束，或频繁追问玩家。
- 重启、重连、死亡与动作中断后的恢复行为。
- 模块接口、不变量、可观察性与测试矩阵。

## 2. 非目标

本设计不负责：

- 定义模型提示词和完整 Decision Contract；由 #10 负责。
- 实现具体 Mineflayer 技能和身体资源锁；由 #11、#14 负责。
- 定义长期记忆存储和关系反思；由 #4 负责。
- 支持多个主要玩家、多个同伴或完整多人社会关系。
- 用固定规则决定同伴人格、意见或所有社交行为。
- 让 Companion Runtime 直接读取或操作 Mineflayer。
- 把共同活动变成固定攻略树或永久任务队列。

## 3. 运行时职责

Companion Runtime 负责回答：

```text
同伴当前是否在世界中？
同伴正在关注什么？
双方现在是否处于一段对话？
我们大致在共同做什么？
双方是否仍然与活动对齐？
同伴当前准备做什么？
身体正在执行什么，以及这与意图有什么关系？
是否需要模型决策、等待、打断或保持沉默？
重启或意外后应该恢复什么上下文？
```

它不负责回答“怎样走到目标”“怎样挖方块”或“模型具体怎样措辞”。

## 4. 状态所有权

### 4.1 事件归约

Companion State 只通过领域事件 Reducer 更新。协调器不能直接修改投影对象：

```text
领域事件
→ Companion Reducer
→ 新 Companion State
→ Attention Router / Decision Coordinator 读取快照
→ 发出命令
→ 命令结果形成新事件
```

### 4.2 事实层与解释层

状态分为：

- 事实层：连接、玩家在线、动作状态、最近聊天和事件序号。
- 工作解释层：当前关注、共同活动摘要、意图和活动对齐。

事实层主要由确定性事件更新；工作解释层可以由模型决策提出更新，但必须通过事件进入投影，并保留触发证据。

### 4.3 v0.1 数量约束

第一版：

- 一个 Companion Runtime 实例对应一个 AI 同伴。
- 一个世界内配置一个主要玩家。
- 同时最多一个 active SharedActivity。
- 同时最多一个 primary Intent。
- 同时最多一个可提交状态的主 Deliberation Run。
- Action Runtime 可以执行一个有资源协调的动作组，但不属于 Companion State 的所有权范围。

未完成约定和未来想法进入 Commitments 或 Memory，不作为并行 active activity。

## 5. Companion State

```ts
interface CompanionState {
  revision: number
  lastEventSequence: number

  identity: CompanionIdentityState
  presence: PresenceState
  attention: AttentionState
  conversation: ConversationState
  activity: SharedActivityState | null
  intent: IntentState | null
  actionBinding: ActionBindingState
  deliberation: DeliberationState
  control: ControlState
  recovery: RecoveryState
}
```

`revision` 每次 Companion Reducer 实际改变状态时递增。模型上下文和决策绑定该 revision，用于拒绝过期结果。

## 6. Identity State

```ts
interface CompanionIdentityState {
  companionId: string
  profileId: string
  profileVersion: string
  primaryPlayerId: string
  worldId: string
}
```

- `profileVersion` 指向当前生效的自然语言同伴档案。
- Identity State 只保存引用，不把完整档案复制进事件投影。
- 主要玩家变更属于后续产品能力，第一版运行中不自动切换。
- 其他玩家可以被观察和交流，但不能自动获得主要伙伴的控制语义。

## 7. Presence 状态机

### 7.1 状态

```ts
type PresenceStatus =
  | 'stopped'
  | 'starting'
  | 'connecting'
  | 'active'
  | 'recovering'
  | 'disconnected'
  | 'stopping'
  | 'faulted'

interface PresenceState {
  status: PresenceStatus
  sinceEventId: string
  connectionAttempt?: number
  disconnectReason?: string
  lastSpawnEventId?: string
}
```

### 7.2 转换

```text
stopped
  → starting
  → connecting
  → active

active
  → recovering       死亡、重生、维度切换、快照协调
  → disconnected     连接关闭
  → stopping         用户或系统要求安全退出
  → faulted          无法信任关键投影或持久化

disconnected
  → connecting       重连
  → stopping

recovering
  → active           权威快照协调完成
  → disconnected
  → faulted

stopping
  → stopped
```

### 7.3 Presence 约束

- 只有 `active` 可以启动普通新动作。
- `recovering` 可以进行快照查询和必要安全动作，但不恢复旧动作。
- `disconnected` 不调用需要实时世界状态的模型决策。
- `faulted` 保留安全停止和诊断，不继续普通陪伴行为。

## 8. Attention State

Attention 表示此刻什么最值得同伴心智处理，不代表必须说话或行动。

```ts
type AttentionPressure = 'calm' | 'engaged' | 'strained' | 'urgent'

interface AttentionTarget {
  kind: 'player' | 'activity' | 'action' | 'threat' | 'place' | 'event'
  refId: string
  summary: string
  evidenceEventIds: string[]
}

interface AttentionState {
  focus: AttentionTarget | null
  pressure: AttentionPressure
  sinceEventId?: string
  lastShiftReason?: string
  pendingCandidates: AttentionCandidate[]
}
```

### 8.1 Attention Candidate

```ts
interface AttentionCandidate {
  id: string
  sourceEventIds: string[]
  kind: string
  priority: 'critical' | 'high' | 'normal' | 'low'
  groupKey: string
  summary: string
  expiresAt?: string
  suggestedDisposition:
    | 'interrupt'
    | 'steer'
    | 'follow_up'
    | 'collect'
    | 'observe'
}
```

候选由事件产生，可以合并、过期或被高优先级候选取代。它不是模型待办列表。

## 9. Conversation 状态机

Minecraft 聊天没有可靠的“对话开始/结束”协议，因此 Conversation State 是当前互动窗口，不是永久会话历史。

### 9.1 状态

```ts
type ConversationStatus =
  | 'available'
  | 'engaged'
  | 'waiting_for_player'
  | 'reply_pending'
  | 'temporarily_suspended'

interface ConversationState {
  status: ConversationStatus
  participants: string[]
  recentTopic?: string
  openQuestion?: string
  lastInboundEventId?: string
  lastOutboundEventId?: string
  lastAddressingConfidence?: number
  suspendedReason?: 'danger' | 'precise_action' | 'disconnected' | 'stopping'
}
```

### 9.2 转换语义

- `available`：没有正在维持的明显对话，但仍可接收聊天。
- `engaged`：最近双方存在连续互动。
- `waiting_for_player`：同伴明确提出问题或等待玩家决定。
- `reply_pending`：已经决定回复，但 Speech Scheduler 尚未发送。
- `temporarily_suspended`：危险、断线或精确操作使普通交流暂缓。

Conversation 不因固定几秒沉默立即结束。长时间没有相关互动时回到 `available`，但旧内容通过近期事件和记忆检索保留。

### 9.3 消息是否对同伴说

`player.chat.received` 保存 addressing evidence：

- 私聊或直接频道。
- 是否提到同伴名字。
- 是否延续同伴刚提出的问题。
- 最近对话参与者。
- 玩家和同伴的距离与共同活动。
- 多人聊天中的显式目标。

Reducer 不把这些证据直接变成确定意图。Attention Router 只在高置信情况下选择直接响应；有歧义时交给主模型理解，或保持观察。

## 10. SharedActivity 状态机

### 10.1 数据结构

```ts
type ActivityStatus =
  | 'proposed'
  | 'active'
  | 'paused'
  | 'completed'
  | 'abandoned'

type ActivityAlignment = 'aligned' | 'uncertain' | 'diverged'

interface ActivityParticipant {
  playerId: string
  role?: string
  currentContribution?: string
  lastRelevantEventId?: string
}

interface SharedActivityState {
  id: string
  summary: string
  status: ActivityStatus
  alignment: ActivityAlignment
  participants: ActivityParticipant[]
  agreedFacts: string[]
  openQuestions: string[]
  referencedPlaceIds: string[]
  startedByEventId: string
  lastUpdatedByEventId: string
  alignmentEvidence: AlignmentEvidenceState
  confirmation: ActivityConfirmationState
}
```

### 10.2 状态转换

```text
null
  → proposed           玩家或同伴提出共同活动

proposed
  → active             双方明确同意，或玩家开始行动且模型确认共同参与
  → abandoned          提议被拒绝或自然失效

active
  → paused             双方暂时停止但仍准备继续
  → completed          真实完成条件成立且同伴确认活动结束
  → abandoned          玩家放弃、活动失去意义或双方改做其他事情

paused
  → active
  → completed
  → abandoned
```

活动终止后从 current activity 投影移出，但产生情景记忆候选。

### 10.3 Alignment 与 ActivityStatus 分离

- `status` 表示共同活动是否仍然存在。
- `alignment` 表示双方现在是否看起来仍在参与同一活动。

例如活动可以是 `active + uncertain`：双方仍计划收集木材，但玩家突然离开林区，AI 不确定计划是否改变。

### 10.4 Alignment 转换权限

```text
aligned
  → uncertain          Alignment Monitor 基于持续证据触发

uncertain
  → aligned            明确相关聊天、行为或模型确认恢复
  → diverged           玩家明确改变计划，或模型基于证据确认

diverged
  → aligned            双方重新确认恢复原活动
  → activity update    更新分工或形成新活动
  → paused/abandoned   原活动暂停或结束
```

Activity Alignment Monitor 永远不能自行设置 `diverged`、`completed` 或 `abandoned`。

## 11. Alignment Evidence

### 11.1 证据结构

```ts
type AlignmentSignalStrength = 'weak' | 'moderate' | 'strong'

interface AlignmentSignal {
  kind:
    | 'relevant_chat'
    | 'relevant_action'
    | 'shared_destination'
    | 'proximity_support'
    | 'moving_away'
    | 'unrelated_action'
    | 'contribution_stopped'
    | 'activity_precondition_lost'
    | 'explicit_plan_change'
  direction: 'supports' | 'drifts'
  strength: AlignmentSignalStrength
  eventId: string
  observedAt: string
}

interface AlignmentEvidenceState {
  recentSignals: AlignmentSignal[]
  uncertainSinceEventId?: string
  lastReviewEventId?: string
}
```

### 11.2 支持信号

- 玩家继续相关采集、建造、移动或战斗。
- 玩家朝共同目标地点移动。
- 玩家聊天继续讨论当前活动。
- 玩家完成自己分工中的可观察步骤。
- 玩家短暂离开后返回活动区域。

### 11.3 漂移信号

- 玩家持续远离活动区域或同伴。
- 玩家持续执行与当前活动无关的行为。
- 玩家停止原本贡献，且其他信号不能解释。
- 活动前置条件已经消失。
- 玩家明确说要改做其他事情。

单次捡拾物品、追逐动物、整理背包、躲避威胁或短暂绕路通常只形成弱信号或被解释为当前活动的一部分。

### 11.4 触发不确定

第一版启发式只决定是否值得重新关注，不决定社交结论：

- 一个明确改变计划的 strong signal 立即产生 review candidate，但最终状态由玩家控制事件或模型更新。
- 两类相互独立的 drift signal 持续跨过观察窗口，可以将 alignment 置为 `uncertain`。
- 单一 moderate signal 需要持续存在且没有支持信号，才进入 `uncertain`。
- 新的明确支持信号可以恢复 `aligned`。

具体时间窗口是内部调优参数，不属于人格配置。初始实现建议：

- Monitor 每秒评估一次聚合状态。
- 普通漂移观察窗口为 15 秒。
- 进入 uncertain 后至少 20 秒才允许再次产生同类 review candidate。
- 同一活动最多每 60 秒提出一次直接确认问题，除非玩家产生新的明确计划变化。

这些默认值必须通过产品场景测试调整，不能被描述成理解玩家意图的普遍规律。

## 12. Activity Confirmation

```ts
interface ActivityConfirmationState {
  lastAskedAt?: string
  lastAskedEventId?: string
  pendingQuestion?: string
  suppressedUntil?: string
  consecutiveUnansweredChecks: number
}
```

### 12.1 防打扰规则

- Alignment Monitor 只产生 review candidate，不直接发送问题。
- 主模型可以选择观察、等待、跟随、暂停自己或询问。
- 已经问过且玩家没有回答时，不能按周期重复同一句问题。
- 玩家专注于明显的新活动时，可以用一次自然问题确认，而不是连续追问。
- 玩家明确说“别老问”后，该偏好进入记忆候选和当前 suppression 情境。
- 危险与精确操作期间不发送普通活动确认。

### 12.2 未回答

没有回答不等于同意，也不等于拒绝。运行时可以：

- 继续低风险、可撤销的当前步骤。
- 暂停会显著改变世界或消耗重要资源的动作。
- 保持附近陪伴。
- 等待新的自然节点再重新评估。

## 13. Intent 状态机

Intent 是同伴当前短期打算，不是完整任务树。

```ts
type IntentStatus =
  | 'active'
  | 'blocked'
  | 'satisfied'
  | 'cancelled'
  | 'superseded'

interface IntentState {
  id: string
  summary: string
  status: IntentStatus
  activityId?: string
  createdByEventId: string
  completionSignal?: string
  blockedReason?: string
  currentActionGroupId?: string
}
```

### 13.1 转换

- 新决策可以创建 intent 或 supersede 旧 intent。
- Action Runtime 的失败可以将 intent 置为 blocked，但不能自动判定活动失败。
- 真实完成信号使 intent satisfied。
- 玩家停止、活动更新或安全接管可以 cancel/supersede intent。
- 一个活动可以先后产生多个 intent。

## 14. Action Binding

Companion Runtime 不拥有动作执行状态，但保存语义绑定：

```ts
interface ActionBindingState {
  activeGroupId?: string
  actionIds: string[]
  intentId?: string
  activityId?: string
  purpose?: string
  acceptedByEventId?: string
  lastTerminalEventId?: string
}
```

- Action Runtime 是动作状态和身体资源的权威。
- Binding 用于解释动作为什么开始、属于哪个活动，以及结果应唤醒哪个决策链。
- 动作终止后 Binding 保留最后终止事件，直到后续决策消费结果。

## 15. Deliberation 状态机

### 15.1 状态

```ts
type DeliberationStatus =
  | 'idle'
  | 'collecting_context'
  | 'running'
  | 'cancelling'
  | 'validating'
  | 'applying'

interface DeliberationState {
  status: DeliberationStatus
  runId?: string
  contextRevision?: number
  triggerEventIds: string[]
  disposition?: 'interrupt' | 'steer' | 'follow_up' | 'collect'
  startedAt?: string
  pendingFollowUps: AttentionCandidate[]
}
```

### 15.2 生命周期

```text
idle
  → collecting_context
  → running
  → validating
  → applying
  → idle

running
  → cancelling
  → collecting_context   新事件需要重跑

任意非 idle
  → idle                 失败、超时、停止或断线清理完成
```

### 15.3 单写者

同一时刻只有一个 run 可以进入 validating/applying。旧 run 即使稍后返回，也必须因 run ID 或 context revision 不匹配而产生 `model.decision.discarded_as_stale`。

## 16. Attention Router

### 16.1 输入与输出

Attention Router 读取领域事件、Companion State 和当前运行通道状态，输出 disposition：

```text
observe
collect
follow_up
steer
interrupt
```

它不生成自然语言，不决定长期活动，也不解释玩家人格。

### 16.2 路由表

| 事件 | 默认 disposition | 说明 |
|---|---|---|
| 管理入口强制停止 | interrupt | 不等待模型，取消动作和推理 |
| 主要玩家明确安全停止 | interrupt | 保守识别，消息仍进入后续对话 |
| 死亡、断线、紧急接管 | interrupt | 先处理本地状态，再决定是否解释 |
| 主要玩家纠正当前行动 | steer | 第一版实际取消旧 run 并重跑 |
| 主要玩家普通聊天 | interrupt 或 collect | 取决于是否直接对同伴、紧急和当前回复阶段 |
| 动作终止 | follow_up | 让同伴处理真实结果 |
| 活动 alignment uncertain | follow_up | 可以选择沉默观察，不必发问 |
| 普通世界观察 | observe | 更新状态或聚合证据 |
| 自然主动机会 | collect/follow_up | 低优先级，压力升高时过期 |

### 16.3 当前 steer 语义

第一版不依赖模型提供运行中追加上下文能力。`steer` 表示“新信息在调整同一件事”，执行方式仍是：

```text
请求取消旧 run
→ 标记旧结果不可提交
→ 合并新旧触发事件
→ 重新组装上下文
→ 启动新 run
```

## 17. 玩家消息并发

### 17.1 模型空闲

直接玩家聊天形成高优先级 candidate，经过极短聚合窗口后开始决策。连续输入的聊天片段可以在不损害响应性的前提下合并。

### 17.2 模型正在运行

- 明确纠正、停止、警告或改变当前计划：取消旧 run，重新决策。
- 普通补充信息：标记 steer，第一版取消并重跑。
- 无关闲聊且旧 run 即将结束：可以作为 follow-up。
- 多条短消息：按同一发送者和话题 groupKey 聚合。

### 17.3 动作正在运行

收到聊天不默认取消身体动作：

- 明确停止或与动作冲突的纠正先取消动作。
- 普通问题可以在移动中回复。
- 精确操作或危险状态由 Speech Scheduler 延迟非紧急语言。
- 需要模型判断是否改变动作时，Action Runtime 继续保持安全执行，直到收到取消或新动作命令。

### 17.4 回复正在发送

已经发送的聊天不能撤回。尚未发送的分段可以因高优先级新事件取消。后续决策应承认情境变化，不能继续发送已经失效的承诺。

## 18. 明确停止

### 18.1 管理入口

CLI 或本地管理 API 的强制停止是确定性控制：

- 立即取消普通动作。
- 取消当前模型 run。
- 取消未发送语言。
- 保留安全反射直到身体稳定。
- 产生控制和取消事件。

### 18.2 游戏聊天中的停止

游戏聊天可能包含玩笑、叙述或对其他玩家说的话，因此使用保守的 Control Intent Detector。

只有同时满足以下证据时走确定性快速停止：

- 发送者是主要玩家。
- 消息明确指向同伴或延续双方当前对话。
- 表达是明确、独立的停止当前行为要求，而不是引用或条件句。

存在歧义时进入高优先级模型决策，不直接取消。无论走哪条路径，原始聊天都保留在对话情境中，使同伴可以自然回应。

## 19. Decision Application

模型结果由 Decision Coordinator 分阶段应用：

```text
结构与 schema 校验
→ run ID 和 context revision 校验
→ 活动与意图变更预检
→ 动作组 schema、依赖和资源预检
→ 语言时机与承诺关联预检
→ 产生各部分 accepted/rejected 结果
→ 通过领域事件提交状态变化
→ 启动动作和安排语言等 Effector
```

### 19.1 部分接受

一次决策的语言、活动、意图、动作和记忆候选是不同 effect：

- 动作组内部采用整体接受或整体拒绝。
- 活动更新可以在动作被拒绝时仍然有效。
- 描述动作承诺的语言依赖动作组接受，动作拒绝时一并拒绝或改为后续说明。
- 纯社交语言可以独立接受。
- 每个拒绝都产生结构化原因，触发 follow-up 而不是静默丢弃。

### 19.2 提交顺序

1. 记录 decision validation result。
2. 提交活动与意图事件。
3. 提交动作请求并等待 Action Runtime accepted/rejected 事件。
4. 动作 accepted 后释放对应 `after_action_accept` 语言。
5. 动作终止后释放或重新生成 `after_action_result` 语言。
6. 记忆候选异步进入 Memory System。

## 20. 主动机会

Proactive Opportunity Scheduler 不直接发送消息，只发布低优先级机会：

- 一段共同工作刚完成。
- 长途同行进入稳定、低压力阶段。
- 返回具有共同意义的地点。
- 发现与旧经历相关的事物。
- 天黑、资源不足或计划到达自然转折。
- 玩家长时间没有明确活动且同伴有合理建议。

### 20.1 抑制条件

- Attention pressure 为 strained/urgent。
- 当前有未回答的同伴问题。
- 玩家刚拒绝建议或要求安静。
- 相同 groupKey 的机会仍在冷却。
- 模型、动作或 Journal 处于 degraded 状态。
- 玩家不在线或 Presence 非 active。

主动机会过期后直接丢弃，不积累成必须完成的队列。

## 21. Control State

```ts
interface ControlState {
  paused: boolean
  pauseReason?: 'player' | 'admin' | 'degraded' | 'recovery'
  hardStopRequested: boolean
  ordinaryActionsAllowed: boolean
  deliberationAllowed: boolean
  proactiveAllowed: boolean
}
```

- 玩家暂停不等于进程停止；同伴仍可感知重要生命周期和安全事件。
- degraded 可以禁止普通动作和模型，但保留安全停止。
- Control State 由控制与故障事件归约，不由同伴人格覆盖。

## 22. Recovery State

```ts
interface RecoveryState {
  needed: boolean
  reason?: 'process_restart' | 'disconnect' | 'death' | 'dimension_change' | 'projection_fault'
  startedByEventId?: string
  pendingReconciliations: string[]
  interruptedActionIds: string[]
  completedByEventId?: string
}
```

### 22.1 进程重启

1. 重放 Companion State 投影。
2. 将未完成 deliberation 标记为 cancelled by restart。
3. 将未发送 speech 标记为 cancelled。
4. 未终止动作等待事件层产生 process restarted interrupt。
5. 共同活动保留，但进入 paused 或 alignment uncertain。
6. 连接服务器并协调权威快照。
7. 产生 `companion.recovery.completed`。
8. 触发一次恢复决策，决定继续、询问、解释或放弃。

不能自动重新执行旧动作。

### 22.2 断线重连

- 断线立即取消需要实时世界的模型结果和动作。
- Conversation 暂停，但不删除最近话题。
- SharedActivity 保留并进入 paused/uncertain。
- 重连协调完成后再由模型决定是否恢复。

### 22.3 死亡与重生

- 死亡由安全和动作层立即终止身体行为。
- 共同活动不自动 abandoned。
- Recovery 保存死亡地点和已知损失证据。
- 重生后先协调背包、位置和维度，再讨论恢复计划。

## 23. Companion Runtime 接口

```ts
interface CompanionRuntime {
  start(): Promise<void>
  requestStop(reason: string): Promise<void>
  pause(reason: string): void
  resume(): void

  snapshot(): Readonly<CompanionState>
  handle(event: DomainEvent): void
}

interface AttentionRouter {
  route(
    event: DomainEvent,
    state: Readonly<CompanionState>,
  ): AttentionDisposition
}

interface ActivityAlignmentMonitor {
  observe(
    event: DomainEvent,
    activity: Readonly<SharedActivityState>,
  ): AlignmentObservation
}

interface DecisionCoordinator {
  schedule(candidate: AttentionCandidate): void
  cancel(runId: string, reason: string): void
  apply(result: StructuredDecision): Promise<DecisionApplicationResult>
}
```

### 23.1 依赖规则

- Companion Runtime 依赖领域事件接口，不依赖 Mineflayer。
- Attention Router 不调用模型。
- Alignment Monitor 不发送聊天，也不能结束活动。
- Reducer 不执行 I/O。
- Decision Coordinator 通过 ModelProvider、Action Runtime 和 Speech 接口协作。
- Memory System 读取事件与状态快照，但不能直接修改 Companion State。

## 24. 不变量

1. Companion State 只通过事件 Reducer 改变。
2. 同一时刻只有一个主模型 run 可以提交状态。
3. 过期 context revision 的模型结果不能应用。
4. 第一版最多一个 active SharedActivity 和一个 primary Intent。
5. Alignment Monitor 只能触发 uncertain，不能自行判定 diverged 或结束活动。
6. 没有玩家回答不能被推断为同意或拒绝。
7. 动作未接受前不能发送对应行动承诺。
8. 明确安全停止不能被同伴档案覆盖。
9. 恢复后不能自动重做重启前未完成动作。
10. 普通主动机会不能抢占玩家聊天、安全事件或当前动作终止。
11. Companion Runtime 不直接访问 Mineflayer 或 SQLite。
12. 关闭 Activity 必须产生可用于情景记忆的终止事件。

## 25. 错误与降级

### 25.1 模型失败

- Deliberation 回到 idle。
- 保留触发事件和当前活动。
- 不产生虚构语言或动作。
- 对主要玩家直接聊天可以给出受控、诚实的系统级失败提示。
- 按模型网关策略重试或等待后续事件。

### 25.2 Decision 校验失败

- 记录失败字段和 schema 版本。
- 不应用非法 effect。
- 可以对不依赖非法字段的纯社交语言单独接受，前提是没有虚假承诺。
- 连续失败进入 degraded，暂停普通动作。

### 25.3 Companion Reducer 失败

遵循事件协议：进入 degraded，禁止新模型和普通动作，从检查点重建，保留安全停止。

### 25.4 Activity Monitor 故障

停止自动漂移判断，但不结束当前活动。玩家聊天和显式活动事件仍正常工作。

## 26. 可观察性

调试状态展示：

- Companion revision 和最后事件 sequence。
- Presence、Conversation、Activity、Alignment、Intent 和 Deliberation 状态。
- 当前 attention focus、pressure 和 pending candidates。
- 最近 alignment signals、uncertain 原因和确认冷却。
- 当前模型 run、context revision 和触发事件。
- 动作 Binding 与最后终止事件。
- Control 与 Recovery 状态。
- 被取消、废弃为 stale 和拒绝的决策数量。

不展示模型私有思维链。

## 27. 测试矩阵

### 27.1 Reducer 契约

- 相同状态和事件产生相同新状态。
- 无关事件不增加 Companion revision。
- 重放得到相同最终状态。
- 非法状态转换被拒绝并产生诊断。

### 27.2 Presence

- 正常启动、连接、生成和停止。
- 断线、重连和重连失败。
- 死亡、重生和维度切换。
- faulted 状态禁止普通动作。

### 27.3 消息与打断

- 空闲时收到直接聊天。
- 模型运行时玩家补充信息导致取消并重跑。
- 玩家明确纠正当前动作。
- 普通问题不会自动取消安全移动。
- 旧模型结果在取消后返回，被标记 stale。
- 已发送语言不重发，未发送分段可取消。

### 27.4 明确停止

- 管理入口停止立即取消动作和模型。
- 主要玩家明确要求同伴停下。
- 玩家引用“停下”一词但不是指挥同伴时不误触发。
- 其他玩家说停下不自动获得主要伙伴控制权。

### 27.5 SharedActivity

- 提议、接受、开始、暂停、恢复、完成和放弃。
- 同一活动中连续产生多个短期 intent。
- 动作失败只阻塞 intent，不自动结束活动。
- 新活动 supersede 或关闭旧活动时保留事件证据。

### 27.6 Alignment

- 玩家捡物品后继续活动，不进入 uncertain。
- 玩家短暂追逐动物后返回，不发确认问题。
- 玩家持续远离且进行无关行为，进入 uncertain。
- uncertain 后玩家恢复相关行为，回到 aligned。
- Monitor 永远不自行设置 diverged。
- 同一活动确认问题满足 60 秒冷却。
- 玩家不回答时不会被推断为同意或拒绝。
- 危险期间确认语言被抑制。

### 27.7 Decision Application

- 活动更新有效但动作组被拒绝。
- 动作组拒绝时对应承诺语言不发送。
- 纯社交语言在无动作时正常发送。
- context revision 过期时所有状态 effect 拒绝。
- 动作接受后 `after_action_accept` 语言才被释放。

### 27.8 Recovery

- 进程重启保留活动但取消旧动作和模型。
- 重连后先协调快照，再触发恢复决策。
- 死亡不自动放弃共同活动。
- 恢复过程中不启动普通新动作。

### 27.9 主动机会

- 完成共同工作产生低优先级机会。
- 高压力、未回答问题和刚被拒绝时主动机会受抑制。
- 过期机会不积累成待办。
- 主模型可以选择沉默，运行时不强制发言。

## 28. 实施顺序

1. 定义 Companion State 与纯 Reducer。
2. 实现 Presence、Control 和 Recovery 状态机。
3. 实现 Attention Candidate 与 Router。
4. 实现 Deliberation 单写者和 stale result 防护。
5. 实现 SharedActivity、Intent 和 Action Binding。
6. 实现 Alignment Evidence Monitor 与冷却。
7. 接入聊天、Action Runtime 和 ModelProvider。
8. 接入主动机会与调试状态。
9. 完成重放、恢复和纵向场景测试。

## 29. 验收映射

本设计满足 #13 的验收要求：

- 持续状态及其事件驱动转换已明确。
- SharedActivity 的 aligned、uncertain 和 diverged 语义已定义。
- 短暂偏离使用证据窗口、支持信号和确认冷却，避免反复询问。
- 持续漂移产生 `companion.activity.alignment_became_uncertain`，由模型决定后续互动。
- 进程重启、断线重连、死亡和维度变化均有恢复状态与测试场景。
