# 世界信息如何到达模型：状态与事件的切分

> 无产品权威。本文记录一条工程判断的推导与依据，不替 [`产品.md`](./产品.md)
> 增加产品判断。⚠ 标记处是需要维护者裁定的。
>
> 依据文档：[原版客户端如何呈现瞬时事件](./vanilla-client-perception.md)、
> [参与者事件队列彻查](./participant-queue-audit.md)、
> [Rust 分支代码梳理](./rust-code-audit.md)、[实盘观测](./no-panic-live-run.md)。
> 施工记录见[生产者摘除](./refactor-log-producer-removal.md)。

## 0. 一句话

世界长什么样**以状态到达**（拉取，最新赢）；只有**在状态里不留痕**的瞬时事才走
事件通道。azalea 已经按这条切好了，我们此前把它切回去了。

## 1. 三个视角

### 1.1 模型的角度：它是**间歇在场**的

这是模型与玩家唯一的结构性差别，也是全部问题的来源。

模型一轮几秒（真实 LLM 延迟），轮与轮之间它**不存在**。它读的是一个瞬间的快照，
无法消费流——给它一串「发生过的事」，它得自己在脑子里重建状态，比直接给状态既费力
又容易错。

所以它需要两类信息，性质**完全不同**：

| | 性质 | 丢了会怎样 |
|---|---|---|
| **现在为真的** | 拉取、幂等、最新赢 | 没关系，下次再看 |
| **期间发生过且不留痕的** | 累积、有序 | 丢了就是丢了 |

判据不是「重要不重要」，是**「它在状态里留没留下痕迹」**。

### 1.2 客户端的角度：原版**不缓冲**

能力全景那份文档自己的「形成链」列已经做了这个区分：`maintained-model` / `event` /
`derived` / `local`。

原版的循环是：收包 → 更新世界模型 → 每 tick 在**当前状态**上行动。**它没有待处理
世界事实的队列。** 玩家 alt-tab 时错过的那声爆炸，没有任何东西替他存着。

反汇编 26.1.2 客户端确认（详见[原版呈现](./vanilla-client-perception.md)）：受伤这条
最典型——`health` 是状态、`hurtTime` 是状态上的倒计时、`lastHurtByMob` 是被维护的
引用、`"Player hurts"` 走字幕。**四条并存，没有一条是队列。**

**原版的队列深度是 0。**

### 1.3 我们的角度：差的只有**在场性**

玩家连续在场，模型间歇在场。事件通道唯一的存在理由是：**我思考的时候世界发生了
什么。**

而 **W02** 要求「AI 对世界的认识应来自它在当前游戏条件下**能够获得的**观察」，
**A06** 说「AI 在游戏内正常获得的信息……包括视口中的世界信息」。一个在场的玩家会
听到那声爆炸。所以补的是**在场性**，不是完整性。

## 2. 判据

**必须以事件到达 =「状态里不留痕」∧「在场玩家会感知到」**

| | 状态里留痕吗 | 在场玩家会感知吗 | 结论 |
|---|---|---|---|
| 玩家说话 | 否 | 是 | **事件** |
| 受伤 / 死亡 | `status` 只有当前值，扣血本身不留痕 | 是 | **事件** |
| 声音 | 否 | 是 | **事件** |
| 有人进出世界 | playerlist 是状态，「进来了」不留痕 | 是 | **事件** |
| 断线 / 重连 / 维度切换 | 状态会变，**原因**不留痕 | 是 | **事件**（W07 要求如实说明介入原因） |
| 实体移动 | **是**——视口里就是当前位置 | — | 状态 |
| 方块变化 | **是**——视口读到的是变化之后 | — | 状态 |
| 区块卸载 | **是**——视口读到 `Unloaded` | — | 状态 |
| 服务端位置回拉 | **是**——`pose` 就是应用之后的当前值 | 否 | 状态 |
| 快照修订号 | — | 否 | 都不是，纯内部 |

对得上 TypeScript 原型：整份 `runtime.ts` 只有两处 `#pushPending`——
`participant.started` 与 `self.health.dropped`，**正是「不留痕」那一类**。

## 3. azalea 已经做了这个切分

工作树：`~/.cargo/git/checkouts/azalea-*/d0cc847`。

### 3.1 `Event` 枚举就是「不留痕」那一类

`azalea/src/events.rs` 的 12 个变体：

```
Init  Login  Spawn  Chat  Tick  Packet(feature-gated)
AddPlayer  RemovePlayer  UpdatePlayer  Death  KeepAlive
Disconnect  ConnectionFailed
```

**实体移动、方块变化、声音一个都不在里面。** 它们被插件应用进 ECS 世界状态，调用方
去查询。这正是原版的 `maintained-model` vs `event`。

### 3.2 投递是无界 + 逐条 + 只警告

`azalea/src/swarm/mod.rs` 的 `event_copying_task`：

```rust
while let Some(event) = rx.recv().await {
    if rx.len() > 1_000   { warn!("...consider disabling the `packet-event` feature") }
    if rx.len() > 10_000  { warn!(...) }
    if rx.len() > 100_000 { warn!(...) }
    if rx.len() > 1_000_000 { warn!("your code is almost certainly leaking memory") }
```

`mpsc::unbounded_channel`，从不丢弃、从不阻塞。**它给的处方是「别产生这些事件」，
不是把队列做聪明。**

### 3.3 状态那条路我们本来就走对了

`refresh_snapshot` 用的是 `capture(bot, …)` 与
`capture_tracked_entities_for_epoch(bot, epoch)`——**直接 `ecs.query::<(Entity,
MinecraftEntityId, LoadedBy, Position, Physics, LookDirection, …)>()`**。视口从来
没用过事件流。

## 4. 我们此前把切分切了回去

`packet-event` 我们没开（`default-features = false, features = ["log"]`）。但自建了
三个 ECS 级生产者插件，把 azalea **有意不作为事件暴露**的状态变化重新变回事件：

| 插件 | 产出 |
|---|---|
| `EntityProducerPlugin` 的 `produce_entity_packet_events` | 实体事件 |
| `BlockSoundProducerPlugin` | 方块 + 区块事件 |
| `ServerPositionCorrectionPlugin` 的 `record_server_position_corrections` | 位置回拉 |

2026-08-06 实盘两分钟摄入：

```
entity=59945   block=593   player_chat=2   player_list=7
```

按 §2 的判据该进事件通道的是 **9 条**，占 **0.015%**。

然后为了扛住自己造的洪水，建了四层有界队列、手写排号锁、三条车道、溢出标记、
omission 计数——合计上千行（详见[队列彻查](./participant-queue-audit.md)）。

**队列不是多余，是我们先制造了它要解决的问题。**

### 4.1 一条实测的具体代价

第四层的车道划分是「有 wake 或 `scope_control` 走 control(8)，否则走 ordinary(16)」，
而 `scope_control` 只含 Lifecycle 与 Overflow。所以**没点名的玩家聊天和实体流水抢
同一条 16 格车道**。实盘确实观察到 `event_type=player_chat` 被丢，而聊天不可重建，
撞 **W08b**。

## 5. 已定：事件通道取「丙」

模型间歇在场带来一个原版没有的问题：**思考期间发生的事，按原版语义已经过期了，
给不给？**

具体场景：模型思考 8 秒，期间有 3 条聊天 + 一声爆炸。字幕的 3 秒窗口早已过期。

三种形态：

| | 语义 | 代价 |
|---|---|---|
| 甲 | 折叠视图（照抄原版）：此刻该知道什么 | 思考期间发生的事静默过期，撞 W02 |
| 乙 | 轮间累积：自你上次醒来以来发生了这些 | 窗口长度由模型延迟决定，无硬上界 |
| **丙** | **两者都给**：折叠视图（现在）+ 轮间摘要（错过的） | 两套呈现 |

**维护者裁定：丙。**

依据也在原版：聊天本来就是这个形状——近处一瞥（有限、会淡出）+ 打开回滚（全量
100 条），**两个通道，不是同一队列调深浅**。这直接对应 W08b。

## 6. 未定

1. **⚠ 声音的呈现窗口。** 原版字幕是 3 秒 + 按文本归并 + 走远即消，那建立在玩家
   连续在场上。丙对声音的投影是「此刻可听 + 错过了什么」，但「错过了什么」的窗口
   多长、要不要保留归并，需要裁定。**维护者已明确此项暂缓。**
2. **⚠ 聊天历史工具（W08b）。** 原版是 100 条上界 + `source` 三态 + 不带时间戳 +
   按需回看。我们当前 6 个工具里没有它，聊天因此被塞进事件通道——而事件通道会丢它。
   新增第 7 个工具是产品决定。
3. **sound event → subtitle 键的映射表**要不要入 `supplies/`。字幕文本（926 段）
   azalea 已经带了，缺的只是这张 1902 → 998 的映射，需按资产索引单取。
4. **事件呈现给模型时的顺序/时间戳**。原版不显示时间戳，顺序是列表顺序。

## 7. 与既有文档的关系

- [原版客户端如何呈现瞬时事件](./vanilla-client-perception.md) —— §1.2 与 §6.1 的
  事实来源，全部由反汇编 26.1.2 client.jar 取得
- [参与者事件队列彻查](./participant-queue-audit.md) —— §4 与 §4.1 的事实来源
- [Rust 分支代码梳理](./rust-code-audit.md) —— §3 描述了四层队列如何逐层长出来
- [实盘观测](./no-panic-live-run.md) —— §4 的摄入计数与 player_chat 被丢的取证
- [生产者摘除施工记录](./refactor-log-producer-removal.md) —— 本文结论的执行
