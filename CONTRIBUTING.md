# MineIntent 贡献流程

## 1. 选择工作

- 非琐碎工作先关联一个 GitHub Issue，确认问题边界和预期产物。
- 开始前阅读[文档入口](./docs/README.md)；涉及产品判断时，直接核对[产品正文](./docs/产品.md)和对应 Issue。
- 提新模块或抽象前先用 `rg` 搜索现有实现、类型和测试。

## 2. 创建分支

- 从最新 `main` 创建短期分支。
- 使用 `feat/`、`fix/`、`docs/`、`refactor/`、`test/` 或 `research/` 前缀。
- 不覆盖或清理他人的未提交改动；需要并行工作时使用独立 worktree。

## 3. 实现与记录

- 一个 Pull Request 解决一个清晰问题。
- 设计理由写入 commit message、紧贴实现的注释或关联 Issue；不要新建长期状态文档代替证据。
- 若改动引入或改变产品语义，在 PR 的“产品假设”中引用准确的产品条目或提案 Issue，并列出仍未决定的部分。
- 不提交 API 密钥、令牌、私人聊天、世界存档、未脱敏日志或本地运行数据。

## 4. 验证

按改动范围运行[验证指南](./docs/guides/validation.md)中的对应检查，并在 PR 中区分：

- 实际运行过的检查；
- 人工观察或真实服务器证据；
- 尚未验证的推断。

改动 `crates/` 时另跑 Rust 工作区的三件套（与 CI 相同）：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace --all-targets --locked
```

工具链由 `rust-toolchain.toml` 钉住（nightly，随 Azalea 的要求），
`rustfmt`/`clippy` 已在其中声明，无需另行安装。
运行方式与环境变量见 [Rust workspace 指南](./docs/guides/rust-workspace.md)。

## 5. Pull Request

- 使用简体中文填写人工创建的 Issue、Pull Request 和审查交流；代码标识与 Conventional Commit 类型可以使用英文。
- PR 正文说明目标、方案、验证、影响和关联 Issue。
- `main` 只通过 Pull Request 变更；合并后删除短期分支。
- 审查期间发现产品文字不清楚或互相冲突时，停止自行推导并询问维护者。
