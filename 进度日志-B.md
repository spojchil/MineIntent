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

- 提交 `a76561a848534d012c9cfa3df0754b7fec83327f`（`契约：预留 P0 公共模块命名空间`）预声明 `agent`、`capability`、`information`、`minecraft`；`information`/`minecraft` 仅有边界文档，后续业务仍由 A 独占。
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

- 冻结 `CapabilityInvocation`、`ExecutionResource`、`mineintent.tool-result.v1`、`CapabilityExecutionContext`、`ScopeGuard`、`ToolCapability`、`ToolCapabilityRegistry` 与进程内 `ToolDispatcher` trait。
- registry 的模型定义与 dispatch 项由同一 capability 实例生成；保持注册顺序，重复名返回结构化 `duplicate_tool_capability`，未知名保持未解析状态。本批次不实现 tool-call claim、run 生命周期或执行状态机。
- 冻结 `move_input` 的模型可见方向枚举、唯一键约束、50..=1500ms 整数边界及 JSON Schema；冻结 `view` 的严格空参数 schema。只实现契约校验，没有实现移动、视野读取或其他具体 capability 动作。
- 增加 2 个确定性 testdata 和 8 条 capability 契约测试，覆盖 registry 同源性/顺序/重复名、资源与协议枚举负例、调用 envelope 未知字段、schema 快照、参数约束、取消/deadline/scope 顺序，以及无 HTTP 的进程内 dispatch trait。

| 命令 | 结果 |
|---|---|
| `cargo test -p mineintent-contracts --offline` | 通过；Agent 11/11、capability 8/8，crate/doctest 通过。 |
| `cargo +stable fmt --all` | 通过；随后提交前使用 `--check` 复核。 |
