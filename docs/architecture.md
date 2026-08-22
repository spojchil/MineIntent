# 当前实现结构

> 无产品权威。绑定 `main`（单栈线合入提交，SHA 于合并后回填）。
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
     ┌────────┬───────────┼───────────┬─────────┬────────┐
     ▼        ▼           ▼           ▼         ▼        ▼
  context  perception  screens     motion     hand    memory
     │        │  │        │  │        │  │      │  │     │
     │        │  └──┐     │  │        │  │      │  │     │
     ▼        ▼     ▼     ▼  ▼        ▼  ▼      ▼  ▼     ▼
   render ──→ world      dispatch ──────────────────────→ agent
     │                       │                              ▲
     └───────────────────────┴──────────────────────────────┘
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
| `perception` | 主动看（`scan`） | 薄壳：几何全在 world 的视口内核 |
| `screens` | 界面互斥域（`chat_box`/`inventory`/`crafting_table`） | 屏的状态转换在此，占用账本在 dispatch；容器屏真相在服务端，组合根随屏事实翻转 |
| `motion` / `hand` | 位移朝向 / 攻挖用 | 工具只表达意图立刻返回，合法性由原版物理自我仲裁 |
| `presence` | 生死去留（`respawn`） | 死亡时唯一还放行的一类（`ToolClass::Vital`）；自动重生已关，起不起来是模型自己的事 |
| `memory` | 单文件长期记忆 | 一个文件、两张脸（工具面 `remember` 与策略面落盘）、一个出口 |
| `context` | 提示装配与压缩 | 受保护三段每轮现拉、永不参与压缩 |
| `dispatch` | 工具编排 | 互斥域账本归此层；工具模块只做状态转换 |
| `agent` | 模型—工具循环内核 | 零项目依赖；见 §5 |
| `companion` | 组合根 | 唯一 `main`；唤醒脚手架也在这里 |

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

## 4. `world/machine` 的六个模块

1590 行的单文件按六件事拆开，各自显式声明依赖（生产码零 `use super::*`）：

| 模块 | 做什么 |
|---|---|
| `mod.rs` | `Module` 公开面、`ConnectionConfig` |
| `state.rs` | `Inner`：共享状态、三个时间窗、写口队列——**可脱离 azalea 单测** |
| `capture.rs` | ECS → `TickSnapshot` 直译 |
| `movement.rs` | 移动 job 终局判定（纯函数判定表）与轮询 |
| `connect.rs` | azalea 接入、客户端回调、停机 |
| `door.rs` | `DoorCommand` 与 tick 内执行 |
| `blocks.rs` | azalea 世界模型的方块读取原语 |

`viewport/` 同理分出 `geometry.rs`（纯几何原语，不认识读取器）。

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
  ECS 只在客户端事件回调里触碰，不跨线程直写。
- 对外全部经共享状态：快照 latest-wins（外部持旧 `Arc` 用多久都行），
  写口走队列在 tick 内执行。
- 视口投影是纯 CPU 重活，组合根放 `spawn_blocking`。

**已知上游隐患**：azalea 在 `AppExit` 清空 ECS 之后，残留事件处理可能在
持 ECS 写锁时重入读锁而自死锁。此时机器线程卡死，`Module::stop` 的合流
超时会如实报错，进程退出时由操作系统回收。

## 7. 验证口径

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
```

无整体放行的 lint。`[workspace.lints]` 的 `unsafe_code = "deny"` 与
`str_to_string = "warn"` 由 11 个 crate 全部接上。
