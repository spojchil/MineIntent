# 参与者事件队列：彻查

> 分支：`refactor/panic-supervision`。无产品权威——本文只记录代码事实与由此导出的
> 工程判断，不替 [`产品.md`](./产品.md) 增加产品判断。
>
> 对象：`crates/middle/src/participant/runtime/queue.rs`（446 行）、
> `ingest.rs`（160 行）、`mod.rs` 的准入与 worker 路径。测试
> `queue_admission.rs` 958 行。
>
> 每条结论都给了可复核的位置。含 ★ 的两条我认为必须处理。

## 0. 结构

四条 lane，容量在 `mod.rs:67-69`：

| lane | 容量 | 进什么 |
|---|---:|---|
| `ordinary` | 16 | 其余一切 |
| `control` | 8 | 有 `wake` 的，或 `scope_control`（= Lifecycle / Overflow） |
| `overflow` | 4 | 丢弃标记，队列自己生成 |
| `terminal` | 1 | 终态 |

`pop_next`（`queue.rs:354`）在四条 lane 的队首里选 **ticket 最小的那个**。

## 1. 队列里没有消息，只有票据

`WorkItem`（`queue.rs:41-54`）的全部字段：

```rust
ticket, ordinal, generation, scope, occurred_at,
event_id, event_type,          // ← 只有类型字符串
wake, scope_control, terminal, terminal_lifecycle, overflow
```

**没有载荷字段。** 事件内容在准入那一刻就分道了（`mod.rs:439-445`）：

- 会成为事实的 → **当场同步** `record_fact()` 进事实环，不经队列
- 其余 → 直接丢掉

所以队列里排的是「某时刻某作用域发生了一件 X 类型的事」，模型永远看不到它。

对照：原版客户端的单位是 tick——收包、应用到世界状态、每 tick 在**当前状态**上
行动。状态天然合并，一个 tick 里 20 次位置更新留下的是一个位置。我们把本该合并成
状态的东西，拆成了一串必须逐条准入、排序、落盘、弹出的票据。

## 2. 对绝大多数条目，票据的唯一效果是加一个计数

`journal_type_for`（`ingest.rs:144`）只在四种情况返回 `Some`：有 `wake`、`terminal`、
`scope_control`、`overflow`。其余返回 `None`，于是 `append_event_journal`
（`mod.rs:1499-1506`）走这条：

```rust
let Some(journal_type) = journal_type_for(item) else {
    self.ingest_counters.record(&item.event_type);
    return Ok(());
};
```

**加一个计数，然后丢弃。**

这包括**没点名的玩家聊天**——它是事实，但事实在准入时就已经记进环里了，它的票据
只用来加计数。`SnapshotChanged` 同理：它的载荷只有 `group` + `snapshot_revision`
（`event.rs:425`），本身就是「状态变到第 N 版」的通知，占一个槽只为加一个计数。

## 3. ★ 控制车道满会阻塞生产者，并闭合一个四节点环

### 3.1 只有控制车道会阻塞

`enqueue`（`queue.rs:198-279`）的分支结构：

- `terminal` → return
- `wake.is_some() || scope_control` → control 有位就 return；**满了就落到 281 行的
  `wake.wait()`**
- `ordinary` 有位 → return
- `ordinary` 满 → 三条出路（并进当前丢失段 / 开新标记 / 并进最新标记），**全都 return**

第三条出路（`queue.rs:269`）是 2026-08-06 加的：`overflow.len() >= 4` 时
`back_mut()` 必为 `Some`，所以可丢事实**永远不会**再阻塞生产者。

**结论：`queue.rs:281` 的阻塞只能由控制车道满触发。**

### 3.2 环

被阻塞的是 backend dispatcher 线程（`facade.rs:1250` 的 `dispatcher_loop`，
生产里唯一的入队者）：

```
worker（tokio 任务）
   await CommandCompletion                          ← 等 move_input 结算
        ↑ 结算者是 ↓
backend-runtime 线程（azalea 的 LocalSet）
   handle_client(bot, Event::Tick, state)           ← driver.rs:103 起
     ├ process_pending_commands → handle_command → finish_command   ← 结算在这
     └ emit_snapshot → route_event → EventBridge::enqueue           ← 阻塞在这
        ↑ 桥腾空要靠 ↓
backend-dispatcher 线程
   卡在 handle_event → ingest_backend_event → queue.rs:281
                                              ← 控制车道满(8)
        ↑ 控制车道腾空要靠 ↓
worker 弹出 —— 而 worker 正在第一步等
```

`handle_client` 一次处理一个事件。它一旦卡在发事件那半，**下一个 tick 的
`process_pending_commands` 就永远不会执行**，队列里的命令再也结算不了。

这正是 2026-08-05 实盘用 cdb 抓到的那个环（见 `c49cfd7` 提交说明）。那一笔修的是
**第一条边**——`release_all` 改为投递即返回，tokio worker 不再阻塞 OS 线程。
**其余三条边原样保留**：worker 的 `await` 同样闭合这个环，只是不占线程。

### 3.3 触发条件

控制车道（8 格）在一次模型轮次期间填满。填它的只有两类：点名的聊天与 Lifecycle。
一次真实模型轮次要几秒，期间 worker 完全不弹出（`process_wake` 在 await 模型）。
玩家连发 8 条 `@Bot …` 就够。

## 4. ★ 两道界限管同一件事，而淘汰策略相反

同一批「待送给模型的事实」被两处界住：

| | 位置 | 容量 | 满了丢谁 |
|---|---|---:|---|
| 队列 ordinary 车道 | `queue.rs:214` | 16 | **丢新来的**，记 omission |
| 事实环 | `ports.rs:449` | 20 | **丢最老的**，记 omission |

事实环的设计意图是「保留最近的 20 条」。而队列在它前面，一满就把**新的**挡在外面。
于是压力下模型看到的是**旧事实**，新的进不来——**恰好与环的意图相反**。

两处都往同一个 `state.omitted` 计数上加（`ports.rs:451` 与 `ports.rs:466`），所以
模型看到的那个数字混了两种成因。

## 5. 车道不是优先级，只是容量池

`pop_next` 取全局最小 ticket。所以一条点名的聊天如果排在 16 条普通条目之后，仍然要
等那 16 条先被弹出、逐条走完 journal。控制车道的唯一好处是**有自己的 8 格，不会因为
ordinary 满而被丢**——不是更快到达。

## 6. 排号闸在生产中一次都不会等待

`queue.rs:153`：

```rust
while !state.closed && state.next_admission != ticket { ... self.wake.wait(state) ... }
```

这是在 mutex + condvar 上手写的排号锁：第 N 个生产者要等 1..N-1 全部准入完毕。

而生产里唯一的入队者是 `dispatcher_loop` 那一根线程的 `while let`
（`facade.rs:1250-1257`）。`emit_internal` / `ingest_event` 是公开 API，但**生产代码
里没有调用者**，只有测试。

单生产者 ⇒ `ticket` 与 `next_admission` 永远同步推进 ⇒ 那个 `while` 一次都不会
`wait`。收益为零，成本每条事实付一次（两次加解锁 + 两次条件判断 + 释放/重取
admission serial）。

与 `rust-code-audit.md` §3.2 对上游三层的结论同形，也与执行仲裁器那次（349 行，
`2d948f3` 删除）同形：并发源在移植时消失了，排号留了下来。

## 7. `open_loss_segment` 被任何一次成功准入清空

`queue.rs:210` 与 `216` —— 控制车道准入也清空。于是「丢一条 → 开新标记 → 来一条
控制事实 → 段被清空 → 再丢一条 → 再开新标记」可以在 4 次内耗尽标记位，之后走
§3.1 的第三条出路（并进最新标记，丢失位置精度）。

`rust-code-audit.md` §3.3 对第三层描述过同一形态，那里的后果是阻塞；这里因为有第三
条出路，后果降级为位置精度损失。

## 8. 第二条 terminal 被静默丢弃

`queue.rs:198-205`：

```rust
if item.terminal {
    if state.terminal.is_none() { state.terminal = Some(item); }
    // else：item 被丢，没有任何记录
    self.commit_admission(&mut state);
    return Ok(QueueAdmission::Accepted);   // ← 仍然报 Accepted
}
```

调用方拿到 `Accepted`，而那条终态事件不存在了。目前 terminal 只由生命周期事件产生，
两条终态同时到达是否可能，我没有验证。

## 9. 溢出标记是 journal 的最大单一来源

`ingest.rs:152-156` 的注释自己记着：

> 实测一次 100 秒运行有 **2,928 条**，仍是本文件最大的单一来源。

100 秒 2928 条落盘，记的是「我丢了东西」，而丢的东西按 §2 大多本来就只会加一个计数。

## 10. 已经做掉的一件

实体与方块不再入队（`backend_event_enters_queue`，`support.rs:258`）。它们在队列里
两条出路都是死的（不成为事实、不能唤醒），却在和**没点名的玩家聊天**抢同一条 16 格
车道，而实盘两分钟摄入 59945 条实体事件。回归测试
`entity_traffic_does_not_take_slots_from_unaddressed_player_chat`。

按同一判据，还有三类同样是「两条出路都是死的」却仍在占 ordinary 车道：
`SnapshotChanged`（`Event::Tick` 每 5 tick 一次 = 4/s，同伴移动时必然变化，
5 秒模型轮次即 20 条 > 16 格）、`SelfState`、`PlayerList::Update`。

## 11. 我没有验证的

1. 控制车道填满的真实频率。§3.3 的路径是从代码推出的，2026-08-05 的实盘环是
   `release_all` 那条边触发的，不是控制车道。
2. 两条终态事件能否同时到达（§8）。
3. `waiting_producers` / `wait_for_waiters` 只在测试里用到，生产语义我没有追。
4. 上游三层队列（`EventDispatchState`、`RuntimeEventQueue`、`EventBridge`）本次没有
   重读，沿用 `rust-code-audit.md` §3 的结论。

## 12. 建议的处置顺序

**第一梯队——机械，且各自独立**

1. 把剩下三类（`SnapshotChanged` / `SelfState` / `PlayerList::Update`）也挡在入队之外，
   并把 `backend_event_enters_queue` 改成正面表述（「能影响模型的才入队」），使新增
   载荷类型默认不入队。
2. 删掉排号闸（§6）。单生产者已由代码确认，删掉是纯减法。若担心将来多生产者，
   改为 `debug_assert` 生产者线程唯一。
3. §8 的第二条 terminal：要么记录，要么明确返回 `Ignored`。

**第二梯队——需要一次判断**

4. §4 两道界限：删掉队列这道，只留事实环。理由是环的策略（保留最近）符合「模型该看
   最新发生的事」，而队列这道方向相反且更紧。删掉之后 `record_pending_omission` 的
   两个成因也合一了。

**第三梯队——需要维护者裁定**

5. §3 的环。彻底解法是事实流改拉取式，那样 dispatcher 线程与前三层队列一起消失
   （`facade.rs` 自己的注释已经指向这个方向）。临时解法是控制车道也永不阻塞——但那
   要决定「点名的聊天满了之后丢哪一条」，是产品判断。
6. §1 的根问题：**世界状态该以状态到达，还是以事件流到达。** 队列层数、omission 计数、
   dispatcher 线程都是它的后果，不是独立问题。
