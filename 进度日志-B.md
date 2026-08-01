# 进度日志-B

## 2026-08-01｜P0-B1：Cargo workspace 骨架

### 范围

- 仅建立可编译、可测试的 Cargo workspace 基线；未开始 I01、I02、I03 或任何业务移植。
- 未修改 `supplies/`、`vendor/`、`移植计划/`，未进入 main/A worktree，未访问网络。

### 变更

- 根 `Cargo.toml` 改为 virtual workspace，成员为 `crates/backend`、`crates/contracts`、`crates/middle`，使用 resolver 2。
- 使用 `git mv` 将既有后端包清单和整个 `src/` 移到 `crates/backend/`；包名、库、CLI 与内嵌的 13 条单元测试保持原样。
- 新增 `mineintent-contracts` 与 `mineintent-middle` 两个无依赖空库；只建立 crate 边界，不定义 DTO、trait 或业务行为。
- 更新根 README 的 workspace 说明和根目录构建/运行命令。仓库当前没有脚本或 CI 文件需要修正。
- 根 `Cargo.lock` 只新增两个无依赖 workspace 包记录，外部依赖版本未变化；`rust-toolchain.toml` 与 `.cargo/config.toml` 保持原样，离线 vendor 配置继续从根目录生效。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `git status --short --branch` | 迁移前位于 `port/b-decision-runtime`，工作树干净。 |
| `cargo test --offline`（迁移前基线） | 首次全量依赖编译超过 120 秒工具时限而中止；超时前无编译错误，正式 workspace 门禁随后完成。 |
| `cargo metadata --offline --no-deps --format-version 1` | 通过；识别三个 workspace member，后端 lib/bin target 均指向 `crates/backend/src/`，根 `target/` 保持不变。 |
| `cargo fmt --all --check` | 失败（exit 1）：当前 nightly 工具链未安装 `cargo-fmt`/`rustfmt`，因此未进入格式比较。 |
| `cargo +stable fmt --all --check` | 通过；使用本机已安装的 stable rustfmt 离线完成全 workspace 等价格式检查。 |
| `cargo test --workspace --offline` | 通过；后端 13/13 单元测试通过，contracts/middle 空库测试及三个 crate 的 doctest 均通过。 |
| `cargo check --workspace --offline` | 通过；三个 workspace member 均完成离线静态编译检查。 |
| `cargo run -p mineintent-backend --offline -- --help` | 通过；迁移后的既有 CLI 包名、可执行文件名和帮助入口保持可用。 |
| `cargo +stable test --workspace --offline` | 失败（E0554）：vendored `simdnbt` 使用 `#![feature(portable_simd)]`，证明现有依赖必须保留 nightly 编译工具链。 |

### 补给申请与明确边界

- 如要求默认工具链上的 `cargo fmt --all --check` 在完全离线环境直接通过，需要预装 `nightly-x86_64-pc-windows-msvc` 的 `rustfmt` 组件。本次未联网安装；stable 编译又被依赖所需的 nightly feature 明确拒绝，因此未为绕过环境缺件而改变既有 nightly 编译工具链。
- P0-B1 没有冻结或预设公共契约；I01/I02/I03 仍由后续独立任务完成。

## 2026-08-01｜P0-I03：Agent/capability contracts

### 命名空间预约

- 提交 `a76561aaa698b533ccd9c8a4af8a30876b3101e6`（`契约：预留 P0 公共模块命名空间`）预声明 `agent`、`capability`、`information`、`minecraft`；`information`/`minecraft` 仅有边界文档，后续业务仍由 A 独占。
- `mineintent-contracts` 只加入锁文件已有的 `serde`/`serde_json`；未加入 runtime、HTTP 或模型 SDK。
- 预约提交前 `cargo +stable fmt --all --check` 与 `cargo test --workspace --offline` 均通过。

### I03 Agent 契约批次

- 提交 `9ea8a97912359c9cfa582cce4a6f255292b758d8`（`契约：冻结 I03 Agent 数据边界`）。
- 冻结 `mineintent.agent-context.v3` 的严格外层结构、run/tool-call 键、tool definition/invocation/result、`mineintent.tool-response.v2`、`mineintent.agent-run.v1`、模型用量与结构化错误。
- 增加进程内 `AgentRunner`、`ModelProvider`、取消信号和 deadline trait/值对象；没有实现循环、provider、状态机或传输层。
- prompt 仅表达外部模板的 `key`/`version` 引用，没有把模板正文写入 contracts。
- `information`/`minecraft` 所属的 memory/status/inventory/sound/omission 内层值保持泛型/不透明；I03 只冻结其所在 envelope，不复制 A 的 DTO。
- TS oracle 允许 invocation 的工具名保持开放字符串，Python provider 只允许发布 `[A-Za-z0-9_-]{1,64}` 名称；因此分别建模 invocation name 与 advertised definition name，两条边界均未放宽。
- 增加 5 个确定性 JSON testdata 和 11 条 Agent 契约测试，覆盖协议/枚举版本、未知字段、必需 nullable、显式 null、ID 形状、参数原样透传、非有限浮点、外部 prompt 引用、无 callback/token 字段、取消/deadline 顺序及 fixture 稳定性。

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-contracts --offline` | 通过；Agent 契约测试 11/11，crate/doctest 通过。 |
| `cargo check -p mineintent-contracts --offline` | 通过。 |
| `cargo +stable fmt --all` | 通过；随后提交前使用 `--check` 复核。 |

### I03 capability 契约批次

- 提交 `84c528b24bd829c7633c7c76c77a8a342ad3178f`（`契约：冻结 I03 capability 执行边界`）。
- 冻结 `CapabilityInvocation`、`ExecutionResource`、`mineintent.tool-result.v1`、`CapabilityExecutionContext`、`ScopeGuard`、`ToolCapability`、`ToolCapabilityRegistry` 与进程内 `ToolDispatcher` trait。
- registry 的模型定义与 dispatch 项由同一 capability 实例生成；保持注册顺序，重复名返回结构化 `duplicate_tool_capability`，未知名保持未解析状态。本批次不实现 tool-call claim、run 生命周期或执行状态机。
- 冻结 `move_input` 的模型可见方向枚举、唯一键约束、50..=1500ms 整数边界及 JSON Schema；冻结 `view` 的严格空参数 schema。只实现契约校验，没有实现移动、视野读取或其他具体 capability 动作。
- 增加 2 个确定性 testdata 和 8 条 capability 契约测试，覆盖 registry 同源性/顺序/重复名、资源与协议枚举负例、调用 envelope 未知字段、schema 快照、参数约束、取消/deadline/scope 顺序，以及无 HTTP 的进程内 dispatch trait。

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-contracts --offline` | 通过；Agent 11/11、capability 8/8，crate/doctest 通过。 |
| `cargo +stable fmt --all` | 通过；随后提交前使用 `--check` 复核。 |

### Oracle 测试映射

Python 25 条中，本轮迁入 I03 能负责的契约级断言如下；Rust 测试名保持指向原断言，而不复刻 Python HTTP 结构：

| Python 测试 | 本轮 Rust 对应 |
|---|---|
| `test_executor_response_envelope_is_strict` | `tool_execution_v2_requires_nullable_observation_and_rejects_transport_legacy`：v2、必需 `observationAfter`、null/object、v1/数组/`roundId` 负例。 |
| `test_arguments_are_forwarded_untouched_for_the_tool_side_to_judge` | `invocation_preserves_open_tool_name_and_arguments_but_validates_keys`：嵌套 arguments 原样保留，不在 Agent 契约预判具体 capability 范围。 |
| `test_float_arguments_survive_the_json_boundary` | 同上：`-30.5` 原样 round-trip，NaN/+Inf/-Inf JSON 拒绝；frame 的非有限浮点输出也拒绝。 |
| `test_request_and_json_are_strict` | `context_v3_round_trips_with_strict_outer_shapes`、`advertised_tool_definition_is_strict_and_provider_safe`、`run_request_uses_external_prompt_reference_and_excludes_transport_configuration`：协议、未知字段、tools 数量/名称与严格 JSON；其中 loopback executor URL 子断言随 HTTP 边界消亡。 |
| `test_tool_call_ids_are_preflighted_and_unique_within_the_run` | 本轮只迁空值、129 ASCII、65 emoji 的 ID 形状和 `ToolCallKey`；整批 preflight/一次性 claim 留给后续 runtime。 |
| `test_cache_counters_are_read_from_each_provider_shape_and_summed` | 本轮只迁统一 `ModelUsage` 四字段、非负整数、显式 0/缺省/null/未知字段语义；provider 形状规范化与跨轮求和留给后续 provider/runner。 |
| `test_decide_enforces_the_run_deadline` | 本轮只迁 `Deadline`/`ExecutionControl` 与取消优先顺序；AgentRunner 的 180s 执行上限留给后续循环实现，HTTP 504 不保留。 |

其余仍须后续逐名迁移的 Python 行为测试（本轮未实现其循环、状态机、transcript、模板或 provider 行为）：

- `test_deepseek_replay_preserves_reasoning_and_tool_call_id`
- `test_stable_content_leads_and_observations_stay_with_their_tool_results`
- `test_invalid_model_tool_data_stays_local_and_keeps_the_tool_pair`
- `test_parallel_calls_all_execute_in_order`
- `test_truncated_and_filtered_completions_fail_instead_of_closing_the_run`
- `test_reported_and_absent_finish_reasons_that_mean_a_real_ending_are_accepted`
- `test_tool_calls_are_capped_per_response_and_per_run`
- `test_transcript_records_tools_rotates_and_honors_the_data_dir`
- `test_transcript_records_the_run_even_when_it_fails`
- `test_leak_guard_catches_call_shaped_mentions_but_not_prose`
- `test_cancelled_run_does_not_block_its_replacement`
- `test_late_cancel_for_superseded_id_does_not_cancel_new_run`
- `test_model_transport_connects_directly_to_the_configured_endpoint`
- `test_model_transport_cancellation_closes_a_blocked_upstream`
- `test_tool_call_ids_are_preflighted_and_unique_within_the_run`（剩余 preflight/claim）
- `test_cache_counters_are_read_from_each_provider_shape_and_summed`（剩余规范化/求和/transcript）
- `test_decide_enforces_the_run_deadline`（剩余 runner 执行上限）
- `test_stable_context_ignores_profile_and_only_renders_memories`
- `test_prompt_carries_behavior_and_the_shared_observation_semantics`

随单进程裁定明确消亡、不得在 Rust 复刻的 Python 测试是 `test_config_requires_an_independent_service_token` 与 `test_decide_authentication_happens_before_body_validation`；它们验证的是已删除 Python 服务的独立 token/HTTP 鉴权顺序。

直接 TS capability/bridge 映射：

- `one registration produces both the advertised contract and dispatch entry` → `registry_derives_ordered_definitions_and_dispatch_from_same_instances`。
- `duplicate capability names fail while the registry is constructed` → `registry_rejects_duplicate_advertised_names_with_structured_error`。
- `move contract exposes one simultaneous key set and rejects duplicate keys` → `move_input_schema_matches_the_model_visible_oracle` + `move_input_arguments_reject_unknown_fields_versions_and_constraint_mutations`。
- `view declares one full read with an empty argument object and its own scan resource` → `view_arguments_and_execution_enums_are_closed`；只迁 schema/resource，未实现 viewport scan。
- `view rejects an already-cancelled signal before starting the scan` → `capability_context_checks_cancellation_deadline_then_scope` 的执行前 guard；具体 read 次数断言留给后续 `view` 实现。
- `the tool response carries a post-handling observation without claiming causation` → `tool_execution_v2_requires_nullable_observation_and_rejects_transport_legacy`。
- `tool bridge is loopback-only, authenticated and forwards strict invocations` → strict invocation/ID/arguments 已迁；loopback/auth/HTTP size-limit 子断言随 bridge 消亡。

### Mutation 验证

- 临时删除 `MoveInputArguments` 的方向唯一性条件后，运行 `cargo test -p mineintent-contracts --test capability_contracts move_input_arguments_reject_unknown_fields_versions_and_constraint_mutations --offline`，目标测试按预期失败（0 passed / 1 failed），报告重复方向被错误接受。
- 立即用相反 patch 恢复条件；同一命令复跑通过（1 passed），`git diff --exit-code -- crates/contracts/src/capability/schemas.rs` 通过，mutation 未进入提交。

### 最终离线门禁

| 命令 | 结果 |
|---|---|
| `cargo +stable fmt --all --check` | 通过。 |
| `cargo test --workspace --offline` | 通过；backend 13/13、Agent contracts 11/11、capability contracts 8/8，middle 空库与全 workspace doctest 通过。 |
| `cargo check --workspace --offline` | 通过。 |
| `git diff --check` | 通过。 |

- 最终审查未修改 `supplies/`、`vendor/`、`移植计划/`、`crates/backend/`；没有联网，没有进入 main/A worktree，没有加入 runtime/HTTP/模型 SDK，也没有实现 I03 之外的业务。
