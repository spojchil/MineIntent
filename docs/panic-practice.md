# panic 处置：我们的做法

> 分支：`refactor/panic-supervision`。无产品权威——本文只记录工程做法与其依据，
> 不替 [`产品.md`](./产品.md) 增加产品判断。
>
> 判据来自三份可核对的证据，而不是偏好：
>
> | 证据 | 出处 |
> |---|---|
> | 四轮实盘（真模型 + Paper 服务端，含断线与死亡） | [实盘观测](./no-panic-live-run.md) |
> | 281 个运行期依赖的 `catch_unwind` 与 panic 密度全量扫描 | 本文 §6 |
> | 18 个知名 Rust 仓库的 panic 策略与用法调查 | 本文 §6 |

## 1. 现状（可核对）

| 项 | 值 | 怎么核 |
|---|---|---|
| 生产代码 `catch_unwind` | **0 处** | `grep -rn catch_unwind crates/*/src --include='*.rs' \| grep -v /tests/` |
| 测试代码 `catch_unwind` | 保留（断言式） | 主要在 `crates/middle/tests/information_adapters.rs` |
| 生产代码显式 panic | **19 处** | 见 §4；计数口径见下 |
| panic 钩子 | 无条件安装 | `crates/app/src/lib.rs` → `devlog::install_panic_hook()` |
| `[profile.release] panic` | `unwind` | 理由写在 `Cargo.toml` 该行上方 |

## 2. 四条规则

### 规则一：默认不捕获

生产代码不写 `catch_unwind`。

**理由**：这个系统里没有不可信代码边界。六个工具是我们写的；azalea 是我们自己
打补丁的钉住 fork；模型 provider 那条路走 `Result`。凡是能 panic 的地方都是我们
该修的缺陷，捕获不减少缺陷，只决定我们是否知道。

模型输出**不构成**反例：模型给的工具参数确实不可信，但那是**数据**不可信而非
**代码**不可信，对的工具是参数校验，不是捕获参数解析时的 panic。

如果将来真去加载自己不控制的插件或脚本，这条重开。

### 规则二：要捕获，必须同时过三道闸

任何一处新的 `catch_unwind` 必须在代码注释里逐条说明它如何满足：

**(a) 这次操作没有改共享状态。**
判据取自 rust-analyzer（`crates/rust-analyzer/src/handlers/dispatch.rs` 的文档
注释）：只读请求包进 `catch_unwind`，*因为它们不改状态，所以从失败里恢复是安全
的*；而改状态的那个入口明确不设防。

我们有过反例：观察回调的租约归还写成了手动调用而非 `Drop`，订阅者 panic 会跳过
那一行，`active_callbacks` 永不归零，此后任何 `unsubscribe` 永久卡死。**捕获遮住
的恰恰是一个状态没归位的缺陷**，删掉捕获才把它暴露出来。

**(b) panic 必须出声。**
记 error 级日志。前提条件已具备：panic 钩子无条件安装，带线程名、位置、消息与
`Backtrace::force_capture()`。rust-analyzer 的线程池之所以敢直接丢弃 panic，注释
写得很清楚——*we should've logged the backtrace already*。没有这个前提就不允许丢。

**(c) 捕获之后不能假装无事发生。**
社区四种用法里没有一种是「吞掉后继续」（见 §6）。允许的收场只有两类：**上报给
调用方**，或**有序退出**。

特别地：**不允许把 panic 压成一条模型可见的普通失败**。那会让缺陷看起来像世界
事实，而模型必然重试——panic 可重现，重试注定再次 panic。（这条也是
`crates/toolloop/src/control.rs` 开头不做循环内 panic 隔离的理由。）

### 规则三：`panic = "unwind"` 保持不变

**不是默认值兜底，是有理由的选择**，理由写在 `Cargo.toml` 对应行上方：删掉捕获
之后，panic 的接管者换成了 tokio 的任务边界（见 §3）。`panic = "abort"` 会让这
一整层失效。

社区佐证：18 个知名仓库里 14 个保持默认 unwind；真正把默认 release 改成 abort 的
只有 deno 和 uv。

**若将来要改 abort**，必须同时做完这些，否则等于埋新债：

1. 重写 `crates/toolloop/src/control.rs` 与 `crates/middle/src/agent/runner.rs`
   里依赖任务边界的理由说明
2. 清掉所有已经失效的 `catch_unwind` 及其注释——deno 与 uv 都设了 abort 却在
   代码里留着 `catch_unwind`（uv 那处注释还写着 "Report panics back to the main
   thread"，在 release 下永不执行），是现成的反面教材
3. 接受「一次工具缺陷终止整个同伴进程」

### 规则四：写 `panic!` 要有资格

生产代码里 19 处显式 panic 是我们**主动**制造的终止点。既然不再捕获，每一处都
直接决定一个任务或一根线程的存亡。新增时的判据：

- **可以**：违反的是本模块自己维护、且编译器无法表达的不变量，继续执行会产生更
  难诊断的后果
- **不可以**：条件依赖外部世界（服务端、模型、网络、文件系统）。那些是失败，不是
  缺陷，该走 `Result`
- **另一条出路**：很多 `unreachable!` 说明的是「类型没把不变量表达出来」。能靠
  类型消除的，就不要靠断言相信——`viewport.rs` 与 participant 入队路径都是这样
  消掉的（合并两个 match，让不可能的分支不再存在）。

计数口径见 §4。

## 3. 接管层的实际形状

删掉捕获不等于没有接管。当前实际存在四层，从内到外：

| 层 | 位置 | 接住什么 | 表现为 |
|---|---|---|---|
| 0 | `crates/app/src/devlog.rs` 的 panic 钩子 | **全部** panic | error 日志 + dev.log，含线程名、位置、backtrace |
| 1 | `process_wake` 里的嵌套 `tokio::spawn` | 工具与 provider 的 panic | `JoinError` → `participant_handler_failed` |
| 2 | `ParticipantRuntime::stop()` 的 worker join | worker 循环其余部分的 panic（journal 落盘、帧捕获、队列记账、终态处理） | `participant_worker_panicked` |
| 3 | backend 两处线程 join（`join_worker_blocking`、dispatcher 的 `Drop`） | 两根不在 tokio 任务里的裸线程 | `tracing::error!` |

第 1、2 层各有回归测试守着，位置在
`crates/middle/tests/participant_runtime/lifecycle_teardown.rs`：

- `agent_run_panic_stops_at_the_nested_task_boundary` — 钉住第 1 层的契约本身。哪天
  `process_wake` 不再 spawn，循环里没有捕获、外面也没有边界，panic 会一路打死
  worker；这条测试是那个变化的报警器。
- `worker_panic_surfaces_as_failure_at_stop` — 钉住第 2 层。回退到修复前的写法验证
  过：该测试变红，且收集到的失败列表是**空的**。

**依赖侧不改变这个形状**：bevy 的 system 捕获只为多打一行「哪个 system panic 了」，
随后原样再抛——单线程执行器在捕获处紧接着 `resume_unwind`
（`bevy_ecs/src/schedule/executor/single_threaded.rs:157-161`），多线程执行器把
payload 存起来、等这一轮调度的其余 system 跑完再 `resume_unwind`
（`multi_threaded.rs:313`）。

## 4. 19 处显式 panic 的清单与去向

### 4.1 计数口径

三条都要做，否则数会虚高（这份文件的早期版本写过 27 和 47，都是口径错误）：

1. 排除测试文件——不只是 `tests/` 目录，还有 `viewport_tests.rs`、
   `entity_events_owner_tests/` 这类**不含** `/tests/` 的路径
2. 排除 `#[cfg(test)] mod` 块
3. 但**不能**把所有 `#[cfg(test)]` 都当模块开头——`facade.rs` 里大量 `#[cfg(test)]`
   加在结构体字段上（`#[cfg(test)] scripted: bool,`），误判会吞掉整片正常代码

### 4.2 清单

| 组 | 处数 | 位置 | 去向 |
|---|---:|---|---|
| Information 适配器 | **8** | `middle/src/participant/information_adapters.rs` | 该控制面已由编译实验坐实不在生产路径上，实盘四轮一次未触发。随那部分代码的处置一起归零 |
| 类型没表达清楚的 `unreachable!` | **6** | `contracts/minecraft/event.rs` ×3、`backend/runtime/observation.rs` ×2、`backend/runtime/dto.rs` ×1 | 靠类型消除，与 `viewport.rs`、participant 入队路径同形 |
| `json!` 字面量 | **3** | `app/src/model/scripted.rs` | 改为直接构造 `serde_json::Map`，类型即 Object，断言消失 |
| CLI 参数解析 | **2** | `backend/src/main.rs` | 独立二进制入口，参数错即退出。规范做法是返回 `Err` 交给 main，属常规，低优先级 |

**预计 19 → 2~4 处。** 前三组共 17 处都有明确路径。

`contracts/minecraft/event.rs` 那 3 处是光秃秃的 `_ => unreachable!()`，**当前就不
满足规则四**——没有任何不变量说明。要么补，要么消除。

### 4.3 密度对照

同一把尺子（都剔除测试文件与 `#[cfg(test)] mod` 块）：

| | 生产行数 | 宏 panic | /KLOC | `unwrap`/`expect` | /KLOC |
|---|---:|---:|---:|---:|---:|
| 我们 | 37,635 | 19 | **0.50** | 65 | 1.73 |
| 271 个运行期依赖 | 1,160,809 | 1,756 | **1.51** | 4,976 | 4.29 |

单 tokio 124 处、bevy_ecs 98 处、portable-atomic 128 处。**我们的密度是依赖侧的
三分之一**——显式 panic 不是我们当前偏高的项。记这一条是为了防止把「减少 panic」
当成不需要论证的好事：规则四的判据是「这处该不该存在」，不是「总数越少越好」。

## 5. 测试里的 `catch_unwind` 是正当的

作断言用（「这里必须 panic」）与规则一不冲突：那是在验证缺陷检测本身，不是在生产
路径上掩盖缺陷。

但有一条教训：`crates/backend/src/runtime/tests/observation.rs` 里曾有一个测试
**只因为外层捕获吞掉了一次真实 panic 才通过**。捕获与断言同在时，要能说清楚断言
到底断言了什么。

## 6. 与社区做法的对照

### 6.1 用量：稀少，且全在边界

我们的 281 个运行期依赖中，生产代码真的调用 `catch_unwind` 的只有 **12 个
（4.3%）**：tokio、bevy_ecs、bevy_app、bevy_tasks、moka、async-task、futures-util、
futures-lite、crossbeam-utils，以及三个我们没启用或编译不进来的（hyper 的 `ffi`
特性、tower-http 的 `CatchPanicLayer`、proc-macro2 的宏展开期探测）。

知名仓库的命中文件数（GitHub 代码搜索）：ripgrep 0、alacritty 0、helix 0、
cargo 1（在 `tests/testsuite/`）、uv 2、deno 3、servo 4、nushell 6、zed 6、
rust-analyzer 9。

### 6.2 用法：四种形态，没有一种是「吞掉后继续」

| 形态 | 实例 | 关键点 |
|---|---|---|
| 请求边界 + 上报 | rust-analyzer `handlers/dispatch.rs` | 只包只读请求；转成 LSP `InternalError`，**带 panic 原文**回给客户端 |
| 捕获后立即退出 | nushell `nu-plugin/src/plugin/mod.rs` | `if unwind_result.is_err() { std::process::exit(1); }`——捕获只为让解栈跑完并把错误送出通道 |
| 捕获后终止应用 | zed `remote_server/src/server.rs` | `log::error!("app panicked. quitting.")` 后返回 `Err` |
| 线程池丢弃 | rust-analyzer `stdx/src/thread/pool.rs` | `// discard the panic, we should've logged the backtrace already`——丢弃**有前提** |

### 6.3 官方口径

`std::panic::catch_unwind` 文档：

> It is **not** recommended to use this function for a general try/catch mechanism.

该用的地方是 **FFI 边界**。三条注意：abort 下接不到；外来异常行为未定义；**丢弃
`Err` 时可能二次 panic**。

## 7. 还没定的

1. **abort 与 unwind 的最终归宿。** 现状（unwind）自洽且有测试守着；改 abort 需要
   走完规则三列的三步。⚠ 我不自己定。
2. **19 处显式 panic 的消除尚未动手。** 清单与去向已在 §4.2 列出，其中
   `contracts/minecraft/event.rs` 那 3 处当前就不满足规则四。
3. **第 1 层把 panic 归到 `participant_handler_failed`，与普通 handler 失败同码。**
   排障时分不清「工具有 bug」和「工具正常失败了」。改它要先定「panic 在失败分类里
   算哪一类」。
4. **停机挂死与本文无关但优先级更高。** azalea 的 `LocalSet` 上的自锁，实盘 100%
   复现，`catch_unwind` 与 abort 都碰不到它。详见[实盘观测](./no-panic-live-run.md) §2。
