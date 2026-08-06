# Rust 分支代码梳理

> 审查对象：`refactor/collapse-concurrency` @ `15330a1`（= `feat/gate-b-vertical-script` 全部 10 提交 + 死锁/角度/事实通道 8 提交）。
> 这是 Rust 的树尖，`main` 上没有 Rust 代码。
>
> 本文只记录**代码事实**与**基于代码事实的判断**，没有产品权威。凡涉及产品判断的地方，
> 引用 [`docs/产品.md`](./产品.md) 的条目编号，并明确标出「这是产品问题，不是我能定的」。
>
> 每条发现给出文件与行号锚点。带 ⚠ 的是我认为**应当先问维护者**再动的。

## 0. 规模基线

| crate | 生产码 | 测试码 | 说明 |
|---|---:|---:|---|
| `contracts` | 7 215 | 3 319 | 三层之间的进程内契约 |
| `backend` | 17 196 | 10 182（`src/` 内） | 协议后端，含 azalea 适配 |
| `middle` | 19 012 | 20 910 | Agent 循环、capability、Information、记忆、语音 |
| `app` | 1 708 | 0 | 组合根 + 模型 provider |
| `toolloop` | 1 022 | 0 | 领域无关的模型—工具循环 |
| 合计 | **46 153** | **34 411** | 185 个 `.rs` 文件，80 564 行 |

编译基线（`cargo check --workspace --all-targets`，nightly 1.99.0，3m27s）：**通过**，两条告警：

- `crates/backend/src/runtime/frame.rs:216` — `method value_at is never used`
- `crates/middle/tests/agent_round_frame.rs:107` — `variant Panic is never constructed`

---

## 1. 契约层：三代协议同堂，两代无人使用

`crates/contracts/src/agent/context.rs` 1 246 行里同时活着 `v3`、`v4`、`v5` 三代 agent-context 协议，
每代都有完整的判别式类型、`Serialize`/`Deserialize`、双向 `PartialEq` 样板。

实测使用情况（`rg` 全库计数，排除定义文件本身）：

| 类型 | 生产码引用 | 仅 contracts 内 | 测试引用 |
|---|---:|---:|---:|
| `AgentContextProtocolV3` | **0** | 11 | 0 |
| `AgentContextProtocolV4` | **0** | 11 | 0 |
| `AgentContextProtocolV5` | 2 | 11 | 0 |
| `StableContextV3` | **0** | 6 | 2 |
| `StableContextV4` | **0** | 8 | 0 |
| `AgentDecisionContextV3` | **0** | 6 | 2 |

v3 与 v4 在 `crates/contracts` 之外**没有任何引用**——出现处只有三类：
自己的定义（`context.rs`）、自己的 fixtures（`fixtures.rs:48-93`）、以及测自己的测试（`tests/agent_contracts.rs`）。

`StableContextV5` 更直接：`context.rs:305` 是 `pub type StableContextV5 = StableContextV4;`——
v5 的 stable 段就是 v4 的那一个 `memory: String`。

**判断**：这是一个从未发版、**进程内**、单进程单二进制的契约。
没有需要兼容的旧客户端，没有滚动升级，没有别人写的解码器。
版本化的全部收益（兼容旧对端）在这里都不存在，成本却照付。

⚠ **但这不是纯技术账**。移植期 TS 栈的地位是「行为 oracle」（`docs/architecture.md:185`），
v3/v4 fixtures 可能正是 oracle 比对的锚。要先确认：**这些 fixture 是否仍在被用作迁移证据**。
如果是，它们属于历史证据，该按 `docs/history/index.md` 的办法处理，而不是留在活契约里；
如果不是，v3/v4 连同其 fixtures 与测试可以整体删除。

### 1.1 反序列化那一半在生产里没有消费者

上下文是**单向**的：`middle` 组装 → `app/src/model/responses.rs` 序列化进模型请求。
模型不会把 `AgentDecisionContextV5` 送回来。

全库检索 `serde_json::from_str::<>` / `from_value::<>` 的生产调用点（`crates/{app,middle,backend,toolloop}/src/`），
没有一处反序列化 agent-context 系列类型。

但 `context.rs` 为反序列化付了很重的代价——每个手写类型都配一个 `Raw*` 镜像结构：

- `RawAgentStatusV5`（`context.rs:369`）
- `RawAgentHotbarV5`（`context.rs:509`）
- `RawAgentChatItemV5`（`context.rs:615`）
- `RawAgentChatV5`（`context.rs:697`）
- `RawAgentEventV5`（`context.rs:783`）

加上 `deserialize_optional_non_null` / `deserialize_finite` / `deserialize_health` / `deserialize_food` /
`deserialize_optional_light` / `deserialize_optional_sound` / `deserialize_optional_non_empty_vec` /
`deserialize_optional_non_empty_omissions` 一整套自定义反序列化器。

**判断**：这半边的唯一消费者是**测试它自己的往返测试**。
一个只有序列化方向有真实消费者的契约，把反序列化写成同等强度的守卫，
守的是一条不存在的入口。

### 1.2 手写 `Serialize` 里有一部分是 serde 已经做了的

`AgentHotbarV5`（`context.rs:472-505`）为了把 `BTreeMap<u8, _>` 的键渲染成 `"0"`..`"8"`，
专门写了一个 `HotbarSlots` 包装类型（15 行）。

serde_json 对整数键的 map **本来就**序列化成字符串键。这一处需要实测确认（见 §9 待验证清单）。

`AgentItemStackV5`（`context.rs:420-443`）手写 `Serialize`/`Deserialize` 把
`struct AgentItemStackV5(String, u32)` 变成 JSON 数组——`#[derive(Serialize)]` 对二元元组结构体
本来就产出数组。手写实现真正多做的只有 `validate()`。

**这不等于「手写都该删」**：`validate()` 在序列化时报错是有意的纪律（不让非法值悄悄进模型上下文）。
但 `#[serde(try_from = "Raw…", into = "Raw…")]` 能在保留校验的同时去掉大部分样板。

---

## 2. `middle` 手工重实现了 JavaScript 的数字与日期语义

`crates/middle/src/information/support.rs` 一个文件里有三样重实现：

| 位置 | 重实现的东西 | 行数 |
|---|---|---:|
| `support.rs:81-140` | ECMA-262 §7.1.12.1 `Number::toString` | 60 |
| `support.rs:148-236` | `Date.parse` 的 ISO 8601 子集 | 89 |
| `support.rs:274-297` | Howard Hinnant `days_from_civil` / `civil_from_days` | 24 |

写明的理由在 `support.rs:41-43`：

> keeps integer/float rendering aligned with the ref/cursor byte guards

也就是：Information 层的 ref/cursor 有**字节长度上限**，而这些上限是 TS 原型按
`JSON.stringify` 的输出算出来的。要让 Rust 侧的字节数与 oracle 逐字节一致，
就得先复制 V8 的数字渲染规则。

**同一个 workspace 里已经有 `chrono`**，而且 `backend` 正是拿它做同一件事：

- `crates/backend/src/protocol.rs:1` — `use chrono::{DateTime, Utc}`
- `crates/backend/src/facade.rs:2311` — `.parse::<chrono::DateTime<chrono::Utc>>()`
- `crates/backend/src/snapshot.rs:151` — `captured_at: chrono::DateTime<chrono::Utc>`

`middle/Cargo.toml` 不依赖 `chrono`，于是自己写了一份历法。
**同一个仓库里现在有两套日期实现**，一套用成熟库，一套手写。

**判断**：这 173 行的存在理由完全绑定在「与 TS oracle 逐字节一致」上。
按已裁定的全栈迁移（TS 整体退役、只作行为 oracle），
需要问的是：**oracle 退役后，字节级一致还是不是要求**？

⚠ 这是产品/工程边界上的问题，不是我能单独定的：
如果 ref/cursor 的字节上限本身是**产品可见**的行为（模型会看到「截断了」），
那换渲染规则就会改变模型看到的截断点；如果它只是内部保护阈值，那就可以换成 `chrono` + 默认渲染。
**先问，再动。**

---

## 3. 同一个三车道准入算法写了四遍，串联在一条链上

这是本次梳理里最大的一处。一条世界事实从产生到进入模型上下文，要**依次穿过四个有界队列**，
四个队列的结构与字段**逐字相同**，前三个连容量都相同：

| # | 类型 | 位置 | 容量（ordinary/control/overflow） |
|---|---|---|---|
| 1 | `EventDispatchState` | `backend/src/runtime/events.rs:20-254` | 256 / 512 / 64 |
| 2 | `RuntimeEventQueue` | `backend/src/runtime/events.rs:256-472` | 256 / 512 / 64 |
| 3 | `EventBridge` | `backend/src/facade.rs:48-320` | 256 / 512 / 64 |
| 4 | `ParticipantEventQueue` | `middle/src/participant/runtime/queue.rs:102-…` | 16 / 8 / 4 |

前三份合计 **725 行**生产码，第四份所在的 `queue.rs` 另有 446 行
（第四份多带清理编排，不是纯队列）。四份之外还有各自的测试。

四份都有同一套成员：`ordinary` / `control` / `overflow` / `terminal` 四个槽，
`next_sequence`（第四份叫 `next_ticket`）/ `next_admission` / `open_loss_segment` 三个游标，
外加 `closed`；同一套方法：`enqueue`、`pop_next`、`record_overflow_loss`、`queued_counts`。

前三份 `enqueue` 的分支形状完全一致——先 terminal，再可丢事件的三段（有位置 / 并入当前丢失段 / 开新丢失段），
再控制车道，最后「取消就退出，否则 wait」：

- `facade.rs:140` ↔ `events.rs:101` ↔ `events.rs:357`（terminal）
- `facade.rs:153` ↔ `events.rs:113` ↔ `events.rs:368`（可丢）
- `facade.rs:165` ↔ `events.rs:124` ↔ `events.rs:380`（并入丢失段，条件表达式逐字相同）
- `facade.rs:190` ↔ `events.rs:147` ↔ `events.rs:407`（控制车道）

判据函数也是逐字复制：

```rust
// facade.rs:1805                          // runtime/events.rs:556
fn is_droppable_event(...) -> bool {       fn is_runtime_droppable_event(...) -> bool {
    matches!(event.kind,                       matches!(event.kind,
        Entity | Block | Sound)                    Entity | Block | Sound)
}                                          }
```

### 3.1 三层是怎么长出来的：每修一次，无界性就往上挪一层

三处的文档注释各自写明了自己存在的理由，连起来读就是一条修补链：

- `EventBridge`（`facade.rs:53`）：「公开回调有意是单线程的，而且可能在回调里阻塞」
- `RuntimeEventQueue`（`events.rs:260`）：「**旧的无界 Tokio channel** 让暂停的公开回调把上游积压撑到无界」
- `EventDispatchState`（`events.rs:67`）：「这是紧挨 runtime broker 之前的准入；**broker 背压时它不能变成无界溢出队列**」

也就是：给回调加了有界队列 → 上游 channel 变成无界积压 → 给上游加有界队列 →
再上游又变成无界积压 → 再加一层。**每一层的理由都是「我下游有界了，所以我也得有界」。**

⚠ 需要问的是根问题，而不是继续加层：**为什么事实要被推到一个可能阻塞的回调里？**
`facade.rs:644-650` 那处 `catch_unwind` 的注释已经自己说出了答案：

> 需要如实记一笔：这根线程本身**没有独立理由**——它是推送式事实流的产物，
> 不像 backend runtime 线程那样被 azalea 的 `LocalSet` 逼出来。
> **事实流改成拉取式之后，这根线程和这处 catch 应当一起消失。**

三层队列、dispatcher 线程、那处 panic 捕获，是同一个根因的四个产物。

### 3.2 稳态下只有一个生产者，而全序准入闸从不真正等待

三份 `enqueue` 开头都有同一道闸：

```rust
let sequence = state.next_sequence;
state.next_sequence = state.next_sequence.wrapping_add(1);
while !state.closed && state.next_admission != sequence {
    self.wake.wait(&mut state);          // facade.rs:133-135
}
```

这是在 mutex + condvar 上手写的**排号锁**：第 N 个生产者必须等 1..N-1 全部准入完毕。

但生产侧只有一个入口 `FacadeInner::route_event`（`facade.rs:613`），
它在生产代码里的调用点只有三处，其中两处在同一根线程上：

| 调用点 | 所在线程 |
|---|---|
| `facade.rs:1175` | 后端 runtime 线程 `mineintent-backend-runtime-{id}`（`facade.rs:812-828`） |
| `facade.rs:1230` | 同上 |
| `facade.rs:861` | 调用方线程，且**只在会话从未启动过**时到达（`admit_stop` 的 `created_idle_session` 分支，`facade.rs:584`） |
| `facade.rs:1060` | `#[cfg(test)]` |

`EventBridge::enqueue` 在生产代码中**只有一个调用点**（`facade.rs:621`），其余全部出现在测试里。

所以稳态运行时只有一个生产者线程，`next_admission` 与 `sequence` 永远同步推进，
那个 `while` 一次都不会真的 `wait`。排号机制的收益为零，成本每条事实付一次。

这与执行仲裁器那次（PR #133 前删除的 349 行）是同一形态：
TS 原型里事实经 HTTP 桥从多个方向进来，排号是必要的；Rust 换成单线程 runtime 之后并发源消失了，排号留下了。

### 3.3 「可丢事实一定被丢」这句注释不成立

`facade.rs:56-58` 写：

> Ordinary entity/block/sound facts beyond their lane are dropped

但 `enqueue` 里可丢事件有三条 `return true` 的出路，三条都不成立时会**落到 `self.wake.wait()` 阻塞生产者**
（`facade.rs:201-206`）。到达条件是：

1. `ordinary` 满（256），且
2. `open_loss_segment == None`，且
3. `overflow` 满（64）

第 2 与第 3 条看似互斥，其实可以同时成立：每次成功准入（含**控制车道**准入，`facade.rs:196`）
都会把 `open_loss_segment` 清空。于是「丢一条 → 开新段 → 来一条控制事实 → 段被清空 → 再丢一条 → 再开新段」
重复 64 次，`overflow` 就满了，而 `open_loss_segment` 恰好是 `None`。
控制车道有 512 格，装得下这 64 条。

**结论**：在消费者停摆的场景下（也就是停机场景），一条本该被丢弃的实体事实会把生产者线程挂住。
这正是 `停机不退出` 那条残余的具体形状——不是"控制车道满了才会阻塞"，可丢车道也会。

### 3.4 四层各自产生 omission 标记，`dropped_count` 四个数不可合成

`BackendEventKind::Overflow` 不在可丢集合里（`facade.rs:1805`、`events.rs:556`），
所以上游产生的 overflow 标记到了下游会占用**控制**车道，并被如实转发。

于是一次突发可以产生四个标记：第 1 层丢了 500 条出一个，第 2 层把幸存的又丢了 100 条出一个，
第 3 层再丢 30 条，第 4 层（容量只有 16/8/4）再丢一批。
模型看到多条「丢了 N 条」，而这些 N **既不是同一批事件，也无法相加**——
它们描述的是同一条流在四个不同截面上的损失，彼此重叠关系不可知。

⚠ 这直接落在我此前向维护者提出、尚未答复的那个问题上（「模型为什么要知道丢弃了多少」）。
现在这个问题更尖锐了：**这个数字在四级串联下根本没有良定义。**
产品裁定要求的是「沉默不得伪装成完整」，一个布尔量「这里有损失」就能满足；
一个不可合成的计数反而是在把未知说成已知——正撞 `产品.md` 的 **W07a**
（「不得把未知说成已知」）。

**这条我不自己动**：要不要保留计数、保留几层，是产品判断加架构判断，按 G06 走。

---

## 4. Information 子系统：约 15 100 行，控制面不在生产路径上

`crates/middle/src/information/` 生产码 **7 225 行**，配套测试
`crates/middle/tests/information_*.rs` **7 875 行**，合计约 **15 100 行**，
占整个 Rust 代码库（80 564 行）的 **19%**。

它实现了一整套通用信息查询控制面：catalog、query、selector、cursor、ref、
access policy、trace、tool session、provider registry。

**但它的控制面在生产里一次都没有被构造。**

实测（`rg`，全库，区分「模块内 / 测试 / 模块外生产码」）：

| 类型 | 模块外生产文件 |
|---|---|
| `InformationRuntime` | 无 |
| `InformationToolSession` | 无 |
| `InformationCatalogTool` | 无 |
| `InformationRegistry` | 无 |
| `InformationRefStore` | 无 |
| `InformationCursorStore` | 无 |
| `InformationAccessPolicy` | 无 |
| `ViewportInformationProvider` | 无 |
| `CurrentStatusProvider` / `InventoryProvider` / `SoundProvider` | 无 |
| `InformationContextComposer` | 无 |
| `InformationTrace` | 无 |

`InformationRuntime` 这个名字在全库只出现在 6 个文件里：
4 个在 `middle/src/information/` 内部，2 个是它自己的测试。
组合根 `crates/app/src/lib.rs` 的 `use` 列表里**没有** `mineintent_middle::information`。

模块外唯一的生产消费者是 `middle/src/participant/information_adapters.rs:23-33`，
而它只用到四样东西：

```rust
use crate::information::{
    format_utc_millis,                                        // support.rs
    geometry::{distance_between, relative_bearing, Point3},   // geometry.rs  153 行
    scope::InformationScopeSource,                            // scope.rs      80 行
    source_ports::{ /* 14 个类型 */ },                        // source_ports/ 407 行
    InformationClock, SystemInformationClock,                 // support.rs
};
```

也就是说：**活着的大约 970 行，剩下约 6 250 行生产码加 7 875 行测试没有生产消费者。**

这与 `docs/architecture.md:103-104` 记录的 TS 侧现状一致：

> Information Runtime 还实现 catalog、help、selector、cursor 和权限机制，
> 但这些通用接口目前没有作为模型工具暴露。

移植把这个状态**原样搬了过来**：连同「没有暴露」这件事一起移植了。

### 4.0 实测验证（不是推断）

把 `information/mod.rs` 换成只保留 `contracts` / `geometry` / `scope` /
`source_ports` / `support` 五项，其余 16 个文件的 `mod` 声明全部摘掉，然后编译：

```
$ cargo check -p mineintent-middle --lib
    Finished `dev` profile ... （通过，11 条 warning）

$ cargo check -p mineintent-app --bins
    Finished `dev` profile ... （通过）
```

**生产二进制 `mineintent` 在整套 Information 控制面缺席的情况下照常编译。**
摘掉的是 **5 110 行**生产码：

| 文件 | 行 | 文件 | 行 |
|---|---:|---|---:|
| `runtime.rs` | 1 295 | `providers/viewport.rs` | 474 |
| `registry.rs` | 475 | `providers/current_status.rs` | 250 |
| `ref_store.rs` | 426 | `providers/inventory.rs` | 218 |
| `context_composer.rs` | 368 | `providers/sound.rs` | 215 |
| `cursor_store.rs` | 345 | `contracts/schemas.rs` | 221 |
| `tool_session.rs` | 337 | `access_policy.rs` | 181 |
| `control.rs` | 112 | `providers/schema.rs` | 67 |
| `trace.rs` | 72 | `providers/mod.rs` | 54 |

（`contracts/v1.rs` 1 093 行没摘，因为 `scope.rs` 依赖它；实际其中大部分也只服务于被摘掉的部分。）

**这个实验还顺带证明了 §2**：摘掉之后 `middle` 冒出 11 条 `never used` 告警，
逐条正是 §2 说的那套 JS 语义重实现：

```
warning: function `javascript_number_to_string` is never used
warning: function `parse_javascript_date_millis` is never used
warning: function `days_from_civil` is never used
warning: function `clone_bounded_json` is never used
warning: struct `JavaScriptJsonFormatter` is never constructed
...（共 11 条，全部在 support.rs）
```

也就是说：**那 173 行 JavaScript 数字/日期语义的手工重实现，唯一的服务对象是
`ref_store` 与 `cursor_store`，而这两个本身不在生产路径上。**
它是服务于死代码的死代码——只是因为 `pub` 而没被编译器报出来。

（实验后已 `git checkout` 还原，仓库状态未变。）

### 4.1 一个由此产生的实际风险：无人接管的 panic

`information_adapters.rs:1-6` 的模块文档写：

> The source-port traits predate the Rust backend `Result` boundary, so failures
> are converted to **stable panics** and are expected to be **caught by the
> Information runtime/provider boundary**.

于是文件里有 `snapshot_or_panic`（`:48`）、`observation_source_or_panic`（`:55`）、
以及 `NON_FINITE_PANIC`（`:39`）。

而**那个 boundary 不在生产路径上**——`InformationRuntime` 里那 9 处 `catch_unwind`
（`runtime.rs:348/362/387/422/664/824/1107/1242`）全都不会执行。
`information_adapters.rs` 自己只有一处 `catch_unwind`，在 `:434`，
且只包住 `subscription.unsubscribe()`，不包这些取快照的路径。

⚠ 我**没有**实测这些 panic 在生产里真的会发生（需要构造 backend 返回 `Err` 的场景），
所以这条标为**需要验证**，不是已证实的缺陷。但「按注释所说的接管者不存在」这一点是确定的。

相关背景：远程分支 `origin/experiment/no-panic-catch` 与 `origin/docs/panic-audit-and-toolloop`
正是在做 panic 处置的专项，这条应当并进去看。
