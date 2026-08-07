# 施工记录：panic 的接管层

> 无产品权威。本文记录**做了什么、修好了什么**；规则本身见
> [panic 处置：我们的做法](./panic-practice.md)，两者不重复。
>
> 分支：`refactor/panic-supervision`，基线 `experiment/no-panic-live`。
> 姊妹篇：[生产者摘除施工记录](./refactor-log-producer-removal.md)。

## 0. 起点

基线分支已经把生产代码的 `catch_unwind` 删到零，并跑了四轮实盘（[实盘观测](./no-panic-live-run.md)）。

但删捕获只完成了一半。另一半是：**缺陷真的发生时，谁负责？**

清点下来当时的实际语义是——dispatcher 线程 panic → 钩子记一行 → 线程死 → **进程
继续跑，事实流停摆，同伴变瞎，直到停机才被 join 到**。这个组合谁都不会主动选，
它不是一个立场，是个缺口。

本分支填的就是这个缺口，并顺带清掉了删捕获留下的尾巴。

## 1. 调查

三份可核对的证据，结论已收进[做法](./panic-practice.md)，此处只记方法与要点：

| 证据 | 方法 | 关键数字 |
|---|---|---|
| 依赖全量扫描 | 按 `Cargo.lock` 交叉 `cargo tree -e normal`，剔除 `#[cfg(test)]` 块与测试文件 | 281 个运行期依赖中，生产码真的调用 `catch_unwind` 的只有 **12 个（4.3%）** |
| 18 个知名仓库 | 取工作区 `Cargo.toml` 的 `[profile.*] panic`；`gh search code` 数用量 | **14/18 保持默认 unwind**；真正改默认 release 的只有 deno 与 uv |
| 用法形态 | 逐个读实现 | 四种形态，**没有一种是「吞掉后继续」** |

最有用的一条来自 rust-analyzer 的文档注释，它把判据写死了：

> Read-only requests are wrapped into `catch_unwind` — **they don't modify the state,
> so it's OK to recover from their failures.**

而改状态的入口明确不设防（"please, don't make bugs :-)"）。这条成了我们规则二(a)。

⚠ 一个反面教材：deno 与 uv 都设了 `panic = "abort"`，代码里却还留着 `catch_unwind`
（uv 那处注释还写着 "Report panics back to the main thread"），在 release 下永不
执行。**会撒谎的代码。**

## 2. 发现并修掉的缺陷

这是本条线最实在的产出。

### 2.1 三处被丢弃的 `JoinError`

删捕获的依据写在 `crates/toolloop/src/control.rs` 开头：不做 panic 隔离，交给调用方
的任务边界接成 `JoinError` 走失败流。生产路径上确实有那个边界，但**接管没有实现**：

```rust
// participant/runtime/mod.rs  stop()  —— 修复前
if tokio::time::timeout(STOP_WORKER_SETTLE, &mut worker).await.is_err() {
```

worker 若已 panic，`&mut worker` 立刻返回 `Err(JoinError)`，`timeout` 给出
`Ok(Err(..))`，`.is_err()` 是 **false**——既不 abort 也不绑定那个 JoinError，径直走到
`lifecycle = Stopped` 与 `Ok(())`。**停机报告成功**，而参与者从 panic 那一刻起不再
处理任何唤醒。

同形的还有 backend 的 `join_worker_blocking` 与 dispatcher 的 `Drop`，两处都是
`let _ = join.join()`。

修复：三处都出声。回归测试
`worker_panic_surfaces_as_failure_at_stop`，回退到修复前验证过——测试变红，且收集到
的失败列表是**空的**。

### 2.2 一处订正：接管其实实现了一半

上一条的提交说明写「那条理由描述的是意图，不是已实现的行为」——**过头了**。

写测试时发现 agent run 跑在 `process_wake` 里一个**嵌套** `tokio::spawn` 中，它的
`JoinError` 在 `mod.rs` 的 `joined.map_err(..)` 处接住，变成
`participant_handler_failed`。**工具与 provider 的 panic 这条路一直是通的。**

真正的缺口只在 agent run **之外**那半个 worker 循环（journal 落盘、帧捕获、队列
记账、终态处理）。补了第二个测试
`agent_run_panic_stops_at_the_nested_task_boundary` 钉住这条契约本身——哪天
`process_wake` 不再 spawn，循环里没捕获、外面没边界，panic 会一路打死 worker。

### 2.3 漏掉的第五层：speech worker

被 `#[ignore]` 掉的那个测试里记着一件我梳理时没写进接管层表格的事：

> panic 会杀掉 speech worker 这个全局唯一的 tokio 任务，同伴从此永久说不出话——
> 而且**没有任何人观察那个 JoinHandle**，所以既不崩溃也不报错。

`SpeechScheduler` 的 `worker: JoinHandle<()>` 只在 `Drop` 里 `abort()`，运行期间无人
查看。

修复：接管点放在 `schedule()`——它是唯一必然被走到的入口。查 `is_finished()`，死了
返回新的 `SpeechScheduleError::WorkerGone` 并 `tracing::error!`，**而不是照常入队**。
排进一个没有消费者的队列，等于把缺陷伪装成「说了但没送到」。

### 2.4 facade 侧的租约泄漏

[实盘报告](./no-panic-live-run.md) §4.1 点名过，一直没修：

```rust
delivery.listener.on_event(event.clone());
delivery.state.finish();          // ← 不捕获，panic 跳过这一行
```

改为 `ListenerLease` 的 `Drop` 归还。与观察面 `ObservationCallbackGuard` 同形，但
后果轻一级——facade 的 `wait_quiescent` 有 `UNSUBSCRIBE_WAIT = 2s` 期限，泄漏表现为
此后每次退订白等 2 秒；观察面那处是裸 `Condvar` 无期限，一次泄漏就永久卡死（实测
表现为测试 600 秒不结束）。

> 顺带订正：实盘报告把 facade 这处也写成「永久阻塞」，据代码不成立。

### 2.5 三个 `#[ignore]` 测试

删捕获时有三个测试因「前提不再成立」被整体标记掉，**其中两个还连带禁用了与 panic
无关的断言**。

永久 ignore 的测试比删掉更糟：看着像有覆盖，实际不跑也不报警。

| 文件 | 处理 |
|---|---|
| `speech_scheduler` | 改写为断言当前契约（panic 杀死 worker → `schedule` 报 `WorkerGone`） |
| `information_adapters` | 拆成三个：dispose/drop 幂等、退订 panic 照常传播、监听回调 panic 不被吞。原本三段覆盖只因第三段失效被整体禁用 |
| `facade` FIFO | 去掉 `PanicListener` 一段，恢复 FIFO 次序、退订与重入三条断言 |

结果：ignored 从 3 归 **0**。

### 2.6 profile 注释指着已删的实现

`Cargo.toml` 的 `[profile.release]` 写着：

> 显式保留 unwind：工具与模型 provider 的 panic 由 catch_unwind 捕获并转成结构化
> 失败（**agent/driver.rs**），这是产品行为。

那处 catch 已经删了。**profile 里写着一条产品行为的理由，指着一个不存在的实现**
——按 **G05** 是必须清的欠账。重写为现在的理由（接管者换成 tokio 任务边界），并写明
改 abort 要同时做完的三件事。

### 2.7 两处删捕获残留

`speech/scheduler.rs` 的 `panic_message` 死函数（只为格式化捕获到的载荷而存在）；
`information_runtime` 两个自 `b1c6f51` 起就红的测试——它们断言的正是被推翻的语义
（把缺陷压成模型可见的普通失败）。删除处留注释写明理由。

## 3. 计数订正记录

同一个数我报过三次，每次口径都有漏，如实记下来：

| 值 | 口径 | 漏在哪 |
|---:|---|---|
| 47 | 只排除路径含 `/tests/` 的文件 | 同文件内 `#[cfg(test)]` 块 |
| 27 | 加了区间探测 | 探测器把 `facade.rs` 里加在**结构体字段**上的 `#[cfg(test)] scripted: bool,` 当成模块开头，吞掉整片正常代码；`viewport_tests.rs`、`entity_events_owner_tests/` 这些不含 `/tests/` 的测试路径也没排除 |
| **19** | 两处都修好 | —— |

度量脚本已入库 `scripts/panic-density.py`，口径写进
[做法](./panic-practice.md) §4.1，免得下次再算错。

顺带的密度对照（同尺子）：我们 0.50 宏 panic/KLOC，271 个运行期依赖 1.51/KLOC
——**我们是依赖侧的三分之一**。记这条是为了防止把「减少 panic」当成不需要论证的
好事。

## 4. 验证口径

每笔提交都跑：

```sh
export PATH="$HOME/.rustup/toolchains/nightly-x86_64-apple-darwin/bin:$PATH"
cargo fmt --all --check
cargo clippy --workspace --all-targets      # error 必须为 0
cargo test --workspace --all-targets --no-fail-fast
```

分支末态：**629 通过 0 失败 0 ignored**（后续生产者摘除又删掉 4 个方块测试，见
姊妹篇）。

两个关键修复都用**回退验证**确认过确实覆盖新代码：临时改回修复前的写法，确认测试
变红，再恢复。

## 5. 留下的

1. **⚠ `panic = "abort"` 还是 `unwind`。** 现状（unwind）自洽且有测试守着；改 abort
   要走完[做法](./panic-practice.md)规则三列的三步。我不自己定。
2. **19 处显式 panic 的消除尚未动手。** 清单与去向在[做法](./panic-practice.md) §4.2，
   预计降到 2~4 处。其中 `contracts/minecraft/event.rs` 那 3 处光秃秃的
   `_ => unreachable!()` **当前就不满足规则四**。
3. **第 1 层把 panic 归到 `participant_handler_failed`**，与普通 handler 失败同码，
   排障时分不清「工具有 bug」和「工具正常失败了」。
4. **停机挂死优先级更高，且与本线无关**：azalea `LocalSet` 上的自锁，实盘 100%
   复现，捕获与 abort 都碰不到它。
