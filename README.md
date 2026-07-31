# MineIntent Minecraft 后端（Rust / MC 26.1）

这是一个独立的离线 Minecraft Java 客户端后端，当前固定目标为 Paper 26.1.2、协议号 775，协议执行层使用已发布的 `azalea 0.16.0+mc26.1`。

## 构建与运行

```powershell
cargo build --offline
.\target\debug\mineintent-backend.exe --host 127.0.0.1 --port 25565 --username MineIntentBot --duration-secs 30
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
- `server_observed`：服务端协议事件（包括位置修正包），以及 Spawn/Death 时的服务端边界快照；观察源通过 `RuntimeHandle` 暴露。

快照事件的 `source` 只约束该次事件中的事实边界：Tick 快照中的本地运动姿态可能是预测值，必须结合事件来源读取；`RuntimeHandle` 的运动预测轨迹也始终单独标记为 `client_predicted`。

快照协议为 `mineintent.minecraft.snapshot.v1`，运行时句柄提供 `snapshot`、`snapshot_source`、`subscribe`、`observation_source`、`send_chat`、`look_relative`、`move_input` 和 `release_all`；读取快照时必须同时检查来源。块读取返回加载状态、绝对坐标、状态 ID、属性和碰撞几何；它是观察原语，不把“已加载”冒充为“可见”，视线/暴露面判断由上层 viewport 完成。临时运行日志统一放在已被 git 排除的 `server-run/`，不会作为提交物。

里程碑证据和待裁决项见 [`进度日志.md`](进度日志.md)，版本/选型证据见 [`M0-版本与选型.md`](M0-版本与选型.md)。
