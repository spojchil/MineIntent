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

## 2026-08-01｜P1-A Information ref-store 原子迁移

### 基线、oracle 与范围

- 开始基线：`83e8746`，worktree `port/a-world-info` clean；完整只读 oracle 为 `supplies/mineintent-main/src/information/ref-store.ts:1-231`，未联网、未生成 TS/JS。
- 只修改 `information/ref_store.rs`、`information/mod.rs`、`tests/information_ref_store.rs` 与本日志；未修改 contracts/scope、manifest/lock/lib、B 模块、backend、supplies/vendor/移植计划或其他禁区。
- 明确没有实现 cursor-store、access-policy、provider 或 runtime。

### 完整生产语义

- 默认限制精确为：global 2048、每 principal 512、每 source interface 256、payload 8192 UTF-8 bytes、每 issuer 32 次、TTL 60000ms；所有配置必须为正值。
- `InformationRefIssuer` 每个实例独立原子计数，且像 TS 一样在实际 issue 校验前消耗次数；fallible `issue -> Result` 使用现有 `InformationReferenceIssueRequest/SelectorRef/Grant/ScopeSnapshot`，不以 panic 表达 TS throw。
- issue 在容量判断前清理 TTL 到期项；容量分别按全局、principal、source interface 计数。payload 经紧凑 JSON 序列化、UTF-8 byte 长度检查和反序列化深拷贝；默认 validUntil 生成 UTC millisecond ISO 字符串，显式 lifetime 不得超过 TTL。
- selector id 为 `iref_<uuid-v4>`；完整 ref 内容不可篡改，绑定 issuer/source interface、principal、grant id/audience、allowed target、connection epoch、world、dimension，以及可选 screen instance/revision；accepted kind 参与 resolve。
- resolve 对到期 ref 执行删除，返回 payload 深拷贝；`size()` 与 TS 一样只读当前条目数，不主动 TTL cleanup。
- grant-ended、connection-changed、world-changed、screen-changed 四类 invalidation 均按 oracle 精确 retain/remove；非 screen-bound ref 不受 screen invalidation。另按任务明确要求提供结构化 `clear()` 与 `size()`。
- store 使用 `Arc + Mutex`，clock 可注入；生产路径无 `unwrap()/expect()/panic!`，lock poisoning 返回结构化错误。

### `stores.test.ts` 前三条一对一映射

TS 本批 **3/3 tests**：

1. `stores.test.ts:27-81` → `opaque_references_bind_principal_grant_scope_target_and_full_ref_content`，逐项覆盖成功 resolve、ref revision 防篡改、target/principal/dimension 拒绝及 screen invalidation。
2. `stores.test.ts:83-102` → `screen_bound_references_require_a_concrete_screen_revision`。
3. `stores.test.ts:104-156` → `reference_limits_isolate_principals_and_interfaces_and_bound_per_read_payloads`，覆盖 per-issuer、per-principal 与 payload byte cap。

另有 5 条 Rust contract 测试，不冒充 TS 对应项：

- 默认常量、TTL 边界/cleanup、accepted kind、payload 深拷贝及 size 不主动清理。
- global/per-interface 独立容量。
- grant/audience/epoch/world/screen/kind 与完整 ref 字段绑定；process/ui/capturedAt 明确不额外绑定。
- 四类 invalidation 的 retain/remove、clear/size。
- metadata/lifetime/options 结构化错误与 Unicode JSON UTF-8 byte cap。

### 验证与剩余边界

- `cargo test --package mineintent-middle --offline --test information_ref_store`：8 passed。
- `cargo test --workspace --offline`：133 passed（本批新增 8），doc tests 通过。
- `cargo check --workspace --all-targets --offline`：通过。
- `cargo +stable fmt --all -- --check`：通过。
- 剩余未迁：`stores.test.ts:158+` 对应 cursor-store，以及 access-policy、provider/runtime；均未进入本提交。

## 2026-08-01｜Information ref issuer 端口独立审查返修

- 对齐 `contracts/v1.ts:312-324` 与 `ref-store.ts`：`InformationReferenceIssuer` 保持对象安全同步端口，签名冻结为 `issue(request) -> Result<InformationSelectorRef, InformationReferenceIssueError>`；provider context 现在可以注入真实、可失败的 store issuer，而不需要 panic、吞错、伪造 ref 或字符串 boxed error。
- 在 information contracts 中定义共享结构化 `InformationReferenceIssueError`，统一承载 per-issuer、metadata/target/screen、容量、payload、lifetime/timestamp 与 store unavailable 等签发失败；`InformationRefStoreError` 仅保留 constructor 的 `InvalidLimits` 以及 resolve/invalidation/clear/size 的结构化锁错误，避免签发错误在 store 与端口重复且漂移。
- `InformationRefIssuer` 实现 `InformationReferenceIssuer`，trait 方法直接委托其固有 `issue`，两条入口经过同一计数、校验、容量和写入生产路径。
- `information_contracts_schema` 的对象安全 contract 测试增加 fallible issue 函数指针证明；`information_ref_store` 新增 `review_fix_ref_issuer_is_a_fallible_object_safe_provider_port`，运行期证明 `&InformationRefIssuer` 可注入为 `&dyn InformationReferenceIssuer`，并观察成功 ref、`PerIssuerLimitExceeded` 与 `CapacityExceeded`。
- 定向 `information_ref_store`：9 passed；定向 `information_contracts_schema`：7 passed。
- `cargo test --workspace --offline`：134 passed、doc tests 通过；`cargo check --workspace --all-targets --offline`：通过；`cargo +stable fmt --all -- --check`：通过。
- 未扩大范围：cursor-store、access-policy、provider/runtime 与其他 information/backend 模块均未修改。

## 2026-08-01｜P1-A Information cursor-store 原子迁移

### 基线、oracle 与范围

- 开始基线：`f52c25f`，worktree `port/a-world-info` clean；完整只读 oracle 为 `supplies/mineintent-main/src/information/cursor-store.ts:1-177` 与 `stores.test.ts:158-233`，未联网、未生成 TS/JS。
- 只修改 `information/cursor_store.rs`、`information/mod.rs`、`information/ref_store.rs` 的最小 crate-private helper、独立 `tests/information_cursor_store.rs` 与本日志；未进入 access-policy、provider、runtime、B/main 或其他禁区。

### 完整生产语义与共享 helper

- 默认/可配置限制精确为：global 2048、每 principal 跨 interface 512、每 interface 256、page-state 8192 UTF-8 JSON bytes、TTL 60000ms；构造和 issue/resolve/invalidate/size 全部使用结构化 `Result`，`Arc + Mutex` 保证容量检查与消费原子性，生产路径无 panic。
- issue 在容量判断前清理过期 cursor；验证正 limit，紧凑 JSON 序列化后按 UTF-8 byte 限制并深拷贝 page-state；生成 `icur_<UUID-v4>` opaque id。`size()` 与 TS 一样不主动清理，TS 没有公开 `clear()`，Rust 未扩增该 API。
- cursor 精确绑定 interface、fields 原顺序、selector **仅 id**、information revision、limit、principal、grant id/audience、connection epoch、world、dimension、screen id/revision。无效 resolve 不消费；匹配成功先原子移除再返回 state/revision，二次 resolve 失败。
- 四类 invalidation 精确对齐 TS。特别是 `screen_changed` 对 stored screen id/revision 全量比较，因此无 screen cursor 遇到有 screen event 也会失效；未套用 ref-store 的 screen-bound 特例。
- 从 ref-store 提取的 helper 仅为 crate-private 泛型错误映射版本：复用既有 `InformationRefClock`、UTC millisecond ISO formatter/expiry parser 与 bounded JSON clone；没有复制第二套日历解析器，也没有改变 ref-store 的公开 API 或行为。

### `stores.test.ts` 最后两条一对一映射

TS 本批 **2/2 tests**：

1. `stores.test.ts:158-197` → `cursors_bind_query_shape_and_are_one_time_continuations`：错误 fields 不消费、正确 resolve 返回 state/revision、成功后二次失败；另锁定 `icur_` UUID-v4 形状。
2. `stores.test.ts:199-233` → `cursor_state_and_per_principal_capacity_are_bounded`：principal 容量跨 interface 生效，page-state byte cap 返回结构化错误。

另有 5 条 Rust contract 测试，不冒充 TS 对应项：默认/metadata/TTL cleanup/size；global 与 per-interface 容量；fields 顺序及全部 resolve 绑定和 selector-id-only；四类 invalidation 与 cursor 特有 screen 规则；Unicode JSON UTF-8 byte 精确边界。

### 验证与剩余边界

- `cargo test --package mineintent-middle --offline --test information_cursor_store`：7 passed。
- `cargo test --package mineintent-middle --offline --test information_ref_store`：9 passed。
- `cargo test --workspace --offline`：148 passed，doc tests 通过。
- `cargo check --workspace --all-targets --offline`：通过。
- `cargo +stable fmt --all -- --check`：通过。
- 剩余未迁：access-policy，以及 provider/runtime；本批均未实现。

## 2026-08-01｜cursor clock 独立审查返修

- 修正 `InformationCursorStore::issue`：锁前只采样一次注入 clock，并以同一个 `now` 同时执行 TTL 过期清理和计算 `validUntil`；entries mutex 内不再调用用户 clock。对空 store 首次签发精确对齐 TS：`evictExpired` 无条目时不取时，随后 TTL 只取时一次。
- 新增 `review_fix_first_issue_samples_clock_once_and_uses_that_value_for_ttl`：side-effect counting clock 证明首次 issue 后调用数为 1；第二次采样只发生在 resolve 的精确 TTL 边界，cursor 判定过期，从而锁定 TTL 来源是首次采样值。
- 未改公开 API、共享 helper 或其他 cursor/ref-store 行为；范围仅为 `cursor_store.rs`、`information_cursor_store.rs` 与本日志。
- 定向 `information_cursor_store`：8 passed（原 7 + review 回归 1）；定向 `information_ref_store`：9 passed。
- `cargo test --workspace --offline`：149 passed，doc tests 通过；`cargo check --workspace --all-targets --offline` 与 `cargo +stable fmt --all -- --check` 均通过。

## 2026-08-01｜P1-A Information access-policy 原子迁移

### 基线、oracle 与范围

- 开始基线：`2ed2999`，worktree clean；完整只读 oracle 为 `supplies/mineintent-main/src/information/access-policy.ts`，未联网、未生成 TS/JS。
- 只修改 `information/access_policy.rs`、`information/mod.rs`、`information/ref_store.rs` 的 crate-private UTC parser 可见性、独立 `tests/information_access_policy.rs` 与本日志；未进入 provider/runtime/context-composer、B/main 或其他禁区。

### 完整生产语义

- 定义 object-safe `InformationAccessPolicy`，端口与 TS 一致只含 `resolve/authorize`；`InMemoryInformationAccessPolicy` 提供 fallible `put/revoke`，以 `Arc<RwLock<HashMap<...>>>` 保证线程安全，锁 poisoning 使用带闭合 operation 的结构化错误，生产路径无 panic。
- `put` 深 clone grant 及其 interfaces/field vectors，按相同 id 替换；`resolve` 同时匹配 grant id/principal 并返回 owned deep clone，调用方后续修改不能污染 store；missing revoke 为 no-op。
- 闭合 authorization operation 为 catalog/help/read，三者采用完全相同规则；闭合 result 为 allowed 或 denied，denial reason 仅 `audience_denied`，对齐 oracle。
- authorize 完整覆盖：expiry、provider audience、allowedInterfaces `*`/列表、可选 connection epoch/world/screen 绑定，以及按 provider interface 查询的 field allowlist。allowlist 未配置或只配置其他 interface 时不限制当前 provider；配置后任何请求字段不在列表即拒绝。
- expiry 复用 ref-store 已验证的 crate-private UTC parser，没有复制日期实现；grant `validUntil` 或 scope `capturedAt` 任一无法解析时都不因 expiry 分支误拒绝，对齐 JavaScript `NaN` 比较结果。ref/cursor 公开 API 与行为不变。

### 测试归类与 deferred

- `access-policy.ts` **没有独立 TS test，因此直接 TS 一对一映射为 0 条**。
- 新增 **6 条 source-characterization**：put/resolve/replace/revoke；expiry 与无效 timestamp；audience/interface；三类 scope binding；per-interface field allowlist；catalog/help/read 同规则。
- 另有 **1 条 Rust contract**：trait object/Send+Sync 注入及 put/resolve 两侧 deep-clone isolation。以上 7 条均不冒充 TS test 映射。
- `runtime.test.ts:179` 的 `effective catalog revisions change with grant-visible fields and purpose is bound` 属于阶段 4 runtime 组合断言，明确 deferred；本批未复制或替代该覆盖。

### 验证与剩余边界

- 定向 `information_access_policy`：7 passed；ref/cursor 回归：9 + 8 = 17 passed。
- `cargo test --workspace --offline`：156 passed，doc tests 通过。
- `cargo check --workspace --all-targets --offline` 与 `cargo +stable fmt --all -- --check`：通过。
- 剩余未迁：provider、runtime、context-composer，以及上述 runtime:179 阶段 4 组合回归。

## 2026-08-01｜阶段 2 Information 中立 support 整理返修

### 范围与结构

- 开始基线：`aef6811`，worktree clean；实现代理未联网，supplies/vendor/移植计划只读，未生成 TS/JS，未进入 provider/runtime/context-composer/kernel。主会话审查时只读核对了官方 `boa-dev/ryu-js` 与仓内 vendored `ryu` 的核心实现，临时 checkout 仅位于 gitignored `target` 并在交付前删除。
- 新增 crate-private `information/support.rs`，统一承载 clock、UTC millisecond parse/format/expiry 与 bounded JSON clone；ref/cursor/policy 不再跨模块依赖 `ref_store` 私有 helper。
- information 根公开中立 `InformationClock/SystemInformationClock`；ref-store 公开兼容 alias `InformationRefClock/SystemInformationRefClock`，旧 trait/type 名与中立类型是同一项。ref/cursor options 的公开字段均使用 `Arc<dyn InformationClock>`。
- 更新 information 模块文档为阶段 1/2 已落地边界；`PayloadNotJsonSerializable` 与 `PageStateNotJsonSerializable` 明确标注为 TypeScript `unknown` parity-reserved，当前 `serde_json::Value` Rust API 不可达。公开 wire DTO/serde 未改变。

### Date.parse 与 JSON.stringify parity

- 中立 parser 保留既有 RFC3339/offset/毫秒语义，并新增 ECMAScript 标准 `YYYY-MM-DD` 按 UTC midnight 解析；`2026-07-13` 固定为 `1783900800000`。完全无效字符串仍返回 None，使 access-policy 保持 JavaScript NaN 比较的“不因 expiry 误拒绝”。
- access-policy source-characterization 新增 date-only 过期拒绝；ref-store 新增 date-only `validUntil` 可签发并在固定 UTC midnight 到期的 oracle characterization。
- bounded JSON 使用自定义 `serde_json::Formatter`：所有 `Value::Number` 先按 JavaScript Number 的 IEEE-754/NumberToString 语义输出，再以该紧凑 UTF-8 JSON 计 byte cap 和 clone；整数也先转 JS Number，`-0` 归一为 `0`。初稿以 `f64::to_string` 取得 shortest digits 的“完全 parity”判断过早：随机差分暴露 tie-case 后，已改为直接依赖仓内既有 `ryu 1.0.23`，由 `ryu::Buffer::format_finite` 取得与 ECMAScript Ryu 实现一致的 shortest digits，再保留既有指数阈值、负零与整数 `.0` 转换。`Cargo.lock` 仅机械补入 `mineintent-middle -> ryu` 的直接依赖边，包版本本身未变化。
- 本地 Node 只读表征：`1.0 -> "1"`/1 byte，`-0 -> "0"`/1，`1e-6 -> "0.000001"`/8，`1e21 -> "1e+21"`/5；包含上述四值及嵌套数组的 fixture 为 85 bytes。生产与测试没有留下 Node/TS/JS 文件。
- support 单元回归另锁定 `1e-7`、`1e20` 阈值、`9007199254740993 -> 9007199254740992` 的 JS safe-integer rounding，以及 bits `0x43179085685d83c9 -> 1658206780088562.2` 的 shortest-decimal tie-case；ref/cursor 均验证嵌套数值 clone 与 84 reject/85 accept 临界 byte cap。临时 Rust 探针通过 stdin 调用本机 Node（未在源码树留下 JS/TS），对确定性随机 **249865** 个有限 f64 比较，结果 `samples=249865 outputs=249865 mismatches=0`。

### 锁边界与验证

- ref-store 新增 counting-clock 回归：空 store 首次 issue 只在 entries mutex 前取时一次，TTL 使用该值；resolve 在精确边界第二次取时并过期。cursor 原有同类回归继续通过。
- Information 相关测试共 57 passed：support 2、access-policy 7、contracts/schema 7、cursor 8、geometry 6、ref-store 12、registry 6、scope/trace 4、source-ports 5。`cargo test --package mineintent-middle --offline` 总计 83 passed。
- `cargo test --workspace --offline`：161 passed，doc tests 通过；`cargo check --workspace --all-targets --offline`、对应 `--locked` 检查与 `cargo +stable fmt --all -- --check`：通过。
- 本次无范围阻塞项；阶段 4 的 provider/runtime/context-composer 与 `runtime.test.ts:179` 仍按原 gate deferred。

## 2026-08-01｜P1-A 阶段 3 current-status/inventory provider 叶子迁移

### 基线、oracle 与范围

- 开始基线：`d81ff6f`，`port/a-world-info` worktree clean；完整只读对照 `providers/current-status-provider.ts`、`current-status-provider.test.ts`、`inventory-provider.ts`、`inventory-provider.test.ts`、`testing/provider-contract.ts`，以及 Rust 冻结 provider SPI/source ports/registry tests。未联网，supplies/vendor/移植计划零修改，未生成 TS/JS。
- 新增 `information/providers`，公开 `CurrentStatusProvider` 与 `InventoryProvider`；两者均由 `Arc<dyn SelfVitalsPort/InventoryPort>` 注入，可直接作为 `Arc<dyn InformationProvider>` 注册，保持 Send+Sync/object-safe。未改冻结 contracts、source ports、manifest/lock，也未进入 backend adapter、sound/viewport provider、runtime/tool-session/context-composer 或 B 路径。
- definition 的 id、中文 description、schema revision、participant audience、每个 field description/value type/`exactly_displayed`/`client_state`、connection+world scope dependencies 与 limits 逐字段对齐 oracle。每个字段持有真实 `InformationValueSchema`：覆盖 number 范围、Zod v4 safe integer、非空字符串、optional 非 null、array/object 成员，并保持普通 `z.object` 对未知键的 strip 语义。
- availability/read 均先在锁外调用 source port；revision 锁内只比较/clone owned snapshot。revision 初始 0，首次观测为 1，相同快照稳定，语义变化恰增 1；Rust `f64` 相等比较使 `-0` 与 `0` 不产生 JSON.stringify 不会产生的漂移。read 不主动检查 OperationControl，精确保持两个 TS provider 未检查 AbortSignal 的行为。
- 两者仅返回请求字段，unavailable 为空；source kind/acquisition、adapter/source revision、observedAt/evidenceIds 精确对齐。current-status 保留 oxygen=20、experienceLevel=0 默认；inventory 完整保留 slot/itemName/count/metadata/durabilityUsed 映射。非有限 source 数值转换失败时返回结构化 `InformationProviderError::Failed`，生产路径没有 panic/expect。

### TS 5/5 一对一映射与 Rust-only

1. `current-status-provider.test.ts:21` `current status provider satisfies the provider contract` → `ts_current_status_provider_satisfies_the_provider_contract`。
2. `current-status-provider.test.ts:30` `defaults missing oxygen to full and reads experience level` → `ts_current_status_defaults_missing_oxygen_to_full_and_reads_experience_level`。
3. `current-status-provider.test.ts:38` `bumps revision only when vitals change` → `ts_current_status_bumps_revision_only_when_vitals_change`。
4. `inventory-provider.test.ts:21` `inventory provider satisfies the provider contract` → `ts_inventory_provider_satisfies_the_provider_contract`。
5. `inventory-provider.test.ts:30` `reports current slots and selected hotbar slot` → `ts_inventory_provider_reports_current_slots_and_selected_hotbar_slot`。

上述两个 contract 映射共同调用机械迁移的 `assert_information_provider_contract`，覆盖 definition 完整性、revision 的 Rust 非负整数类型、只返回请求字段、逐值 runtime schema、unavailable 去重/不得与 value 重叠，以及每个请求字段恰好 value/unavailable 二选一。

另有 **4 条 Rust-only contract tests**，不冒充 TS 映射：两个 provider 各一条精确 definition/严格 schema 正负例；各一条 registry trait-object 注入、并发 revision、available/空 field reasons、`-0` 稳定性。inventory 条目另锁定只请求 slots 时不泄漏 selectedHotbarSlot，以及语义变化只增一次。

### mutation、验证与 deferred

- mutation：临时将 `current_status.rs` 的缺省 oxygen 从 `20.0` 改为 `19.0`，运行 `cargo test -p mineintent-middle --test information_current_status_provider ts_current_status_defaults_missing_oxygen_to_full_and_reads_experience_level --locked --offline`；指定测试按预期失败，差异 `Some(19.0) != Some(20.0)`。维护侧另独立反转 `RevisionTracker` 的快照变化条件，`ts_current_status_bumps_revision_only_when_vitals_change` 按预期以 revision `0 != 1` 失败。两处均已恢复；current-status 5/5、inventory 4/4、registry 6/6 定向测试通过。
- `cargo test --workspace --all-targets --locked --offline`：174 passed；`cargo check --workspace --all-targets --locked --offline`、`cargo +stable fmt --all -- --check`、`git diff --check`：通过。
- 冻结 SPI 没有发现必须修改却被本轮范围挡住的实现障碍。明确 deferred：source-port 的 backend adapter；sound/viewport providers；Information runtime/tool-session/context-composer；B 所有的 participant/runtime、app、models、speech、execution、events。ProviderReadRequest 的 selector/page 形状未扩张，runtime 层的 field/budget/access/cancellation 组合校验也未提前实现。

## 2026-08-02｜P1-A 阶段 3 sound information provider 叶子迁移

### 基线、oracle 与范围

- 开始基线：`03ce6eb`，A worktree clean；只读对照 `supplies/mineintent-main/src/information/providers/sound-provider.ts`、`sound-provider.test.ts`、`testing/provider-contract.ts`，以及现有 Rust `SoundHistoryPort`/`SoundObservation`、current-status/inventory provider 与 registry SPI。未联网；supplies/vendor/移植计划、Cargo manifest/lock、contracts、backend、B/A2 均零修改。
- 新增 `information/providers/sound.rs` 并从 `providers/mod.rs` 公开 `SoundInformationProvider`。不改变冻结 SoundPort：read 按 TS 顺序在请求 `recentSounds` 时调用 `recent(20.0)`，随后只调用一次 `revision()`；空历史返回 `recentSounds: []` 且 `unavailable` 为空。
- definition 精确对齐：`sound_information`、中文描述、`sound-information:1`、participant、`recentSounds`、quantized、sound_projection、连接+世界 scope、1/16384/2000 limits、notes 与 `sound-provider.v1` source metadata。`z.object` 未知键 strip、可选 soundName/category 非 null、distance/volume 非负、direction 四枚闭合值、pitch finite、observedAt 为 Z-only ISO datetime 均有运行期 schema。
- SourcePort 的 `revision: f64` 在 provider wire 边界转换为非负 JavaScript safe integer `u64`；read 遇负数、fraction、超过 `Number.MAX_SAFE_INTEGER`、NaN、Infinity 返回结构化 `InformationProviderError::Failed`，不 panic/unwrap/expect。availability 因冻结 SPI 无 Result，只在无效 source revision 时降为 0；有效路径与 TS 完全一致。

### TS 2/2 一对一映射与 Rust-only

1. `sound-provider.test.ts:21` `sound provider satisfies the provider contract` → `ts_sound_provider_satisfies_the_provider_contract`。
2. `sound-provider.test.ts:30` `returns an empty list, not unavailable, when nothing was heard` → `ts_sound_provider_returns_an_empty_list_not_unavailable_when_nothing_was_heard`。

两条测试均复用既有 Rust `information_provider_contract_support::assert_information_provider_contract`，保留 definition 完整性、revision JSON 整数、请求字段/available-unavailable 互斥与运行期 schema 断言。

另有 **3 条 Rust-only contract tests**，不冒充 TS 映射：definition/schema 全字段正负例；object-safe registry 注入、`recent(20)` 上限、source/observedAt/evidence metadata；无效 source revision 的结构化错误回归。

### mutation、验证与 deferred

- mutation：临时将 `RECENT_SOUND_LIMIT` 从 `20.0` 改为 `19.0`，运行 `cargo test -p mineintent-middle --test information_sound_provider rust_contract_sound_is_object_safe_limited_and_preserves_wire_metadata --locked --offline`；测试按预期失败（实际 `Some(19)`、期望 `Some(20)`），已恢复并复跑 sound 5/5。
- 维护侧复审修正 revision 上界：原草稿以 `u64::MAX as f64` 判断会让浮点 `2^64` 穿过并在 cast 时饱和；现与 JavaScript wire 语义一致地限制为 `Number.MAX_SAFE_INTEGER`，负数、fraction、`2^53`、`u64::MAX as f64`、NaN、Infinity 均由测试覆盖。非 fallible availability 对这些非法 source 值固定降为 revision 0，read 返回结构化错误。
- `cargo test --workspace --all-targets --locked --offline`：191 passed；`cargo check --workspace --all-targets --locked --offline`、`cargo +stable fmt --all -- --check`、`git diff --check`：通过。
- 明确 deferred：backend sound producer/adapter、viewport provider、Information runtime/tool-session/context-composer，以及 B/A2 所有路径。本轮未扩张 selector/page、取消、budget/access 或 backend 生产行为。
