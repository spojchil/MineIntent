# 删掉全部 panic 捕获之后的实盘观测

> 分支：`experiment/no-panic-live`（= Rust 树尖 `refactor/collapse-concurrency`
> + 云端实验 `bf32aac` + 本次删掉最后 8 处）。
>
> 目的：维护者的判断是「我怀疑我们的异常捕获捕获的不是异常，而是错误」。
> 本文只记录实盘观测到的事实，以及这些事实支持什么、不支持什么。

## 0. 装置

| 项 | 值 |
|---|---|
| 服务端 | Paper 26.1.2-74（`fill.papermc.io` 官方构建，sha256 校验通过），本机 25565 |
| 模型 | **真模型** DeepSeek `deepseek-v4-flash` @ `/responses` |
| 二进制 | `target/debug/mineintent`（debug 构建，保留符号与 PDB，panic 钩子带 `Backtrace::force_capture`） |
| 生产码 `catch_unwind` | **0 处**（测试文件里仍有断言式用法，属正当） |
| 调试器 | Windows SDK `cdb.exe`，`~*k` 抓全部线程栈 |

四轮实盘，产物在 `server-run/nopanic/run{,2,3,4}/`（不入库）。

## 1. 结论先写：**一次 panic 都没有**

| # | 场景 | panic | 结果 |
|---|---|---|---|
| run | 300 s 正常运行 + 假人聊天 + 计划停机 | **0** | 停机**挂死** |
| run2 | 70 s 正常运行 + 假人聊天 + 计划停机 | **0** | 停机**挂死** |
| run3 | 运行中**硬杀服务端**，制造断线 + 重连风暴 | **0** | 优雅降级，转 `Err` |
| run4 | 运行中 **RCON `/kill`**，制造死亡 | **0** | 死亡事实入账；停机**挂死** |

模型完成了多轮真实工具调用（`view` / `say` / `move_input`），
同伴在游戏里确实走动了（z=7.5 → 8.97），也确实说了话。
**这些路径上没有任何东西 panic。**

## 2. 真正观察到的失败形态：**挂死，不是崩溃**

三个跑到计划停机的运行（run / run2 / run4），**三个全部挂在停机上**：
打印完 `开始停机` 与 `lifecycle=Stopped` 之后，`已停止。` 那一行永远不出现，
进程继续存活（观测到最长 9 分 44 秒仍在）。

`cdb` 抓栈：**20~21 根线程全部 `Wait`，零运行**。run2 与 run4 的栈**逐帧相同**：

```text
mineintent_backend::runtime::driver::run_with_handle          ← MineIntent
 └ azalea::swarm::builder::start
    └ start_with_opts::async_fn$0
       └ tokio::task::local::LocalSet::tick                   ← 单线程
          └ azalea 自己的 async_block$2
             └ azalea::client_impl::Client::username()
                └ Client::profile → component::<GameProfileComponent>
                   └ lock_api::RwLock<bevy_ecs::World>::read()
                      └ parking_lot::raw_rwlock::lock_shared_slow   ← 永久阻塞
```

**成因**：azalea 的 swarm 收尾代码在 `LocalSet` 的一个任务里**同步**阻塞于
ECS World 的读锁；而这把锁只能由**同一个 `LocalSet` 上**的 ECS runner 任务释放。
`LocalSet` 是单线程的，被阻塞的任务不让出线程，ECS runner 就永远轮不到。
这是单线程执行器上的经典自锁。

旁证：run2 停机时另有一条 bevy 报错
`Unable to send event AppExit — Event must be added to the app with add_event()`。
但它**只在 run2 出现**（run 没有），所以它是同一场竞速的另一个症状，不是成因。

### 2.1 这个挂死**不是**删捕获造成的

对照此前带着捕获跑的三轮（`server-run/live-0806{,b,c}`）：

| 运行 | 捕获 | 结果 |
|---|---|---|
| live-0806 | **有** | 挂 |
| live-0806b | **有** | 挂 |
| live-0806c | **有** | 干净退出 |
| run / run2 / run4 | **无** | 挂 |

带着捕获也挂过两次。**删捕获既不是成因，也修不了它**——
`catch_unwind` 只能接住 unwind，接不住"永远不返回"。

诚实地说：3/3（无捕获）对 2/3（有捕获）这个样本量**不足以**说明删捕获让它更容易发生。
我不作这个断言。

## 3. 顺带观测到的三件事

### 3.1 断线时，运行时把"你断线了"说成"移动没有效果"

run3 硬杀服务端后，日志里运行时是**知道**的：

```text
ERROR azalea_client::plugins::connection: Error reading packet: ConnectionReset
[failure] source=BodyRelease code=body_motor_unavailable summary=body motor unavailable during release
WARN  azalea_client::plugins::join: failed to create connection: os error 10061   （反复重连）
```

但模型收到的是"移动没有效果"，于是它这样推理：

> The movement had no effect - I remained at z=9.3. …
> The movement has no effect now. **I'm perhaps stuck or blocked by something.**

同伴以为自己被什么挡住了，实际上它已经不在这个世界里了。

这撞 `产品.md` 的 **W07a**（「不得把未知说成已知」）与 **W07**
（「运行时可以阻止……但必须**如实说明介入原因**」）。
⚠ 属产品判断，我不自己改。

### 3.2 死亡瞬间：事实进来了，但一秒内出了 13 条遗漏标记

run4 用 RCON `/kill` 打死同伴。journal 全部 20 条：

| 条数 | 类型 |
|---:|---|
| 15 | `participant_events_omitted` |
| 4 | `overflow` |
| 1 | `lifecycle` |

时间线：

```text
01:09:19.696Z  lifecycle                      ← 死亡事实，进来了
01:09:19.837Z  participant_events_omitted
01:09:20.699Z  participant_events_omitted     ← 此后 1 秒内 12 条
   …           droppedTypes 全部是 ["block"]
```

**好消息**：死亡事实没被挤掉——此前的事实通道收紧起作用了。
**坏消息**：死亡引发的区块重载风暴让 participant 队列（16/8/4）瞬间饱和，
一秒里产出 13 条「我丢了 N 条 block」。journal 里 19/20 条是「我丢了东西」。

这正是[代码梳理](./rust-code-audit.md) §3.4 说的：四级串联下 `dropped_count` 无良定义。

另外：所有条目 `wake: null`——同伴**没有因自己的死亡醒来**，也没调用 `respawn`。

### 3.3 玩家聊天被当成"可重建事实"丢弃

run/run2/run3 都出现过：

```text
WARN mineintent_middle: 可重建事实被丢弃（队列饱和）；模型本轮会看到 omission 标记
     omitted=1 event_type=player_chat
```

**聊天不可重建。** 丢了就是丢了，没有任何办法补回来。
把它归进「可丢」车道，与 **W08b**（「公屏历史工具应当让其保存范围内的**全部**消息可查」）冲突。
⚠ 同样是产品判断。

## 4. 对「捕获的是异常还是错误」这个判断的回答

结论比原判断更靠前一步：**在实际会执行的路径上，那些捕获什么也没接到。**

- 四轮实盘、真模型、正常路径 + 断线 + 死亡，`catch_unwind` **一次都没有触发**。
- 失败真的发生时，形态是 `Err`（`body_motor_unavailable`）或**挂死**，不是 panic。
- 挂死恰恰是 `catch_unwind` 帮不上忙的那一类。

所以那六处捕获不是"把错误当异常接住了"，而是**为一种在生产里不发生的失败形态上的保险**，
同时真正发生的失败形态（死锁）无人负责。

**但有一处例外，而且它恰好证明了维护者的判断**——云端上一轮实验
（`bf32aac` 提交说明）删掉捕获后，`cargo test` 从「不到一分钟」变成「600 秒不结束」，
抓栈定位到：

> `ObservationCallbackGuard` 只 pop thread-local 栈，租约靠调用点手动 `finish_callback()` 释放。
> 订阅者 panic 会跳过那一行，`active_callbacks` 永不归零，
> 此后任何 `unsubscribe`（含 `Drop`）永久卡在 `wait_for_quiescence` 的 Condvar 上。

**那是一个被捕获遮住的真缺陷**（RAII 该做的事写成了手动调用），已在该提交中修好。

### 4.1 同形的缺陷在 facade 侧还没修

云端只修了观察面，**facade 侧一模一样的写法还在**：

```rust
// crates/backend/src/facade.rs  handle_event
for delivery in deliveries {
    if !delivery.state.begin() { continue; }
    delivery.listener.on_event(event.clone());   // ← panic 就跳过下一行
    delivery.state.finish();                     // ← 不是 Drop，是普通语句
}
```

一旦某个 listener panic，`finish()` 被跳过 → `remove_subscription` 里的
`state.wait_quiescent()` 永久阻塞。与观察面那处是同一个缺陷。

**这条应当修**，而且修法和云端那次一样：把租约归还搬进守卫的 `Drop`。
它与「要不要保留捕获」无关——**无论捕获去留，RAII 都该是 RAII**。

## 5. 我没有验证的

1. **`information_adapters.rs` 的 8 处 `panic!` 一次也没触发。** 它们的条件是
   `backend.snapshot()` 返回 `Err`；即使硬断线也没有发生（观察面按设计在断线/死亡时仍可读）。
   所以「它们没有接管者」这个梳理结论仍然成立，但**没有实盘证据说明它们会不会真的被打到**。
2. **只有四轮，且都是短时长（70~300 s）。** 长时运行、多次重连、维度切换都没测。
3. **没测有捕获与无捕获在同一场景下的成对重复。** 第 2.1 节的对照来自不同日期、
   不同代码版本的运行，只够支持「挂死不是删捕获造成的」，不够支持更强的结论。
4. **run3 是我手动杀掉的**，没有跑到计划停机，所以它不计入停机挂死的样本。

## 6. 建议的下一步

1. **修 facade 侧的租约归还**（§4.1）。与实验结论无关，纯缺陷，机械改动。
2. **停机挂死是当前最该修的东西**（§2）。它 100% 复现、成因已定位到具体调用栈。
   由于阻塞点在 azalea 内部（`start_with_opts` 收尾调 `Client::username()`），
   按「受控依赖 bug 修在源头」的既有判断，应当**改 fork**，不在自己层绕行。
3. **捕获的去留**：⚠ 这条我不自己定。实盘证据支持「删掉它们不会让情况变坏」，
   但不支持「删掉它们更好」——它们本来就没在触发。
   真正的问题是 §2 那个死锁和 §4.1 那处 RAII，两者都与捕获无关。
