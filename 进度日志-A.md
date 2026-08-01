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

## 2026-08-01｜I01 独立审查返修

### 配置 oracle 对齐

- 证据：`supplies/mineintent-main/src/minecraft/config.ts`。
- 以消费式 `validate_and_normalize()` 取代容易误解的纯 `validate()`；成功值明确返回 trim 后的 `worldId/server.host/identity.username`。
- worldId 按 trim 后 1..128、username 按 trim 后 1..64 校验；长度按 JavaScript/Zod 的 UTF-16 code unit 口径。移除 ASCII/16 字符限制。
- `profilesFolder` 存在时只要求原字符串非空，不 trim；空串拒绝、纯空格保留。
- reconnect 三个 delay 使用 `u64` 表达整数非负；移除 `initialDelayMs <= maxDelayMs` 的非 oracle 限制。
- 继续固定目标 `26.1.2` / protocol `775`；合法 `microsoft` 枚举在 schema 形状校验后返回 `UnsupportedAuth`。

新增/调整测试覆盖：64 字符用户名通过、65 拒绝、三项 trim 返回值、blank world/host/username、空 profiles、空格 profiles 保留、`initialDelayMs > maxDelayMs` 通过、stableReset 取 0、Microsoft/旧版本拒绝及 strict unknown-field。

### 可等待取消与期限

- `CancellationSignal::cancelled()` 与 `Deadline::elapsed()` 返回现有 `BoxFuture`；文档冻结“已触发首 poll Ready、未触发保持 Pending 并在状态转换时 wake”的契约。
- `OperationControl` 增加 `cancelled()` / `deadline_elapsed()` 转发入口并保留 `preflight()`。
- 测试只用 `std::future::{ready,pending}`、安全手写 `Waker` 和单次 `poll`；未引入 executor、timer、线程或通道。
- `CancellationSignal`、`Deadline`、Backend/Observation/Motor/InformationFacade trait object 编译通过；I02 无源码变更。

### 验证

- `cargo +stable fmt --all --check`：通过。
- `cargo test --workspace --offline`：通过；backend 13、I01 26、I02 17，共 56 条测试通过，doc tests 通过。
- `cargo check --workspace --offline`：通过。
- 仅修改 I01 的 `config.rs`、`api.rs`、专属测试与本日志；manifest、`lib.rs`、I02、B 模块、backend、`Cargo.lock` 均无变更。

## 2026-08-01｜P1-A Information contracts/schema 第一批

### 基线、oracle 与边界

- 开始基线：`ad65f7a 构建：预留 P1 middle 并行模块`，worktree `port/a-world-info` clean。
- 只读 oracle 固定为 `supplies/mineintent-main@6fb3ed0c007601b4e1eb1cb0a9d10525ac2a2467`。
- 本提交只修改 `crates/middle/src/information/mod.rs`、`information/contracts/**`、`tests/information_contracts_schema.rs` 与本日志；未修改 manifest/lock、`src/lib.rs`、I02、backend、B 模块或只读目录。
- `contracts/v1.ts:3-205` 的枚举全集、catalog/selector/query/help/read/error wire DTO，`v1.ts:218-379` 的 grant/caller、字段 schema 抽象、provider/ref SPI、session/invalidation/trace DTO 已冻结。异构 `Partial<T>`/generic selector/page state 在对象安全边界擦除为严格 JSON value/map，不包含 provider 实现或 registry。
- `v1.ts:206-216` 的 scope 直接复用 P0 I02 `InformationScopeSnapshot`；基础 unavailable reason 提供到 I02 facade reason 的显式转换，没有改动冻结契约。
- `schemas.ts:10-55` 对应闭合 enum、literal/version discriminator、strict object、UTF-16 字符长度、数组上限、selector/page 整数约束与 UTC ISO datetime parser。optional 表示缺省而非显式 `null`；请求整数接受 JS 中等价的 `1.0`，拒绝负数、非整数和超出 safe-integer 的值。
- provider/ref/value-schema trait 均为对象安全边界；异步 provider read 复用 P0 的 `BoxFuture + OperationControl`，未引入 executor、timer、线程、通道或新依赖。

### `schemas.test.ts` 逐断言映射

TS oracle 共 **1 个 test / 5 条 assert**，集中对应 Rust `information_request_schemas_are_strict_and_versioned`：

1. `contracts/schemas.test.ts:9-11`：最小 `list_interfaces` 成功。
2. `contracts/schemas.test.ts:12-15`：catalog 伪造 `audience` 因 strict unknown-field 失败。
3. `contracts/schemas.test.ts:16-21`：`current_status`/`status:1`/`health` read 成功。
4. `contracts/schemas.test.ts:22-28`：read 伪造 `worldId` 因 strict unknown-field 失败。
5. `contracts/schemas.test.ts:29-32`：未知 `interfaceId` 的 help 失败。

Rust 另有 6 个边界测试，不冒充 TS 对应项：

- `exported_v1_enumerations_are_complete_and_strict`：17/3/8/9/5/11 项公开全集及闭合枚举。
- `query_parser_preserves_unicode_and_optional_fields`：Unicode、空 help fields、缺省 optional、显式 null 拒绝。
- `selector_and_page_constraints_match_the_zod_schema`：selector/page 边界、JS 整数 `1.0`、safe integer、日历日期。
- `exported_selector_parser_is_strict_and_versioned`：selector v1、unknown-field 与错误版本拒绝。
- `representative_wire_dtos_reject_unknown_fields`：非请求 wire DTO 同样 strict。
- `provider_spi_traits_are_object_safe`：provider/value-schema/ref issuer 三个 trait 可构造 trait object 类型。

### 验证

- `cargo test --package mineintent-middle --offline --test information_contracts_schema`：7 passed。
- `cargo +stable fmt --all --check`：通过。
- `cargo test --workspace --offline`：85 passed（backend 13、agent contracts 14、capability contracts 8、I01 26、I02 17、本批 7），doc tests 通过。
- `cargo check --workspace --offline`：通过。
- `git diff --check`：通过；提交前范围检查无越界文件。

### 本批明确未迁

- `geometry.ts`、self-vitals/inventory/sound source ports、`perception.ts:1-89` 留给下一批，当前未创建相应模块或测试。
- `perception.ts:91+` viewport 投影 kernel 明确禁止进入本批；backend viewport 仍是唯一 kernel。
- stores、provider 实现、catalog/registry、reference/cursor store、runtime/facade 组装及 backend 11 项行为缺口均未实现。
- memory 继续受 `DELTA-01/Q12` gate 约束，未进入 Information contracts/schema。

## 2026-08-01｜P1-A geometry/source-port 第二批

### 基线与写入范围

- 开始基线：`5d9c02c`，worktree `port/a-world-info` clean；沿用上一批已完成的 `supplies/mineintent-main@6fb3ed0` 只读 oracle 调研，未联网。
- 只修改 `information/geometry.rs`、`information/source_ports/**`、`information/mod.rs`、两份 `information_*` 专属测试及本日志。
- 未修改 contracts、manifest/lock、`src/lib.rs`、events/execution/speech、backend、I01-I03、supplies/vendor/移植计划或其他禁区。

### 机械迁移边界

- `information/geometry.ts:1-38`：`Point3`、复用 P0 同值 `RelativeDirection`、三维 distance、Mineflayer yaw/pitch 符号约定及四象限 relative bearing；保留 `[−π/4, π/4)` 等原区间和同点默认 `ahead`。
- `source-ports/self-vitals.ts:1-12`：vitals/experience/effect DTO 与同步对象安全 `SelfVitalsPort`。
- `source-ports/inventory.ts:1-16`：slot/state DTO 与同步对象安全 `InventoryPort`。
- `source-ports/sound.ts:1-16`：sound DTO 与同步对象安全 `SoundHistoryPort`。
- `source-ports/perception.ts:4-89`：pose/block/entity、looked-at/visible DTO、frustum/options/metrics、显式 `PerceptionBlockAt::{Block, Unloaded}` 与同步对象安全 `PerceptionPort`。
- 所有 object DTO 使用 `deny_unknown_fields`；optional 缺省与显式 `null` 分离；所有 TS `number` 保持 `f64`，serde 输入/输出拒绝非有限值，不擅自增加整数、正数或范围产品约束。

### `geometry.test.ts` 映射

TS oracle 共 7 个 test，本批严格迁移前 6 个：

1. `geometry.test.ts:6-9` → `distance_between_computes_3d_euclidean_distance`。
2. `geometry.test.ts:15-27` → `look_direction_matches_mineflayers_own_yaw_pitch_to_direction_formula`。
3. `geometry.test.ts:29-37` → `relative_bearing_classifies_target_position_relative_to_self_facing`。
4. `geometry.test.ts:39-50` → `relative_bearing_agrees_with_the_rightward_axis_of_look_direction_at_every_yaw`。
5. `geometry.test.ts:52-56` → `relative_bearing_rotates_with_self_yaw`。
6. `geometry.test.ts:58-61` → `relative_bearing_defaults_to_ahead_when_target_is_exactly_at_self_position`。

`geometry.test.ts:65-74` 的 `bearing labels agree with the right axis of view-relative coordinates` 依赖生产函数 `perception.ts:501 viewRelativePosition`。本批禁止迁移 perception 91 行后的投影 kernel，因此该第 7 项明确留作后续 **VIEW-01** 回归；没有在测试中复制公式冒充覆盖。

### Rust-only source-port 契约测试

以下 5 条不计入 TS 映射：

- `rust_contract_source_port_traits_are_object_safe`。
- `rust_contract_source_port_dtos_are_strict_and_preserve_unicode`。
- `rust_contract_optional_fields_reject_explicit_null`。
- `rust_contract_numeric_dtos_reject_non_finite_serialization`。
- `rust_contract_perception_unloaded_is_an_explicit_closed_enum`。

### 验证与未迁边界

- 定向：`information_geometry` 6 passed；`information_source_ports` 5 passed。
- `cargo +stable fmt --all --check`：通过。
- `cargo test --workspace --offline`：98 passed（本批新增 11），doc tests 通过。
- `cargo check --workspace --offline`：通过。
- 明确未迁：`perception.ts:91+` 的常量、扫描、raycast、投影与 `viewRelativePosition`；stores/providers/catalog/runtime/backend gap 均未进入本批。backend viewport 仍是唯一生产 kernel。

## 2026-08-01｜P1-A registry/scope/trace 第三批

### 基线、oracle 与范围

- 开始基线：`c8e29c8`，worktree `port/a-world-info` clean；只读 oracle 为 `supplies/mineintent-main@6fb3ed0`，未联网。
- 只修改 `information/{registry,scope,trace,mod}.rs`、`tests/information_{registry,scope_trace}.rs` 与本日志；未修改 contracts/source_ports/geometry、manifest/lock/lib、B 模块、backend 或其他禁区。

### 迁移内容

- `registry.ts:43-133`：冻结 definition 副本并校验 description/schema revision、audience/dependency 重复、字段元数据/source kind、正数 limits、pagination 与 selector kind。
- `registry.ts:135-200`：重复 provider id、seal/register/read 时序使用结构化错误；descriptor 在 seal 前可读，provider/revision/version 读取要求 sealed，`seal` 与公开调用使用同一确定性 descriptor 构造。
- provider 以 `Arc` 持有，registry state 以 `RwLock` 保护；调用 `definition()` 在加锁前完成，返回 provider 后 availability/read 不持 registry lock，锁内不调用 provider/user code。
- descriptor 按 wire id、field id 的 JavaScript UTF-16 字符串顺序确定性排列；catalog canonical copy 另将 audiences/fieldIds 排序，不改变公开 descriptor 中原 audience 顺序。
- SHA-256 输入严格为紧凑 JSON：根字段 `targetMinecraftVersion/providers`，descriptor 字段 `id/description/schemaRevision/audiences/fieldIds`；digest 取小写十六进制前 16 位。固定 fixture 得到 `catalog:1.21.1:5c2f95176291633f`，并以本地 Node 内置 `crypto` 离线交叉核对。
- `scope.ts:6-54`：对象安全 scope source、owned capture/update；process session 永远参与比较，其余仅按 connection/world/dimension/ui/screen dependency 比较原字段组合。
- `trace.ts:3-32`：对象安全 sink、默认 1024/可配置正容量、新est-retained FIFO read trace、owned records 快照与 noop sink；锁 poisoning 恢复不 panic。

### 测试映射

TS `registry.test.ts` 共 **1 个 test，本批 1/1 完整对应**到 `registry_is_deterministic_sealed_and_rejects_duplicate_providers`：

1. `registry.test.ts:50-58`：相反注册顺序分别 seal。
2. `registry.test.ts:60`：catalog revision 相等。
3. `registry.test.ts:61`：descriptor id 稳定排序为 `current_status, ui_context`。
4. `registry.test.ts:62`：seal 后 register 返回 sealed error。
5. `registry.test.ts:64-66`：重复 provider id 拒绝。
6. `registry.test.ts:67`：seal 前 provider read 拒绝。

Rust 另有 7 条 contract/characterization，不冒充 TS 对应项：

- registry：固定 canonical hash 与 field 排序、完整 definition 结构化校验、seal/read 生命周期错误，共 3 条。
- scope：owned mutable source、五类 dependency 精确比较，共 2 条 characterization。
- trace：固定容量/顺序/快照、对象安全 noop sink，共 2 条 characterization/contract。
- oracle 目录无独立 `scope*.test.ts` 或 `trace*.test.ts`，因此上述 scope/trace 测试不计 TS 映射。

### 验证与未迁边界

- 定向：`information_registry` 4 passed；`information_scope_trace` 4 passed。
- `cargo +stable fmt --all --check`：通过。
- `cargo test --workspace --offline`：115 passed（本批新增 8），doc tests 通过。
- `cargo check --workspace --offline`：通过。
- 未迁：access-policy、reference/cursor store、provider 实现、catalog service/runtime/facade 组装及 backend gaps；本批没有扩展到任何 provider/user 行为。

## 2026-08-01｜registry 独立审查返修

- 对齐 `registry.ts:141-149`：`register()` 先在不调用 provider 的读锁 preflight 检查 sealed；释放锁后才读取、冻结、验证 definition；最后在写锁内再次检查 sealed，覆盖并发 seal 竞态。provider/user code 始终不在 registry lock 内执行。
- 对齐 `registry.ts:157,175-185`：公开 `descriptors()` 在未 seal 时即可返回当前已注册 provider 的确定性 descriptor；`provider()`、`catalogRevision()`、`targetMinecraftVersion()` 继续要求 sealed。
- 新增 `review_fix_unsealed_descriptors_are_readable_while_provider_requires_seal`：未 seal descriptor 成功且 provider 返回 `NotSealed`。
- 新增 `review_fix_sealed_register_does_not_read_provider_definition`：计数 provider 在 sealed register 上直接得到 `Sealed`，definition 调用数保持 0。
- 定向 `information_registry`：6 passed。
- `cargo +stable fmt --all --check`：通过；`cargo test --workspace --offline`：117 passed、doc tests 通过；`cargo check --workspace --offline`：通过。
