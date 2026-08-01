# A 线进度日志

## 2026-08-01｜P0 I01/I02 契约冻结

### 基线与写入范围

- 开始基线：`a76561a 公共模块命名空间预约`，worktree `port/a-world-info` clean。
- 只修改 `crates/contracts/src/minecraft/**`、`crates/contracts/src/information/**`、I01/I02 专属 `tests/testdata` 与本日志。
- 未修改 manifest、`Cargo.lock`、root 配置、agent/capability、backend/middle、supplies/vendor/移植计划。
- `contracts` 原有 `serde/serde_json` 足够；未请求依赖补给，未联网。

### 提交

1. `ea1a131 契约：冻结 I01 Minecraft 后端边界`
   - std-only `BoxFuture`、`CancellationSignal`、`Deadline`、对象安全 Backend/Observation/Motor trait。
   - snapshot 固定 `mineintent.minecraft.snapshot.v1`；event 固定 `mineintent.minecraft.backend-event.v2`，必需 source、发生时 dimension 文档语义及 overflow。
   - offline/microsoft 枚举保留，Microsoft 校验返回 `unsupported_auth`；目标 `26.1.2` / protocol `775`。
   - ready/death/dimension/reconnect/close/overflow 固定序列与 DTO-only scripted fake data builder。
   - 23 条 I01 契约测试；未实现任何连接状态机、事件 producer、背压通道、移动或 viewport kernel。
2. `c17f8a5 契约：冻结 I02 Information 门面`
   - 仅暴露 `scope_snapshot`、`compose_passive_observations`、原子 `read_viewport`。
   - 四类 plain values 与 denied/unavailable/partial/timeout fixture。
   - catalog/ref/cursor/provider/adapter revision 不进入 B 可见面；memory 不进入 I02。
   - 17 条 I02 契约测试。
3. `425d943 测试：收紧 event v1 拒绝负例`
   - v1 负例直接针对单个 event envelope，不依赖宽泛的整文件失败断言。

### Oracle 测试映射

- Backend 12 条契约名：`config_rejects_unknown_fields_and_non_target_version` 至 `twenty_cycles_leave_no_owned_resources`，与 TS `minecraft-backend.test.ts:98-298` 顺序对应。
- Motor 5 条契约名：`look_relative_preserves_right_down_sign_convention`、`pre_cancelled_look_never_dispatches`、`move_presses_all_keys_then_releases_in_reverse`、`cancelled_move_releases_every_pressed_key`、`move_rejects_empty_duplicate_or_invalid_sets`；另有裁定测试 `all_fifteen_direction_subsets_preserve_uncancelled_axes`。
- I02 保留四个 adapter oracle 名，并增加 facade、原子 viewport、scope、四类 omission、泄漏边界与 memory gate 测试。
- 这些是契约/fixture 断言；11 项 backend 行为缺口仍待后续实现测试，不以 scripted data 冒充运行时证据。

### 门禁与 mutation

基线及 mutation 恢复后均执行：

- `cargo +stable fmt --all --check`：通过。
- `cargo test --workspace --offline`：通过；backend 13、I01 23、I02 17，共 53 条测试通过，doc tests 通过。
  - 首次运行因 Azalea 冷编译超过工具 120 秒上限被终止，无测试失败；以 600 秒上限原命令重跑通过。
- `cargo check --workspace --offline`：通过。

Mutation：

1. 临时把 `BackendEventProtocol::V2` 的 serde discriminator 从 `mineintent.minecraft.backend-event.v2` 改为 `.v1`。
2. 运行 `cargo test --package mineintent-contracts --offline --test i01_minecraft_contract event_v2_rejects_v1_discriminator`。
3. 结果按预期 exit 1：固定 v2 fixture 报 `unknown variant ...v2, expected ...v1`，指定测试失败。
4. 恢复 `.v2` 后，同一 targeted test 通过；随后三项 workspace 门禁全部复跑通过。
5. Mutation 未进入提交，恢复后源码无 diff。

### 保留 gates

- `DELTA-01/Q12`：memory refactor gate；本轮没有 memory 类型或实现。
- `Q04`：游戏内停止权限；contracts 只冻结宿主 `stop(reason)`。
- `Q11`：重启/回档后的 AI 身份连续性；仅传递 session/epoch/world 事实。
- `Q13`：玩家 UUID/名称的长期别名归并；DTO 同时保留名称与可选 UUID。
- `Q14`：聊天历史持久化范围；不在 I01/I02 裁定。
- `Q01/Q07`：提示词模板组织属于 B；不阻塞 AgentRunner trait 和 25 条 Python 行为测试迁移。

新《移植任务书.md》覆盖旧附件中“Python Agent Service/25 条 Python 测试本次不迁”的旧说法；本轮只完成 A 所有的 I01/I02，不提前越界实现 AgentRunner。
