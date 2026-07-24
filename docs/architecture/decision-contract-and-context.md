---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# 同伴决策协议与上下文包

> 这是 v0.2 clean-slate 接受基线，对应 Issue [#52](https://github.com/spojchil/maineintent/issues/52)。最新实验分支实现了 V2 单轮结构化决策、上下文绑定和核心 effect dispatch；本文同时规定的格式修复、freshness guard、Coordinator、多 route 运行语义和主动机会消费尚未完整实现。模型工具循环仍只是[候选提案](../proposals/embodiment-architecture-reflection.md)。上游：[目标系统](./target-system.md)、[同伴运行时](./companion-runtime.md)、[领域事件](./domain-events-and-journal.md)。

## 1. 目的

本文定义主同伴模型与确定性运行时之间的实现级边界：运行时怎样组成一次有来源、有预算的上下文，模型怎样提出语言、活动、具身意图和记忆效果，以及运行时怎样经过 Grounding 与 Behavior Synthesis 后验证和提交执行效果。

协议的结构用于保证真实、可取消、可审计的执行，不编码同伴的性格、关系风格或固定任务树。人格仍来自自然语言同伴档案、共同经历和当前情境。

## 2. 范围与非目标

本文负责：

- `ContextPackageV2` 和上下文片段的来源、信任、预算与截断元数据。
- `CompanionDecisionV2` 的具身意图、语义引用、Grounding 和行为合成边界。
- 模型调用、结构校验、修复、过期拒绝和效果提交。
- 动作依赖图、动作组原子预检及语言承诺的依赖。
- collect、steer、interrupt 和 follow-up 的模型运行语义。
- 提示注入边界和可观察性要求。

本文不负责具体模型厂商、底层玩家输入与协议状态机的执行细节、完整记忆算法，也不保存或展示模型的私有思维链。

## 3. 核心不变量

1. 模型输出是提议，不是事实，也不直接修改状态。
2. 决策只适用于它声明的 run、上下文版本和世界前提。
3. 同一决策的动作组要么完整通过预检，要么整个动作效果被拒绝。
4. 动作组被接受前，不发送依赖该动作的承诺语言。
5. “已经完成”等结果语言只能依赖已验证的终止事件。
6. 玩家聊天、记忆正文和世界文本均是数据，不因内容像指令而取得系统权限。
7. 不持久化私有思维链；只保存简短运行解释和最终效果。
8. 未知协议版本、effect、引用或额外字段默认拒绝，不静默猜测。
9. 主同伴模型不能选择运行时 skill、输入模板或协议事务；允许硬编码的是客户端真实输入、协议状态机与安全不变量，不是玩家措辞、对象类别或规划产物。

## 4. 版本与决策引用

```ts
type ContextProtocol = 'mineintent.context.v2'
type DecisionProtocol = 'mineintent.decision.v2'

interface DecisionContextRef {
  runId: string
  companionId: string
  sessionId: string
  worldId: string
  companionRevision: number
  throughEventSequence: number
  profileVersion: string
  capabilityRevision: string
}
```

- Runtime 只读取精确的 context/decision `v2`；v1、交叉版本或混合字段直接拒绝，不进入 compatibility adapter。
- `runId` 唯一标识一次模型运行；重新组装上下文必须创建新 run。
- `companionRevision` 来自 Companion State，只在影响同伴决策的状态变化时递增。
- `throughEventSequence` 是组装时已纳入的最后领域事件序号。
- `profileVersion` 防止档案编辑后应用旧人格决策。
- `capabilityRevision` 是模型可理解的身体可供性、限制与证据要求的内容哈希，不暴露输入模板或协议事务名称。
- 高频世界变化不全部增加 revision，动作仍须在提交时预检当前世界。

JSON Schema 是代码中的协议权威；本文 TypeScript 是等价说明。历史模型运行保留当时协议名和原始输出。

## 5. 来源与信任

```ts
type ContextTrust =
  | 'runtime_authoritative'
  | 'verified_observation'
  | 'player_statement'
  | 'profile_instruction'
  | 'remembered_record'
  | 'derived_summary'
  | 'untrusted_content'

interface ContextSourceBase {
  kind: 'runtime' | 'event' | 'profile' | 'memory' | 'player' | 'summary'
  ids: string[]
  trust: ContextTrust
  observedAt?: string
  validAt?: string
}

interface ContextSource extends Omit<ContextSourceBase, 'kind'> {
  kind: ContextSourceBase['kind'] | 'capability_registry'
}
```

只能生成 `capability_registry`；`skill_registry` 不是合法来源，能力内容不得包含输入模板或协议事务的可调用名称。

信任不是简单总排序：当前运行时状态对连接、动作和已验证游戏状态有权威性；观察只证明指定时间看到的内容；玩家陈述只证明玩家说过；档案对人格和表达有效但不能伪造事实；记忆和摘要始终可被原始证据纠正。

指令优先级从高到低为：产品安全、真实性与协议约束；运行时管理控制；当前同伴档案；玩家当前请求；记忆、观察和世界内容。低层内容不能要求忽略高层、绕过 Grounding/Behavior Synthesis 或把未验证内容写成事实。

## 6. 上下文片段

```ts
type ContextSection =
  | 'product_constraints'
  | 'companion_profile'
  | 'relationship_core'
  | 'current_state'
  | 'trigger_events'
  | 'observations'
  | 'retrieved_memories'
  | 'capabilities'
  | 'requested_knowledge'

interface ContextFragment<T = unknown, S extends ContextSource = ContextSource> {
  id: string
  section: ContextSection
  source: S
  content: T
  budget: {
    estimatedTokens: number
    originalEstimatedTokens: number
    truncated: boolean
    truncationReason?: 'section_budget' | 'total_budget' | 'recency_limit' | 'deduplicated'
    omittedItemCount?: number
  }
}
```

每个片段必须能回到事件、档案版本、记忆记录或注册表。无来源的大段拼接摘要不能进入主上下文。

## 7. Context Package

```ts
interface ContextPackageV2 {
  protocol: 'mineintent.context.v2'
  ref: DecisionContextRef
  createdAt: string
  trigger: {
    eventIds: string[]
    route: 'new' | 'collect' | 'steer_rerun' | 'interrupt_rerun' | 'follow_up' | 'proactive'
    reason: string
    priority: number
  }
  limits: {
    maxInputTokens: number
    reservedOutputTokens: number
    estimatedInputTokens: number
  }
  fragments: ContextFragment[]
  omissions: Array<{ section: ContextSection; reason: string; count?: number }>
}
```

Context 只允许 `capability_registry`，只描述身体可供性、限制、证据要求和可靠性。旧 `skill_registry`、完整 Backend snapshot 和 `availableSkills` 均不是合法片段。

### 7.1 固定组装顺序

1. 不可截断的产品约束。
2. 当前同伴档案；核心档案超预算视为配置错误，不自行改写人格。
3. 主要玩家和关系核心：稳定称呼、明确边界和重要约定。
4. 当前 Companion State：活动、注意、对话、意图、动作和控制状态。
5. 触发事件：保留原始结构和必要文本。
6. 当前认知观察：只含同伴合理可感知的信息。
7. 检索记忆：附记录 ID、来源、状态、置信度和证据摘要。
8. 当前身体可供性：能否转向、移动、交互等能力边界、限制、证据要求和可靠性摘要；不列输入模板、协议事务名称或对象专用命令。
9. 运行时批准的按需知识。

### 7.2 预算与截断

- 先应用分节条目和 token 子预算，再应用总预算。
- 产品约束、档案核心、当前控制状态和触发事件不可静默丢弃。
- 对话保留最近未解决轮次；较旧轮次使用有来源摘要。
- 观察按显著性和新近度裁剪，不补入未见信息。
- 记忆逐条裁剪，不能丢失 ID、状态、来源和置信度。
- 只列本轮相关身体可供性；模型不能凭旧记忆要求当前不存在的能力。V2 没有选择输入模板或协议事务的结构字段，自由文本不按内部名称扫描。
- 所有截断和遗漏必须出现在元数据中。
- 必需片段仍超总预算时以 `context_budget_exceeded` 失败，不发送残缺请求。

相同投影、事件序列、档案、检索结果和预算应产生稳定顺序、内容等价的上下文。密钥、令牌、私有绝对路径、原始协议缓存和管理凭证永不进入上下文。

## 8. Decision Contract

```ts
interface CompanionDecisionV2 {
  protocol: 'mineintent.decision.v2'
  runId: string
  context: DecisionContextRef
  summary: string
  effects: DecisionEffectV2[]
}

interface EffectBase {
  id: string
  kind: string
}
```

`summary` 是最多 240 字符的运行解释，不是思维链、事实或执行指令。effect ID 在同一决策内唯一，只允许字母、数字、`-`、`_`，长度 1–64。

## 9. 语言效果

```ts
interface SpeechEffectV2 extends EffectBase {
  kind: 'speech'
  text: string
  audience: { kind: 'primary_player' | 'nearby_players'; playerIds?: string[] }
  timing: 'now' | 'after_intent_accepted' | 'after_intent_terminal'
  dependsOn?: string[]
  terminalCondition?: 'completed' | 'failed' | 'cancelled' | 'any'
  purpose: 'reply' | 'acknowledge' | 'coordinate' | 'report' | 'social' | 'ask'
}
```

- `now` 不能包含未获准行为的确定承诺或未验证结果。
- v2 的 `after_intent_accepted` 只能依赖 embodied intent effect；`after_intent_terminal` 必须引用 embodied intent 并声明终止条件。
- 发送前仍检查玩家在线、节流、长度和高压力时机。
- 具体结果通常由终止事件触发新决策，不能在动作开始前猜测。
- 内部行为计划、controller 调用和协议事务从不成为语言依赖对象。

## 10. 活动与意图效果

```ts
interface ActivityEffect extends EffectBase {
  kind: 'activity'
  operation: 'propose' | 'activate' | 'update' | 'pause' | 'complete' | 'abandon'
  activityId?: string
  expectedRevision?: number
  summary?: string
  companionContribution?: string
  agreedFacts?: string[]
  openQuestions?: string[]
  reason: string
  evidenceEventIds: string[]
}

interface IntentEffect extends EffectBase {
  kind: 'intent'
  operation: 'set' | 'replace' | 'clear'
  intentId?: string
  expectedRevision?: number
  summary?: string
  reason: string
  activityId?: string
  completionSignals?: string[]
  invalidationSignals?: string[]
}
```

新活动由运行时分配 ID；修改必须携带对象 ID 和 expected revision。`complete`、`abandon` 不能仅凭漂移推断，需确认或明确事件。意图只描述近期打算；完成和失效信号是可观察标签，不能嵌入代码。

## 11. 具身意图与内部行为计划

`action_group`、`actions[].skill` 和对象专用 model-facing skill 已从当前协议删除。它们不会由 adapter 接受，也不占据新的实现目录；Git 历史足以解释旧原型。

### 11.1 具身意图

```ts
interface SemanticGoalV1 {
  schema: 'mineintent.semantic-goal.v1'
  objective: SemanticGoalExpressionV1
  methodGuidance: MethodGuidanceV1[]
}

type SemanticGoalExpressionV1 =
  | { kind: 'state'; state: SemanticStateV1 }
  | { kind: 'all'; goals: SemanticGoalExpressionV1[] }
  | { kind: 'any'; goals: SemanticGoalExpressionV1[] }

interface SemanticStateV1 {
  id: string
  concept: string
  description: string
  arguments: Record<string, SemanticTermV1>
}

type SemanticTermV1 =
  | { kind: 'self' }
  | { kind: 'referent_role'; role: string }
  | { kind: 'value'; value: string | number | boolean; unit?: string }

interface MethodGuidanceV1 {
  description: string
  referentRoles: string[]
  strength: 'required' | 'preferred' | 'suggested'
}

interface EmbodiedIntentEffect extends EffectBase {
  kind: 'embodied_intent'
  summary: string
  desiredOutcome: string
  semanticGoal: SemanticGoalV1
  referents: Array<{
    role: string
    selection:
      | { kind: 'context_ref'; ref: string }
      | { kind: 'message_referent'; eventId: string; expression: string }
  }>
  constraints: {
    maxDurationMs?: number
    interruptibility: 'immediate' | 'checkpoint'
  }
}
```

- `desiredOutcome` 只是面向人的摘要与调试来源，不进入 Behavior Synthesizer，不参与行为分派。
- `semanticGoal` 是行为合成唯一可消费的期望状态契约。它规定组合结构、语义引用、方法约束强度和安全不变量，不在系统设计中枚举“看、砍、跟随”等任务或状态谓词。`concept`、`description` 和参数名是给语义规划器的数据，不是输入模板、协议事务名或执行权限。
- `all`/`any` 只表达成功状态的逻辑组合。同一目标可有多种方法，同一方法也可同时促成多个状态；不得因为当前实现只有一种计划，就把该计划固化为 goal kind。
- `methodGuidance` 保留玩家对方法的显式要求或建议，但仍然不选择输入序列。`required` 不可被规划器静默替换；`preferred` 只在不可行时允许改道并保留可解释原因；`suggested` 可由规划器结合当前可供性决定。一句话中的方法究竟是必须、偏好还是举例，由 Companion 依语境理解，不用动词表硬编码。
- `semanticGoal` 不接受世界坐标、协议实体 ID、Mineflayer 类型、输入模板或协议事务名作为语义引用。未声明的 referent role、伪造句柄和试图将输入序列/代码塞入契约的字段在 semantic validation 拒绝。新 `concept` 本身不获得任何权限；规划器无法理解或没有可行方法时返回 `unsupported_goal`/`no_feasible_plan`，不使用关键词 fallback。
- `desiredOutcome`、`summary` 和 `role` 都是语义数据，不是隐式命令槽位；词面碰巧等于任何内部名称既不触发执行，也不构成拒绝理由。
- `context_ref` 只能选择 Context Composer 本轮签发的 opaque ref；`message_referent` 必须引用真实消息和其中实际存在的表达。
- 模型不得提交世界坐标、协议实体 ID、内部 observation ID 或 Mineflayer 类型。
- “我”“那里”“这棵”等语义由模型结合对话理解；Grounding 只负责把选择绑定到有来源、有时效的认知观察、当前活动目标、话语角色或记忆，不用固定关键词表冒充理解。
- identity 已知不等于空间位置已知。聊天发送者在协议实体表中被追踪，也不能让 Grounding 使用其墙后或身后的精确坐标；位置无证据时必须保持 unknown。

### 11.3 Grounding 与行为合成

```ts
interface GroundedReferent {
  handle: string
  role: string
  referentResolution: 'resolved'
  evidenceIds: string[]
  worldId: string
  epoch: number
  validUntil: string
  spatialKnowledge: 'known' | 'estimated' | 'unknown'
}

interface GroundedSemanticGoalV1 {
  schema: 'mineintent.semantic-goal.v1'
  objective: GroundedGoalExpressionV1
  methodGuidance: Array<{
    description: string
    groundedReferentHandles: string[]
    strength: 'required' | 'preferred' | 'suggested'
  }>
}

type GroundedGoalExpressionV1 =
  | { kind: 'state'; state: GroundedSemanticStateV1 }
  | { kind: 'all'; goals: GroundedGoalExpressionV1[] }
  | { kind: 'any'; goals: GroundedGoalExpressionV1[] }

interface GroundedSemanticStateV1 {
  id: string
  concept: string
  description: string
  arguments: Record<string,
    | { kind: 'self' }
    | { kind: 'grounded_referent'; handle: string }
    | { kind: 'value'; value: string | number | boolean; unit?: string }
  >
}

interface GroundingInformationGap {
  referentHandle: string
  property: string
  requiredByStateIds: string[]
}

interface GroundedEmbodiedIntent {
  effectId: string
  groundingStatus: 'complete' | 'partial'
  semanticGoal: GroundedSemanticGoalV1
  referents: GroundedReferent[]
  missingInformation: GroundingInformationGap[]
  constraints: EmbodiedIntentEffect['constraints']
}

type EmbodiedGroundingResult =
  | { status: 'grounded'; intent: GroundedEmbodiedIntent }
  | {
      status: 'needs_clarification' | 'invalid' | 'unavailable'
      effectId: string
      reasonCode: string
      ambiguousRoles?: string[]
    }

interface BehaviorPlanRef {
  id: string
  intentEffectId: string
  connectionEpoch: number
  basedOnInformationReadIds: string[]
  basedOnEvidenceIds: string[]
  controllerContractRevision: string
  validUntil: string
}

type BehaviorSynthesisResult =
  | { status: 'planned'; plan: BehaviorPlanRef }
  | { status: 'information_needed'; intentEffectId: string; needs: GroundingInformationGap[] }
  | { status: 'unsupported_goal'; intentEffectId: string; stateIds: string[] }
  | { status: 'no_feasible_plan'; intentEffectId: string; reasonCode: string }
```

Grounding 先验证语义引用并把 `referent_role` 改写为有来源、有时效的 grounded handle。它只回传结构化 `semanticGoal`，不把 `desiredOutcome`、玩家原话或协议实体表传给 Behavior Synthesizer。Grounding 之后仍不能直接执行。Behavior Synthesizer 根据 grounded semantic goal、合法 Information Read、对象当前可供性、Epistemic Map、身体状态和安全限制形成内部计划。

Behavior Synthesizer 可以是确定性策略、模型辅助规划或混合实现，但输入不包含 raw protocol/完整世界，输出一律视为待验证计划。模型辅助不会扩大权限：伪造 handle、越过 Information/Epistemic 边界、资源冲突和安全失败仍由确定性预检拒绝。

Behavior Synthesizer 不得接收或对玩家原话、`desiredOutcome`、`summary` 或未 Grounding 的 `role` 做关键词/正则/保留词分派。它消费的是通用目标组合契约、grounded handle、方法约束与可供性，而不是固定命令枚举表。确定性实现可明确声明只支持契约的子集，不支持的目标返回 `unsupported_goal`；模型辅助或混合实现也不因此获得额外执行权。

`BehaviorPlanRef` 只让 Decision Coordinator 追踪计划来源、时效和 controller contract revision，不规定计划内部是逐 tick 输入、连续控制器还是受约束流程。该抽象在 v0.3 重新设计；v0.2 先固定[合法信息接口、Help 发现与 UI 会话](./information-access-and-ui.md)。

收到“看向我”时，主模型依语境形成“自己的视觉注意与说话者建立关系”的状态表达，其参数引用 `self` 和 `referent_role: speaker`，并用消息指代选择说话者。系统设计不把这个关系注册成 `look_at_player` 别名，也不规定它对应哪段输入；规划器根据当前身体、感知和可供性负责实现。

Grounding 若已确定说话者身份，即使没有方向证据，也返回 `groundingStatus: 'partial'`、`spatialKnowledge: 'unknown'` 和 `missingInformation: spatial_direction`，让意图进入 Behavior Synthesizer。Synthesizer 可以合成转动扫描、移向有证据的候选区域或等待新观察等信息获取计划，不能读取 raw entity 坐标一步转准。若无法形成合法的信息获取计划，它只返回 `information_needed`；是否开口询问、等待或放弃由 Companion 决策层决定，不是 Behavior Synthesizer 的职责。

只有“我”的身份本身歧义时，Grounding 才返回 `needs_clarification`；证据过期、伪造时返回 `invalid`，已知不在当前世界或当前具身目标不可用时返回 `unavailable`。头部到达某角度只表示 Motor 阶段完成；必须由后续视觉观察确认是否真的看见。所谓持续跟踪只是同一具身意图仍有效并被控制循环持续满足，不是 `track_player` 技能。

“家里木材不够了，去砍面前这棵树”和“这棵树挡路了，去砍了它”可以最终合成同一段身体行为，但 Companion 不应产生同一个上层目标：

- 前者的期望状态是可用木材增加；指定树是资源来源/方法引导。树被破坏但掉落物未获得时不能宣称目标完成，后续还可根据库存决定继续获取或返家存放。
- 后者的期望状态是指定阻挡物被移除，必要时还要验证所在区域可通行；使用斧头连续攻击只是候选实现。木材进入背包可以是副作用，不是这个目标的成功根据。

“去砍”在具体对话中是强制方法、偏好方法还是对目的的自然语言解释，由 Companion 结合语气、关系和当前情境填入 `methodGuidance.strength`。Behavior Synthesizer 只尊重已形成的契约，不重新阅读原话。这保证系统可以“同目标换方法”和“同方法服务不同目标”，而不引入 `chop_tree_for_wood`/`chop_tree_for_path` 之类任务技能。

### 11.4 提交与拒绝

具身意图可能在三个阶段停止：

1. semantic validation：引用不属于本轮、消息表达不存在或 schema 无效；
2. grounding：身份指代歧义、证据过期/伪造、已知跨世界或当前具身目标不可用；仅空间位置未知不在此停止，而是以 `partial` 继续；
3. synthesis/preflight：当前无可行行为、无法合法补足缺失信息、资源冲突、安全拒绝或计划的信息 revision/证据条件不成立。

`information_needed` 不是询问动作，而是返给 Companion Runtime 的结构化决策事实。Companion 可在后续决策中自然询问、等待新证据或放弃意图；Coordinator 不会把它自动转换为固定聊天文本。

只有 Grounding 与内部计划预检都成功，才允许发送依赖“已经开始行动”的承诺语言。运行中的副作用、取消和部分结果继续逐项记录，最终事实仍由 Perception、Motor feedback 与 Outcome Verification 决定。

## 12. 记忆候选与后续关注

```ts
interface MemoryCandidateEffect extends EffectBase {
  kind: 'memory_candidate'
  memoryKind: 'episode' | 'world_fact' | 'player_preference' | 'relationship' | 'commitment' | 'procedural'
  content: string
  sourceClaim: 'player_stated' | 'observed' | 'derived'
  evidenceEventIds: string[]
  subjects: string[]
  placeIds?: string[]
  activityId?: string
  confidence: number
  validAt?: string
}

interface AttentionEffectV2 extends EffectBase {
  kind: 'next_attention'
  waitFor: Array<'player_message' | 'embodied_progress' | 'embodied_terminal' | 'world_event' | 'natural_opportunity'>
  focus: string
  embodiedIntentIds?: string[]
  earliestProactiveAt?: string
  expiresAt?: string
}
```

记忆候选不直接落入长期记忆；Memory System 校验证据、分类、重复和冲突。模型不能创建 profile 来源或 verified 状态。承诺候选必须能关联已发送语言或玩家明确话语。

每个决策最多一个 next attention。它只是给 Router 的建议，不建立无限模型循环，也不能压过玩家消息、危险和管理控制。

## 13. 组合限制

```ts
type DecisionEffectV2 =
  | SpeechEffectV2
  | ActivityEffect
  | IntentEffect
  | EmbodiedIntentEffect
  | MemoryCandidateEffect
  | AttentionEffectV2
```

- 每个决策最多一个 activity、intent、embodied intent 和 next attention。
- 可有多个 speech 和 memory candidate，但受数量和文本长度限制。
- speech 只能引用 embodied intent effect。内部 controller plan、grounded handle 和行为计划 ID 都不是模型可引用的依赖目标。
- effect ID 不作为跨决策长期 ID；持久对象由运行时分配。
- 空 `effects` 合法，表示有意识地观察；仍须给出 summary。

## 14. Model Provider 与输出修复

```ts
interface ModelProvider {
  runDecision(input: {
    context: ContextPackageV2
    outputSchema: object
    signal: AbortSignal
  }): Promise<{
    rawOutput: unknown
    provider: string
    model: string
    usage?: { inputTokens?: number; outputTokens?: number }
  }>
}
```

优先使用厂商 structured output；不支持时可回退为 JSON 文本，但经过同一严格校验。Provider 只处理传输、鉴权、取消和格式差异，不应用业务效果。

解析顺序为严格 JSON、schema、引用与语义校验。禁止用正则从 Markdown “尽量提取” JSON。结构失败允许至多一次修复调用；修复只接收验证错误、原始输出和同一 schema，不增加世界上下文，仍绑定原 run。再次失败产生 `model.decision.invalid`，不应用效果。

超时、取消、拒绝和限流分别记录。故障兜底不能假装模型理解了消息或动作已开始。

## 15. 过期判定

Coordinator 在模型返回和提交前检查：

1. `runId` 是否仍是当前可提交 run。
2. companion、session、world 是否一致。
3. companion revision 是否相同。
4. profile 和 capability revision 是否相同。
5. 是否有晚于 `throughEventSequence` 的阻断事件。
6. 每个具身意图的引用、Grounding 证据和内部计划前置条件是否仍成立。

阻断事件包括明确停止、死亡/重生、断线/维度切换、活动或意图 revision 改变、暂停、主要玩家身份变化和更高优先级危险。无关普通观察不应只因 sequence 增长误杀决策，但动作仍重新预检。

过期时所有状态、动作和记忆效果均不应用，语言也不从旧结果摘取发送；如仍需回答则新建 run。系统产生 `model.decision.discarded_as_stale` 并记录 guard 差异，由 Router 决定是否重跑。

## 16. Decision Coordinator

```text
1. 校验协议、schema、ID、语义引用和数量
2. 校验 run 与 freshness guards
3. 校验 activity / intent 状态转换
4. Grounding 语义指代，区分 `complete`/`partial`，拒绝身份歧义、伪造、过期和已知不可用目标；无证据定位保持 unknown
5. Behavior Synthesizer 形成内部计划
6. 对内部行为计划做原子预检并预留运行资源
7. 校验 speech 依赖和真实性时机
8. 形成不可变 Application Plan
9. 提交 activity / intent / grounded embodied intent
10. 启动内部计划，取得 accepted 或 rejection
11. 只发送依赖条件已满足的 speech
12. memory candidate 进入候选队列，更新 next attention 并记录逐项结果
```

不同种类效果可以部分接受，例如证据不足的记忆候选被拒绝，而不依赖具身行为的正常回复仍发送。Coordinator 只接收严格解析的 v2 proposal，内部行为计划不可部分预检通过。计划形成后若出现停止、断线或危险抢占，未提交效果停止提交，已启动 controller 按其取消契约终止并释放资源。

```ts
interface DecisionApplicationResult {
  runId: string
  contextRevision: number
  status: 'applied' | 'partially_applied' | 'rejected' | 'stale'
  effects: Array<{
    effectId: string
    status: 'accepted' | 'rejected' | 'deferred' | 'cancelled_before_commit'
    code?: string
    producedIds?: string[]
  }>
}
```

## 17. 运行中事件

- `collect`：只在尚未发起调用的短聚合窗口加入事件。
- `steer`：取消请求或标记旧结果不可提交，加入新事件并创建新 run/context；语义上仍是同一话题的修改。
- `interrupt`：同样取消并重跑，但表示高优先级事件替换当前关注。
- `follow_up`：提交阶段无法安全取消时排队，完成后以最新状态新建 run。
- `observe`：只更新投影，不调用模型。

不假设模型厂商支持真正的运行中上下文注入。因此 steer 明确实现为取消旧推理并重新运行。无法网络取消的迟到结果因 run ID 不匹配拒绝。重跑必须使用新 run、revision 和事件集合。

## 18. 提示注入与权限

- 系统提示明确标记片段来源和数据边界。
- 玩家可要求同伴改变做法，但不能通过聊天提升为管理控制或绕过具身意图、Grounding 与内部计划 schema。
- 书、告示牌、物品名、服务器消息、记忆和外部文本永远不解释为系统指令。
- 档案可编辑人格，但不能关闭真实性、证据、停止、安全和权限约束。
- 主模型只看到身体可供性、限制和证据要求，不看到可调用 skill、输入模板或协议事务集合。V2 schema 没有执行选择字段；自由文本即使碰巧包含内部名称也只作为语义数据处理，不获得权限且不因词面相同被拒绝。
- 模型不能请求任意代码、shell、文件或网络访问。

## 19. 事件与审计

至少记录：

- `model.context.composed`：ref、触发、每节 token、来源、截断和遗漏。
- `model.decision.started`：run、provider、model、deadline。
- `model.decision.cancelled`：steer、interrupt、stop 或 timeout。
- `model.decision.invalid`：schema 错误和修复次数。
- `model.decision.finished`：usage、耗时和输出哈希。
- `model.decision.discarded_as_stale`：guard 的期望与实际值。
- `model.decision.application_planned`：效果、Grounding 结果和内部计划预检。
- `embodiment.referent.grounded_partial/grounded_complete/rejected`：语义选择、证据、空间已知性、缺失信息和拒绝原因。
- `embodiment.plan.synthesized/information_needed/rejected`：内部计划引用、使用的信息/可供性、所需信息和拒绝原因。
- `model.decision.applied`：逐效果结果和产生的领域对象 ID。

原始输出可按本地调试策略保存，但不是状态恢复的事实来源。恢复只重放已提交后产生的领域事件。

## 20. 实现接口

```ts
interface ContextComposer {
  compose(request: {
    protocol: ContextProtocol
    runId: string
    triggerEventIds: string[]
    route: ContextPackageV2['trigger']['route']
    limits: ContextPackageV2['limits']
  }): Promise<ContextPackageV2>
}

interface NormalizedDecisionProposal {
  sourceProtocol: 'mineintent.decision.v2'
  runId: string
  context: DecisionContextRef
  effects: DecisionEffectV2[]
}

interface DecisionProtocolDispatcher {
  parse(raw: unknown, context: ContextPackageV2): CompanionDecisionV2
  normalize(decision: CompanionDecisionV2, context: ContextPackageV2): NormalizedDecisionProposal
}

interface V2ProposalNormalizer {
  normalize(decision: CompanionDecisionV2, context: ContextPackageV2): NormalizedDecisionProposal
}

interface DecisionCoordinator {
  preflight(proposal: NormalizedDecisionProposal): Promise<ApplicationPlan>
  apply(plan: ApplicationPlan): Promise<DecisionApplicationResult>
}
```

Dispatcher 先读取精确 protocol，再用严格 schema 解析；只接受 context.v2 与 decision.v2。v1、`action_group`、`skill_registry`、混合字段或额外字段直接拒绝。Coordinator 只接收规范化 proposal。

建议位置为 `src/context/`、`src/models/contracts/v2.*` 和 `src/companion/decision-coordinator.ts`。不得建立 `src/models/compat/` 或旧协议 adapter。TypeScript 类型应从运行时 schema 推导，防止类型与 JSON Schema 漂移。

## 21. 测试与验收

### Schema 与上下文

- v2 效果有版本化 schema；未知字段、重复 ID、错误引用和伪造 context ref 拒绝。
- context/decision v1、V2 中夹带 action group/skill registry 和混合字段均拒绝；Coordinator 测试只接收 normalized proposal。
- 相同输入产生稳定片段顺序和内容等价结果。
- 每个片段保留来源、信任、原始/实际 token 和截断信息。
- 必需内容超预算失败，不静默丢弃。
- 恶意记忆或世界文本不能改变指令优先级、伪造 grounded referent 或取得 controller/计划选择权限。
- 正常 speech、summary、desiredOutcome 即使包含与内部输入/协议名称相同的词，也不因词面命中而执行或拒绝。
- `semanticGoal` 只接受版本化的目标组合、参数和方法引导契约；额外字段、输入序列/代码、未声明 role 和坐标引用均拒绝。新语义 `concept` 不得直接获得执行权限，不支持时显式返回 `unsupported_goal`。
- 保持 `semanticGoal` 不变、仅改写 `desiredOutcome`/`summary` 不改变 Grounding 结果或 Behavior Plan；不同措辞生成相同语义目标时计划等价。
- “木材不足”和“树挡路”在对同一树产生相同身体计划时，仍保留不同的上层期望状态、完成证据和后续决策；不生成原因专用技能。

### 过期、动作和语言

- 推理期间改变活动、编辑档案、停止或断线，旧结果全部拒绝。
- 无关普通观察不会误杀仍适用决策。
- steer 创建新 run，迟到结果拒绝。
- 身份指代歧义、证据无效、目标不可用或内部计划预检失败使具身意图停止；仅空间信息未知必须以 `partial` 进入合成。
- v2 具身意图拒绝时，依赖它的承诺语言不发送；无依赖的社交回复可正常发送。
- “完成”语言只由已验证结果触发。
- “看向我”在身份已知、方向未知时产生 `partial` Grounding，不会使用 tracked entity 精确坐标；Synthesizer 可扫描或返回 `information_needed`，Companion 可随后询问，只有新视觉证据才能生成“看见”。
- 静态点、移动实体和可交互表面不产生对象专用 model-facing skill；同一具身意图路径经过 Grounding 与行为合成。
- “看向我/看我一下/转过来瞧瞧/你能找到我吗”等相同语义不同表达产生等价 grounded outcome；包含相同动词但语义不同的讨论不会误触发行为。
- 两种具有相同可供性的对象复用同一行为合成路径；增加新对象类型不增加玩家措辞或对象专用分支。

### 记忆与审计

- 无证据事实候选被拒绝，不影响其他合法效果。
- 每个接受效果能追到 context、run、触发事件和领域事件。
- 重启不重放模型原始输出或重复应用未提交计划。

## 22. v0.2 实现门

1. `CompanionDecisionV2` 不含 `actions[].skill`、输入模板、协议事务或其他执行选择字段。
2. v1 `action_group`、旧 model-facing skill schema 和 `availableSkills` 直接删除；Coordinator 只接受 normalized v2 proposal。
3. `follow_player`、`collect_wood` 等原型对象专用能力不进入新运行时；后续能力只从 Grounding + Behavior Synthesis + 受约束 controller/plan 形成。
4. Context 能力片段只描述身体可供性、限制和可靠性，不暴露命令目录。
5. 从玩家消息到 semantic referent、grounded evidence、内部计划、Motor feedback 和最终事实具有完整 trace。
6. Behavior Synthesizer 不按玩家原话、desiredOutcome 或 role 的关键词/正则分派行为，并通过同义表达与同可供性对象等价测试。
