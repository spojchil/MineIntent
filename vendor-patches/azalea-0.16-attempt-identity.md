# Azalea 0.16 attempt identity / pre-Init cancellation 补丁清单

> 工作树：`production-integration`；基线：
> `.codex-tmp/vendor-baselines/new13-15-pre-deepseek-warm-20260803-2035`。
> 本清单覆盖 NEW-13/15 V1 的 vendor seam：不可变 attempt token + 按
> `(Entity, token)` 精确取消 `CreateConnectionTask`。backend epoch 绑定与
> connect deadline 接线不在本切片内。

## 核心类型

- `azalea_client::join::AttemptToken`：`u64` newtype，`Copy + Clone + Debug +
  Eq + Hash + Component`，进程内 `AtomicU64` 自增 `mint()`。仅在
  `Client::start_client` 与 `rejoin_after_delay` 各铸一次；handler 与事件
  产生处一律携带、绝不重铸。自动重连每次铸新 token。

## azalea-client 文件

| 文件 | 目的 | 关键符号/位置 |
| --- | --- | --- |
| `src/plugins/join.rs` | token 定义；`StartJoinServerEvent` 携带 token；callback 改为 `(Entity, AttemptToken)`；`CreateConnectionTask { task, attempt_token }`；`CancelConnectionTaskEvent(entity, token)` + 取消系统；`ConnectionFailedEvent` 盖章；poll 增加 current-token 复核；单测 | `AttemptToken::mint`（约 L57-80）；`StartJoinServerEvent`（L84-98）；`CancelConnectionTaskEvent`/`cancel_create_connection_task`（约 L128-140/L265-280）；`poll_create_connection_task` stale 跳过（约 L295-315）；`mod tests`（L380+） |
| `src/plugins/connection.rs` | `RawConnection` 携带 token；`handle_raw_packet` 在产生处给 `ReceiveGamePacketEvent` 盖章；`new_networkless_with_token` 供测试显式构造，`new_networkless` 保持 legacy 便利构造（内部 mint 独立 token） | `RawConnection::attempt_token()`（约 L230）；`handle_raw_packet(..., attempt_token, ...)`（L300+） |
| `src/plugins/disconnect.rs` | `DisconnectEvent.attempt_token: Option<AttemptToken>`；清理仅在事件 token 与 entity 当前 token 匹配（或两者皆无的 legacy 测试路径）时执行；连接死亡自动 disconnect 从 `RawConnection` 盖章 | `remove_components_from_disconnected_players` 匹配规则（L95-125）；`disconnect_on_connection_dead`（L150+） |
| `src/plugins/packet/game/events.rs` | `ReceiveGamePacketEvent`/`WorldLoadedEvent` 增加必填 `attempt_token` | 两个事件结构体 |
| `src/plugins/packet/game/mod.rs` | `process_packet(..., attempt_token)`；`GamePacketHandler` 持有 token；两处 `WorldLoadedEvent` 与 `ClientboundDisconnect` 事件在产生处盖章 | `GamePacketHandler.attempt_token`（L90）；`WorldLoadedEvent` 写点（约 L285/L1430） |
| `src/plugins/packet/login/mod.rs` | `process_packet(..., attempt_token)`；`LoginPacketHandler` 持有 token；`ClientboundLoginDisconnect` 盖章 | `LoginPacketHandler.attempt_token`；`login_disconnect` |
| `src/plugins/packet/config/mod.rs` | 同上，`process_packet`/`process_raw_packet` 增加 token；`ClientboundDisconnect` 盖章 | `ConfigPacketHandler.attempt_token`；`disconnect` |
| `src/test_utils/simulation.rs` | `Simulation` 持有 `attempt_token`，实体插入 `AttemptToken` 组件，`RawConnection` 用同一 token；`disconnect()` 写 `Some(token)` | `Simulation::attempt_token`；`create_local_player_bundle(..., token)` |
| `tests/simulation/attempt_identity.rs`（新增） | 集成验收：packet/WorldLoaded 事件 token 与实体一致；stale `DisconnectEvent(A)` 不移除 B、matching `DisconnectEvent(B)` 正常清理 | 两个 `#[test]` |
| `tests/simulation/mod.rs` | build.rs 生成文件，加入 `mod attempt_identity;`（字母序首位） | 生成产物 |

## azalea 文件

| 文件 | 目的 | 关键符号/位置 |
| --- | --- | --- |
| `src/client_impl/mod.rs` | `Client` 增加私有 `attempt_token: Option<AttemptToken>`；`new_with_attempt_token`/`with_optional_attempt_token` 构造；只读 `attempt_token()` getter；`start_client` 铸 token 并经 callback 收回 `(entity, token)`；`disconnect()` 盖章 | `Client::attempt_token()`（L165+）；`start_client`（L225+）；`mod tests`（L590+） |
| `src/auto_reconnect.rs` | 自动重连每次 mint 新 token 写入 `StartJoinServerEvent` | `rejoin_after_delay` |
| `src/events.rs` | 共享 `attempt_matches_current` helper；`packet_listener`/`disconnect_listener`/`connection_failed_listener` 消费时比较 event token 与实体当前 `AttemptToken`，tokenless legacy 仅当实体也 tokenless | helper（L40+）；三个 listener |
| `src/container.rs` | `handle_menu_opened_event` 消费时比较 token，stale A packet 不得移除当前 B 的 `WaitingForInventoryOpen` | `handle_menu_opened_event` |
| `src/swarm/mod.rs` | `SwarmEvent::Disconnect(Box<Account>, Box<JoinOpts>, Option<AttemptToken>)` 携带旧 attempt 的 Client token，天然区分 A/B；`event_copying_task` 从持有的 Client 读取；`IntoIterator` 从实体当前 `AttemptToken` 组件构造 | 枚举变体（L70+）；`event_copying_task`（L260+）；`mod tests`（L360+） |
| `examples/testbot/main.rs` | 迁移 `SwarmEvent::Disconnect` tuple pattern（第三个元素 `_attempt_token`） | `swarm_handle` |

## checksum 更新（严格对应实际改动文件）

- `azalea-client/.cargo-checksum.json` 的 `files` 映射：9 个既有条目 hash
  修改 + 1 个新条目（`tests/simulation/attempt_identity.rs`），共 10 条目；
  `package` 字段未动。`.cargo-checksum.json` 自身不在其 `files` 映射中，
  若按工作树文件 diff 计数则另算 1 个。
- `azalea/.cargo-checksum.json` 的 `files` 映射：6 个既有条目 hash 修改
  （`auto_reconnect.rs`、`client_impl/mod.rs`、`swarm/mod.rs`、`events.rs`、
  `container.rs`、`examples/testbot/main.rs`）；`package` 字段未动。checksum
  文件自身另计。
- 全部条目已用 sha256 全量复核，`checksum mismatches=0`。

## 父级审查返修 2：packet source fence（2026-08-03）

- `handle_raw_packet` 在函数最前面做 fail-closed source admission：只有
  `ecs.get::<AttemptToken>(entity).copied() == Some(attempt_token)` 才继续；
  entity 不存在、无 token、或 token 不匹配都直接安全忽略（不反序列化、不调
  用任何 process handler、不排队消息）。这是消息产生处最靠近 parser 的闸门，
  挡在 `game/login/config::process_packet` 可能触发的 ECS 状态修改/派生事件
  之前。
- 新增 3 个 connection 单测：
  `stale_attempt_packets_are_ignored_before_parsing`（stale A + invalid bytes
  返回 Ok 且零事件；同一 bytes 用 matching B 到达 parser 返回 Err，证明短路
  在反序列化前）、`missing_or_tokenless_entities_are_fail_closed`、以及
  `stale_serializable_packet_does_not_mutate_ecs_and_matching_applies`
  （可序列化 SetHealth：stale A 不改 `Health` 组件、matching B 正常应用并
  排队带 B token 的事件）。
- Mutation：临时移除 source admission，`stale_attempt_packets_are_ignored_before_parsing`
  在 `result.is_ok()` 断言失败（stale bytes 进入了 parser）；恢复后通过。
  变异前后已上报 `temporary_change_active` / `=false`。
- 测试边界：source fence 的证明使用真实 `handle_raw_packet` +
  真实反序列化/SetHealth handler（非伪事件）；单测直接构造
  `QueuedPacketEvents` 并断言事件/ECS 效果，不依赖网络。

## NEW-13 V2 backend 接线说明（2026-08-03，仅追加说明，未改 vendor）

### 数据结构与增长边界

- `SourceTokenBindings`（`crates/backend/src/runtime.rs`，`EntityProducerRuntimeState`
  内）：`token_to_epoch: HashMap<AttemptToken, u64>` +
  `epoch_to_token: HashMap<u64, AttemptToken>`。每个 stamped join attempt 至多
  增加一对条目，条目在 RuntimeSession 生命周期内**永不删除**（历史 token 必须
  保留以防重绑），增长边界 = 会话内成功进入 attempt 流的 stamped join 数。
- `AttemptAdmissionState::Reserved/Bound` 新增 `attempt_token:
  Option<AttemptToken>`；`CanonicalSourceAdmission` 新增 `attempt_token`。
- backend 复用 vendor opaque `azalea::join::AttemptToken`（Hash/Eq/Copy）做
  key，不读取其内部 u64，也不引入第二套 source generation；backend 既有
  reconnect control token（u64）与 vendor token 是不同层级，命名分开。

### 一对一绑定语义

- `bind(token, epoch)`：token 已绑他 epoch 或 epoch 已绑他 token 均返回
  false 且不修改；同 pair 幂等。
- 绑定时机：`admit_canonical_join_started_with_token`（`StartJoinServerEvent`
  reader）首次登记；`bind_reserved_attempt_locked` 再次幂等确认。
- 消费点全部比较真实来源 token：stamped packet/WorldLoaded/Disconnect/
  ConnectionFailed、`Client::attempt_token()`（Init/Login/Spawn/Chat/Death/
  Tick/AddPlayer/RemovePlayer/UpdatePlayer/ConnectionFailed 高层副作用前）、
  `SwarmEvent::Disconnect` 第三字段、reconnect return 的 Client token。

### fallback 保留位置

- `EntitySourceFence` 仅服务 tokenless legacy 路径：首次未复用 tokenless
  连接可用；same-entity rebind 后 tokenless StartJoin/source/Client 全部
  fail closed，绝不把当前 B token 补给 A 事件。stamped 路径不再经过
  `allows_unstamped`（但 `bind_reserved_attempt_locked` 的 init_path fence
  检查已限定为 tokenless，避免误拒 stamped B）。

### 未完成

- pre-Init connect deadline（NEW-15）未接线：`spawn_phase_deadline` 对
  `TransportPhase::Connecting` 仍不调度。backend token↔epoch 绑定本身已完成
  并有 7 个新增 deterministic 测试（含 admission→publication race、A/B 复用、
  一对一映射、reconnect mismatch、tokenless fallback），backend lib 189 passed。

## 父级审查返修 1（2026-08-03）

- 精确取消升级为三条件匹配：entity + 实体当前 `AttemptToken` +
  `CreateConnectionTask.attempt_token` 全部相等才移除；新单测
  `cancel_requires_task_owned_and_current_token_match` 构造 `current=B, task=A`，
  证明 cancel(B) 不删 A、cancel(A) 也不能越过 current fence，matching 才生效。
- `AttemptToken::mint` 改用 checked `try_update`（`checked_add`，到 `u64::MAX`
  不再推进、不 wrap、不重复）；`mint_from(&AtomicU64)` 边界 helper + 单测
  `checked_mint_never_wraps_or_reissues`。
- 消费端 token fence：`auto_reconnect`（stale 不装 timer；
  `InternalReconnectAfter` 保存来源 token，触发时复核，stale 则移除且不发
  `StartJoinServerEvent`）、`events.rs` 三个 listener（stale A 不进 B channel）、
  `container.rs`（stale packet 不改 B 组件）；共享 `attempt_matches_current`
  规则（tokenless 仅当实体也 tokenless）。新增 6 个 azalea 单测。
- `Client::new(entity, ecs)` 改为构造时只读快照实体当前 token 并永久保留，
  `disconnect()` 不再于调用时读取 current；新增快照测试。
- `ReceiveGamePacketEvent` 文档示例补 `..`，避免 doctest/API 文档过期。

## 关键竞态与不变量（测试如何证明）

1. **匹配取消即 drop**：`cancel_create_connection_task` 在
   `poll_create_connection_task` 之前移除匹配组件；组件内 `bevy_tasks::Task`
   drop 即取消/丢弃底层连接 future。单测用受控 pending future + drop probe
   （`Arc<AtomicBool>` guard）证明 future 实际被 drop；随后同 Entity 换 B，
   stale cancel(A) 不碰 B，matching cancel(B) 仍生效。
2. **迟到 A 不装 B**：`poll_create_connection_task` 在安装 `RawConnection`
   前复核 `当前 AttemptToken == task.attempt_token`，不匹配则跳过（连
   `ConnectionFailedEvent` 也不发）。单测以 A 的 task 已 resolve 但实体已是
   B 的场景驱动，断言无 `RawConnection`、无失败事件、B token 不变。
3. **stale disconnect 不伤 B**：`DisconnectEvent` 在产生处盖章
   （`Client::disconnect` 用 Client 持有的 token；kick/连接死亡用 handler /
   `RawConnection` 的 token）；清理系统按 entity 当前 `AttemptToken` 精确
   匹配。集成测试先发 `Some(A)`（保留 B），再发 `Some(B)`（正常清理）。
4. **高层 handoff 不伪装**：`Client` 句柄携带创建它的 attempt token
   （getter）；`SwarmEvent::Disconnect` 携带事件拷贝任务持有的旧 Client
   token；单测直接断言。`Event::Init/Login` 与客户端 handler 成对收到的
   `Client` 携带同一 token（`event_copying_task` 持有该 attempt 的 Client）。

## 迁移注意

- `ReceiveGamePacketEvent`、`WorldLoadedEvent`、`ConnectionFailedEvent` 新增
  必填 `attempt_token` 字段；`DisconnectEvent` 新增
  `attempt_token: Option<AttemptToken>`（legacy 测试可显式传 `Some(mint())`，
  生产不得缺省为“读取时的当前 attempt”）。
- `StartJoinServerEvent` 新增必填 `attempt_token`，callback 通道类型改为
  `(Entity, AttemptToken)`。
- `process_packet`（game/login/config）与 `handle_raw_packet` 新增 token 参数。
- `Client::new(entity, ecs)` 在构造时只读快照 entity 当前 token 并永久保留；
  entity 无 token 时才为 `None`。生产路径一律走 `start_client`/
  `add_with_opts` 返回的 token-carrying Client，`disconnect()` 使用快照。
- `SwarmEvent::Disconnect` tuple 增加第三个元素。
- backend 侧仅做保持编译迁移（测试构造传 `synthetic_attempt_token()`，
  `SwarmEvent::Disconnect` pattern 增加占位）；未开始 token admission /
  epoch 绑定 / deadline 接线。
- 验证命令（vendor 非 workspace 成员，须用 scratch 副本测试）：
  - `cargo test --manifest-path .codex-tmp/vendor-test/azalea-client/Cargo.toml --lib --offline`：9 passed
  - `cargo test --manifest-path .codex-tmp/vendor-test/azalea-client/Cargo.toml --test main --offline`：25 passed
  - `cargo test --manifest-path .codex-tmp/vendor-test/azalea/Cargo.toml --lib --offline`：29 passed
  - `cargo test -p mineintent-backend --lib --locked --offline`：182 passed
  - `cargo check --workspace --all-targets --locked --offline`：exit 0
  - 默认/stable `cargo fmt --all -- --check`、vendor rustfmt `--check`、`git diff --check` 全部通过
- 环境限制：`eyre`/`criterion`/`backtrace` 未 vendored，azalea examples 的
  testbot 无法在本环境离线编译（示例已做最小 pattern 迁移，未做完整编译门禁）。
