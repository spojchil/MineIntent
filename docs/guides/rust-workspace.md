# MineIntent Rust workspace（MC 26.1）

MineIntent 的全 Rust 单进程实现。目标服务端 **Paper 26.1.2 / 协议号 775**。

| crate | 职责 |
|---|---|
| [`crates/contracts`](../../crates/contracts) | Agent、capability、Information 与 Minecraft backend 的严格进程内契约与 wire DTO |
| [`crates/backend`](../../crates/backend) | 自有协议后端：连接生命周期、命令与观察、full/directed 视口原子投影（[为什么依赖 fork](../../crates/backend/README.md)） |
| [`crates/middle`](../../crates/middle) | Agent 循环、capability registry 与派发、Information provider、记忆、语音、Participant runtime |
| [`crates/app`](../../crates/app) | 组合根：装配上述三层与模型 provider，产出可执行的 `mineintent` |

## 构建与运行

```bash
cargo build --workspace
cargo test --workspace --all-targets
```

启动一个同伴（离线模式服务器）：

```bash
MINEINTENT_MC_HOST=127.0.0.1 \
MINEINTENT_MC_PORT=25565 \
MINEINTENT_MC_USERNAME=MineIntentBot \
MINEINTENT_MODEL=deepseek \
DEEPSEEK_API_KEY=... \
cargo run -p mineintent-app --bin mineintent
```

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `MINEINTENT_MC_HOST` / `MINEINTENT_MC_PORT` | `127.0.0.1` / `25565` | 目标服务器 |
| `MINEINTENT_MC_USERNAME` | `MineIntentBot` | 离线身份（本版本只支持 offline） |
| `MINEINTENT_WORLD_ID` | `local-world` | 世界标识，进入 scope 与 journal |
| `MINEINTENT_DATA_DIR` | `.mineintent` | 记忆、journal 与调试产物目录 |
| `MINEINTENT_MODEL` | `scripted` | `scripted` = 确定性假模型；`deepseek` = 真实模型 |
| `MINEINTENT_DEBUG` | 关 | 见下节 |
| `MINEINTENT_MAX_RUNTIME_SECS` | 无 | 到时按正常停机路径退出，供无人值守验收 |

## 发版构建

```bash
cargo build --release -p mineintent-app --bin mineintent
```

`[profile.release]` 开了整体 LTO、单编译单元与符号剥离：实测单文件
**47.2 MB（debug）→ 23.8 MB（release）**。两处刻意不做：

- **不设 `panic = "abort"`**：工具与模型 provider 的 panic 由 `catch_unwind`
  捕获并转成结构化失败，这是产品行为；abort 会让一次工具 panic 杀掉整个同伴进程。
- **不用 `opt-level = "z"`**：ECS 每秒 20 tick 的热路径对吞吐敏感，
  拿运行时性能换几 MB 体积不划算。

排障需要栈回溯时用 debug 构建复现，不依赖发版二进制里的符号。

## 开发者模式

`MINEINTENT_DEBUG=1` 打开后，数据目录下会出现：

- `dev.log`：装配、生命周期心跳、每轮模型请求与响应摘要、故障流；
- `model-io/`：每轮模型请求与响应的**原文**。

它与 journal 分工不同：journal 是产品事实的持久记录（有 schema、要迁移），
dev.log 是排障用的过程记录，不承诺格式稳定，默认关闭。排障先看它。

## 事实来源边界

- `commanded`：后端主动发出的聊天、视角、移动与停止动作。
- `client_predicted`：Azalea 客户端物理采样与 Tick 快照，**不当作服务端事实**。
- `server_observed`：服务端协议事件（含筛选后的位置修正包）与 Spawn/Death 边界快照。

快照事件的 `source` 只约束该次事件内的事实边界：Tick 快照里的本地姿态可能是预测值，
必须结合来源读取。死亡不会自动重生——重生是同伴自己的 `respawn` 工具调用；
自动重连与资源包接受保持关闭，均须上层明确决定。

## 视口

`visibleBlocks` 是 `[BlockInfo, x, y, z]` 整数体素并按距离排序；`BlockInfo` 无视觉属性时
是方块名字符串，有属性时是名称加白名单属性的对象。`visibleEntities` 最近优先、有限并带
`truncated`。未加载方块让相关可见性射线保守失败，不会被当作空气。`directed` 最多接收
16 个唯一世界坐标，逐坐标返回 seen 或闭合的 unseen 原因。
