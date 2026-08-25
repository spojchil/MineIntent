# 当前实现结构

> 无产品权威。绑定 `feat/block-memory-tables` 当前工作树（基线 `9b4ae96`）与
> Azalea fork `cce19dfa7b120eef090c48d96f5b25851cd89d9a`。
>
> 本仓只有一条线。此前并存的两套实现都已移出：
> TypeScript 原型（`src/`、`agent-service/`、`mcserver/`）已随本线落 `main` 删除，
> 代码快照见 tag `typescript-prototype`（`e9b18c4`）；
> 早期 Rust 移植（`crates/{app,backend,middle,contracts,toolloop}`）已删除，
> 证据见 git 历史。

## 0. 一句话

`companion` 是唯一组合根、唯一可执行。它把**接入世界**、**工具编排**、
**上下文策略**、**模型内核**四件事接在一起，其余 crate 各自只做一件事。

## 1. 依赖图

```
                      companion（组合根，唯一知道所有人的地方）
                          │
   ┌────────┬─────────┬───┴────┬────────┬───────┬────────┬─────────┬──────┐
   ▼        ▼         ▼        ▼        ▼       ▼        ▼         ▼      ▼
context perception screens   motion   hand    jobs   presence   memory  wait
   │        │  │      │  │      │  │    │  │    │  │     │  │      │      │
   │        │  └───┐  │  │      │  │    │  │    │  │     │  │      │      │
   ▼        ▼      ▼  ▼  ▼      ▼  ▼    ▼  ▼    ▼  ▼     ▼  ▼      ▼      ▼
 render ──→ world        dispatch ────────────────────────────────────→ agent
   │                         │                                        ▲
   └─────────────────────────┴────────────────────────────────────────┘
```

两条读法上要注意的：

- **`agent` 不认识 MineIntent。** 它是零项目依赖的模型—工具循环内核，
  本体是独立仓库 midturn 的 git 依赖（见 §5）。
- **`world` 不认识模型。** 它只做「原版连接与感知」，出口是 tick 快照。

## 2. 各 crate 一句话

| crate | 职责 | 关键约束 |
|---|---|---|
| `world` | 接入 azalea、tick 快照、视口内核 | **直译无损、政策外置**：不过滤、不判重要性、不做丢弃决策 |
| `render` | 快照 → 模型可读文字 | **全部纯函数**；呈现选择归此处，事实归快照 |
| `perception` | 主动看（`scan`）、查记忆（`blocks`，含 SQL 面） | 薄壳：几何全在 world 的视口内核；SQL 面读记忆的快照，查询期不持锁 |
| `screens` | 界面互斥域（`chat_box`/`inventory`/`container`） | 屏的状态转换在此，占用账本在 dispatch；容器屏真相在服务端，组合根随屏事实翻转 |
| `motion` / `hand` | 位移朝向 / 攻挖用 | 工具只表达意图立刻返回，合法性由原版物理自我仲裁 |
| `jobs` | 任务表（`list`） | 只读、`ToolClass::Free`；槽位是唯一真相源，不另建镜像 |
| `presence` | 生死去留（`respawn`） | 死亡时唯一还放行的一类（`ToolClass::Vital`）；自动重生已关，起不起来是模型自己的事 |
| `memory` | 单文件长期记忆 | 一个文件、两张脸（工具面 `remember` 与策略面落盘）、一个出口 |
| `wait` | 让时间过去（`wait`） | `ToolClass::Free`；**永远可打断**，判据由组合根的门铃给（见下） |
| `context` | 提示装配与压缩 | 受保护**两段**（人设、记忆）每轮现拉、永不参与压缩；处境不在前缀里，随帧追加 |
| `dispatch` | 工具编排 | 互斥域账本归此层；工具模块只做状态转换 |
| `agent` | 模型—工具循环内核 | 零项目依赖；见 §5 |
| `companion` | 组合根 | 唯一 `main`；唤醒脚手架也在这里 |

`wait` 的打断判据不在 `wait` 里：组合根的 `doorbell.rs` 同时是 `agent::Observer` 和
`wait::Interruptions`。判据是两个计数的差——唤醒每投递一批敲一次铃，每次模型请求开始
拍一张快照，**计数比快照大 = 有模型还没看见的唤醒**，那就一秒都不等。这补上了「模型
决定要等」到「等真的开始」之间那次推理（实测 3–4 秒）的窗口。

**铃由组合根的唤醒投递点敲，帧那一侧不敲。**两个投递点都走 `NextModelRequest`，投递
类别分不出谁是谁；而帧的发车闸门是「有非终局进展」，只要同伴在动就一直来。把帧算作
打断，等待会在它最主要的用途上当场失效：开始挖、`wait`、进展帧、立刻醒、再 `wait`
——正是这件工具要消灭的轮询。判据钉在唯一知道区别的地方，比钉在一个已经不承载这个
区分的投递类别上结实：后者在类别改动时会静默失效，而且单测照样绿。

## 3. 数据怎么到模型

**世界长什么样以状态到达**（拉取，最新赢）；只有**在状态里不留痕**的瞬时事
才走事件通道。

```
azalea ECS ──每 tick──→ TickSnapshot (latest-wins, Arc)
                            │
                            ├─ self/entities/players/world_meta  ← 状态
                            └─ chat/damage/jobs: Window<T>        ← 时间窗
                                    │
                                  render（纯函数）
                                    │
                       companion::situation（随帧追加，不进前缀）
                                    │
                                  agent → 模型
```

方块**不在快照里**——最深最重的嵌套留在 azalea 世界模型原地，
`perception::scan` 按需拉（`world::machine::blocks`）。

时间窗不是队列：条目自带 `tick` 与单调 `seq`，「取某 seq 之后的条目」
是读方一行过滤。逐出用原版常量（聊天 100 行、声音 60 tick）。

## 4. `world/machine` 的模块

按事情拆开，各自显式声明依赖（生产码零 `use super::*`）：

| 模块 | 做什么 |
|---|---|
| `mod.rs` | `Module` 公开面、`ConnectionConfig` |
| `state.rs` | `Inner`：共享状态、时间窗、写口队列——**可脱离 azalea 单测** |
| `capture.rs` | ECS → `TickSnapshot` 直译 |
| `job.rs` | `JobSlot`：后台任务的形状，**每个任务恰好一条终局** |
| `movement.rs` / `mining.rs` | 两个动词各自的判定表与轮询；移动含战争迷雾高层状态机 |
| `navigation.rs` | 观察 frontier 的稳定身份、目标谓词与朝最终目标的引导 |
| `observed.rs` | 最后所见的方块记忆 → 冻结碰撞快照与 `BlockSource` |
| `connect.rs` | azalea 接入、客户端回调、停机 |
| `door.rs` | `DoorCommand` 与 tick 内执行 |
| `blocks.rs` | azalea 世界模型的方块读取原语 |

`viewport/` 同理分出 `geometry.rs`（纯几何原语，不认识读取器）与
`incremental.rs`（方块记忆：三态、按区段分片）。

## 4a. 方块记忆是一张关系，两种表示

`位置 → Option<方块事实>`：`Some` 是「看过、有东西」，`None` 是「看过、是空的」，
**表里没有这个键就是「没看过」**。三态因此不占第三份存储。

两种表示按 16³ 区段分片、边界对齐：有东西那支是条目表（一格约百字节），
是空的那支是位图（一格一位，一区段 512 字节定长）——载荷差三个数量级，
合并表示就是把位图的好处扔掉。分片买到快照便宜（每片一个 `Arc`，写时才分裂）、
盒扫可行、两支代价对称。

**写口只有 `observe(at, Option<fact>, tick)` 一个**，删除这个操作在 API 上不存在：
「亲眼见空」是一条载荷为空的观察，不是把记录删掉。

模型面是两张虚表（`crates/perception/src/vtab.rs`），底下就是这本记忆，
一行都不产生；「没看过」只能以两表反连接的形式出现，因为补集无界。

## 4b. 战争迷雾 `go_to`

外部 `go_to` 当前仍是**精确身体格**目标；是否另提供 near/reach 语义仍在
[Issue #139](https://github.com/spojchil/MineIntent/issues/139) 等待产品决定，当前实现
不静默改写。已观察地图寻路按下面的有限状态循环：

```text
Direct(精确目标)
  └─路段未达→ Survey(固定姿态十二向真实观察)
                 ├─新冻结图可直达→ Direct
                 └─仍不可直达→ Frontier(已知图上可达的观察边界) → Survey
```

- `BlockMemory` 同时保存三态和最后所见 `state_id`。`BlockSource` 对未知格返回
  `None`，fork 的 `missing_block_state` 将它解释为不可穿过、不可站立的哨兵；
  已知格从最后所见恢复碰撞语义，不回读离屏后的实时世界。这是 W02/W04c 在动作图
  上的边界。所有严格观察图路径都关闭自动挖掘，避免挖掘成本分支读取 loaded world。
- 每条 A* 腿使用同一份冻结记忆；活视野继续写下一版，不会让一次搜索混入多个地图
  版本。计划判重键直接使用 `BlockMemory` 的内容 revision，以及冻结脚下本体支撑的
  坐标和原始 `state_id`，不再重扫、排序、哈希整张地图。revision 只在三态或完整
  `BlockFact` 真正变化时推进，因此楼梯朝向等原始属性仍会开放新计划。
- 路径交给腿以后，执行期障碍检查只刷新到最新的**已观察**状态，不读取离屏实时世界。
  Azalea 可以据此对当前腿做局部障碍/卡住 patch；fork 的
  `recalculate_partial_paths(false)` 关闭 full-goal partial continuation，后续规划只经
  高层状态机。fork 的 calculation generation 与结果二次验代拒绝晚到计算；
  `Pathfinder::queued_goto_id` 让尚未被 listener 消费的排队请求也可观察，
  `Client::force_retire_pathfinding()` 在同一 ECS 写锁内推代并清空 goal/计算/执行
  组件——这两件事都已下沉到 fork，MineIntent 不再自建镜像生命周期。仍有 client ECS
  的移动终局都会同步 retirement，不留下僵尸 goal；断线终局则由连接生命周期销毁
  整个 client ECS。
- 规划期禁止挖掘的路径，执行期也禁止：`PathfinderOpts::allow_mining` 随
  `PathFoundEvent` 存进 `ExecutingPath`，执行器据此取 `can_mine`。此前执行器硬编码
  `can_mine: true` 且读真实加载世界，于是「未知格当空气」在规划期只是乐观假设，
  到执行期会变成真的把那格挖掉——一次没人授权、也没有任何事件报告的动作（W02/W03）。
- 开放世界无法用有限证据证明“所有 frontier 已穷尽”。因此状态机不产生这种结论；
  同一位置/冻结图不重复同类计划，并给战争迷雾任务有限工作预算：每投递一段与身体
  每移动一格各扣一单位，额度为 `max(64, 初始曼哈顿距离 × 8)`。达到额度产生
  `NavigationLimitReached`，明确只表示机器主动收束、**不证明目标不可达**（W07/W07a）。

## 4c. 挖掘请求与结果

- fork 的 `Client::start_mining` 在同一 ECS 写锁内直接写入 `MiningQueued`；MineIntent
  每 tick 再用同一读锁核对 `Mining`、`MiningQueued`、`MineBlockPos`、`MineProgress`
  与 `MineTicks` 的目标一致性，并查询目标格是否还有 `BlockStatePredictionHandler`
  的待确认预测。因此 queued→active 的调度窗口不会被误判成中断、反复重发清零
  进度；本地预测出的空气也不会在服务端确认前冒充成功。挖碎只由已收敛的目标变空气
  证明；目标读不到产生独立终局，不能默认成实心。预测从首次 pending 起超过协议确认
  边界会单独报告 `PredictionNotSettled`，不冒充成功、不可挖或进度停滞。queued 长时间
  未被消费单独报告 `DispatchNotObserved`；queued 在进入匹配 Active 前消失报告
  `RequestEnded`，不再用
  无进展窗口或循环补发掩盖调度/权限拒绝。只有曾进入匹配 Active 才允许一次补发；
  第二次仍未进入 Active 就终局。`Blocked` 只表示目标仍为实心且 Active 状态下的
  `MineProgress` 连续一个窗口没有严格增长，总工期本身不设上限。

## 4d. 任务生命周期

- 连接结束后不再有 tick，因此 client 断线、swarm 断线、连接线程退出与显式
  `Module::stop` 都在生命周期边界幂等收掉移动/挖掘槽，落独立 `ConnectionEnded`
  终局；它不冒充取消、超时或目标失败。

## 5. `agent` 是 git 依赖 midturn

模型内核不在本仓：它是独立仓库
[midturn](https://github.com/spojchil/midturn)（曾用名 agent/toolturn），
与 azalea 同款接法——根 `Cargo.toml` 的 `[workspace.dependencies]` 钉死 rev，
依赖键保持 `agent`（`agent = { package = "midturn", git = …, rev = … }`），
源码里仍写 `agent::`。

升级流程：上游更新 → 本地验收（上游测试 + 对抗读）→ 改根 Cargo.toml 的
rev → 修下游破口 → 推送。内核自己的测试在上游仓跑，不占本仓 CI。
（`crates/agent` 目录已删除，见 git 历史。）

## 6. 线程模型

- `world::machine` 独占一个线程（tokio current_thread + LocalSet，azalea 需要）。
  ECS 只在这个所有者线程的客户端回调与 Bevy schedule 内触碰；外部调用不跨线程直写。
- 移动/挖掘槽及其副作用同样由该线程单写。`Module::stop` 只记录不可逆请求、使排队
  导航租约失效并唤醒 owner；已领取的 tick/命令先完成，owner 销毁 LocalSet/runtime、
  收掉任务并报告完成后，外部才发布 `Stopped`。超时只返回合流失败，不虚称已经停止。
- Azalea fork 在 `Update` 内给 A* 分配 calculation generation、应用结果时二次验代；
  已交付执行腿在 `GameTick` 做障碍与超时 patch，MineIntent 每 tick 只刷新其观察源。
- 对外全部经共享状态：快照 latest-wins（外部持旧 `Arc` 用多久都行），
  写口走队列在 tick 内执行。
- 视口投影是纯 CPU 重活，组合根放 `spawn_blocking`。

**已知上游隐患**：azalea 在 `AppExit` 清空 ECS 之后，残留事件处理可能在
持 ECS 写锁时重入读锁而自死锁。此时机器线程卡死，`Module::stop` 的合流
超时会如实报错且不会发布 `Stopped`，进程退出时由操作系统回收。

## 7. 验证口径

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
```

无整体放行的 lint。`[workspace.lints]` 的 `unsafe_code = "deny"` 与
`str_to_string = "warn"` 由 11 个 crate 全部接上。
