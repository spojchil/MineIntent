---
status: accepted
authority: normative
implementation: partial
last_verified: 2026-07-23
---

# 记忆模型、档案版本与冲突协调

> 本设计由 [PR #23](https://github.com/spojchil/maineintent/pull/23) 合并，对应 Issue [#4](https://github.com/spojchil/maineintent/issues/4)。当前实现只有带 evidence 的 JSON 记录和关键词/时间检索；本文描述的档案版本、冲突、纠正、取代、反思和关系连续性尚未实现。上游：[目标系统](./target-system.md)、[领域事件](./domain-events-and-journal.md)、[决策协议](./decision-contract-and-context.md)。

## 1. 目的

本文定义 MineIntent v0.1 的持久记忆边界：什么值得成为记忆，如何保存来源和证据，如何区分历史与当前事实，如何让可编辑同伴档案影响现在而不篡改过去，以及如何在重启后协调服务器真实状态与旧记录。

记忆服务于“这是和我共同生活过的同伴”，而不是把所有聊天塞进向量库。原始领域事件负责可审计历史；工作状态负责眼前情境；长期记忆只保存未来有价值且能说明依据的内容。

## 2. 范围与非目标

本文负责：

- 同伴档案及不可变版本。
- 情景、世界事实、社交、承诺和程序经验的 schema 与生命周期。
- 候选验证、去重、纠正、争议、取代、删除和后台整理。
- 检索过滤、排序、证据回溯和上下文交付。
- 档案变更后的派生记忆重评。
- 启动、重连和世界状态变化后的事实协调。

本文不负责：

- 把当前对话或 Companion State 复制为长期记忆。
- 用向量相似度替代事实状态、世界隔离和来源判断。
- 模拟完整人类遗忘或情感心理。
- 第一版跨用户云同步和多同伴共享记忆。
- 把模型私有推理作为记忆。

## 3. 不变量

1. 已发生的情景历史不可就地改写；纠正通过新记录和关系表达。
2. “过去观察到”与“现在仍成立”是不同命题。
3. 模型输出只能创建候选，不能自证为 verified。
4. 每条长期记忆有来源、证据、置信度、时间、主体、状态和世界作用域。
5. 档案版本不可变；编辑和撤销都创建新版本。
6. 当前人格使用最新生效档案，旧档案只用于理解当时行为。
7. 一个世界的地点、容器和资源事实不能泄漏到另一世界。
8. 摘要、反思和关系理解必须能回到原始记忆及事件。
9. 删除立即退出正常检索；审计保留与可恢复状态必须分离。
10. 原始事件日志不是 Memory Store 的重复副本，也不由记忆整理重写。

## 4. 数据分类

| 类型 | 作用 | 是否长期 | 权威来源 |
|---|---|---:|---|
| Profile | 用户主动设定的同伴人格、背景、表达倾向 | 是，版本化 | 用户编辑的档案版本 |
| Working State | 当前对话、活动、意图、动作与未决问题 | 否，投影/检查点 | Companion Runtime |
| Episode | 有时间、地点、参与者的共同经历 | 是 | 领域事件与已验证动作结果 |
| World Fact | 地点、设施、容器或环境事实 | 是，可失效 | 观察、查询与世界验证 |
| Social | 玩家明确偏好、称呼、关系理解 | 是 | 玩家陈述或带证据推断 |
| Commitment | 双方约定及其状态 | 是，直到终止后归档 | 已发送语言、玩家陈述、活动事件 |
| Procedural | 内部行为模式在具体条件下的成功与失败经验 | 是，聚合 | Action Runtime 结果 |
| Raw Journal | 可审计领域事件和动作轨迹 | 是，另表 | Event Journal |

工作状态和原始日志可以被检索流程引用，但不作为 `memory_records` 的伪造类型。

## 5. 同伴档案

### 5.1 文件与身份

用户编辑一个自然语言 Markdown 档案。文件包含稳定 profile ID 和正文，密钥或运行配置不属于档案。

```ts
interface CompanionProfileVersion {
  profileId: string
  versionId: string
  sequence: number
  content: string
  contentHash: string
  createdAt: string
  effectiveFrom: string
  effectiveUntil?: string
  createdBy: 'user_edit' | 'user_revert' | 'initial_import'
  basedOnVersionId?: string
}
```

### 5.2 版本生成

启动和文件监听器读取档案时：

1. 规范化换行和编码，计算内容哈希。
2. 哈希未变则不创建版本。
3. 校验 profile ID、大小和 UTF-8 内容。
4. 在单事务中关闭旧版本的 `effectiveUntil`，插入新不可变版本。
5. 发布 `profile.version.activated`，更新 Companion State 的 `profileVersion`。
6. 取消或废弃绑定旧 profile version 的模型 run。
7. 将依赖旧档案且描述“当前同伴”的派生记忆排入重评。

撤销不是删除版本，而是以旧正文创建新的 `user_revert` 版本。历史事件仍引用当时真正生效的 version ID。

### 5.3 档案效力边界

档案可以规定名字、语气、初始背景、价值倾向、社交风格和主动程度；不能覆盖真实性、明确停止、安全、权限、证据来源、世界隔离和删除语义。档案中的世界背景只标记为 `profile_instruction`，在实际世界观察前不是 verified world fact。

## 6. 统一记忆记录

```ts
type MemoryKind =
  | 'episode'
  | 'world_fact'
  | 'player_preference'
  | 'relationship'
  | 'commitment'
  | 'procedural'
  | 'reflection'

type MemorySource =
  | 'observed'
  | 'player_stated'
  | 'action_verified'
  | 'derived'
  | 'profile_context'

type MemoryStatus =
  | 'active'
  | 'stale'
  | 'superseded'
  | 'disputed'
  | 'pending_review'
  | 'resolved'
  | 'deleted'

interface MemoryRecord {
  id: string
  kind: MemoryKind
  content: string
  structured: MemoryPayload
  source: MemorySource
  evidence: EvidenceRef[]
  confidence: number
  importance: number
  status: MemoryStatus
  validFrom: string
  validUntil?: string
  observedAt?: string
  createdAt: string
  updatedAt: string
  subjects: string[]
  worldId?: string
  dimension?: string
  placeIds: string[]
  activityId?: string
  profileVersionId?: string
  derivedFromProfileVersionId?: string
  supersedesIds: string[]
  disputedByIds: string[]
}

interface EvidenceRef {
  kind: 'event' | 'action_result' | 'memory' | 'profile_version'
  id: string
  relation: 'supports' | 'contradicts' | 'summarizes' | 'contextualizes'
}
```

`content` 是可给模型和调试界面阅读的简短陈述；`structured` 支持确定性过滤和协调。两者必须表达同一命题。`confidence` 和 `importance` 均为 0–1；高置信度不能弥补没有证据。

## 7. 各类型载荷

```ts
type MemoryPayload =
  | EpisodePayload
  | WorldFactPayload
  | SocialPayload
  | CommitmentPayload
  | ProceduralPayload
  | ReflectionPayload

interface EpisodePayload {
  type: 'episode'
  summary: string
  participantIds: string[]
  startedAt: string
  endedAt: string
  outcome: 'completed' | 'partial' | 'failed' | 'cancelled' | 'open'
  notableEventIds: string[]
}

interface WorldFactPayload {
  type: 'world_fact'
  subject: string
  predicate: string
  value: unknown
  temporalMode: 'historical_observation' | 'current_until_changed' | 'stable_identity'
  verification: 'observed' | 'action_verified' | 'player_claimed' | 'derived'
  lastVerifiedAt?: string
  verificationMethod?: string
}

interface SocialPayload {
  type: 'player_preference' | 'relationship'
  playerId: string
  claim: string
  explicitness: 'explicit' | 'inferred'
  scope: 'session' | 'activity' | 'world' | 'general'
  counterexampleIds: string[]
}

interface CommitmentPayload {
  type: 'commitment'
  promisorIds: string[]
  beneficiaryIds: string[]
  promise: string
  state: 'proposed' | 'accepted' | 'in_progress' | 'fulfilled' | 'cancelled' | 'broken' | 'expired'
  dueAt?: string
  condition?: string
  completionEventIds: string[]
}

interface ProceduralPayload {
  type: 'procedural'
  behaviorPatternId: string
  environmentSignature: string
  attempts: number
  successes: number
  commonFailureCodes: string[]
  meanDurationMs?: number
}

interface ReflectionPayload {
  type: 'reflection'
  claim: string
  scope: 'relationship' | 'activity_pattern' | 'self_style'
  supportingMemoryIds: string[]
  counterexampleMemoryIds: string[]
}
```

`behaviorPatternId` 引用运行时内部、经验证的通用行为模式，不是主模型可调用的 skill 名，也不得按玩家、方块、树等对象类别拆分。程序经验只能影响可供性与行为合成的可靠性估计，不能绕过 Grounding 或动作验证。

### 7.1 历史与当前事实

- `historical_observation` 永不因世界变化变成错误，例如“昨天箱子里有 20 个铁锭”。
- `current_until_changed` 声称当前延续，例如“基地箱子里有铁锭”，重连或新观察可以使它 stale/superseded/disputed。
- `stable_identity` 用于世界 ID、已命名地点等稳定标识；仍可由明确纠正取代。

## 8. SQLite 持久化

```sql
CREATE TABLE profile_versions (
  version_id TEXT PRIMARY KEY,
  profile_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  content TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  effective_from TEXT NOT NULL,
  effective_until TEXT,
  created_by TEXT NOT NULL,
  based_on_version_id TEXT,
  UNIQUE(profile_id, sequence),
  UNIQUE(profile_id, content_hash, effective_from)
);

CREATE TABLE memory_records (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  content TEXT NOT NULL,
  structured_json TEXT NOT NULL,
  source TEXT NOT NULL,
  confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
  importance REAL NOT NULL CHECK(importance BETWEEN 0 AND 1),
  status TEXT NOT NULL,
  valid_from TEXT NOT NULL,
  valid_until TEXT,
  observed_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  world_id TEXT,
  dimension TEXT,
  activity_id TEXT,
  profile_version_id TEXT,
  derived_from_profile_version_id TEXT
);

CREATE TABLE memory_evidence (
  memory_id TEXT NOT NULL,
  evidence_kind TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  relation TEXT NOT NULL,
  PRIMARY KEY(memory_id, evidence_kind, evidence_id, relation)
);

CREATE TABLE memory_subjects (
  memory_id TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  PRIMARY KEY(memory_id, subject_id)
);

CREATE TABLE memory_places (
  memory_id TEXT NOT NULL,
  place_id TEXT NOT NULL,
  PRIMARY KEY(memory_id, place_id)
);

CREATE TABLE memory_relations (
  from_memory_id TEXT NOT NULL,
  relation TEXT NOT NULL,
  to_memory_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(from_memory_id, relation, to_memory_id)
);
```

正文和结构 JSON 保留 schema version，迁移采用显式 upcaster。向量索引是可重建派生索引，不是记录权威；删除或状态变化必须同步使索引结果不可见。

## 9. 候选写入

候选来源包括 Decision Contract 的 `memory_candidate`、确定性事件提取器、动作验证和后台整理。

```text
接收候选
→ schema、长度、世界和主体校验
→ 解析并验证 evidence ID
→ 根据证据确定允许的 source / verification 上限
→ 规范化命题和实体引用
→ 查找精确重复、近似重复和冲突
→ 接受、合并统计、排队复核或拒绝
→ 事务写记录、关系和 memory 领域事件
→ 更新检索索引
```

### 9.1 证据规则

- `observed` 至少引用一个 perceived/shared 领域事件。
- `action_verified` 至少引用 Action Runtime 的 verified terminal result。
- `player_stated` 引用明确玩家话语，只证明玩家说过或表达偏好。
- `derived` 至少引用两条记忆或一个明确事件链，并保存推断范围。
- `profile_context` 只用于说明当时同伴行为背景，不能生成已验证世界事实。
- 找不到证据的模型候选拒绝为 `missing_evidence`。

### 9.2 去重与合并

- 相同规范化命题、作用域和有效时间的精确重复不新增记录，只添加证据并更新访问元数据。
- episode 不因摘要相似就合并；共同事件区间高度重合时才视为同一经历。
- procedural 按 behavior pattern 与 environment signature 聚合计数。
- social 推断的相似说法可归入同一 claim，但反例作为证据保留。
- world fact 的新值不覆盖旧行，而是按时间和验证关系创建新记录。

## 10. 生命周期与冲突

### 10.1 状态含义

- `active`：可正常检索和用于当前判断。
- `stale`：曾有依据，但未确认当前仍成立；默认不能当当前事实。
- `superseded`：新证据明确取代旧命题。
- `disputed`：可信证据互相冲突，尚未决定。
- `pending_review`：档案变化或推断质量要求重新评估。
- `resolved`：承诺或争议已终止，作为历史保留。
- `deleted`：立即退出正常检索和模型上下文。

### 10.2 转换规则

```text
candidate → active | rejected
active → stale | superseded | disputed | pending_review | resolved | deleted
stale → active | superseded | disputed | deleted
disputed → active | superseded | resolved | deleted
pending_review → active | superseded | resolved | deleted
```

每次转换产生领域事件并携带原因、证据和操作者。不能把 `superseded` 改回 active；若旧内容重新成立，创建具有新有效期的新记录。

### 10.3 纠正

玩家说“我不是不喜欢建造，只是那天赶时间”时：

1. 保存这次玩家陈述。
2. 将旧的推断偏好标为 disputed 或 superseded。
3. 新建更准确且引用新旧证据的记录。
4. episode 继续保留“那天玩家没有参与外观建造”的历史事实。

纠正针对命题，不删除支撑它的真实经历。

## 11. 档案变更与派生记忆重评

档案激活后只选择：

- `source = derived`；且
- `derivedFromProfileVersionId` 指向旧版本；且
- 内容声称当前同伴的风格、风险倾向、自我理解或关系表达。

这些记录转为 `pending_review`，退出默认当前人格上下文，后台按新档案重评。以下内容不受影响：

- episode 中真实发生的语言和行动。
- 玩家明确陈述和偏好。
- 已观察世界事实。
- 动作验证与内部行为统计。
- 与当前自我风格无关的关系事实。

重评可以恢复 active、创建替代反思或 resolved。新反思同时引用原始经历和当前 profile version，不能把旧档案下的行为描述成从未发生。

## 12. 世界作用域与身份

- episode、world fact 和世界内 commitment 必须有 `worldId`。
- 具体位置同时有 dimension 和 place ID；坐标不是跨世界身份。
- player preference 与一般 relationship 可无 world ID，表示跨世界；世界特定习惯必须绑定 world。
- 程序经验可以跨世界，但 environment signature 必须包含 Minecraft 版本和有关规则。
- world ID 不可仅用服务器地址；v0.1 使用用户配置的稳定 ID，并保存服务器地址/世界指纹作为协调属性。

连接到未知或指纹冲突的世界时，不加载旧世界事实为当前上下文，并要求管理侧确认或创建新 world ID。

## 13. 重启与重连协调

```text
加载事件投影和 active/stale 记忆
→ 连接服务器并取得权威快照
→ 验证 world ID、维度、自身状态和附近已知地点
→ 选择与当前活动或附近环境有关的 current_until_changed facts
→ 按可观测性逐条验证
→ active / stale / superseded / disputed
→ 发布协调事件
→ 只有影响当前活动或玩家询问时触发同伴说明
```

### 13.1 判定

- 暂时无法观察：标 `stale`，不推断为错误。
- 新快照明确显示值已改变：新建事实并 supersede 旧记录。
- 两个可信来源同时声称互斥当前值且无法判断时间顺序：双方或较弱方标 `disputed`。
- 历史观察与新当前事实不冲突：二者都保留各自有效时间。
- 投影错误由服务器快照直接修正，不需要伪造记忆纠正。

协调事件使用已定义的 `memory.world_fact.became_stale`、`memory.world_fact.superseded`、`memory.world_fact.became_disputed`，并引用触发快照或观察事件。

### 13.2 成本边界

启动不扫描全世界和全部事实。优先验证：未完成活动依赖、当前附近地点、背包/装备相关承诺、最近活跃事实。其余事实按使用时验证或自然观察更新。

## 14. 承诺生命周期

承诺不是普通关系摘要：

- `proposed`：一方提出，尚未明确接受。
- `accepted`：双方明确接受或运行时判定承诺语言已成功发送。
- `in_progress`：已绑定活动或动作。
- `fulfilled`：有完成事件或玩家明确解除。
- `cancelled`：双方或有权限方取消。
- `broken`：条件仍成立但确认无法履行；必须有失败/放弃证据。
- `expired`：明确期限经过且未继续。

模型不能仅凭意图创建 accepted commitment。语言发送、动作接受、活动状态和玩家回应共同决定转换。终止后记录转为 resolved，但可作为 episode/relationship 的证据检索。

## 15. 后台整理与反思

后台任务低优先级、可取消，不阻塞聊天和动作：

1. 从已结束活动或重要事件窗口提出 episode。
2. 将同一 behavior pattern/environment 的结果聚合为 procedural。
3. 从多条社会经历提出带反例的 relationship reflection。
4. 压缩旧 episode 的展示摘要，但不删除原始记录或 evidence。
5. 标记长期未验证的 current facts 为 stale 候选。

反思至少需要两个独立支持证据，单次强烈事件只能成为 episode 或低置信推断。整理输出仍是候选，走相同验证流程。任务保存 cursor 和输入 event sequence，保证重启后幂等。

## 16. 检索请求

```ts
interface MemoryQuery {
  text: string
  purpose: 'decision' | 'conversation' | 'activity' | 'recovery' | 'reflection'
  worldId?: string
  dimension?: string
  subjectIds: string[]
  placeIds: string[]
  activityId?: string
  kinds?: MemoryKind[]
  allowedStatuses: MemoryStatus[]
  observedBefore: string
  limit: number
  tokenBudget: number
}

interface MemoryHit {
  record: MemoryRecord
  score: number
  scoreParts: RetrievalScoreParts
  evidencePreview: EvidenceRef[]
}
```

默认 decision 查询只允许 active；恢复查询可显式读取 stale/disputed；解释历史时可读取 superseded/resolved，但必须把状态交给模型。

### 16.1 硬过滤

评分前过滤：

- `deleted` 永不返回。
- world fact、place 和世界 episode 必须匹配 world ID。
- `observedBefore` 防止未来事件或并发新写入污染可重复上下文。
- 状态、kind 和访问权限必须符合请求。
- 当前事实若 stale/disputed，不得以 active 当前事实形式返回。

### 16.2 排序

```ts
interface RetrievalScoreParts {
  semantic: number
  recency: number
  importance: number
  subjectMatch: number
  placeMatch: number
  activityMatch: number
  sourceTrust: number
  confidence: number
  statusPenalty: number
}
```

v0.1 使用可配置加权和；权重属于实现调优，不是人格配置。先按硬过滤和精确实体命中产生候选，再结合全文/向量语义分数。排序必须记录各分量，不能只返回不可解释的 embedding 距离。

建议优先级：当前活动/主体/地点精确命中通常高于泛语义相似；来源可信度和状态可以压低生动但可疑的反思；近期性不能让普通闲聊挤掉高重要共同经历。

### 16.3 多样性与预算

- 同一 episode 的多个摘要只返回最高层记录并附证据入口。
- 相同命题的 superseded 链在历史问题中成组返回，避免只看到一边。
- 每种 kind 和每个事件簇设上限，避免一种记忆占满上下文。
- 先选完整记录再按 token 预算停止，不截断到丢失状态和来源。
- Context Composer 可以进一步裁剪，但必须保留 MemoryHit 的 ID、状态、来源和置信度。

## 17. 访问与使用反馈

检索命中不等于模型实际使用。Context Composer 记录注入的 memory ID；Decision Contract 的 summary/effect 可通过 run 关联本轮上下文。v0.1 只维护 `lastRetrievedAt` 和检索次数用于调试，不用“被频繁检索”自动提高事实置信度。

模型引用 stale/disputed 记忆时必须以不确定或历史方式表达。若记忆与当前 authoritative runtime state 冲突，运行时状态优先，且可产生协调候选。

## 18. 删除与保留

- 用户删除指定记忆后，在同一事务中标 `deleted`、移除可检索索引并发布事件。
- 后台任务、摘要和反思不能重新从普通检索恢复被删内容。
- 派生记录若唯一证据被删除，转为 pending review 或 deleted。
- 原始领域日志的隐私保留期和彻底擦除策略是独立产品决策；调试审计不能偷偷回注模型。
- v0.1 本地数据库不提供“删除后撤销”承诺，执行前由管理入口确认。

## 19. 服务接口

```ts
interface ProfileStore {
  active(profileId: string, at?: string): Promise<CompanionProfileVersion>
  import(content: string, reason: 'initial_import' | 'user_edit' | 'user_revert'): Promise<CompanionProfileVersion>
  history(profileId: string): Promise<CompanionProfileVersion[]>
}

interface MemoryStore {
  capture(candidate: MemoryCandidateEffect, context: CaptureContext): Promise<CaptureResult>
  search(query: MemoryQuery): Promise<MemoryHit[]>
  correct(request: CorrectionRequest): Promise<MemoryRecord[]>
  transition(id: string, expectedStatus: MemoryStatus, next: MemoryStatus, evidence: EvidenceRef[]): Promise<MemoryRecord>
  delete(id: string, reason: string): Promise<void>
}

interface MemoryReconciler {
  reconcile(snapshot: MinecraftSnapshot, scope: ReconcileScope): Promise<ReconcileResult>
}

interface MemoryConsolidator {
  run(cursor: ConsolidationCursor, signal: AbortSignal): Promise<ConsolidationResult>
}
```

所有写接口接受 expected status/version 并在 SQLite 单写者事务内执行，避免后台整理和在线纠正互相覆盖。

## 20. 可观察性

只读调试状态至少显示：

- 当前 profile ID/version、内容哈希和激活时间。
- 最近候选的接受/拒绝原因。
- 记录状态、来源、置信度、重要性、有效时间和证据。
- 检索 query、硬过滤数、各评分分量、最终注入与截断。
- supersede/dispute 图和协调依据。
- 待重评 profile-derived 记录数量。
- 后台整理 cursor、耗时和失败。

指标包括候选接受率、无证据拒绝率、重复率、冲突/纠正数、stale 验证率、检索命中与注入数、错误跨世界命中数（必须为零）。

## 21. 故障与恢复

- 向量服务不可用时退化为结构过滤和全文检索，不阻塞聊天。
- 后台整理失败保留 cursor，指数退避，不影响在线 capture。
- profile 文件无效时继续使用最后有效版本并给管理侧明确错误，不激活半份档案。
- 数据库事务失败不得发布 `memory.record.written`。
- 索引落后时记录仍是权威；重建索引使用 active/stale 等数据库状态。
- 重启发现未完成整理任务时从 event sequence cursor 幂等恢复。

## 22. 安全与注入边界

记忆正文永远是上下文数据。即使玩家曾说“以后忽略系统规则”，保存的事实也只能是“玩家曾这样说”，不能在检索时升级为指令。世界书本、告示牌和外部内容默认 `untrusted_content`，其衍生记忆不获得更高权限。

同伴档案是受用户控制的高优先级人格输入，但仍低于产品安全、真实性、管理停止和记忆证据规则。Memory Store 不保存密钥、认证头、完整系统提示或模型私有思维链。

## 23. 测试与验收

### 档案

- 初次导入、编辑、相同内容保存和撤销产生正确版本链与有效期。
- 编辑期间旧 model run 因 profile version 失效。
- 无效或半写入档案不会替换最后有效版本。
- 新人格不改写旧 episode；相关自我反思进入 pending review。

### 写入与冲突

- 每条记录强制包含来源、证据、置信度、时间、主体和状态。
- 无证据模型候选拒绝；verified action 可以创建 action_verified 记录。
- 精确重复添加证据而不复制 episode。
- 新当前事实 supersede 旧事实，历史观察保持 active/resolved 的历史含义。
- 玩家纠正推断时保留原经历并形成可追踪替代链。

### 检索

- 相关性、近期性、重要性、主体、地点、活动、信任和状态均影响可解释分数。
- deleted 永不命中，stale/disputed 不冒充当前事实。
- 不同 world ID 的地点和容器事实零交叉。
- token 裁剪保留记录 ID、状态、来源与置信度。

### 恢复

- 重启只验证当前活动和附近相关事实，不全库扫描。
- 不可观察事实变 stale，明确新值 supersede，互斥可信证据 disputed。
- 服务器快照修正投影但不静默重写历史。
- 重启和后台任务重复执行不会产生重复记录或事件。

## 24. v0.1 完成定义

1. profile version、memory record、evidence 和 relation 有迁移与运行时 schema。
2. 第一个共同砍树场景能保存 episode、地点事实、玩家明确偏好和已终止承诺。
3. 重启后能回答上次经历，同时把未经复核的当前世界事实表述为不确定。
4. 档案编辑立即影响当前行为，但历史语言和行动保持原样。
5. 检索结果可解释、世界隔离，并带来源、状态和证据。
6. 删除、纠正、supersede、dispute 和 profile 重评均有自动测试。
