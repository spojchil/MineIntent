# MineIntent（Rust workspace / MC 26.1）

本仓库是 MineIntent 全 Rust 单进程移植的 Cargo workspace。目前包含：

- `crates/backend`：Minecraft Java 客户端运行时、命令/生命周期事件、typed observation source，以及 full/directed viewport 原子投影；
- `crates/contracts`：Agent、capability、Information 与 Minecraft backend 的严格进程内契约和 wire DTO；
- `crates/middle`：Agent loop、transcript、prompt、Information provider、生产 `view` capability 与轮末 viewport sampler 适配层。

当前仍缺生产 `ToolDispatcher`、Participant Runtime、app composition root 和 concrete
`MinecraftBackendApi` facade，因此尚未形成 Paper→生产 Agent 的端到端发布。

既有后端固定目标为 Paper 26.1.2、协议号 775，协议执行层使用已发布的 `azalea 0.16.0+mc26.1`。

## 构建与运行

```powershell
cargo build --workspace --locked --offline
cargo test --workspace --all-targets --locked --offline
cargo run -p mineintent-backend --offline -- --host 127.0.0.1 --port 25565 --username MineIntentBot --duration-secs 30
```

程序 stdout 是逐行 JSON 事件流。stdin 可以逐行接收严格的 `BackendCommandEnvelope`：

```json
{"protocol":"mineintent.minecraft.backend-command.v1","id":"chat-1","issuedAt":"2026-08-01T00:00:00Z","command":{"type":"send_chat","message":"hello"}}
{"protocol":"mineintent.minecraft.backend-command.v1","id":"move-1","issuedAt":"2026-08-01T00:00:00Z","command":{"type":"move","directions":["forward"],"durationMs":1000,"sprint":false,"jump":false,"crouch":false}}
```

也可以用 `--send-chat MESSAGE` 做本地 M2 冒烟测试。用户名必须符合 Minecraft 的长度限制（最多 16 个字符）。
命令边界会拒绝空聊天、换行/NUL、重复方向、非法角度，以及不在 50–1500ms 内的移动时长。

## 事实来源边界

- `commanded`：后端主动发出的聊天、视角、移动和停止动作。
- `client_predicted`：Azalea 客户端物理采样，以及运行中由 Tick 产生的当前快照，不当作服务端事实。
- `server_observed`：服务端协议事件（包括筛选后的 `ClientboundPlayerPosition` 位置修正包），以及 Spawn/Death 时的服务端边界快照；观察源通过 `RuntimeHandle` 暴露。

快照事件的 `source` 只约束该次事件中的事实边界：Tick 快照中的本地运动姿态可能是预测值，必须结合事件来源读取；`RuntimeHandle` 的运动预测轨迹也始终单独标记为 `client_predicted`。

快照协议为 `mineintent.minecraft.snapshot.v1`，运行时句柄提供 `snapshot`、`snapshot_source`、`subscribe`、`observation_source`、`send_chat`、`look_relative`、`move_input`、`release_all` 和显式 `respawn`；读取快照时必须同时检查来源。死亡不会自动调用 `respawn`，自动重连与资源包接受也保持关闭，均需由上层明确决定。块读取返回加载状态、绝对坐标、状态 ID、属性和碰撞几何；`RuntimeObservationSource::viewport(&ViewportOptions::default())` 再按主仓库语义执行矩形视锥、暴露面、0.25 格遮挡射线和实体命中盒采样，输出绝对坐标且不穿不透明墙。临时运行日志统一放在已被 git 排除的 `server-run/`，不会作为提交物。

viewport 的 `visibleBlocks` 是 `[BlockInfo, x, y, z]` 整数体素并按距离排序；`BlockInfo` 无视觉属性时为方块名称字符串，有属性时为名称与白名单属性对象。`visibleEntities` 最近优先且是带 `truncated` 的有限列表；未加载方块会让相关可见性射线保守失败，不会被当作空气。`directed` 查询最多接收 16 个唯一世界坐标，并逐坐标返回 seen 或闭合的 unseen reason。Azalea 的角度输入是度，投影内部转换为与主仓库几何函数相同的弧度约定，输出 frame 仍使用 `yawDegrees/pitchDegrees`。

workspace 总进度与边界见 [`进度日志.md`](进度日志.md)；各并行切片的详细证据见根目录 `进度日志-*.md`，版本/选型证据见 [`M0-版本与选型.md`](M0-版本与选型.md)，增量决策台账见 [`需要决策的新问题.md`](需要决策的新问题.md)。
