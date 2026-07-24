---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-23
applies_to: codex/trustworthy-passive-context@57d438e
---

# MineIntent 模型接口与工具说明

> 当前实验实现的人类可读视图。代码中的 Zod schema 和 TypeScript 类型是最终权威；本文件用于审查接口语义，不参与运行。

## 1. 首先明确：当前没有 model-facing tool call

当前生产模型请求不含 OpenAI-compatible `tools`、`tool_choice` 或 `functions`。

模型的交互方式是一次结构化决策：

```text
ContextPackageV2
→ 模型
→ CompanionDecisionV2
→ Dispatcher / Grounding / Behavior / Motor
```

模型不能直接调用代码函数，也不能选择 Mineflayer 方法、键鼠模板、协议事务或世界坐标。

这不是 DeepSeek 的能力限制，而是当前 MineIntent 的接口选择。DeepSeek 同时提供 Tool Calls、思考模式下的多轮工具调用和 JSON Output；当前 Agent Service 只使用了最后一种。

### 1.1 DeepSeek 的三个协议能力不是同一件事

| 能力 | 解决的问题 | 当前 Agent Service |
|---|---|---|
| 思考模式 | 在最终输出前进行内部推理，以 `reasoning_content` 和最终 `content` 分开返回 | 没有显式开关；对默认开启思考的模型，依赖供应商默认值 |
| Tool Calls | 让模型请求宿主执行函数，并将结果作为 `role: tool` 消息回传 | 未使用 |
| JSON Output | 保证最终 `content` 是合法 JSON 字符串 | 已使用 `response_format: {"type":"json_object"}` |

JSON Output 不会执行工具，也不会使模型获得新的游戏信息。它只约束当前请求的最终文本格式。

当前请求还存在三项 DeepSeek 接入事实需要显式化：

- 请求传入了 `temperature: 0.4`；若思考模式开启，DeepSeek 会接受但忽略该参数。
- Prompt 已包含 `JSON` 字样和完整 schema，但没有按 DeepSeek 文档建议提供输出样例，也没有显式设置 `max_tokens`。
- 当前代码只读取最终 `message.content`。在没有工具调用的单次请求中无需保存 `reasoning_content`；一旦加入思考模式 Tool Calls，就必须在该工具调用链及后续请求中完整回传它，否则 DeepSeek 会返回 HTTP 400。

因此，“加入模型工具”不是在请求里补一个 `tools` 数组即可，而是需要一个有预算、可取消、能保存完整 assistant/tool 消息的 provider loop。

## 2. 模型输入：`ContextPackageV2`

每次决策的 Context 都绑定一个 `runId` 和当前同伴状态：

```ts
interface ContextPackageV2 {
  protocol: 'mineintent.context.v2'
  ref: {
    runId: string
    companionId: string
    sessionId: string
    worldId: string
    companionRevision: number
    throughEventSequence: number
    profileVersion: string
    capabilityRevision: string
  }
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
  omissions: ContextOmission[]
}
```

`fragments` 可包含：

| section | 内容 | 信任边界 |
|---|---|---|
| `product_constraints` | 产品和安全不变量 | Runtime authoritative |
| `companion_profile` | 初始性格与记忆设定 | Profile instruction |
| `relationship_core` | 主要玩家与关系核心 | Runtime authoritative |
| `current_state` | 当前活动、意图和控制状态 | Runtime authoritative |
| `trigger_events` | 聊天或其他触发事件 | 保留 player/event 来源 |
| `observations` | 完整 Information Read envelope | Verified observation |
| `retrieved_memories` | 检索到的记忆及证据 | Remembered record |
| `capabilities` | 身体可供性元数据 | Runtime authoritative |

### 2.1 相对坐标观察

普通 viewport 不向模型公开世界绝对坐标。可见对象使用：

```ts
type RelativePosition = [right: number, up: number, forward: number]
```

- 原点是本次 Read 捕获时同伴的脚部位置。
- `right/forward` 由当时的身体 yaw 定义；`up` 与世界竖直轴对齐。
- 默认量化到 0.5 格。
- 只有通过视锥、遮挡和 loaded-world 检查的对象才能发布。
- 坐标用于理解几何关系；选择目标必须使用同项的 opaque `ref`。

示例：

```json
{
  "protocol": "mineintent.information-read.v1",
  "readId": "read_...",
  "interfaceId": "viewport_information",
  "informationRevision": 42,
  "observedAt": "2026-07-22T14:00:00.000Z",
  "validUntil": "2026-07-22T14:00:15.000Z",
  "values": {
    "visibleBlocks": {
      "blocks": [
        {
          "ref": "iref_...",
          "name": "gold_block",
          "relativePosition": [-1.5, 1, 3]
        }
      ],
      "truncated": false
    }
  }
}
```

## 3. 模型输出：`CompanionDecisionV2`

```ts
interface CompanionDecisionV2 {
  protocol: 'mineintent.decision.v2'
  runId: string
  context: DecisionContextRef // 必须与输入 Context.ref 完全一致
  summary: string
  effects: DecisionEffectV2[] // 最多 16 个
}
```

### 3.1 Effect 类型

| kind | 作用 | 是否直接执行身体动作 |
|---|---|---:|
| `speech` | 说话、协调、提问或报告 | 否 |
| `activity` | 提议、更新、暂停或完成共同活动 | 否 |
| `intent` | 设置或清除当前意图 | 否 |
| `embodied_intent` | 提出带语义引用的期望身体状态 | 仅进入 Grounding，不直接执行 |
| `memory_candidate` | 提交带来源的候选记忆 | 否 |
| `next_attention` | 表达后续关注条件 | 否 |

## 4. `embodied_intent` 接口

```ts
interface EmbodiedIntentEffect {
  id: string
  kind: 'embodied_intent'
  summary: string
  desiredOutcome: string
  semanticGoal: {
    schema: 'mineintent.semantic-goal.v1'
    objective: SemanticState | AllExpression | AnyExpression
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
  constraints: {
    maxDurationMs?: number
    interruptibility: 'immediate' | 'checkpoint'
  }
}
```

### 4.1 选择已观察对象

```json
{
  "id": "embodied_attention",
  "kind": "embodied_intent",
  "summary": "将视觉注意移到已观察的对象",
  "desiredOutcome": "视觉注意覆盖该对象",
  "semanticGoal": {
    "schema": "mineintent.semantic-goal.v1",
    "objective": {
      "kind": "state",
      "state": {
        "id": "state_attention",
        "concept": "self.attention_includes",
        "description": "自身视觉注意覆盖选定对象",
        "arguments": {
          "observer": { "kind": "self" },
          "subject": { "kind": "referent_role", "role": "subject" }
        }
      }
    },
    "methodGuidance": []
  },
  "referents": [
    {
      "role": "subject",
      "selection": { "kind": "context_ref", "ref": "iref_..." }
    }
  ],
  "constraints": {
    "maxDurationMs": 8000,
    "interruptibility": "immediate"
  }
}
```

### 4.2 选择玩家话语中的指代

`看向我` 中的 `我` 不需要转换成坐标或 `look_at_player` 技能：

```json
{
  "role": "speaker",
  "selection": {
    "kind": "message_referent",
    "eventId": "event_player_chat_...",
    "expression": "我"
  }
}
```

Grounding 先绑定说话者身份。若没有合法方向证据，它返回 `partial + spatial_direction unknown`，Behavior 可合成有界扫描，不能使用 tracker 精确坐标一步转准。

## 5. 身体能力元数据与真实实现状态

`BODY_CAPABILITY_CATALOG` 只是描述数据，不包含回调函数、controller ID 或输入模板。

| affordance | Catalog 已描述 | 当前 Behavior/Controller 真实状态 |
|---|---:|---|
| `gaze_change` | 是 | 已接入：渐进转头、身份扫描、取消、超时和视觉验证 |
| `locomotion` | 是 | 尚未接入当前 Behavior 链路 |
| `primary_interaction` | 是 | 尚未接入当前 Behavior 链路 |
| `secondary_interaction` | 是 | 尚未接入当前 Behavior 链路 |
| `inventory_selection` | 是 | 尚未接入当前 Behavior 链路 |
| `wait` | 是 | 只有 Runtime 安全停止/释放输入，尚无通用 Behavior operator |

这张表暴露了一个当前需要评审的不一致：Catalog 正在描述目标身体可供性，而不是仅列出已上线的执行能力。如果模型将其解读为“现在可用”，会产生合法但当前无法合成的语义目标。

## 6. 内部 Information 查询协议（当前未注册给模型）

代码中仍有两个查询适配器：

```text
information_catalog
information(interfaceId, operation: help | read)
```

它们当前只是 `InformationRuntime` 的内部契约/测试适配器，没有进入模型 HTTP 请求。

`information_catalog` 的输入为：

```ts
interface InformationCatalogRequest {
  operation: 'list_interfaces'
  knownCatalogRevision?: string
}
```

```ts
type InformationQueryRequest =
  | {
      interfaceId: InformationInterfaceId
      operation: 'help'
      availability?: 'all' | 'current'
      search?: string
      fields?: string[]
    }
  | {
      interfaceId: InformationInterfaceId
      operation: 'read'
      schemaRevision: string
      fields: string[]
      selector?: InformationSelectorRef
      page?: { cursor?: string; limit?: number }
    }
```

生产 Context Composer 目前使用固定、有界的被动 Read plan，将完整 envelope 注入 Context，不运行 model tool loop。

### 6.1 如果启用 Tool Calls，推荐的边界

模型可调用的工具只应是信息查询接口：

| 工具 | 给模型的职责说明 |
|---|---|
| `information_catalog` | 列出当前调用者可发现的信息接口、schema revision 和整体可用性；不确定有哪些信息时先调用它 |
| `information` / `help` | 查询指定接口有哪些字段、字段语义、精度、来源和当前 availability；不读取字段值 |
| `information` / `read` | 按已知 schema revision 读取指定字段，返回带 provenance、revision、时效和逐字段 availability 的证据 envelope |

```text
首次模型请求（ContextPackageV2 + information tools）
  → assistant.tool_calls
  → MineIntent 校验参数、权限、预算、deadline
  → InformationRuntime 执行 catalog/help/read
  → role: tool + tool_call_id + 结果 envelope
  → 带完整 assistant 消息继续请求
  → 无 tool_calls 后输出 CompanionDecisionV2
```

必须继续保留以下边界：

- `information_catalog` 和 `information` 可成为模型工具；身体动作不可成为模型工具。
- Tool result 必须保留 availability、revision、observedAt、validUntil、来源和截断信息，不能压扁为一句自然语言。
- 每个决策轮限制调用次数、Read 次数、返回字节、deadline 和可见字段；现有 `InformationToolSession` 已具备这些宿主侧预算语义。
- `reasoning_content` 只为满足供应商续接协议而保存在 provider 会话中，不写入游戏聊天、Context、产品日志或长期记忆。
- 工具参数只能选择接口、字段、selector 和分页，不能形成“脚下方块”“看向玩家”“砍树”等对象或任务专用工具。
- 工具调用阶段只获得证据；最终仍须输出 `CompanionDecisionV2`，由 Dispatcher、Grounding、Behavior 和 Motor 接管。

### 6.2 DeepSeek `strict` Tool Calls 的适配限制

DeepSeek 的 `strict` 模式目前是 Beta，需要使用 `/beta` base URL，并要求每个 object 的全部 properties 都列入 `required`、`additionalProperties` 为 `false`。它也不支持 `minLength`、`maxLength`、`minItems` 和 `maxItems`。

当前 Zod 契约包含 optional 字段、字符串长度和数组数量约束，所以不能不经转换就把生成的 JSON Schema 注册为 DeepSeek strict function。可行做法是：

1. 为 DeepSeek strict 单独生成兼容的窄工具参数 schema，例如用 `anyOf` 表达 help/read 两种完整形态；
2. API strict 只作为第一层格式约束，宿主仍用现有 Zod schema、授权、预算和 revision 规则复验；
3. 不把完整 `CompanionDecisionV2` 伪装成一个 function tool。它是最终决策协议，不是宿主能力调用。

## 7. 接口与实现的分层

```text
模型语义输出
  CompanionDecisionV2 / EmbodiedIntentEffect
                 │
                 ▼
  DecisionProtocolDispatcher
                 │ 校验 Context/ref/effect
                 ▼
  GroundingEngine
                 │ opaque ref → 内部 grounded handle
                 ▼
  BehaviorSynthesizer
                 │ grounded semantic goal → 有界 plan
                 ▼
  VisualAttentionController
                 │ plan → 连续控制与当前可见性复查
                 ▼
  MinecraftMotorDriver
                 │
                 ▼
  Mineflayer / Minecraft protocol
```

关键边界：

- 模型只表达期望状态和语义引用。
- Grounding 的 `context_ref` 只接受本轮 viewport block/entity ref；`message_referent` 当前只验证表达式属于该消息，随后绑定消息发送者，并非通用指代解析。
- Behavior 不读取玩家原话、`summary`、`desiredOutcome`、世界坐标或 Mineflayer 对象。
- Motor 不选目标；它只执行有界的底层控制。
- 只有模型明确标为 `after_intent_terminal` 且依赖 `completed` 的话术会等待对应终态；`timing: now` 的文本不会经过语义 Claim Policy，运行时目前不能阻止模型把无依据的完成断言立即发出。

## 8. 当前已知接口问题

1. `viewport_information` 的 `informationRevision` 目前过度跟随 raw perception revision。真实世界中，Read 与 Grounding 之间的无关协议更新可能立即使 ref 失效。
2. Capability Catalog 混合了“目标身体可供性”和“当前已实现能力”。
3. 当前生产 Behavior 仅实现 `self.attention_includes`；其他 semantic concept 会显式返回 `unsupported_goal`。
4. 相对坐标与 opaque ref 有单元测试，但现有确定性 Paper companion 场景使用 `message_referent`，没有覆盖 `context_ref`；最新实验提交也没有关联 Paper run，不能宣称整条路径已验收。
5. DeepSeek provider 配置没有显式声明 thinking 开关/强度，行为依赖所选模型与供应商默认值。
6. JSON Output 请求没有显式 `max_tokens`，也没有覆盖 DeepSeek 文档所述空 `content` 情况的有限重试策略。
7. 当前没有 Tool Calls 状态机；直接注册内部 Information adapters 会缺少 assistant `reasoning_content`/`tool_calls` 回传和最终决策阶段切换。
8. Controller 复查可见性时没有要求 perception revision 比动作前更新；事件名虽为 `outcome_verified`，尚不能证明一定产生了 fresh observation。

## 9. 代码索引

| 内容 | 文件 |
|---|---|
| Context 与 Decision schema | `src/models/contracts.ts` |
| 模型 system prompt | `agent-service/prompt.py` |
| 实际 HTTP 请求（无 tools） | `agent-service/server.py` |
| 能力元数据 | `src/capabilities/catalog.ts` |
| 被动 Information Context | `src/information/context-composer.ts` |
| Information 契约 | `src/information/contracts/v1.ts` |
| 未注册的 Information 工具适配器 | `src/information/tool-session.ts` |
| Grounding | `src/grounding/grounding-engine.ts` |
| Behavior 合成 | `src/behavior/behavior-synthesizer.ts` |
| 注视 Controller | `src/motor/visual-attention-controller.ts` |
| 底层 Motor Driver | `src/minecraft/motor-driver.ts` |

## 10. 供应商协议参考

- [DeepSeek：思考模式](https://api-docs.deepseek.com/zh-cn/guides/thinking_mode/)
- [DeepSeek：Tool Calls](https://api-docs.deepseek.com/zh-cn/guides/tool_calls/)
- [DeepSeek：JSON Output](https://api-docs.deepseek.com/zh-cn/guides/json_mode/)
