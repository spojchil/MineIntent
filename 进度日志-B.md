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
- [x] 2026-08-01 补给已到位：维护者安装了默认 nightly 工具链的 `rustfmt`；主线 `d81ff6f` 已复验 `cargo fmt --all -- --check` 通过。上表保留 P0-B1 当时的失败记录，不再视为当前阻塞。
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

以下表格与 `agent-service/test_server.py`、`agent-service/test_prompt.py` 的 25 个方法一一对应；每个 Python 源测试只出现一行。HTTP/Python 机制被替换时，仍迁移其严格边界或取消安全意图，不把任何源测试标为“无需迁移”。

| Python 源测试 | contract 已覆盖或后续实现归属 |
|---|---|
| `test_deepseek_replay_preserves_reasoning_and_tool_call_id` | 后续 AgentRunner/model-provider：reasoning replay、tool 配对与跨轮 usage；I03 已冻结 run/tool-call key。 |
| `test_stable_content_leads_and_observations_stay_with_their_tool_results` | 后续 AgentRunner/template：消息顺序与 prefix 稳定；I03 已冻结 context/tool-result envelope。 |
| `test_executor_response_envelope_is_strict` | contract 已覆盖：`tool_execution_v2_requires_nullable_observation_and_rejects_transport_legacy`。 |
| `test_tool_call_ids_are_preflighted_and_unique_within_the_run` | contract 已覆盖 ID 形状与 `ToolCallKey`；后续 runtime 实现整批 preflight 和一次性 claim。 |
| `test_invalid_model_tool_data_stays_local_and_keeps_the_tool_pair` | 后续 AgentRunner：失败 tool 配对与 loop continuation；I03 已冻结结构化错误和 nullable observation。 |
| `test_cache_counters_are_read_from_each_provider_shape_and_summed` | contract 已覆盖 `ModelUsage` 四字段及 strict 非负整数；后续 provider/runner 实现形状规范化、求和与 transcript。 |
| `test_parallel_calls_all_execute_in_order` | 后续 AgentRunner/runtime：并行调用执行与声明顺序。 |
| `test_truncated_and_filtered_completions_fail_instead_of_closing_the_run` | 后续 AgentRunner/model-provider：finish reason 失败语义。 |
| `test_reported_and_absent_finish_reasons_that_mean_a_real_ending_are_accepted` | 后续 AgentRunner/model-provider：允许的结束原因集合。 |
| `test_tool_calls_are_capped_per_response_and_per_run` | 后续 AgentRunner：每响应 8、每 run 32 的执行上限与 preflight。 |
| `test_arguments_are_forwarded_untouched_for_the_tool_side_to_judge` | contract 已覆盖：`invocation_preserves_open_tool_name_and_arguments_but_validates_keys`。 |
| `test_float_arguments_survive_the_json_boundary` | contract 已覆盖：有限浮点 round-trip，NaN/+Inf/-Inf 拒绝。 |
| `test_transcript_records_tools_rotates_and_honors_the_data_dir` | 后续 AgentRunner/transcript：格式、轮转和 data dir。 |
| `test_transcript_records_the_run_even_when_it_fails` | 后续 AgentRunner/transcript：失败 run 仍落盘。 |
| `test_leak_guard_catches_call_shaped_mentions_but_not_prose` | 后续 AgentRunner：closing leak guard。 |
| `test_request_and_json_are_strict` | contract 已覆盖协议、未知字段、tools、ID 与非有限 JSON；旧 executor URL 作为 transport 字段由严格进程内 request 拒绝。 |
| `test_config_requires_an_independent_service_token` | contract 已覆盖：`python_test_config_requires_an_independent_service_token_maps_to_strict_contract_rejection` 拒绝遗留 `serviceToken`/`modelApiKey`，进程内 API 不提供共享 token 槽位。 |
| `test_decide_authentication_happens_before_body_validation` | contract 已覆盖：`python_test_decide_authentication_happens_before_body_validation_maps_to_no_transport_phase` 拒绝 Authorization/callback/body transport 字段；进程内 request 只有四个契约字段，不存在可表达的 auth-before-body 阶段顺序。 |
| `test_cancelled_run_does_not_block_its_replacement` | 后续 AgentRunner/runtime：旧 run 取消和 replacement 并发；I03 的取消通知 future 可唤醒阻塞工作。 |
| `test_late_cancel_for_superseded_id_does_not_cancel_new_run` | 后续 runtime：run identity、supersession 与迟到取消隔离。 |
| `test_decide_enforces_the_run_deadline` | contract 已覆盖 `Deadline`、可等待取消分支及取消优先；后续 AgentRunner/runtime 用 timer 实现 180s 上限。 |
| `test_model_transport_connects_directly_to_the_configured_endpoint` | 后续 ModelProvider 实现：模型 endpoint 行为；I03 不引入 HTTP/SDK。 |
| `test_model_transport_cancellation_closes_a_blocked_upstream` | 后续 ModelProvider 实现：把 I03 取消通知接入阻塞上游并等待退出。 |
| `test_stable_context_ignores_profile_and_only_renders_memories` | 后续外部 prompt template + I01 memory：稳定上下文渲染。 |
| `test_prompt_carries_behavior_and_the_shared_observation_semantics` | 后续外部 prompt template/AgentRunner：行为文本与 observation 语义；I03 只冻结模板 key/version。 |

直接 TS capability/bridge 映射：

- `one registration produces both the advertised contract and dispatch entry` → `registry_derives_ordered_definitions_and_dispatch_from_same_instances`。
- `duplicate capability names fail while the registry is constructed` → `registry_rejects_duplicate_advertised_names_with_structured_error`。
- `move contract exposes one simultaneous key set and rejects duplicate keys` → `move_input_schema_matches_the_model_visible_oracle` + `move_input_arguments_reject_unknown_fields_versions_and_constraint_mutations`。
- `view declares one full read with an empty argument object and its own scan resource` → `view_arguments_and_execution_enums_are_closed`；只迁 schema/resource，未实现 viewport scan。
- `view rejects an already-cancelled signal before starting the scan` → `capability_context_checks_cancellation_deadline_then_scope` 的执行前 guard；具体 read 次数断言留给后续 `view` 实现。
- `the tool response carries a post-handling observation without claiming causation` → `tool_execution_v2_requires_nullable_observation_and_rejects_transport_legacy`。
- `tool bridge is loopback-only, authenticated and forwards strict invocations` → strict invocation/ID/arguments 已迁；loopback/auth 安全意图由严格拒绝遗留 transport 字段覆盖，不复刻 HTTP 机制。

### 维护审查返修

- `CancellationSignal` 增加对象安全的 `cancelled()` future；已取消信号立即返回结构化 `AgentError`，可取消实现必须登记 waker。`ExecutionControl::cancelled()` 暴露等待分支，文档明确要求与 `Deadline::expires_at()` 对应的 runtime timer 一起 select，并在任一分支唤醒后调用 `check_at(now)` 保留“取消优先于 deadline”。
- agent/capability 两组 `FixedCancellation` 均实现 ready/pending 通知；手写单次 poll 断言 triggered=`Ready`、active=`Pending`。测试还以 `dyn AgentRunner<Context=...>`、`dyn ModelProvider<Request=..., Response=...>`、`dyn ToolDispatcher<Observation=...>` 和 `Arc<dyn ToolCapability>` 编译/调用公开 trait。
- 新增两个显式 Python 来源映射测试，严格拒绝 `serviceToken`、`modelApiKey`、Authorization、callback URL/token、executor/service URL 及 body transport 字段；序列化的进程内 `AgentRunRequest` 顶层精确只有 `runId/context/tools/promptTemplate`，没有可表达的 auth/body 阶段顺序。
- 用 PowerShell 正则分别提取两个 Python 文件的方法名和本节表格首列并做集合比较：`SOURCE_COUNT=25`、`LOG_COUNT=25`、`LOG_UNIQUE_COUNT=25`、`MISSING_COUNT=0`、`EXTRA_COUNT=0`。

### Mutation 验证

- 临时删除 `MoveInputArguments` 的方向唯一性条件后，运行 `cargo test -p mineintent-contracts --test capability_contracts move_input_arguments_reject_unknown_fields_versions_and_constraint_mutations --offline`，目标测试按预期失败（0 passed / 1 failed），报告重复方向被错误接受。
- 立即用相反 patch 恢复条件；同一命令复跑通过（1 passed），`git diff --exit-code -- crates/contracts/src/capability/schemas.rs` 通过，mutation 未进入提交。

### 最终离线门禁

| 命令 | 结果 |
|---|---|
| `cargo +stable fmt --all --check` | 通过。 |
| `cargo test --workspace --offline` | 通过；backend 13/13、Agent contracts 14/14、capability contracts 8/8，middle 空库与全 workspace doctest 通过。 |
| `cargo check --workspace --offline` | 通过。 |
| `git diff --check` | 通过。 |

- 最终审查未修改 `supplies/`、`vendor/`、`移植计划/`、`crates/backend/`；没有联网，没有进入 main/A worktree，没有加入 runtime/HTTP/模型 SDK，也没有实现 I03 之外的业务。

## 2026-08-01｜P1：middle 并行叶子共享脚手架

### 范围与变更

- 本提交只建立 A/B 并行开发共同需要的 crate 依赖与模块入口，不声明 events、execution、information 或 speech 的任何迁移已经完成，也不包含阶段 2 行为实现。
- `mineintent-middle` 加入 `mineintent-contracts` 路径依赖，以及 lock/vendor 已有的 `serde` derive、`serde_json`、`thiserror 2`、`uuid`（`v4/serde`）、`tokio`（仅 `fs/macros/rt/sync/time`）和 `sha2 0.11`。
- 一次性在 `middle/src/lib.rs` 声明 `events`、`execution`、`information`、`speech`；四个 `mod.rs` 只有边界文档。后续 `information` 由 A 独占，另外三个模块由 B 独占，双方不再修改共享入口。
- 根 lock 只为既有 `mineintent-middle` package 增加上述 7 个依赖引用；离线解析没有新增 registry package 或修改依赖来源。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `git status --short` | 开始前位于 `port/b-decision-runtime`、HEAD=`1462116f721b8e446aefe552001dea0e162b9764`，工作树干净且无 `index.lock`。 |
| `cargo check --workspace --offline` | 通过；middle 及全 workspace 离线解析/编译成功。 |
| `cargo test --workspace --offline` | 通过；backend 13、I03 Agent 14、I03 capability 8、I01 26、I02 17 条测试全部通过，middle 空库与 doctest 通过。 |
| `cargo +stable fmt --all --check` | 通过。 |

- 未访问网络，未操作 main worktree，未修改 `supplies/`、`vendor/`、`移植计划/`、backend 或 contracts。

## 2026-08-01｜P1-B：events journal 第一批

### 范围与实现

- 本批只机械迁移 `events/journal.ts`：严格的 `mineintent.event.v1` 应用事件信封、单 writer 队列串行 JSONL append、flush barrier，以及 Unix 文件权限 `0600`。该信封明确不同于 backend 事实流的 `mineintent.minecraft.backend-event.v2`。
- writer actor 按请求进入队列的顺序逐条完成写入，单条 JSON 后只追加一个换行；阻塞文件 I/O 在 `spawn_blocking` 中执行，没有持锁跨越 await。append 返回前完成该行 flush，显式 `flush()` 等待此前已入队请求完成。
- 没有迁移 execution、speech、capability、model、runtime、app、memory 或 information，也没有修改 manifest、lock、共享 `lib.rs` 或禁区文件。

### Characterization / contract tests

- TS oracle 没有独立 `journal.test`；本批测试仅标为 characterization/contract tests，不计作任何 TS 测试的一一迁移。
- Windows 当前平台执行 2 条：严格 v1 serde/未知字段拒绝/v2 discriminator 拒绝；25 次含并发 append 仍形成 25 个完整单行、写入集合与返回事件一致、先入队记录保持首位，并在 flush 后可见。
- 另有 1 条 `cfg(unix)` 权限测试，断言创建后的 journal 文件权限精确为 `0600`；当前 Windows 门禁不虚报该平台测试已执行。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-middle --test events_journal --offline` | 通过；当前平台 2/2。 |
| `cargo test --workspace --offline` | 通过；含 middle journal 当前平台 2/2，workspace 既有测试全部通过。 |
| `cargo check --workspace --offline` | 通过。 |
| `cargo +stable fmt --all --check` | 通过。 |
| `git diff --check` | 通过。 |

## 2026-08-01｜P1-B：execution contracts + arbiter

### 范围与实现

- 本批只迁移 `execution/contracts.ts` 与 `execution-arbiter.ts`：`body/chat/memory/viewport` 四类独立资源、lease 字段、三类普通 refusal、四态 job 与窄 `JobOutcome`。
- `acquire` 对同资源冲突返回 `AcquireDecision::Refused`，不同资源互不阻塞。lease handle 携带外部不可构造的私有 generation；release 幂等，旧 handle 和 invalidate 前 handle 均不能删除替代租约。
- job handle 立即返回并持有共享状态；settle/cancel、`jobs_for` 插入顺序和 prune 对齐 oracle。晚 cancel 不改 settled 历史状态；invalidate 只取消 running job、epoch 加一并清空全部 lease，外部 handle 和可等待 watch signal 同步观察取消。
- 所有 await 只发生在取消通知等待中，不持有 arbiter/job 锁。同步临界区不执行用户回调、I/O 或序列化；std Mutex 中毒使用 `PoisonError::into_inner` 显式恢复，避免无关 panic 扩散，生产路径不对锁调用 `unwrap`。
- 未修改 manifest、lock、共享 `lib.rs`、events、speech、information、contracts/backend 或禁区文件；未实现工具动作、上层组装、planner、runtime、app 或 memory。

### `execution-arbiter.test.ts` 8/8 映射

| TS oracle test | Rust test |
|---|---|
| `leases are per resource, so chat and memory stay free while the body is held` | `leases_are_per_resource_so_chat_memory_and_viewport_stay_free_while_body_is_held` |
| `a refusal is returned rather than thrown so one conflict cannot kill a run` | `a_refusal_is_returned_rather_than_panicking_so_one_conflict_cannot_kill_a_run` |
| `releasing is idempotent and frees the resource exactly once` | `releasing_is_idempotent_and_frees_the_resource_exactly_once` |
| `a stale release after invalidation cannot evict the lease that replaced it` | `a_stale_release_after_invalidation_cannot_evict_the_replacement_lease` |
| `a job hands back a handle immediately and reports its outcome later` | `a_job_returns_a_shared_handle_immediately_and_reports_its_outcome_later` |
| `cancelling a running job aborts its signal` | `cancelling_a_running_job_updates_shared_state_and_wakes_its_signal` |
| `scope loss voids every lease and running job in one step` | `scope_loss_voids_every_lease_and_running_job_in_one_step` |
| `settled jobs are pruned while running ones survive to be reported` | `settled_jobs_are_pruned_while_running_jobs_survive_in_insertion_order` |

- 额外 Rust contract test：`additional_execution_contracts_are_strict_and_keep_outcomes_narrow`，覆盖资源、状态、refusal 枚举闭集，未知字段/遗留 transport 字段拒绝，以及 outcome 不泄漏 resource/run/tool 等内部字段；不计入 8 条 TS 一一映射。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-middle --test execution_arbiter --offline` | 通过；9/9（8 条 oracle 映射 + 1 条额外 contract test）。 |
| `cargo test --workspace --offline` | 通过；含 execution 9/9，workspace 既有测试全部通过。 |
| `cargo check --workspace --offline` | 通过。 |
| `cargo +stable fmt --all --check` | 通过。 |
| `git diff --check` | 通过。 |

### 维护审查返修：可选 summary 拒绝 null

- `JobOutcome.summary` 保持缺省时为 `None`、序列化时省略；字段一旦出现，只接受 string，显式 `null` 严格拒绝，与 TS `summary?: string` 对齐。
- `execution` contracts 中没有其他 `Option` 字段；arbiter API 与行为未改。额外 contract test 同时断言 omitted 接受和 `null` 拒绝。

## 2026-08-01｜P1-B：speech contracts + chat input + segmentChat

### 范围与实现

- 本批迁移 `speech/contracts.ts` 全部闭集 DTO 与 `SpeechTransport` 边界、`chat-input.ts`，以及 `speech-scheduler.ts` 中独立的文本清洗/`segmentChat` 纯函数；未实现 scheduler 队列、限速、timer、发送 transport、duplicate id、stop/cancellation 或 failure event 触发行为。
- chat input 直接消费 P0 冻结的 `BackendEventEnvelope<ProtocolChatEvent>`（backend event v2），不重定义底层 chat wire；过滤非 chat kind、非 public chat position 和缺失/空 sender。
- 点名与 ongoing conversation 大小写不敏感；single-party 只在排除 participant 后唯一在线者就是 sender 时成立。evidence 固定为 explicit name、ongoing conversation、single party 顺序，无证据时为 `not_addressed`；输出保留 source event id、sender、occurredAt、world/dimension/epoch、verified 与原文，“停一下”不进入特殊控制路径。
- `segment_chat` 机械保持 JS 清洗、标点回退和 Unicode code point 计数语义；没有改成 byte、grapheme cluster 或仅按空白切分。
- speech DTO 使用严格 serde；`verified`、`dimension`、`conversationActiveWith` 缺省可省略且显式 `null` 拒绝。

### `speech.test.ts` 6/8 映射

| TS oracle test | Rust test / 状态 |
|---|---|
| `chat input records sender, addressing evidence, time and world context` | `chat_input_records_sender_addressing_evidence_time_and_world_context` |
| `addressing is symmetric for players under the same multiplayer input conditions` | `addressing_is_symmetric_for_players_under_the_same_multiplayer_input_conditions` |
| `the only online player is addressed by single-party conditions without naming the participant` | `sole_online_player_is_addressed_by_single_party_conditions_without_naming_participant` |
| `a straggler chat from someone other than the sole online player is not single-party addressed` | `straggler_chat_from_someone_other_than_sole_online_player_is_not_single_party_addressed` |
| `stop wording remains ordinary addressed player text` | `stop_wording_remains_ordinary_addressed_player_text` |
| `segmentChat respects Unicode length and keeps ordered content` | `segment_chat_respects_unicode_code_point_length_and_keeps_ordered_content` |
| `scheduler rate limits and preserves segment order` | 未迁；下一批异步 scheduler 队列/确定性 clock-waiter。 |
| `scheduler stop cancels queued speech before it is sent` | 未迁；下一批 scheduler stop/cancellation。 |

- 额外 Rust tests 2 条：`additional_non_player_public_chat_and_missing_sender_are_filtered` 与 `additional_speech_contracts_are_closed_strict_and_optional_non_null`；不计入 TS 6/8。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-middle --test speech_input_segment --offline` | 通过；8/8（6 条 TS 映射 + 2 条额外 contract tests）。 |
| `cargo test --workspace --offline` | 通过；含 speech 8/8，workspace 既有测试全部通过。 |
| `cargo check --workspace --offline` | 通过。 |
| `cargo +stable fmt --all --check` | 通过。 |
| `git diff --check` | 通过。 |

## 2026-08-01｜P1-B：SpeechScheduler

### 范围与实现

- 本批完整迁移 `speech-scheduler.ts` 的 scheduler 生产语义，复用既有 `SpeechRequest`、`SpeechEvent`、`SpeechTransport` 与 `segment_chat`；没有修改 chat-input，也没有进入 participant、runtime、capability 或其他模块。
- scheduler 使用一个 Tokio worker 和 `VecDeque` FIFO。`schedule` 同步校验/入队并同步发出 `scheduled`，实际首段及后续段均由异步 worker 投递；成功发送后记录 Tokio `Instant`，下一段/请求遵守 minimum interval。
- queued request id 重复返回结构化 `DuplicateRequest`；request 完成或失败出队后可重用同一 id。transport 失败发出 `failed`、丢弃当前 request 并继续下一项，不终止 worker。
- `stop` 通过清空队列和 `Notify` 唤醒 worker 取消当前 timer，按 FIFO 为全部排队 request 发出 `cancelled`。schedule 后同一同步调用序列立即 stop 时，首段尚未投递，事件精确为 `scheduled`/`cancelled`。
- transport 调用、event callback 与所有 await 均在锁外；Mutex poison 显式恢复。构造期 max segment length/runtime 缺失及 schedule 的空请求/重复 id 均返回结构化错误，生产路径不以 panic 表达校验失败。
- `mineintent-middle` 既有 Tokio 依赖只增加 `test-util` feature，用于 paused time/advance；未新增 package，lock 未变化。

### `speech.test.ts` 最后 2/2 映射

| TS oracle test | Rust test |
|---|---|
| `scheduler rate limits and preserves segment order`（line 76） | `scheduler_rate_limits_and_preserves_segment_order` |
| `scheduler stop cancels queued speech before it is sent`（line 91） | `scheduler_stop_cancels_queued_speech_before_it_is_sent` |

- 本批额外 Rust tests 5 条：transport 失败续跑、queued duplicate/reuse、stop 全队列 FIFO cancellation、构造校验、请求校验；不冒充 TS 映射。speech.test.ts 至此累计 8/8。
- 全部异步测试使用 Tokio `start_paused`、`advance` 与 task yield；没有 wall-clock sleep 或竞速等待，#125 只替换等待机制，保留 oracle 行为断言。

### 命令与结果

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-middle --test speech_scheduler --offline` | 通过；7/7（2 条 TS 映射 + 5 条额外 tests）。 |
| `cargo test --workspace --offline` | 通过；含 scheduler 7/7，workspace 既有测试全部通过。 |
| `cargo check --workspace --all-targets --offline` | 通过。 |
| `cargo +stable fmt --all -- --check` | 通过。 |
| `git diff --check` | 通过。 |

- 首次将全部门禁串在一个 PowerShell 调用时触及 120 秒工具时限，未产生失败诊断；拆分为以上独立命令后均明确通过。

## 2026-08-01｜阶段 2 B 范围整理返修

### Scheduler 线性化与错误边界

- 当前段由 worker 在 scheduler 锁内从 `queued` claim 为 `dispatching`，该状态转换是 stop/dispatch 的线性化点。stop 先到时移除并取消 queued 请求；worker 先 claim 时允许当前段完成且不提前发 `cancelled`。若请求还有剩余段，stop 只写入 `cancel_after_dispatch`，worker 在锁外发出当前段 `sent` 后移除余段并在锁外发 `cancelled`，之后不再调用 transport。transport、event callback 与所有 await 仍在队列锁外。
- 无 sleep 的 `Condvar` 阻塞 transport 回归覆盖两种已 claim 情形：单段请求只产生 `scheduled`/`sent`；多段请求只发送当前段，随后按 `sent`/`cancelled` 顺序终结并不发送余段。既有 oracle stop 测试继续覆盖 stop 先于 claim 时 `scheduled`/`cancelled` 且零发送。
- `SpeechTransport::send` 的 panic 在 transport actor 边界转换为 `failed` reason，当前请求丢弃，worker 随后继续 FIFO；没有捕获或改变 `on_event` panic 语义。
- constructor 使用 `Instant::checked_add` 拒绝不可表示的 `minimum_interval`，返回 `SpeechSchedulerBuildError::InvalidMinimumInterval`；worker deadline 计算也不再使用可能 panic 的 `Instant + Duration`。

### 契约、依赖与文档整理

- middle execution 删除重复的 `ExecutionResource`，直接复用并重导出 `mineintent_contracts::capability::ExecutionResource`。P0 类型未实现 `Hash`，因此 arbiter 私有四资源 lease 表从 `HashMap` 最小调整为 `Vec` 查找；公共 API、lease identity/generation 与行为未改。
- Tokio 生产 features 收窄为 `macros/rt/sync/time`；仅测试使用的 `fs/test-util` 移到 dev-dependencies，未新增 package，Cargo.lock 未变化。
- 更新 middle crate、Cargo package 与 events journal 的过时脚手架/迁移所有权文档；chat-input 复用 segment 的 JavaScript whitespace helper。
- 没有修改 Drop、arbiter shutdown 或 journal 公共错误模型；它们继续作为后续生命周期/错误模型设计项。重复 UTC formatter 本次未抽取：在当前允许范围内只能引入 events/execution 反向耦合，或新增未授权的共享根模块。

### 测试与门禁

- scheduler 新增 4 条额外 Rust tests：单段与多段 claim/stop 并发线性化、transport panic 后续跑、不可表示 interval 的结构化构造错误；不冒充 TS 映射。`speech.test.ts` 映射仍为 8/8，scheduler 测试现为 11 条（2 条 oracle + 9 条额外）。
- execution 额外 contract test 增加编译期同型赋值，证明 middle 导出的资源类型就是 P0 contracts 权威类型；execution 测试计数仍为 9 条。

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-middle --test speech_scheduler --offline` | 通过；11/11。 |
| `cargo test -p mineintent-middle --test execution_arbiter --offline` | 通过；9/9。 |
| `cargo test -p mineintent-middle --test speech_input_segment --offline` | 通过；8/8。 |
| `cargo test -p mineintent-middle --test events_journal --offline` | 通过；当前平台 2/2，Unix 权限测试保持 `cfg(unix)`。 |
| `cargo test --workspace --offline` | 通过；workspace 既有与新增测试及 doctests 全部通过。 |
| `cargo check --workspace --all-targets --offline` | 通过。 |
| `cargo +stable fmt --all -- --check` | 通过。 |
| `git diff --check` | 通过。 |
