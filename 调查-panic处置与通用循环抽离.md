# 调查：panic 处置与通用循环抽离

> 性质：**调查记录与工程产出汇报**，无产品权威（`产品.md` G01/G03）。
> 其中「待决」条目不构成裁定；「已核实」条目是对当前实现的事实陈述。
> 日期：2026-08-06。代码分支：`feat/gate-b-vertical-script`、`experiment/no-panic-catch`。

## 0. 一句话

审计全仓 panic 捕获，删掉无依据的九处、给保留的写明理由；随后开实验分支把可执行的六处全删、装 panic 钩子实跑，**当场找到一处被捕获遮住的生产缺陷**（观察回调租约非 RAII → 死锁）。同期把领域无关的模型—工具循环抽成独立 crate `toolloop`。

---

## 1. 外部对照调研

克隆并读了四个 Rust agent 项目的核心抽象：`rig`、`swiftide`、`listen`（真实资金的生产系统）、`rig-tap`。

### 1.1 决定性的数字

| 仓库 | 生产代码 `catch_unwind` | 生产裸线程 |
|---|---|---|
| rig（`crates/`） | **0**（44 处全在 `tests/`） | 0 |
| swiftide | **0** | 0 |
| listen（279 个 .rs） | **0** | 0 |
| rig-tap | **0** | 0 |
| **本项目（调查起点）** | **16** | **2** |

listen 的替代做法：错误走 typed `Result` + 指数退避重试 + **每次都 `tracing`**；panic 交给 `tokio::spawn` 的任务边界。没有一家做「catch 完压成一条普通失败且不出声」。

### 1.2 我们唯一的特殊性（实测确认）

裸线程 `mineintent-backend-runtime-{id}` 是**正当的**，但原先记录的理由（README 只写「是什么」不写「为什么」）和我一开始的推断都不对。

- ❌ 不是「bevy 是 `!Send`」——`SwarmBuilder` 带 `where Self: Send`，并特意用 `SubApp` 而非 `App` 保住 Send
- ✅ 真实原因：**azalea 内部自建 `LocalSet` 并大量 `spawn_local`**
  - `azalea/src/swarm/builder.rs:451` `LocalSet::new()`，其后注释「start_ecs_runner must be run inside of the LocalSet」
  - `azalea-client/src/client.rs:115`「This function panics if it's called outside of a Tokio `LocalSet`」
  - 编译探针实测：`Rc<tokio::task::local::Context>` cannot be sent between threads safely

`!Send` 只说明不能 `tokio::spawn`；再加上「它跑满整个应用生命周期」才推出不能在 `main` 里 await。**两条合起来**才逼出这根线程。理由与证据已写入 `facade.rs` 的 spawn 处（提交 `5745dbb`）。

**dispatcher 线程没有独立理由**——它是推送式事实流的产物。

### 1.3 值得记的外部设计

| 来源 | 形状 | 对本项目的相关性 |
|---|---|---|
| rig | `AgentRun` / `AgentRunStep` sans-IO 状态机，与我们**同名同形**（我们的 agent 层本就照 Rig 形状做，见 `supplies/MANIFEST.md` 的 simple-agent 条目） | 已印证 |
| rig | 每个生命周期点**一个专属动作类型**，编译器拒绝非法组合；`presentation` 与 `raw_result` 分离 | 待议 |
| rig | `AgentRun` 全量 `Serialize`，可跨进程恢复 | 与 `产品.md` §12「断线或重启后恢复」同向 |
| swiftide | `StopReason` 七变体一等公民；历史即状态，靠「有 tool_calls 无对应 result」补执行 | 待议（#116 明确：框架对不完整工具历史的自动修复**不得**被当作世界事实） |
| swiftide | `ApprovalRequired(Box<dyn Tool>)` 装饰器 + `FeedbackRequired` 挂起 | **不适用**——`产品.md` A11「产品不提供额外的停止接口」 |
| listen | 模型产出 `Condition + Action` 而非动作；世界变化时重求值；`Now` 也是一种条件 | 方向候选，改动大 |
| rig-tap | `ObservabilityEvent` 信封：版本化 wire、每个 payload 带 `truncated`、`error_class` + `retriable`、稳定 `call_id` | **可直接借形状**，正对我们的可观测性缺口 |

---

## 2. 仓库既有材料的复核

**重要：`提案接受` 标签当前零个 issue 持有**，按 `产品.md` G03，下列 issue 是**材料不是裁定**。

| 材料 | 与本次调查的关系 |
|---|---|
| #98「一轮没有定义」 | 本次关于 Body 粒度/并发的推导，它写在前面且更精确。最终立场：「能力契约表达键盘状态；世界回答实际发生了什么；中间层不设组合闸门」 |
| #99「到达制度缺席」 | 提出把「哪些事实够格到达」改写为**每周期预算**（sporadic server / temporal isolation），并指出成本单位是**模型轮次**；另提 NAPI 形状「帧只说有变化，来看」 |
| #104「MCP-like 分层」 | 第 3 条：长动作应暴露状态与终止能力，不占前台 |
| #111「产品假设没有语法标记」 | 判据：**产品假设拒绝的是完整合法的输入；机制拒绝的是缺失、畸形或已失效的输入**。并指出变异测试在产品假设上给出**反向信号** |
| #108「方向偏离审计」 | 「像个安静的人」是偏离，非产品目标。**本次一度误引，已更正** |
| #116 | 给外部 Agent Runtime 设了三条硬闸：自动修复不得当世界事实、必须默认禁用外部 tracing、checkpoint 只有在不引入第二套事实源时才有价值 |
| PR #117 | TS 期曾删掉独立轮末帧，改为每工具 `observationAfter`；Rust 期 中期更新-05 又裁回轮末帧，理由更硬（N 进 N 出下逐工具视口构造上就是过期的） |

### 2.1 现行产品条款（`origin/main:docs/产品.md`，148 行）

本次一度读了**已被取代**的 `docs/product-design.md`（473 行，本地 `main` 落后至 PR #101）。当前权威条款中与本调查相关的：

- **W05/W05a**：运行时不必提供全部信息，但不得**暗中**删除或歪曲已进入正常感知范围的信息；影响必须能由**公开的事实和能力边界**解释
- **W07**：可以阻止不合法/不安全/超出能力/已失效的动作，**但必须如实说明介入原因**
- **W07a**：不得把自己的决定说成 AI 的决定，**不得把未知说成已知**，不得把准备执行说成已经完成
- **W09**：每个决定都需要理由和记录；**没有理由的选择保持待决**
- **G05**：不得通过代码、配置或测试**静默决定**尚未决定的产品问题
- **A11**：停止能力来自已有外部权力（封禁、终止进程）；**产品不提供额外停止接口**
- **N02/N03**：不把「与安静的人不可区分」作为完成标准；不刻意模拟人类缺陷

---

## 3. panic 捕获审计

### 3.1 判据的演进

1. 起点：「panic 是 bug 还是异常」
2. 修正：**关键不是 panic 的性质，是这段代码跑在谁的监督之下**
3. 再修正（维护者指出）：Java 的 `catch` 接的是**有人故意抛的信号**；Rust 里 `Result` 才是那个通道。除非我们故意 panic，`catch_unwind` 接住的就是**错误**，不是异常
4. 补充：论证「不得不」时不能只说「如果 panic 了会很糟」，还要能说出**什么会 panic**
5. 最终分类：**范畴性理由**（只依赖位置、换任何代码都成立、后果无法补救）vs **具体理由**（依赖能指名触发源、后果可由监督者改善）

### 3.2 处置结果（`feat/gate-b-vertical-script`）

```
删除 9 处：driver 6 + middle listener 2 + runner 转录 1
  全部跑在 process_wake 里被 await 的 tokio 任务内——tokio 已隔离且做得更好
  （panic → JoinError → fail_runtime_sync + journal_failure + 可能 Faulted，保留原始消息）
  原先压成 tool_dispatch_panicked：与工具正常失败不可区分，模型会重试，而 panic 必然可重现

连带删除 toolloop 里零使用者的 CatchUnwindFuture / catch_future_panic

保留 6 处，逐处写明理由，全部改为带结构化字段的 tracing::error!
  （此前 4 处完全静默、2 处 eprintln! 进不了 MINEINTENT_LOG 与 devlog）
```

### 3.3 全仓仅有的一处「故意抛」

`information_adapters.rs` 的 9 处 `panic!`，模块头明写「expected to be caught by the Information runtime/provider boundary」——**而那个接手方从未接线**。

唯一落在活路径上的是 `SoundHistoryInner::record`（一次调用里 `finite()` 被调六次）。原先它被 backend dispatcher 泛泛接成「订阅者回调 panic」，**含义在翻译中丢失**。

已改回 `Option`：非有限值跳过该条观察 + `tracing::warn!` 带 `field`。按 W05a，「这条声音的坐标不是有限数，跳过」解释得了；「回调 panic 了」解释不了。其余 8 处在未接线的 source-port 实现里，随 Information 子系统处置一并决定。

---

## 4. 实验分支：`experiment/no-panic-catch`

删掉全部**可执行**的六处捕获，装 `devlog::install_panic_hook`（线程名 + 位置 + 消息 + `Backtrace::force_capture` → tracing 与 dev.log），跑测试。

### 4.1 发现一（生产缺陷，已修）

整套测试从「不到一分钟」变成「600 秒不结束」。`sample(1)` 抓栈，两个线程停在同一处：

```
Subscription::drop → close → remove_observation_subscription
  → wait_for_quiescence → parking_lot::Condvar::wait      ← 永久
```

**`ObservationCallbackGuard` 只 pop thread-local 栈，租约靠调用点手动 `finish_callback()` 释放。** 订阅者 panic 跳过那一行 → `active_callbacks` 永不归零 → 之后任何 unsubscribe（含 `Drop`）永久死锁。

这正是审计阶段「指不出触发源」的那个触发源。修法是把归还挪进守卫的 `Drop`——RAII，正常返回与 unwind 两条路径都走它。

> **这一处与「catch 与否」无关，应无条件回到主线**：即使保留全部捕获，手动租约对也是错的，只是 catch 恰好挡住了唯一一条 unwind 路径才没暴露。

### 4.2 发现二（测试靠吞 panic 才通过）

`AUnsubscribesBListener` 假设自己只被调用一次，而测试先发 Entity 再发 Block，第二次 `.take()` 得到 `None` 就 `.expect()` panic。它在主线上通过，是因为 `catch_unwind` 把它吞掉并记成一句「listener panic isolated」。

### 4.3 发现三（预测被证实，且比预想的糟）

删掉 facade 那处后，`facade_subscription_fifo_panic_unsubscribe_and_reentry_are_bounded` 报的是 **Timeout 而不是崩溃**——panic 杀死 dispatcher 线程之后是**静默挂起**：进程还活着，事实流断了，没有任何人说话。

三处测试的前提被移除，已标记 `#[ignore]` 并写明新行为。54 组测试 0 失败、3 忽略。

---

## 5. 通用循环抽离：`toolloop`

```
crates/toolloop/          1064 行，独立 crate
  run.rs        564   状态机（sans-IO，零领域耦合）
  transcript.rs 365   转录
  control.rs    112   取消与 deadline（无 panic 隔离）
  lib.rs         23
```

依赖只有 `mineintent-contracts` + serde + tokio（仅 `macros` + `time`：deadline 要定时器，`select!` 要宏；不引 `rt`/`net`/`fs`）。

同期：1249 行视口三件套移出 `agent/` 到 `crate::viewport`（其中 `mirror` + `reducer` 1118 行**生产不可达**）；`ToolDispatcher` 增加 `after_batch` 批末钩子（默认 `Ok(None)`，纯增量）。

**沙盒验证**（`scratchpad/toolloop-lab/`，未入库）：拆出 `agent-contracts` **631 行**通用切片后，`toolloop` 可独立编译。编译器抓到两处 grep 找不到的耦合：
- `ToolExecution<Observation = JsonAgentFrame>` —— 泛型的**默认类型**是 Minecraft 的帧
- `deserialize_optional_non_null` —— 通用 serde 帮手住在领域文件 `context.rs` 里

`agent/` 从 2284 行降到 1033 行。**剩余四步未做**：dispatcher 实现 `after_batch` → driver/runner 去 Sampler 泛型并搬入 → context/prompt 搬去领域侧 → 拆 contracts。

---

## 6. 已核实的实现偏差（`architecture.md` 已记七条之外）

| # | 偏差 | 依据 |
|---|---|---|
| 1 | **轮内中断事实到不了模型** | `ParticipantObservationAfterSource` 实现完整但从未接线（`factory.rs` 走默认 `NullObservationAfter`）。而中期更新-06/08/11 三次重申「轮内中断事实走 `observationAfter`」。后果：同伴在自己这一轮里被打，它不知道 |
| 2 | **相反键抵消未按轴处理** | #98 更正三要求「前后抵消不影响左右轴」；`direction_for` 的 `(forward, back, left)` 落到 `_ => WalkDirection::None`，整个不动 |
| 3 | **跨层背压死锁环** | 四个无超时 `Condvar`；最内层 `CommandCompletion::wait_blocking` 在 `Drop` 里且无 deadline。四层控制车道容量 512/512/512/**8**，最后一层比上游小 64 倍 |
| 4 | **无损管线喂有损环** | 三道 512 深的无损背压，终点是 `ParticipantFactOwner` 的 20 格环（满则丢最老 + 记 omission）。前四层保护的性质第五层不保留 |
| 5 | **约 43% 的 middle 生产不可达** | `information/` 7192 行（`InformationRuntime::new` 唯一调用点在测试里）+ `viewport/mirror`&`reducer` 1118 行 |
| 6 | **观察订阅面零生产订阅者** | `ObservationEventListener` 的 10 个实现全在测试里；`ProtocolObservationSource::subscribe` 零生产调用者 |

---

## 7. 待决（不构成裁定，按 W09 需要理由与记录）

1. **六处保留捕获的终局形态**。当前建议：删 `events.rs` 与 `facade.rs` 两处（扇出隔离与线程存活改由**上下文顶端的监督者**承担：panic → 记录 → 按 A11 交外部重启）；`speech` 那处从「包 `transport.send`」挪到「包整个 `worker_loop`」；三处 Drop 保留（范畴性理由：unwind 中 panic = `abort()`，无法补救）。净效果是失败模式从**静默降级**变为**响亮终止**。
2. **`ExecutionResource` 的通道数**。#98 只裁了键盘，未裁鼠标；`look` 是否独立通道未决。当前 `Body` 把移动与转头合成一个互斥租约，`产品.md` 无对应记录——按 W09 保持待决，按 G05 代码里那个 enum 是一次静默决定。
3. **到达预算的形状**（#99）：挂 run 还是时间窗、能否累积、何时补充。
4. **长动作的后台 job 形态**（#104 第 3 条、PR #73）。
5. **打断政策与打断后重入**（中期更新-11 §6 明确留 W08a 批次）。
6. **约 8300 行生产不可达代码的处置**：接线 / 封存到分支 / 删除。
7. **事实流形态**：推送式无损 vs 拉取式有界（#99 的 NAPI 形状是具体候选）。这一条同时决定 dispatcher 线程与两处 catch 的存废。

---

## 8. 分支索引

| 分支 | 内容 |
|---|---|
| `feat/gate-b-vertical-script` | 主线：toolloop 抽离、panic 审计与处置、viewport/participant 的 `unreachable!` 消除、声音历史非有限值改 `Option` |
| `experiment/no-panic-catch` | 实验：全删可执行捕获 + panic 钩子 + 观察回调租约 RAII 修复 |
| `docs/panic-audit-and-toolloop` | 本文件 |

未入库：`scratchpad/toolloop-lab/`（631 行契约切片的独立编译验证）、`scratchpad/agent-frameworks/`（四个外部仓库克隆）。
