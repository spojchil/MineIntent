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

## 配置

优先级从高到低：**环境变量 → `.env` → `mineintent.toml` → 内置默认**。
形状参考生态惯例（如 atuin：TOML 文件叠加带前缀的环境变量）。

`mineintent.toml`（工作目录下，或用 `MINEINTENT_CONFIG` 指定路径）：

```toml
[minecraft]
host = "127.0.0.1"
port = 25565
username = "MineIntentBot"
world_id = "local-world"

[model]
provider = "responses"          # scripted = 确定性假模型
# 密钥只放路径，不放密钥本身
api_key_file = "/path/to/api-key"
# endpoint = "https://api.deepseek.com/responses"
# model = "deepseek-v4-flash"
# reasoning_effort = "none"     # none | low | medium | high
```

对应的环境变量（覆盖文件值）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `MINEINTENT_MC_HOST` / `MINEINTENT_MC_PORT` | `127.0.0.1` / `25565` | 目标服务器 |
| `MINEINTENT_MC_USERNAME` | `MineIntentBot` | 离线身份（本版本只支持 offline） |
| `MINEINTENT_WORLD_ID` | `local-world` | 世界标识，进入 scope 与 journal |
| `MINEINTENT_DATA_DIR` | `.mineintent` | 记忆、journal 与调试产物目录 |
| `MINEINTENT_MODEL` | `scripted` | `scripted` 或 `responses` |
| `MINEINTENT_MODEL_API_KEY` / `_FILE` | 无 | 密钥本身，或密钥文件路径 |
| `MINEINTENT_MODEL_ENDPOINT` / `_NAME` / `_REASONING_EFFORT` | 见上 | 覆盖模型接入参数 |
| `MINEINTENT_CONFIG` | `./mineintent.toml` | 配置文件路径 |
| `MINEINTENT_DEBUG` | 关 | 见下节 |
| `MINEINTENT_LOG` / `RUST_LOG` | 见下节 | 分级日志过滤器（EnvFilter 语法） |
| `MINEINTENT_MAX_RUNTIME_SECS` | 无 | 到时按正常停机路径退出，供无人值守验收 |

**密钥只从环境变量或文件路径读，不从配置文件读**——配置文件是要进版本库的。

启动：

```bash
cargo run -p mineintent-app --bin mineintent
```

## 模型接入

provider 按**协议形状**分层，不按供应商：`crates/app/src/model/responses.rs`
对接 OpenAI 系的 `/responses` 协议，DeepSeek 只是当前用这个形状的一家，
换供应商改配置即可。chat/completions 形状已随 `deepseek-chat` 退役一并移除。

provider 内部把 Responses 的 `instructions + input[]` / `output[]` 与
Agent 状态机认的 `message { content, tool_calls }` 互转，
因此新增供应商不会波及上层。

## 开发者模式

`MINEINTENT_DEBUG=1` 打开后，数据目录下会出现：

- `dev.log`：装配、生命周期心跳、每轮模型请求与响应摘要、故障流；
- `model-io/`：每轮模型请求与响应的**原文**。

它与 journal 分工不同：journal 是产品事实的持久记录（有 schema、要迁移），
dev.log 是排障用的过程记录，不承诺格式稳定，默认关闭。排障先看它。

## 分级日志

排障输出走 `tracing`，写 stderr，用 `MINEINTENT_LOG`（其次 `RUST_LOG`）过滤，
标准 EnvFilter 语法：

```bash
MINEINTENT_LOG="warn,mineintent_middle=debug" cargo run -p mineintent-app
```

默认 `warn` 加本项目三个 crate 到 `info`——azalea/bevy 在 debug 级别每 tick
都有输出，默认放开会淹掉要看的东西。

**它和 journal 是两条不同的通道**：日志按 severity 分级、可丢弃、格式不承诺
稳定；journal 按事实类型定名、有 schema、要迁移（OTel OTEP-0202 对 events 与
logs 的区分同理）。所以高频的队列摄入量不进 journal，只按类型计数，停机时
在日志里出一条总账。

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
