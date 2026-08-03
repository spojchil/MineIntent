# 任务书：backend runtime.rs 行为中立模块拆分

> 依据：中期更新-17（文件规模三档纪律）。本任务**单车道独占 crates/backend**，
> 进行期间不得有其他车道并行改动该 crate。

## 目标

把 `crates/backend/src/runtime.rs`（当前 17,541 行）拆为 `runtime/` 模块目录，
每个结果文件 ≤2000 行（目标 ≤1500）。**纯移动，零逻辑变化。**

## 基线（验收侧已在 e1be737 独立核实，不得倒退）

- `cargo test --workspace --all-targets --locked --offline`：**592 通过 / 0 失败**；
- `cargo check --workspace --all-targets --locked --offline` exit 0；
- 默认与 `+stable` 的 `cargo fmt --all -- --check`、`git diff --check` 全过。

开工第一步：自己复跑并记录基线数字（含 `-p mineintent-backend --lib` 的用例数），
写入进度日志后再动文件。

## 规则

1. **不改公开 API**：crate 外可见路径经 `runtime/mod.rs` 重导出保持不变；
   `lib.rs` 的对外形状不动。
2. **`#[cfg(test)]` 单测随其所测的私有项**进入对应子模块文件；测试总数不得减少，
   前后计数写日志。
3. 跨新子模块的私有项用最小可见性（`pub(super)` 优先于 `pub(crate)`），
   每一处升格记录进日志。
4. 模块边界按文件内现有注释分区与职责推导（先在日志里给出模块地图再动手）；
   `entity_events.rs` 是已开的先例，风格对齐它。
5. **分批提交**：每批一个内聚模块；每批后跑
   `cargo test -p mineintent-backend --locked --offline` +
   `cargo check --workspace --all-targets --locked --offline` 必须绿；
   最后一批后复跑完整基线四件套，数字与基线一致才算完成。
6. 提交信息中文，前缀 `拆分：`。
7. 范围外：vendor/、其他 crate、facade.rs/viewport.rs（它们走借道拆分，不在本任务）。
8. 进度落 `进度日志-拆分.md`（模块地图、逐批门禁数字、可见性升格清单）。

## 完成定义

runtime.rs 不复存在（或仅剩 ≤200 行的 `runtime/mod.rs` 门面）；所有新文件 ≤2000 行；
完整基线四件套数字与开工基线一致；日志齐全。
