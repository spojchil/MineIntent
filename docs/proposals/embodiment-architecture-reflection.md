---
status: proposed
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
source_commit: e50c8f9
---

# 具身架构反思与工具接口草案

> 2026-07-22 的反思报告，范围为 `src/information/`、`src/grounding/`、`src/behavior/`、`src/models/decision-protocol.ts`、`src/capabilities/`。本文中的事实错误、基线冲突和未决选项已在[决策登记册](./embodiment-decision-register.md)编号；在相应 ADR/PR 接受前，本文不覆盖[产品设计](../product-design.md)、[目标系统](../architecture/target-system.md)或 [Information Runtime](../architecture/information-runtime.md)。

本文记录一次对当前具身与信息架构的完整审视，包含证据、根因分析、一处被证伪的判断，以及一份替代方案的接口草案。

**阅读提示：**原文曾把 §1–§7 描述为“已达成方向”，但 GitHub 上没有对应的接受 Issue、PR 或 ADR；此次审计将全文统一视作 proposal。§8.1（同伴没有主动性）与 §8.3（记忆系统与产品承诺不匹配）对产品核心目标的影响不亚于本文主体，但两者都不由本草案解决。

---

## 1. 核心诊断

当前架构的组织原则是：

> **在决策前证明正确。**

模型必须在一次输出里，用受校验的语义目标表达意图，用可溯源的引用指认目标，并让二者在协议层通过一致性检查——只有全部成立，行动才被允许发生。

本文主张改为：

> **在执行后观察结果。**

模型表达一个动作，运行时执行并如实返回实际发生了什么，模型据此修正。正确性不由事前证明保证，而由反馈回路收敛。

这不是风格偏好。前者是编译器对程序的要求，后者是身体对世界的关系。MineIntent 要造的是后者。

### 1.1 一个具体对照：「看那只羊」

**当前架构下，模型必须输出：**

```jsonc
{
  "referents": [
    { "role": "subject", "selection": { "kind": "context_ref", "ref": "vp_e3" } }
  ],
  "semanticGoal": {
    "objective": {
      "kind": "state",
      "state": {
        "id": "s1",
        "concept": "self.attention_includes",
        "arguments": {
          "observer": { "kind": "self" },
          "subject":  { "kind": "referent_role", "role": "subject" }
        }
      }
    },
    "methodGuidance": []
  }
}
```

然后依次经过：

1. `decision-protocol.ts` — 校验 `vp_e3` 确由本轮 context 发出、role 双向匹配、嵌套 ≤8 层、状态 ≤32 个
2. `GroundingEngine` — 把 ref 绑回证据，检查 `worldId` / `epoch` / `validUntil`
3. `BehaviorSynthesizer` — 判断 `concept` 是否受支持、句柄是否 current、方法指导是否为 `required`
4. 合成 `BehaviorPlanV1`
5. `VisualAttentionController` — 重新解析目标、限速转头、验证

**任何一环不通过，整个决策作废，包括同伴本来要说的话。**

**本文主张的架构下，模型输出：**

```jsonc
{ "tool": "look", "input": { "turnYawDegrees": -30, "turnPitchDegrees": 0 } }
```

运行时转头，返回：

```jsonc
{
  "status": "completed",
  "observed": {
    "lookedAtEntity": { "type": "sheep", "distanceBand": "near" },
    "viewport": { }
  }
}
```

模型看到结果，决定下一步。**如果转过去发现是另一只羊，或者羊已经走了——再转一次。**

这就是消歧和时效校验的全部实现。第 1–4 步整体消失。

### 1.2 推导链

这个结论不是从「简化架构」出发得到的，而是从三个各自独立的观察汇合而成：

**(一) 两件事被混为一件。**

- **(A)** 模型不能执行任意代码、不能调用超出人类能力的原语 —— 正确且必要
- **(B)** 模型不能直接指称目标（`prompt.py`:「不要输出……世界坐标、实体 id」）

(B) 是全部复杂度的来源：模型不能命名目标 → 只能**描述** → 需要 Grounding 把描述绑回证据 → 需要 referent roles → 需要 `semanticGoal` 表达期望状态 → 需要 Synthesizer 反解 → 需要 246 行协议校验维持一致性。

**(二) (B) 已经被两个更简单的机制解决了。**

1. **原语集合**：`MinecraftMotorDriverApi` 只有 `look` / `dig` / `releaseAll`。没有传送，没有 setPosition。**不可能动作在原语层就不存在**——不需要在语义层再防一次。
2. **输入控制**：context 由 runtime 亲手构造，只包含合法 Read 的产物。**模型不可能引用它从未被告知的东西。**

关键区分：

> **纯洁性来自输入控制。可审计性才需要输出携带证明。**

因为 context 完全由你构造，输入边界是可信的。**在可信的输入边界上再要求携带证明的输出，是为同一个保证付第二次钱**——而第二次付费恰好是唯一会让同伴失声的那次（见 §2.5）。

**(三) 执行期验证已经存在，使决策期证明冗余。**

`VisualAttentionController` 已经在执行时重解析目标、重校验句柄、返回 `observedTarget` 与证据。模型若指认了不存在或过期的东西，**行为会失败并如实报告**——这正是[产品设计](../product-design.md) §12 期望的行为。

两道关都设了。前一道（协议拒绝）拦下的东西后一道本就会拦，而后者的处理方式更像人：前者是沉默，后者是「咦，它不见了」。

### 1.3 已考虑并被否决的反对意见

**这一节是本文最需要保留的部分。**下列反对意见都在讨论中被提出过，其中前两条是作者最初坚持、后被推翻的。将来若想重新引入引用校验或语义目标层，应先回到这里。

---

**反对 1：视野里有三只羊，模型想看其中一只，总得说是哪只——没有引用怎么消歧？**

不需要消歧。**看过去，视口回来的就是那只。**如果不是想看的那只，再看一次。

消歧不由标识符完成，由**反馈回路**完成。而这恰好是人的做法：你不会说「我要看 2 号羊」，你转头，然后看到什么就是什么。

> 这条最初被作者判定为「引用不可替代的真价值」，是错的。它假设了系统必须**事先**保证指对目标；一旦接受「不保证，看了就知道」，这个需求就消失了。

---

**反对 2：羊已经走了怎么办？`basedOnInformationRevision` 能发现引用过期。**

不需要比对版本号。**看过去，羊不在了——视口本身就是那个答案。**

时效校验是把「观察」这件事又用元数据实现了一遍。观察本身就是最直接、最新鲜的时效检查。

> 同样最初被判定为「真价值」，同样是错的，同样源于「事先保证」的思路残留。

---

**反对 3：`look` 和 `dig` 之间目标移动了，这不是竞态吗？**

**在原版里人也会打空。**

如实返回「打到了空气」，模型再调整。这不是需要根绝的竞态，是正确行为——而且是[产品设计](../product-design.md) §2.1 明确列举的可信玩家行为（「操作失败」）。

试图消除这个竞态，等于试图让同伴比人更准。

---

**反对 4：模型可能幻觉一个根本不存在的目标。引用校验能当场拦下它。**

能，但代价是整个决策作废、同伴对玩家完全无反应（`runtime.ts:285` → `:298`）。

而执行期验证也能拦下它，代价是同伴说「我没看到你说的那个」。

**同一个保证，两种代价。第二种是产品要的。**

[产品设计](../product-design.md) §12 写的是「AI 不应伪造成功，也不应在内部失败后无回应地停住」——协议拒绝恰好制造了后者。

---

**反对 5：多轮往返意味着更多模型调用，延迟和费用都是真的。**

是真的。但[产品设计](../product-design.md) §2.1 明确写道：

> 停下来观察、反应迟缓、偶尔迷路、操作失败、没有及时接话或选择保持沉默，都可以是可信玩家行为。

**多轮往返产生的「慢」不是要优化掉的成本，正是目标行为。**一个瞬间锁定正确目标、一次成功的玩家反而可疑。

需要管的是别失控——**用预算管，不用类型系统管**。`InformationToolSession` 的 `maxCalls` / `deadlineAt` 已经实现。两者代价对比：

- 预算超限 → 「这一轮它没看清，先说句话」
- 类型系统拒绝 → 整个决策作废，同伴失声

---

**反对 6：复杂行为（连续攻击、挖掘放置）总需要指明对象吧？粗粒度工具不是必须带 id 吗？**

**粒度与引用是正交的两件事。**

粗粒度工具 + 准星引用完全兼容：`attack()` / `dig()` / `use()` 全部无参，目标在调用瞬间由准星获取，控制器内部维持短暂锁定。

一旦签名写成 `attack(entityId)`，ref、时效校验、Grounding 立即全部回归。

> 这是最容易混淆的一点。「工具要做更复杂的事」不蕴含「模型要能命名对象」。详见 §4.2。

---

**反对 7：`semanticGoal` 的价值在于让运行时选择「更像人的执行方式」，而不是让模型指定怎么做。**

该理由成立的前提是**一个目标存在多种可行方法**，运行时才有得挑。

当前每个语义目标恰好映射到唯一一个控制器（`behavior-synthesizer.ts:141` 只认 `self.attention_includes`）。**抽象层的自由度是零。**

这是 v0.5 的想法在 v0.1 的实现——想法本身可能是对的，但在有多种执行方法之前，它只产生开销。

### 1.4 为什么这不只是「更简单」

如果只是复杂度权衡，那还可以争论。但反馈回路架构在三个方面**更符合产品目标**，而不只是更省事：

1. **它就是注意力的机制。**人的注意力天然是 `听到声音 → 转头 → 看 → 再决定`。固定的单次快照使同伴在一轮决策内**物理上无法「先看一眼再想」**——它拿一份盲拍快照，发一个动作，等 `action_result` 终止才能再思考。这正是让人一眼认出 bot 的「木然、慢半拍」。

2. **它就是原版的交互模型。**见 §4.1：Minecraft 玩家只有准星一种指称手段，不存在实体 id。用准星而非句柄，**更接近[产品设计](../product-design.md) §2.1 的身体等价**，而不只是更好写。

3. **它让失败变成可交流的经历。**「我没看到你说的那个」「打空了」「挖到一半」都是可以说出口的事实，是共同经历的一部分（`§3.2`）。协议拒绝产生的是沉默——沉默不构成经历。

### 1.5 这条结论的适用边界

本文主张的是**模型边界上**取消事前证明，不是取消一切校验：

- **控制器内部**的目标锁定、句柄重校验、作用域检查 —— **保留**（见 §4.3）
- **信息层**的 grant、audience、schemaRevision、scope 失效 —— **保留**，那是输入控制，正是纯洁性的来源
- **原语集合**的约束 —— **保留且是根本**

取消的只有一样：**要求模型证明自己有权指认某个目标。**

---

## 2. 当前实现的事实

### 2.1 身体能力

`MinecraftMotorDriverApi`（`src/minecraft/contracts.ts:272`）的全部原语：

```ts
look(yaw, pitch, signal)
dig(position, signal)
releaseAll()
```

`BehaviorSynthesizer`（`src/behavior/behavior-synthesizer.ts:141`）只接受 `self.attention_includes` 一个语义概念，其余全部返回 `unsupported_goal`。`dig` 没有任何控制器调用。

**同伴当前唯一能做的身体动作是转头。**

### 2.2 能力目录与实现不一致（P0）

`src/capabilities/catalog.ts:19` 向模型声明六种能力，其中五种不存在：

| 目录声明 | 实现 |
|---|---|
| `gaze_change` | ✅ |
| `locomotion` | ❌ |
| `primary_interaction` | ❌ |
| `secondary_interaction` | ❌ |
| `inventory_selection` | ❌ |
| `wait` | ❌ |

它在 `src/context/context-composer.ts:81` 以 `trust: 'runtime_authoritative'` 注入上下文，且**绕过了 InformationRuntime 的全部治理**——无 grant、无 schemaRevision、无 availability、无 evidenceIds、不进 trace。

系统内唯一一条假信息，恰好走在唯一一条不受信息治理管辖的通路上。它系统性地诱导模型提出必然被拒的意图，直接制造[产品设计](../product-design.md) §3.3 判定为致命的「语言与行动不一致」。

### 2.3 交互式 API 被当作静态快照消费

[Information Runtime](../architecture/information-runtime.md) §15 决定不执行 tool loop。该产品判断本身合理，但实现仍保留完整的交互式 API 表面，且**无生产消费者**：

| 机制 | 生产调用方 |
|---|---|
| `help` 操作 | 0 |
| 分页 / `nextCursor` / `InformationCursorStore`（197 行） | 0 |
| `knownCatalogRevision` → `not_modified` | 0 |
| `InformationToolSession` / `InformationTool`（153 行） | 0 |
| selector / `InformationRefStore`（262 行） | 仅 Grounding 的 `resolveContextReference` |

放弃 tool loop 的理由是「不允许模型通过反复分页导出全部世界状态」——**这是预算问题**。而 `InformationToolSession` 已经实现了 `maxCalls`、`maxReadCalls`、`maxReturnedBytes`、`deadlineAt`，完整且有测试。

安全的 tool loop 已经写好，却因为一个它自身已经解决的理由被停用。

### 2.4 `READ_PLAN` 硬编码 schemaRevision（真 bug）

`src/information/context-composer.ts:17-20`：

```ts
{ interfaceId: 'viewport_information', schemaRevision: 'viewport-information:7', ... }
```

第 34 行刚取回 catalog（内含每个接口的实时 `schemaRevision`），却只保留 `catalogRevision`，丢弃其余。随后 `runtime.ts:256` 比对失败 → `stale_schema` → 进 `omissions`。

**后果**：任何人修改一次 provider 的 schemaRevision，该接口即从模型上下文静默消失，同伴局部失明且无任何报错。viewport 已迭代到 `:7`，说明该值持续变动。catalog 机制正是为防止此事而建，却被绕过。

### 2.5 引用证明的脆性

`src/models/decision-protocol.ts` 中会否决整个决策的校验：

| 校验 | 触发条件 | 安全价值 |
|---|---|---|
| `invalid_context_ref` (:148) | ref 字符串与本轮不符 | 冗余（见 §3.2） |
| `invalid_message_referent` (:153) | 未逐字引用玩家原话子串 | 无 |
| `undeclared_referent_role` (:188) | 使用未声明角色 | 无 |
| `unused_referent_role` (:191) | 声明了未使用的角色 | 无 |

全部经 `fail()` → `DecisionProtocolError` → `runtime.ts:285` 抛出 → `:298` catch → **不应用任何 effect，包括 speech**。

**一个 id 抄错一个字符，或把玩家的话转述而非逐字引用，同伴就完全不回应玩家。**这是陪伴产品最不可接受的失败模式。

`unused_referent_role` 尤为典型：声明未使用在安全上零风险，纯属编译器式洁癖，代价却是同伴失声。

### 2.6 缺失的中间层

| 层级 | 时间尺度 | 现状 |
|---|---|---|
| 任务（`collect_wood(10)`） | 分钟 | 已删除（正确） |
| **动作（挖、打、走）** | **秒** | **从未建立** |
| 输入（`look(yaw, pitch)`） | tick | 存在 |

删除 V1 skills 的方向是对的——`collect_wood` 把「一起找树、赶路、砍、不够再找」压缩没了，而那正是[产品设计](../product-design.md) §1 要保留的共同经历。但删除后中间层没有补上，同伴因此失去身体。

### 2.7 被动观察规模实测

条件：`horizontalRadius: 32, verticalRadius: 20, maxDistance: 32, halfAngle: 70°`，合成 world（`blockAt` 为纯算术函数）。

| 场景 | limit=20 | limit=200 | blockAt 调用 |
|---|---|---|---|
| 开阔平地 | 28ms | 23ms | 231,086（两者相同） |
| 森林 | 22ms | 20ms | 229,446（两者相同） |
| 地下全实心 | 21ms | 21ms | 283,038 |

**结论：**

1. `limit` 对扫描成本零影响——`slice` 发生在全部计算之后。20 → 200 在扫描上免费，只增加 token。
2. 真实 `blockAt` 需区块查找 + block 对象构造 + registry 查询，单次成本高一到两个数量级。**23 万次调用在生产环境更可能是 0.3–3 秒**，而 `timeoutMs` 为 5000。需用真实 backend 实测。
3. 唯一有效的性能杠杆是**按距离分壳扫描并早退**，不是 `limit`。
4. 地下全实心返回 0 是正确行为（全包围方块无暴露面），但耗费 28 万次调用返回空。

### 2.8 一处被证伪的判断（记录以防误改）

审查中曾判断 `isVisibleFromEye`（`src/information/source-ports/perception.ts:172`）会把非固体可见方块（火把、告示牌、作物、玻璃）误判为不可见，理由是射线会穿过非遮挡目标并命中其后方的墙。

**该判断错误。**射线长度被截断在 `distanceToCenter + STEP`，根本无法到达目标背后。实测：

```
torch-on-wall visible? true | first 3: [ 'torch', 'stone', 'stone' ]
torch-with-far-wall visible? true
```

`perception.test.ts:51`（`transparent visible blocks do not hide farther surfaces`）已覆盖该场景。**当前遮挡判定实现正确，不应修改。**

---

## 3. 根因分析

### 3.1 两件事被混为一件

- **(A) 模型不能执行任意代码、不能调用超出人类能力的原语** — 正确且必要（见 ADR 0004、0005）
- **(B) 模型不能直接指称目标** — 全部复杂度的来源

`agent-service/prompt.py` 要求「不要输出……世界坐标、实体 id」。为满足 (B)，模型不能命名目标，只能**描述**目标 → 需要 Grounding 将描述绑定到证据 → 需要 referent roles → 需要 `semanticGoal` 表达期望状态 → 需要 `BehaviorSynthesizer` 反解为控制器计划 → 需要 246 行协议校验维持一致性。

### 3.2 (B) 已被两个更简单的机制解决

1. **原语集合**：`MinecraftMotorDriverApi` 只有 `look` / `dig` / `releaseAll`，不存在传送或 setPosition。**不可能动作在原语层就不存在。**
2. **输入控制**：context 由 runtime 亲手构造，只包含合法 Read 的产物。**模型不可能引用它从未被告知的东西。**

纯洁性来自输入控制；输出携带证明属于可审计性。在可信输入边界上再要求携带证明的输出，是为同一个保证付第二次钱——而第二次付费恰好是唯一会让同伴失声的那次。

### 3.3 语义目标层的自由度为零

`semanticGoal` 的正当理由是让运行时选择「更像人的执行方式」。该理由只在**一个目标存在多种可行方法**时成立。当前每个语义目标恰好映射到唯一一个控制器，抽象层没有任何可利用的自由度——是 v0.5 的想法在 v0.1 的实现。

### 3.4 执行期验证使决策期证明冗余

`VisualAttentionController` 已在执行时重解析目标、重校验句柄、返回 `observedTarget` 与证据。模型若指认了不存在或过期的东西，行为会失败并如实报告——这正是[产品设计](../product-design.md) §12 期望的行为。

两道关都设，而前面那道（协议拒绝）拦下的东西后面那道本就会拦，且后者的处理方式更像人。

### 3.5 使问题难以察觉的三个机制

1. **约束比能力好写。**纯洁性约束自包含、可验证、能写出漂亮测试；能力是脏的、要调参、会在真服务器上以奇怪方式失败。高工程品味持续把项目推向可验证的那一半。
2. **文档为不存在的系统背书。**[目标系统设计](../architecture/target-system.md)的架构图仍包含已删除的 `Action Runtime`；1500 行设计文档读起来项目已完成大半，实际能力是「转头 + 说话」。
3. **可信性被投射到不可观测的一半。**信息纯洁性对其他玩家不可观测——除非同伴依据穿墙信息行动，旁观者无从判断。真正暴露 bot 的是移动质量、反应时机、聊天节奏与空闲行为。**一个完美可信但不可用的感知通道，与一个损坏的感知通道，产生的可观察行为完全相同。**

---

## 4. 架构结论

### 4.1 准星就是指针

原版 Minecraft 中玩家只有一种指称手段：**准星**。不存在「选中实体 #4823」，只有「把准星移过去，然后左键或右键」。

`raycastLookedAtBlock` 已经实现了准星。它是全系统唯一需要的指针。

由此，§3.2 提到的「消歧」与「时效」两个 ref 的残余价值也一并消解：

- **消歧**：不需要指明是哪只羊。看过去，视口回来的就是那只；错了就再看一次。
- **时效**：不需要比对 `basedOnInformationRevision`。看过去，羊不在了——**视口本身就是答案**。

look 与 dig 之间目标移动导致打空，在原版中人也会打空。如实返回「打到了空气」，模型再调整。这不是需要根绝的竞态，是正确行为。

### 4.2 粒度与引用正交

这是最易混淆的一点：**工具粒度可以粗，引用方式必须细。**

粗粒度工具 + 准星引用完全兼容：`attack()` / `dig()` / `use()` 全部无参，目标在调用瞬间由准星获取，控制器内部维持短暂锁定。

一旦签名写成 `attack(entityId)`，ref、时效校验、Grounding 立即全部回归。

**参数只允许三种形式：**

1. 无参（作用于准星当前所指）
2. 相对量（左转 30°、前进 5 格、槽位号）
3. 本轮观察中出现过的东西——**作为便利，不作为证明**；无法解析时是普通执行结果，不是协议错误

**禁止：**世界坐标、实体 id、持久句柄。

### 4.3 界线：模型边界无标识，控制器内部有锁定

「持续看着那只羊」「跟着它走」这类连续控制，控制器内部必然要在若干 tick 内知道跟踪对象。这**完全在模型看不见的层面**。`VisualAttentionController.#resolveTarget` 即是此类，设计正确，保留。

要退役的是其上方那层：模型命名目标 → Grounding 绑定证据 → Synthesizer 反解计划。

### 4.4 工具 = 动作，不是任务

**判据：如果一次工具调用期间玩家可能想说点什么，它就太粗了。**

砍一棵树的过程中玩家会说话；砍碎一个方块的过程中不会。

### 4.5 关于 pathfinder 的重新评价

`b0d7596` 移除 `mineflayer-pathfinder` 曾被评为能力退步。按本文结论应部分修正：

寻路器产生的是**最优路径**，而最优移动是已知的 bot 特征。人类移动是「看着要去的方向、走、撞到了再调整」。`move(direction, blocks)` 配合 1 格自动跳跃与「被什么挡住」的如实回报，**既更简单也更符合[产品设计](../product-design.md) §2.1 的身体等价**。

移除本身可能是对的，问题在于没有补上替代品。长距离导航是独立课题，不应由这一层解决。

---

## 5. 工具接口草案

仅定义签名、返回形状与失败语义，不含实现。

### 5.1 共享返回形状

```ts
interface ActionOutcome {
  protocol: 'mineintent.action-outcome.v1'
  tool: ToolName
  /** 意图达成度，不是成败判断 */
  status: 'completed' | 'partial' | 'no_effect' | 'interrupted'
  reasonCode: string
  /** 实际发生了什么——事实，不是成功断言 */
  observed: Record<string, unknown>
  metrics: { durationMs: number }
  evidenceIds: string[]
}
```

`status` 语义：

- `completed` — 意图达成
- `partial` — 做了一部分（走了 4 格被水挡住；挖到一半超时）
- `no_effect` — 什么都没发生（准星上没有目标）
- `interrupted` — 被玩家停止指令、危险反射或新决策取消

**不使用 `failed`。**「打空了」「没挖完」是事实，不是错误。这是[产品设计](../product-design.md) §12 的直接要求，也是模型自我修正的唯一依据。

#### 5.1.1 两个受众，一个来源

`ActionOutcome` 有两个消费者，需求不同：

| 受众 | 需要 | 形式 |
|---|---|---|
| Runtime（journal / telemetry / 验证 / 回放） | 精确、可比对 | 完整结构化，含 `reasonCode`、`evidenceIds`、精确数值 |
| 模型 | 情境理解 | **知觉等价的投影** |

关键约束：**模型看到的字段不得携带人不可能拥有的精度。**

反例：若返回 `distance: 6.2`，模型就可能说出「它离我大概 6.2 格」——**没有人类玩家这么说话**，这是当前设计主动制造的 bot 特征。

正确做法**已经存在于 `viewport-provider.ts`**，直接沿用：

```ts
distanceBand: 'very_near' | 'near' | 'medium' | 'far'   // 枚举，非数值
direction: RelativeDirection                             // 相对方位
relativePosition: viewRelativePosition(pose, position)   // 量化到 0.5
```

`visibleEntities` 中的 `distance` 与 `aimPosition` 被明确标注为内部值、不发布。**枚举 + 量化即可达成知觉等价，不需要任何生成器。**

**例外——仪表类信息保留精确数值。**血量、饥饿、背包数量是玩家从界面上直接读到的，「17 个橡木原木」就是他看到的内容。这里用数字不是精度泄漏，是等价。

> **已否决的方案：自然语言渲染。**
>
> 曾提议把模型可见的观察渲染成散文（「前面是一片橡树林，右前方十几格有个火把」）。该方案被否决，理由是决定性的：
>
> 1. **散文需要一个生成器。**规则式的必然脆弱，且它本身就在替模型做显著性判断——若渲染器输出「周围没什么特别的」而那里有只苦力怕，模型永远无法恢复，认知错误被变成了感知盲区。
> 2. **模型式生成器等于在感知通路里放一个会幻觉的组件**，比精度泄漏严重得多。
> 3. **成本并未消失**，只是从上下文转移到了额外一次模型调用。
>
> 枚举 + 量化以零成本达成同一目标。详见 §8.5。

### 5.2 工具集

| 工具 | 参数 | 对应人的动作 |
|---|---|---|
| `look` | 相对角度 | 转头 |
| `move` | 方向 + 距离 | 走 |
| `jump` | 无 | 跳 |
| `dig` | 无（准星） | 按住左键 |
| `attack` | 有界连击 | 连点左键 |
| `use` | 无（准星 + 手持物） | 按右键 |
| `select_slot` | 槽位号 | 滚轮 |
| `information` | 已有实现 | 看仪表 / 回想 |

---

#### `look`

```ts
look(input: {
  turnYawDegrees: number      // -180 ~ 180，身体相对
  turnPitchDegrees: number    // -90 ~ 90
  precision?: 'glance' | 'aim'  // 容差，默认 glance
}): ActionOutcome & {
  observed: {
    lookedAtBlock: { name: string; withinReach: boolean } | null
    lookedAtEntity: { type: string; username?: string; distanceBand: DistanceBand } | null
    viewport: ViewportRead     // 转头后的新视口
  }
}
```

**只接受相对角度。**视口已提供 `direction`（front/left/…）与 `distanceBand`，模型据此换算——这与人的做法一致。

**注意 `lookedAtBlock` 用 `withinReach` 而非 `distance`。**当前 `viewport-provider.ts:128` 是全系统唯一仍向模型发布原始距离数值的字段（`0.25` 的倍数，上限 `MAX_LOOK_DISTANCE = 4.5`）。实体已改用 `distanceBand`，准星方块却给裸数字，既不一致，也是唯一能让模型说出「它离我 3.25 格」的入口。而 `MAX_LOOK_DISTANCE` 本身就是触及距离——这个数字几乎没有信息量，布尔值足够。

转头由 `VisualAttentionController` 的限速逼近执行（`MAX_YAW_SAMPLE`），不瞬移。该实现正确，直接复用。

`precision: 'aim'` 收紧容差并允许更长时间，用于交互前的精确对齐。

失败语义：无。转头总会完成，只是可能没看到期望的东西——那是 `observed` 的内容，不是失败。

---

#### `move`

```ts
move(input: {
  direction: 'forward' | 'back' | 'left' | 'right'
  blocks: number              // 1 ~ 16
  sprint?: boolean
}): ActionOutcome & {
  observed: {
    movedBlocks: number
    blockedBy?: { name: string; direction: string }
    fell?: { blocks: number }
  }
}
```

无寻路。走直线，1 格台阶自动跳跃，撞上就停并如实回报。

- `completed` — 走满 `blocks`
- `partial` — 走了一部分，`blockedBy` 说明原因
- `no_effect` — 一格没动（面前就是墙）

长距离导航不由本工具解决。

---

#### `jump`

```ts
jump(): ActionOutcome & { observed: { landed: boolean; heightGained: number } }
```

---

#### `dig`

```ts
dig(input: { maxDurationMs?: number }): ActionOutcome & {
  observed: {
    brokenBlock?: { name: string }
    /** 未挖完时的进度提示 */
    progress?: 'in_progress' | 'tool_ineffective'
  }
}
```

作用于准星当前所指方块。执行前重新 raycast，**不信任任何先前的目标记录**。

- `completed` — 方块破坏，`brokenBlock` 给出名称
- `partial` — 到时未破坏（工具不对或时间不够）
- `no_effect` — 准星上没有方块 / 超出交互距离
- `interrupted` — 被取消

**注意**：破坏方块与掉落物拾取是两件事。本工具只报告破坏，不声称获得了物品——背包变化由 `information` 读取确认。这是避免「虚报完成」的关键。

---

#### `attack`

```ts
attack(input: {
  maxSwings?: number          // 默认 1，上限 ~10
  maxDurationMs?: number
}): ActionOutcome & {
  observed: {
    swings: number
    hits: number
    target?: { type: string; username?: string }
    targetLost?: 'moved_out_of_reach' | 'died' | 'occluded'
  }
}
```

作用于准星当前锁定的实体。控制器内部在挥击间维持准星跟踪（这是人的手部动作），但**模型从不命名目标**。

`hits` 与 `swings` 分开报告——打空是常态，必须如实呈现。

`target.died` 不等于「我杀死了它」。若需确认，由后续观察得出。

---

#### `use`

```ts
use(input: { maxDurationMs?: number }): ActionOutcome & {
  observed: {
    placedBlock?: { name: string }
    openedContainer?: { kind: string }
    consumedItem?: { name: string }
    heldItem: { name: string } | null
  }
}
```

对应原版右键：放置方块 / 打开容器 / 进食 / 使用物品。行为取决于准星所指与手持物品——这正是原版的语义，**不拆成多个工具**。

`heldItem` 始终返回，使模型能理解「为什么什么都没发生」。

---

#### `select_slot`

```ts
select_slot(input: { slot: number }): ActionOutcome & {
  observed: { heldItem: { name: string; count: number } | null }
}
```

仅快捷栏 0–8。容器与背包 GUI 是独立课题（需要「打开界面才能读取」的信息语义），不在本草案内。

---

#### `information`

沿用 `InformationTool` / `InformationToolSession` 现有实现，接上预算即可。

建议初始预算：`maxCalls: 4`、`maxReadCalls: 3`、`maxReturnedBytes: 24_576`、`deadlineAt: now + 3s`。

### 5.3 被动 vs 主动的划分

按人的实际情况分配，而非全被动或全主动：

| 通道 | 方式 | 理由 |
|---|---|---|
| `current_status`（生命/饥饿/效果） | **被动注入** | 本体感觉是免费且持续的 |
| `inventory_information` | **被动注入** | 手上有什么不需要"查" |
| `sound_information` | **被动注入** | 听觉是被动的 |
| `viewport_information` | **主动调用** | **视觉需要主动指向** |

这比「全部被动的单次快照」与「全部主动的 tool loop」两个极端都更贴近[产品设计](../product-design.md) §2.1 的信息等价。

同时它顺带解决了 §2.4 的硬编码问题：viewport 改为按需调用后，`READ_PLAN` 只剩三个稳定接口，且应改为从 catalog 读取实时 `schemaRevision`。

### 5.4 成本与它的正当性

多轮往返意味着更多模型调用——延迟与费用都是真实的。

但[产品设计](../product-design.md) §2.1 明确写道：「**停下来观察、反应迟缓、偶尔迷路、操作失败**，都可以是可信玩家行为」。

**多轮往返产生的「慢」不是要优化掉的成本，正是目标行为。**瞬间锁定正确目标、一次成功的玩家反而可疑。

需要管的是别失控——用**预算**管（`InformationToolSession` 已实现），不用**类型系统**管。前者的代价是「这一轮它没看清，先说句话」；后者的代价是整个决策作废、同伴失声。

---

## 6. 可退役清单

若采纳本草案，以下模块在模型边界上失去职责：

| 模块 | 行数 | 说明 |
|---|---|---|
| `src/grounding/` | 480 | 语义引用绑定证据 |
| `src/information/ref-store.ts` | 262 | 模型可见的引用发放 |
| `src/information/cursor-store.ts` | 197 | 分页游标（本就无消费者） |
| `src/behavior/behavior-synthesizer.ts` + `contracts.ts` | 210 | 语义目标反解为计划 |
| `decision-protocol.ts` 中的引用校验 | ~120 | §2.5 四项 |
| **合计** | **~1270** | |

**保留**：`InformationRuntime` 主体、access-policy、registry、providers、source-ports、`VisualAttentionController`（作为 `look` 的执行层）、`SpeechScheduler`、journal / memory / telemetry。

退役应在 tool loop 验证有效之后进行，不应提前删除。

**附带收益：**`ref` 从模型可见载荷中退役后，单条可见方块记录从 95 字节降至 18 字节。同样上下文预算下可见方块数从约 210 提升到约 1100——**这是 §8.5「数目过于保守」的实际解法**，且不增加任何扫描成本。

---

## 7. 建议顺序

| # | 动作 | 依赖 | 可逆 |
|---|---|---|---|
| 1 | **speech 在具身部分被拒时仍然落地** | 无 | 是 |
| 2 | 能力目录砍到与实现一致（或纳入信息治理） | 无 | 是 |
| 3 | `READ_PLAN` 改用 catalog 返回的实时 `schemaRevision` | 无 | 是 |
| 4 | 删除 `unused_referent_role` / `invalid_message_referent` 校验 | 无 | 是 |
| 5 | `lookedAtBlock.distance` → `withinReach`（§5.2 `look`） | 无 | 是 |
| 6 | 可见方块选择改进：距离分层配额 + 显著性优先（§8.5 ①②） | 无 | 是 |
| 7 | 接上 `InformationTool` + 预算，viewport 转为主动调用 | 3 | 是 |
| 8 | 实现 `look` / `move` / `jump`（复用现有控制器与限速逻辑） | 7 | 是 |
| 9 | 实现 `dig` / `attack` / `use` / `select_slot` | 8 | 是 |
| 10 | 验证观感后，按 §6 退役；`ref` 退出载荷后放宽方块数目 | 9 | 否 |

第 1–6 步都不引入新机制：1–4 是纯删除与降级，5 是一个字段替换，6 是排序策略调整。**无论最终走哪条路线，这六步都需要进行。**

**第 1 步是唯一会导致同伴对玩家完全无反应的路径，优先级最高。**

第 10 步顺带解决「可见方块数目过于保守」——`ref` 退出模型可见载荷后，同样上下文预算可容纳约 5 倍方块（§6、§8.5）。**在此之前不必调整 `limit`**：实测表明 `limit` 既不影响扫描成本，也不是当前的瓶颈。

---

## 8. 已提出但未决的问题

以下问题在本次审视中被发现，但**尚未讨论或达成结论**。它们不阻塞 §7 的前四步，但其中若干条对产品核心目标的影响不亚于本文主体。

### 8.1 同伴没有主动性（可能是 P1）

`ContextTrigger` 只有四种：`startup`、`player_chat`、`action_result`、`danger`（`src/context/context-composer.ts:25`）。全部是被动的，**没有任何定时器或空闲触发**。

同时 `next_attention` 效果定义了 `earliestProactiveAt`（`src/models/contracts.ts:229`），`route` 枚举中有 `'proactive'`，`decision-protocol.ts:109` 还在校验这个时间窗口的合法性——**但没有任何代码读取它**。契约完整，消费端不存在。

后果：**同伴只在被搭话时才动，其余时间纹丝不动。**

这直接冲击两个产品目标：

- [产品设计](../product-design.md) §3.1「既不会像工具一样只有收到命令才启动」——当前恰好就是工具行为
- [产品设计](../product-design.md) §2.1 的行为图灵测试——一个静止不动、只在被呼叫时响应的玩家会被立刻识别

**这是「不被识别为机器人」这个目标上最大的单一缺口，而且它与信息纯洁性无关。**信息纯洁性对外部观察者不可观测；空闲行为是高度可观测的。

未决：主动性应由模型驱动（读取 `earliestProactiveAt` 并调度）还是由运行时驱动（空闲计时器触发 `idle` trigger）？前者与现有契约一致但依赖模型自觉，后者更可靠但可能机械——[产品设计](../product-design.md) §6.2 明确要求「不应机械定时发言」。

### 8.2 危险反射无法逃离

`#considerDanger`（`src/companion/runtime.ts:626`）在生命值 ≤ 8 时 `releaseAll()` 并说「我受伤了，先停一下！」。

但同伴**没有腿**——[目标系统设计](../architecture/target-system.md) §5.1 声明的「低血量逃生触发」在当前实现下不可能发生。释放身体输入是当前唯一能做的事，而它恰好是原地不动。

§5 的 `move` 落地后应重新设计这条路径：反射层应能后退、转身、跑开，且不经过模型延迟。

另需重新审视：10 秒冷却（`:627`）与生命值 8 的阈值是硬编码的，与[产品设计](../product-design.md) §4「不要求把社交判断编码成固定阈值」的方向存在张力。安全反射保留硬编码是合理的，但阈值本身需要论证。

### 8.3 记忆系统与产品承诺不匹配

`FileMemoryStore` 共 86 行（`src/memory/memory-store.ts`），检索为关键词重合度打分 + 时间衰减：

```ts
score = overlap * 10 + 1 / (1 + ageDays)
```

没有摘要、没有整合、没有遗忘、没有语义检索、没有跨会话的关系状态建模。

而[产品设计](../product-design.md)把长期记忆列为核心价值之一（§3.4、§9.4、§15.5），「长期陪伴」这个产品定义几乎完全依赖它。

对比：信息 + Grounding + Behavior + Motor 四层合计 4195 行，产出一个动词；记忆 86 行，承载整个产品的持续关系。

**这是当前投入分配与产品价值之间最大的错配。**本文主体（工具化具身）不解决它。

未决：记忆是否应在具身工具落地之前先行？考虑到 §16 原型的第 6、7 步（记住共同活动、重连后能回答）完全依赖记忆，而第 2–5 步依赖具身，两者可能需要并行。

### 8.4 文档与实现的真实性

[目标系统设计](../architecture/target-system.md)的架构图仍包含 `Action Runtime ├── bounded controller plans ├── resource locks`——该模块已于 `29052bf` 删除。类似的漂移可能不止一处。

1500 行设计文档使项目读起来完成度远高于实际（实际能力：转头 + 说话）。这是 §3.5 所述「难以察觉问题」的三个机制之一，且它会持续生效。

建议：在[目标系统设计](../architecture/target-system.md)每个模块标注 **已实现 / 契约已定 / 未开始** 三态。这是低成本、高杠杆的改动——它让文档停止替不存在的系统背书。

未决：是否需要一条 CI 检查，防止文档描述与模块存在性再次漂移。

### 8.5 viewport 的信息形状（可能比本草案更重要）

**本节保持「待决」。**「没有好办法描述视觉信息」是对现状的诚实判断，不应被任何口号盖过——下面区分已经确定的部分与仍然开放的部分。

#### 已确定：瓶颈不是数目，是每条记录的成本

当前发布形状（`viewport-provider.ts:158-162`）实测：

```text
{"ref":"ref_27d67218-1978-4dd0-9280-13c18e2c619a","relativePosition":[3,0,-8],"name":"oak_log"}
                                                                                     95 bytes
["oak_log",3,0,-8]                                                                    18 bytes
```

`iref_` + UUID 占 41 字节，是整条记录的大头。设计意图中的「四元组紧凑格式」是 18 字节，实际发出的是 95。

| | 每条 | 20KB 预算下 |
|---|---|---|
| 当前（带 ref） | 95 B | ~210 块 |
| 纯四元组 | 18 B | **~1100 块** |

而按 §1.3、§4.3 的结论，**模型边界上的 ref 本来就要退役**——这不是新的取舍，是已定决策的直接后果。

结合 §2.7 的实测（`limit` 对扫描成本零影响），结论是：

> **可见方块数目确实过于保守，但瓶颈从来不是扫描或 `limit`，是每条记录里那个即将退役的 ref。同样上下文成本可容纳约 5 倍的方块。**

#### 已确定：两个确定性的选择改进

放宽到 ~1000 之后，「最近 N 个」的排序截断问题会缓解但不消失。以下两条**不需要生成器、不需要额外模型调用**：

**① 距离分层配额。**近 / 中 / 远各占 N/3，而非全局按距离排序后截断。当前在密集地形中，远处的树线与山脊永远被近处的草挤掉。改动位于 `perception.ts:156` 的 `candidates.sort` 之后。

**② 显著性优先于距离。**一张静态方块分类表——人造物（火把、告示牌、箱子、门、工作台、熔炉、床、铁轨）与矿物排在地形（草、土、石、沙、树叶）之前。**纯查表，不涉及场景理解。**

②直接服务[产品设计](../product-design.md) §6.3：玩家放置的火把与告示牌是明确列出的非语言交流渠道。二十格外墙上的一个火把，比第 300 块草有意义得多——而按距离排序它永远进不来。

**③ 同名连通块合并**（记录，但不推荐先做）：几何上可行，但合并后模型需从 extent 反推形状，可能比列表更难读。风险大于收益。

#### 仍然开放

即使做完上述改进，核心问题仍未解决：**最近 N 个方块的投影，与「一个人看到的世界」之间仍有距离。**

[产品设计](../product-design.md)要求理解的是「前方是一片橡树林」「北边地势升高」这类整体判断，而体素列表不直接提供它。它足以证明「没有作弊」，但**是否足以让模型表现得像一个在看世界的人，尚未验证**。

而对外部观察者而言，**一个完美可信但不可用的感知通道，与一个损坏的感知通道，产生的可观察行为完全相同**。

仍待探索的方向：

- **跨帧变化检测**（「刚才那里没有这个箱子」）——变化是人类注意力的主要驱动，而当前每轮都是无记忆的独立快照。这条与 §8.1 的主动性直接相关：**察觉变化正是主动发起交流的天然理由**，且它不机械。
- 分区聚合的可行形式（若存在一种既不需要生成器、又能表达整体地势的编码）。

> **已否决：自然语言渲染。**详见 §5.1.1 的方框。核心理由是散文需要一个生成器，而任何生成器都要么脆弱、要么把幻觉引入感知通路，且成本只是从上下文转移到额外的模型调用。**枚举 + 量化在精度等价上达成同一目标且零成本，但它不解决整体判断问题——那部分仍然开放。**

### 8.6 性能：分壳早退未实现

§2.7 的实测表明唯一有效的性能杠杆是按距离分壳扫描并早退（当前为全量扫描后 `slice`）。真实 backend 下的实际耗时仍需实测——若接近 `timeoutMs: 5000`，viewport 会静默进入 `omissions`，同伴局部失明且无报错。

viewport 转为主动调用（§5.3）后，该问题的影响面变化：调用更少但更关键，超时的代价更高。

### 8.7 其他

- **长距离导航**（跨区块移动、迷路与重新定位）不由本草案解决，需要独立设计。§4.5 主张移除 pathfinder 可能是对的，但这使长距离移动完全没有方案。
- **容器与背包 GUI** 需要「打开界面才能读取」的信息语义，是 `information` 层的扩展而非新工具。
- **多人环境**：`interpretPlayerChat` 已有 addressing 判断，但 `runtime.ts:206` 直接丢弃所有非主要玩家的消息。[产品设计](../product-design.md) §14 允许第一版不处理复杂多人关系，但「完全不回应其他玩家」在盲测中本身就是一个特征。

---

## 9. 附录：本文的形成过程

记录这一节的原因与 §1.3 相同：**一份只有结论的文档，几个月后会变成又一份无法再审视的权威。**保留过程，是为了让将来的自己能重新判断，而不是只能照做。

### 9.1 起点

审视的起点是一个落差：项目自述目标是「能在原版生存环境中像真人朋友一样长期陪伴的 AI 玩家」，而代码实际能做的身体动作只有转头。

第一轮发现的问题（能力目录说谎、V1 skills 被删后中间层缺失、投入分配与产品阶段判断相反）都是**从这个落差直接读出来的**，不需要深入架构。

### 9.2 三个转折点

结论不是一次得出的。真正推翻既有框架的四步，都由项目作者提出：

**转折一：「我当时想的就是走 tool 注册和调用的路子，不知道为什么现在被搞复杂了。」**

这句话把问题从「当前架构有哪些 bug」转成了「当前架构为什么存在」。顺着追查，发现 `InformationToolSession`（安全的、带预算的 tool loop）**早已写好且有测试，但生产调用方为零**——放弃它的理由（防止导出全部世界状态）恰好是它自己已经解决的问题。

**转折二：「人本身也会记不清，我们不能要求 AI 给出引用解释。而且我们只需要控制输入的信息暴露。」**

这一步区分了**输入控制（纯洁性）**与**输出证明（可审计性）**。因为 context 完全由 runtime 构造，输入边界可信，输出证明就是重复付费。这是 §1.2(二) 的来源。

**转折三：「我只需要把视角移动到上次看到羊的角度，返回新的视口。没对齐模型会再次对齐，对齐了就正常，完全不需要检验。」**

这一步推翻了审查者当时仍在坚持的立场——即「消歧」与「时效」是引用不可替代的残余价值（现记录于 §1.3 反对 1、2）。用反馈回路取代事前保证之后，两者同时消失。

**转折四：「散文丢失了太多信息，而且仍然需要一次模型调用。四元组已经是权衡后的结果，因为我们没有什么好的办法去描述信息。」**

审查者曾提议把模型可见的观察渲染成自然语言，理由是 JSON 会泄漏人不可能拥有的精度。**该提议被否决，且否决理由是决定性的**：散文不会自己写出来，生成器要么脆弱、要么把幻觉引入感知通路，成本只是转移而非消失。

有价值的是这一步之后发生的事：**精度泄漏的问题确实存在，但项目中已有更好的解法**——`visibleEntities` 用 `distanceBand` 枚举与量化 `relativePosition`，不发布原始 `distance`。**枚举 + 量化以零成本达成了散文想达成的目标**，且不需要生成器。

于是批评从「JSON 是错的」收缩为一个具体缺陷：`lookedAtBlock.distance` 是唯一仍在发布原始数值的字段。同时问题的重心从**编码**转移到**选择标准**与**每条记录的成本**（§8.5）。

### 9.3 审查者被证伪的三处判断

**其一（技术）**：曾判断 `isVisibleFromEye` 会把火把、告示牌等非固体方块误判为不可见。实测证伪，原因见 §2.8。现已记录为「实现正确，不应修改」。

**其二（架构）**：曾判断引用机制保有「消歧」「时效」两项不可替代的价值。被转折三推翻。

**其三（表示）**：曾提议用自然语言渲染取代结构化观察。被转折四推翻——**提出方案时没有计算它的实现成本**（生成器从哪来），而这个成本恰好是决定性的。项目中已存在的枚举 + 量化方案以零成本达成同一目标。

### 9.4 方法论教训

值得记下的是**第二处错误的性质**。

它不是看漏了代码，而是**审查者在阅读架构的过程中，吸收了这套架构的前提**——「系统必须事先保证指对目标」。在这个前提下，消歧和时效确实需要机制来实现，引用确实不可替代。前提本身从未被质疑，因为它不以主张的形式出现，而是以「问题的形状」出现。

这正好解释了原始困境（「被惯性思维和历史文档所困，难以觉察」）的机制：

> **一套架构最难被审视的部分，不是它的结论，而是它悄悄定义的问题。**

代码、文档、契约、测试都在反复强化那个问题形状。读得越认真，越容易接受它。

因此 §1.3 的形式（记录被否决的反对意见及其理由）比 §1 的结论更重要——它保留的是**问题被重新定义的那一刻**，而结论只是那次重定义的副产品。

### 9.5 与原始困境的关系

本文识别的三个「使问题难以察觉的机制」（§3.5）——约束比能力好写、文档替不存在的系统背书、可信性被投射到不可观测的一半——在本次审视中全部实际出现过：

- 审查者最初也把重点放在可验证的信息层，而非可观测的行为层
- [目标系统设计](../architecture/target-system.md)的架构图使已删除的 `Action Runtime` 看起来仍然存在
- §8.1（同伴没有主动性）直到讨论后期才被发现，而它对「不被识别为机器人」的影响可能大于全部信息纯洁性工作

**第三条尤其值得警惕**：它至今仍未被处理。
