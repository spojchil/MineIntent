# 上下文压缩：裁决材料

> 无产品权威。本文记录已下的裁定、调查所得的业界做法、上下文构成的实测清册，
> 以及尚未裁决的条目。选项不是产品判断，判断在维护者。
>
> 状态：**进行中**。2026-08-17 起，维护者逐条给出裁定，本文随之更新。

---

## 1. 来历：它是被提前实现的

压缩当前**已实现且是活的**，但它从未经过裁定。

| 提交 | 日期 | 事实 |
|---|---|---|
| `e5d0d5e` | 08-10 | 上下文策略首版。提交信息自陈：「`Compaction` **等压缩政策裁定后补**」 |
| `906b0c8` | 08-11 | 「Compaction 首版」落地。当日**无裁定记录** |

`需要决策的新问题.md` 与 `产品.md` 中「压缩」二字均未出现。

补上的不是空壳，是一整套产品政策：压缩要**再打一次模型**；模型被要求同时交回
**长期记忆全文**与对话摘要；「值得留的先并入记忆落盘，再以摘要替换会话」这条被
那次提交自己命名为「金律」；摘要用第一人称过去式；世界状态不入摘要。

现状参数：内核侧 `compact_above_bytes: 256 * 1024` 默认开着，组合根
`SessionConfig::default()` 原样收下。两次实盘验收（08-17）离阈值太远，**从未触发过**——
所有关于它的判断目前都来自读码，没有任何运行证据。

---

## 2. 已下的裁定

### 裁定一（2026-08-17）：压缩与长期记忆无关

压缩不得读写长期记忆。要长期记住什么，由模型自己在被压掉之前调 `remember`——
记忆是模型的主动动作，不是别的机制的副作用。

**随之作废：**

- `COMPACTION_INSTRUCTIONS` 里 `memory_full_text` 字段与相关规则；
- `ContextStrategy::compact` 中的记忆读取、并发比对与写回三步；
- 「值得留的经历先经记忆落盘再入摘要」这条自封的「金律」；
- 「记忆被 `remember` 或维护者改过就放弃本次压缩」的竞态防御（没有写入就没有竞态）。

失败模式由两种收敛为一种：只会「摘要没出来」，不再有「摘要出来了但落盘失败」。
`ContextStrategy` 仍持 `MemoryFile`，但在压缩这条路上**只读**。

附带消掉一个注入面：摘要器不再有写记忆文件的通道，被读进上下文的文件内容里若
含注入指令，就没有了「经摘要进入永不压缩的受保护段」这条捷径。

### 裁定二（2026-08-17）：方块信息移出会话区

**屏蔽会话历史里的一切方块信息**，改为从 `BlockMemory` 投影呈现——与受保护三段
（人设 / 记忆 / 处境）同一形状：每轮现拉、永不参与压缩。

理由与后果：

- 呈现即记忆的投影，两者按定义一致，不存在水位线对不齐；
- 历史里没有方块信息，压缩无从抹起，`BlockMemory` 与压缩的失配问题消失；
- 模型不必再把一串 `+`/`-` 叠加还原成当前状态（省去 diff 叠加与新旧判断）；
- 方块事实从「不可重导出的一次性事实」变回「可重导出」，压缩金律的前提重新成立。

**呈现纪律**：提示词引导模型用散文描述世界，不堆坐标与方块名。
例：「(x, y, z) 那边一片针叶林，旁边是海」。**不描述全部，只描述它认为重要的**。
特殊方块（熔炉、箱子等）可用坐标 + 自然语言。

**随之不再需要**：`render_block_changes` 的 git 式差异行作为轮末帧文本、
`Passive` 信箱投帧、「空 diff 不投帧」的判断。diff 退回它本来的位置——
只用于推进记忆，不再用于说话。

**待定（阻塞实现）**：每轮投影出去的那一份**具体是什么**。

- 读法 A：投影仍是结构化的（近处逐格 / 远处地貌标签），提示词只约束模型的说话风格；
- 读法 B：投影本身就是散文，由模型主动产出并存入记忆，格点只在它主动 `scan` 时露出。

两者做出来是两套系统。**这一条未裁。**

⚠ 无论取哪条，都有一个尺寸约束：易变段在提示尾部，**每轮都在未命中区**。
一次默认视口 `scan` 就把整个可见集逐格吸进记忆，走几分钟即数千格；按实测
28 字节/格，把记忆整份铺出来是十万字节量级 × 每轮。**所以必须有一层压缩，
「散文」就是它。**

---

## 3. 上下文构成清册（实测）

排序轴是**可重导出吗**——压缩金律的前提，也是每条该怎么处置的分界。

### 免压缩区（每轮现拉，全部 system 角色）

| # | 是什么 | 实测尺寸 | 可重导出 |
|---|---|---|---|
| 1 | 人设 | ~230 字 | 是（配置） |
| 2 | 长期记忆全文 | 随记忆文件 | 是（文件） |
| 3 | 处境：维度/天色/天气、位置朝向、生命饥饿、附近实体、聊天未读 | ~120 字 | 是（快照） |
| 4 | 工具表 | **9 098 字节** | 是（定义） |
| 5 | **方块信息**（裁定二新增） | 待定，见 §2 | 是（记忆） |

前四条形状已经是对的：可重导出、每轮现拉、稳定的在前吃缓存。

### 会话区（会被压缩）

| # | 是什么 | 谁产的 | 实测 / 形状 | 可重导出 |
|---|---|---|---|---|
| 6 | 内心独白 | 模型文本输出 | 实盘两轮 367 / 1431 输出 token | **否** |
| 7 | 思考（reasoning） | 模型 | 实盘 `reasoning_output_tokens: None`（当前链路未回传） | 否 |
| 8 | 工具调用（名 + 参数 JSON） | 模型 | 每次几十字节 | 否（意图记录） |
| 9 | 工具回执 | 各工具 | 见下表，**差三个数量级** | 分情况 |
| 10 | 唤醒投递（user 角色） | 组合根 | 每条 15~40 字节 | **否**——一次性事件 |
| 11 | 轮末帧 | 组合根 | 7 070 字节 / 256 条 | 裁定二：移出 |
| 12 | 耐久事实 | 内核 | 带 `kind` 的 JSON | 否，且**内核要求逐项原样保留、序列一致**，否则整个压缩结果被丢弃 |

### 第 9 条内部差距极大

| 回执 | 字节 | 性质 |
|---|---|---|
| `chat_box say` | ~44 | 一次性动作确认 |
| `inventory move` | ~91 | 同上 |
| `motion` / `hand` 各动词 | 几十 | 同上 |
| `scan` 环视（聚合后） | ~200 | **可重导出**——再看一次就有 |
| `inventory open` 全清单 | ~400（**曾 ~1 100**） | 已改，见下 |
| 容器开屏（工作台）清单 | ~430（**曾 ~1 400**） | 已改，见下 |

### 裁定三（2026-08-17）：用法全文不随开屏投递

**已实施。** 用法是**静态文本**，随开屏无条件重发等于每开一次就往会话区塞一份
一模一样的几百字节，而它一个字都不会变。查用法自己是一个动作。

- `inventory` 增 `describe` 动作；`open` 只给清单 + 一行指路；
- `container` 增 `describe`，种类从**当前开着的那个屏**读（`open_screen.kind`），
  模型不用也不该自己报种类；没有开着的屏就如实拒绝；
- 组合根的开屏通知同改。`chat_box` 的 `open{describe}` 本来就是这个形状，未动。

顺带修一处失真：`model_surface` 曾自己拼一份开屏通知，改用法时它没跟着变，
「保真导出」当场不保真。装配抽成 `render::render_container_opened`，
组合根与导出**共用一份**——措辞归呈现层，组合根只管副作用（占域、屏状态翻转）。

实测（`model_surface` 全导出）：**20 752 → 18 083 字节**；
其中唤醒投递段 4 303 → 2 017，工具回执段 2 897 → 2 258。

### 一次真实运行的 token 分布（08-17 验收，DeepSeek）

| | 输入 | 命中缓存 | **未命中** | 命中率 |
|---|---:|---:|---:|---:|
| run/1 | 20 266 | 12 928 | 7 338 | 64% |
| run/2 | 108 151 | 91 904 | 16 247 | 85% |
| 全程 | | | **23 585** | |

⚠ 读数注意：`ModelUsage::merge` 是**累加**（`midturn/src/types.rs:76`），
「轮结束」打印的 `input_tokens` 是该轮**全部模型请求之和**，不是任何一次请求的
上下文大小。按输出 token 反推，run/2 约 7 次请求，每次约 15k。

**真正花钱的是未命中区。** 稳定前缀（工具表、人设）第一次之后就命中，不是大头。

---

## 4. 业界做法调查（2026-08-17）

### 4.1 谁来压缩

| | 执行者 |
|---|---|
| Claude Code | **另开一个更小的 Claude**，用户看不见 |
| Codex CLI（本地路） | 同一模型、同一端点；摘要提示词作为**新 user 消息**追加到历史尾 |
| Codex CLI（远端路） | **OpenAI 服务端** `POST /v1/responses/compact`，返回压好的 `ResponseItems` |
| Anthropic API | **API 自己**（`compact_20260112` beta），返回一等的 `compaction` 内容块 |
| Gemini CLI | **专用压缩模型别名**：`gemini-2.5-pro` → `chat-compression-2.5-pro` |
| OpenCode | 会话当前模型，但走一个**隐藏的专用 agent** |
| Cline | 主模型。理由是复用现成 prompt cache，输入几乎全命中，只付输出 |

**没有一家让摘要器兼任长期记忆写手。** Claude Code 甚至专门加了
`agent-prompt-summarization-no-tools-guard` 禁止摘要器用工具。裁定一与业界一致。

### 4.2 呈现方式（摘要怎么回到上下文）

- **Gemini CLI**：伪造一问一答——`user` 放 `<state_snapshot>`，`model` 回
  `"Got it. Thanks for the additional context!"`，后接保留的最近 30% 历史；
- **Codex CLI**：`user` 角色，前缀伪装成第三方——
  「Another language model started to solve this problem and produced a summary…」，
  放在末位，后接最近 20k token 的用户消息；
- **OpenCode**：`assistant` 消息带 `summary: true`；被剪掉的工具输出**原地留痕**
  显示为 `[Old tool result content cleared]`；压缩后**自动重放最后一条用户消息**；
- **Anthropic API**：一等 `compaction` 内容块，后续请求带回去，API 自动丢弃它之前的一切。

我们现状：`InputMessage::text("user", "【我此前的经历记述…】\n{summary}")`，
形状接近 Codex，但**没有「保留最近 N」这一层**。

### 4.3 提示词倾向：两派

**叙事派**（写给「接手的另一个模型」）：

- Claude Code：**九个小节**，要求逐条按时间顺序分析，明确要求
  **「直接引用原文关键短语而不是转述」**以防漂移；先在 analysis 标签里做思维链，
  注入前把思维链**剥掉**——用 CoT 保质量，但不为它付 token；
- Codex：`"You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff
  summary for another LLM that will resume the task."` 四个重点：当前进展与关键决策、
  约束与用户偏好、剩余 TODO、继续所需的关键数据；
- OpenCode：五个标题，另加「自动匹配用户语言」。

**状态派**（写给「状态管理器」）——Gemini CLI：

```xml
<scratchpad> …私下推理… </scratchpad>
<state_snapshot>
  <overall_goal/> <key_knowledge/> <file_system_state/>
  <recent_actions/> <current_plan/>
</state_snapshot>
```

Factory.ai 的探针评测（回忆/产物/续作/决策四类探针）：结构化 3.70 >
Anthropic SDK 3.44 > OpenAI `/compact` 3.35，结论是**「结构强制保留」**——分段就漏
不掉文件路径与决策，自由摘要漏了没人知道。但**三家的「产物追踪」分都低于 2.5**，
说明文件/产物状态靠摘要保不住，需要专门处理。

### 4.4 两段式：我们只做了重的那一半

各家几乎都是两段：

```
第一段  剪枝 / 遮蔽   —— 不叫模型，纯规则丢旧工具输出
第二段  摘要          —— 叫模型，重且有损
```

- OpenCode：Prune（删旧工具输出，打 `compacted` 时间戳）→ Compact；
- Anthropic API：`clear_tool_uses_20250919` / `clear_thinking_20251015` 独立于
  compaction，可单用。官方称清工具结果是「**最安全、最轻的一种压缩**」，
  实测 100 轮网搜任务省 84% token；
- Cline：先用简明提示替换重复文件读取；
- SWE-agent：超出最近 5 轮的观察折成一行；
- Codex：保最近 20k token 用户消息不动。

`arXiv:2508.21433`《The Complexity Trap》在 SWE-bench Verified 上对打
（SWE-agent，5 组模型 × 500 例）——**只做遮蔽**（保留推理与动作，只把 M=10 轮外的
工具输出换成占位符）对 **LLM 摘要**：

```
观察遮蔽   54.8%   $0.61
LLM 摘要   53.8%   $0.64
不压       53.4%   $1.29
```

遮蔽比不压省 52.7%，解题率还略高于摘要。作者给的原因之一：摘要导致**轨迹变长**，
模型看到摘要后倾向继续磨。

**我们只有第二段，没有第一段。**

### 4.5 递归压缩会烂

- Codex CLI 专门做过 **Compactor 2 重写**，因为旧逻辑会产生「摘要的摘要」；
- MemGPT 论文的递归摘要基线在 DMR 上 35.3%，其分层外置存储 93.4%；
- Amp 干脆退出压缩：上下文满了就**开一个新 agent**，带结构化任务状态过去，
  把「怎么压缩对话」换成「怎么交接任务」。

### 4.6 外部派生状态需要写屏障

`conikeec.substack.com` 的论点：上下文编辑是**没有写屏障的垃圾回收器**。

> agent 不会因为旧上下文被清掉而崩溃，它会**继续基于自己再也无法查证的证据推理下去**。

开的方子：根集分析、写屏障（近期输出引用过的结果标记受保护）、安全回收点
（任务完成后才压，不在半途）、终结化（清掉之前让 agent 先提取落盘）。

我们的 `BlockMemory` 曾是教科书级的「没有写屏障的外部引用」，且比一般情况更糟——
增量设计**保证**那些方块不会再被报一次。**裁定二从根上消掉了这个问题**：
方块信息不再进历史，也就没有被压掉的可能。

---

## 5. 未裁决清单

1. **方块投影的具体形状**（§2 读法 A / B）——阻塞裁定二的实现。
2. **触发条件**。现状 `compact_above_bytes: 256KB` 是**绝对字节**，与模型窗口脱钩，
   换模型即失准。业界一律用窗口比例，且在系统性提前（Gemini CLI 95% → 70% → 0.5）。
3. **要不要先做第一段（剪枝 / 遮蔽）**。它不需要产品裁定、收益最大、风险最低，
   而我们跳过了它。
4. **保留最近 N**。各家都保，我们不保。保什么、保多少。
5. **谁来压缩**。同模型 / 更小的模型 / 专用 agent。当前是同模型自持。
6. **摘要形状**：叙事派还是状态派；若状态派，小节如何切。
7. **会话区逐条处置**（§3 表 6~12 每一条）：内心独白、思考、工具调用、
   各类工具回执、唤醒投递、耐久事实。
8. **唤醒投递不可重导出**。「alice 说了什么」若不入摘要就真没了，它没有第二份来源，
   与方块不同。
9. **压缩的可观察性**。现在压缩成功或放弃全程静默，四条 `return unchanged()`
    放弃路径一条都不出声，实跑时分不清「没触发」和「触发了但放弃了」。

---

## 6. 定了之后要改哪几处

| 位置 | 现状 |
|---|---|
| `crates/context/src/lib.rs` | `COMPACTION_INSTRUCTIONS`、`SUMMARY_PREFIX`、`impl Compaction`、模块头的「金律」 |
| `crates/companion/src/main.rs` | `SessionConfig::default()`（阈值）、轮末帧观察端与 `Passive` 投递 |
| `crates/render/src/lib.rs` | `render_block_changes`（裁定二后的去留） |
| `crates/world/src/viewport/incremental.rs` | 模块头「已经成功送入模型上下文的事实」这句定义 |
| `crates/perception/src/lib.rs` | `absorb_projection` 的吸收口径 |
| `docs/guides/validation.md` | 压缩当前无任何实盘证据，须如实标注 |

---

## 7. 参考

- [Context Compaction Research: Claude Code, Codex CLI, OpenCode, Amp](https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f)
- [Codex compaction: prompts and full analysis](https://gist.github.com/sam-saffron-jarvis/30403c1bc5682bf9f69fa00933aad815)
- [Anthropic Compaction API](https://platform.claude.com/docs/en/build-with-claude/compaction)
- [Anthropic: Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [The Complexity Trap (arXiv:2508.21433)](https://arxiv.org/html/2508.21433v1)
- [Factory.ai: Evaluating Context Compression](https://factory.ai/news/evaluating-compression)
- [Context Editing Is a Garbage Collector Without Write Barriers](https://conikeec.substack.com/p/context-editing-looks-like-a-feature)
- [Dive into Context Engineering: Lessons from the Gemini CLI](https://leslieo2.github.io/posts/context-engineering/)
- [MemGPT: Towards LLMs as Operating Systems](https://arxiv.org/pdf/2310.08560)
- [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts)
