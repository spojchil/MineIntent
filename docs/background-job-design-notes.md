# 后台 job 模型：讨论记录与前置调查

> 无产品权威。本文记录 2026-08-07 一次设计讨论的内容、为它做的前置调查，
> 以及**哪些已定、哪些没定**。不替 [`产品.md`](./产品.md) 增加产品判断。
>
> 写下来的理由：这些结论此前只存在于对话里，压缩上下文就会丢。

## 1. 维护者提出的设计

原话要点：

- **所有持续动作**（移动、转头等）改为**后台 job** 的形式；
- 模型给的**工具数组要在 1 tick 内处理完**（"这是我的理想情况，到时看实际计算看看"）；
- 处理完就返回**这一 tick 内的帧事件 + 后台 job 信息**；
- 帧事件包含**声音**；声音淡出先看 azalea 有没有，没有就我们维护 **60 tick 滑动窗口**；
- **聊天**同理，azalea 有现成的就用，没有就自己维护；
- **事件本身可以在模型空闲时推送**；如果模型正在调工具，就附加到轮末帧。

思路来源是 coding agent（Codex / Claude Code / opencode）的后台进程——
它们同样活过若干轮工具调用。

## 2. 前置调查：azalea 给了什么

### 2.1 声音：什么都不给

```rust
// azalea-client/src/plugins/packet/game/mod.rs
pub fn sound(&mut self, _p: &ClientboundSound) {}          // :1025
pub fn sound_entity(&mut self, _p: &ClientboundSoundEntity) {}  // :1586
pub fn stop_sound(&mut self, _p: &ClientboundStopSound) {}      // :1587
```

三个空函数体、参数带下划线——**包收下就丢**。声音只能我们自己维护。

**60 tick 与原版对上了**：反汇编 26.1.2 得到聊天淡出常量
`private static final int TIME_BEFORE_MESSAGE_DELETION = 60`（= 3 秒），
声音字幕时限同样是 3 秒（见`docs/vanilla-client-perception.md`（在 `refactor/panic-supervision` 分支））。

但原版比「滑动窗口」多两条，抄的时候别丢：

- **按文本归并**——40 次脚步显示 1 条，不是 40 条
- **逐帧重判可听**——走远立刻消失，不等满 3 秒

### 2.2 聊天：只给事件，不存历史

`ChatReceivedEvent { entity, packet }`（`azalea-client/src/plugins/chat/mod.rs:200`）
是一次性 Message，没有任何累积；`last_seen_messages` 在 `chat/handler.rs:77` 还是
`// TODO: implement` + `default()`。也得我们自己维护。

注意原版这里是**两个通道**：overlay 60 tick 淡出 + 打开聊天回滚**全量 100 条**。
后者对应 W08b，是还没定的第 7 个工具（`docs/world-information-design.md`，在 `refactor/panic-supervision` 分支）。

### 2.3 死锁：是 azalea 的缺陷，上游未修

见[实盘观测](./no-panic-live-run.md) §2。结论：
`swarm/builder.rs:555` 取 ECS 写锁，`:560` / `:580` 在该作用域内调 `username()`
取同一把 `parking_lot::RwLock` 的读锁——不可重入，当场自锁。上游 `6249c29` 一字未改。

**这条独立于 job 模型，可以先修。**

## 3. 前人怎么做的

### 3.1 opencode

`packages/core/src/background-job.ts`：

```ts
type Status = "running" | "completed" | "error" | "cancelled"
type Info   = { id, type, title?, status, started_at, completed_at?, output?, error?, metadata? }
Interface   = { start, extend, wait, cancel, waitForPromotion }
```

`packages/opencode/src/tool/task.ts:26-33,60` 的提示词：
「background=true 异步启动、**立即返回**」「完成时**会自动通知你**」
「**不要 sleep、不要轮询进度、不要问它状态**」。

**但 opencode 的 `shell` 工具没有后台模式**——只有超时然后 kill
（`tool/shell.ts:550-564`）。它的切分是：**shell 同步、子代理异步**。

### 3.2 Claude Code

Bash 工具契约原话：

> `run_in_background` runs the command detached: it keeps running across turns and
> **re-invokes you when it exits**.

Monitor 工具契约原话：

> Events arrive on their own schedule and **are not replies from the user**,
> even if one lands while you're waiting for the user to answer a question.

完成通知带 `[SYSTEM NOTIFICATION - NOT USER INPUT]` 抬头，明确写「不要把它当成用户的确认」。

**两种投递模式都有**：空闲时 re-invoke，忙碌时插在工具调用之间到达。
与维护者提的「空闲推送 / 忙时附帧」是同一形状。

### 3.3 可迁移的两个细节

- **`extend`**——往运行中的 job 追加上下文，而不是取消重来。
  映射到同伴：「继续走，但左转」不该是 stop + start，否则身体上会看到顿挫。
- **明令禁止轮询**。在 MineIntent 这边理由更硬：一轮只有 16 次模型请求
  （`toolloop/src/run.rs:10`），模型若反复查 job 状态会烧光预算。
  **所以不该有「查 job」这个工具**——job 状态搭帧的车来，没有别的问法。

## 4. 讨论中厘清的几件事

### 4.1 通道只有两条，不是三条，而且都已经有了

维护者的原话：「本质就是，模型问我们的返回和我们主动的发送。这就是事件。」

我此前分成三条（问答 / job 汇报 / 拉状态）是多余的。就两条：**拉**与**推**，都存在。

### 4.2 推的投递粒度比「轮末帧」更细

`observe_after`（`middle/src/participant/production.rs:758`）在**每一次工具调用之后**
都 `owner.drain(&self.scope, self.generation)` 排空事实并组装整帧。
`observationAfter` 那道按资源分类的闸已经拆掉（见 PR 前的重构），现在每个工具都走。

`drain_pending_facts`（`participant/runtime/mod.rs:1300`）是轮首那一次，
与轮内每次工具调用的排空是同一个事实环的两个取用点。

**所以「附加轮末帧」这件事不必新建**：轮内事件已经是每次工具返回时投递。
「轮末帧」是另一样东西（动过身体才追加的视口帧）。

### 4.3 job 模型会顺手拆掉那个四节点环

worker 不再 `await` 命令结算，队列彻查 §3.2（`docs/participant-queue-audit.md`，在 `refactor/panic-supervision` 分支） 那个环的
第一条边就没了。这是白拿的。

### 4.4 `view` 不该变成 job

Claude Code：`Read` 同步且快，`Bash(run_in_background)` 异步。
opencode：`shell` 同步带超时，`task` 异步。
**没有一个把「读」做成异步**——调用方要的就是当下的答案。

映射：`view` = `Read`（同步），`move_input` / `look_relative` = 后台（异步）。

推论：动作工具变 job 之后只是入队、必然很快，**「1 tick」的瓶颈自始至终只有 `view`**。
`visible_blocks` 最坏扫 65×65×41 ≈ 17 万体素、每个体素还要射线
（见[代码梳理](./rust-code-audit.md) §8.3）。**要定这个目标，先测它。**

而且更准的约束可能是「**工具派发不阻塞 tick 循环**」而不是「工具数组在 1 tick 内做完」——
前者可测可守；后者可能被 `view` 一个人卡死。⚠ 未定。

## 5. 自身受伤：现在一处都没有

- `ProtocolEntityEvent::Hurt`（`contracts/minecraft/event.rs:261`）存在，
  但实体事件**明确排除自己**：
  `is_admitted_non_local_entity(local, target) = local != target`（`backend/runtime/producers.rs:367`）。
  所以它是「**别人**被打了」。
- `ProtocolSelfEvent`（`event.rs:366`）**只有一个变体** `ServerPositionCorrection`，不带血量。
- 自身伤害目前只以 `status.health` 存在——**状态**。而扣血本身不留痕。

这正是移植时丢的东西：TS 原型整份只有两处 `#pushPending`，
一处 `participant.started`，另一处就是 `self.health.dropped`。

维护者确认这块「本来就打算是事件，受伤事件，打算后来逐步完善」。

**补的时候要避开两颗雷**：

1. **别走 `Entity`**——`backend_event_enters_queue`（panic-supervision 分支）
   已经把 `Entity(_)` 整个挡在队列外。自身受伤应走 `SelfState` 或新变体。
2. **`SelfState` 目前在 `backend_event_is_fact` 里是 `false`**，届时要一起改。
   另外队列彻查 §12.1（同上分支） 提议把 `SelfState` 也挡在队列外，
   理由是「目前只有 `ServerPositionCorrection`」——这个理由**自身受伤一进来就不成立**。
   那条建议若采纳，应写成**按变体判**而非按 payload 类型判，否则补受伤时要回来拆。

## 6. 「空闲推送」需要补一个判据

「空闲时推送」= 空闲时唤醒。而现在唤醒只认被点名的聊天
（`evaluate_backend_wake`，`participant/runtime/mod.rs:996-1017`）。

规则一放开，就出现两个**正交**的分类，而现在只有第一个：

| | 判据 | 现状 |
|---|---|---|
| **算不算事件**（进帧） | 「状态里不留痕 ∧ 在场玩家会感知到」 | 已定（`docs/world-information-design.md`，在 `refactor/panic-supervision` 分支） |
| **要不要唤醒**（空闲时开一轮） | **没有** | 只有被点名的聊天 |

**是事件 ≠ 该唤醒。** 没点名的聊天是事件（在场玩家听得见），但不该开一轮；
声音、别人进出世界、别的实体受伤，都是事件，都不该唤醒。

不补这个判据，繁忙世界里同伴会被唤醒到停不下来，每一轮还烧一份模型预算
（每 run 16 次请求、180 秒期限，`toolloop/src/run.rs:10-12`）。

**还有一条更麻烦的：自激。** 同伴自己走路产生事件 → 唤醒自己 → 再走 → 再唤醒。
判据里得有「自己造成的不算」，或者用现成的事实来源标签
（`commanded` / `client_predicted` / `server_observed`，见 `backend/README.md`）来挡。

### 6.1 我建议的起点（⚠ 未采纳，供裁定）

唤醒判据起得**很窄**，宁可以后再放：

- **疼**（自身受伤 / 死亡）
- **被点名**（现有的）
- 其余一概只进帧、不唤醒

这样「走进岩浆」有人管，「隔壁有人挖矿」不会把同伴吵起来。以后放宽是加条目，不是改结构。

而且这么定的话 **Q08 可以继续挂着**——同伴不会因为没人说话就自己活动起来，
它只是疼了会叫。「疼了能不能出声」和「没人时活不活动」是两个问题，可以分开定。

## 7. 尚未决定的

1. ⚠ **「1 tick」是不是正确的目标**，还是应改成「不阻塞 tick 循环」。**先测 `view`**。
2. ⚠ **唤醒判据**（§6）。
3. ⚠ **job 活过模型这一轮意味着什么**：身体在「没有人在家」时继续动。
   W03 说「动作必须通过实际游戏过程发生，并根据实际结果判断成功或失败」，
   而一个在模型缺席期间跑完的 job，结果由谁判断？下一轮读到的是既成事实。
4. ⚠ **job 该报什么**。opencode 的 `Info` 给了骨架，同伴还要两样：
   **到目前为止的实测效果**（走了多远、转了多少度，现在 `move_input` 阻塞完返回的就是它，不能丢），
   以及**为什么结束**（完成 / 被世界挡住 / 因作用域失效被取消 / 被模型停掉）。
   压成一个 `completed` 就是把未知说成已知，撞 **W07a**。
5. ⚠ **声音的归并与可听性重判**要不要跟着原版做（§2.1）。
   注：声音的**呈现窗口**维护者已明确暂缓。
