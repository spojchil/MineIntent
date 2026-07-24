---
status: accepted
authority: normative
implementation: not-applicable
last_verified: 2026-07-23
---

# MineIntent 贡献与开发规范

## 1. 基本原则

- 以当前产品设计、系统设计和已接受 ADR 为实现依据。
- 优先完成可验证的纵向场景，不先建立没有真实使用者的抽象层。
- Minecraft 真实状态和程序验证结果高于模型声明。
- 不提交密钥、玩家私人数据、世界存档、大型服务端文件或第三方研究仓库。
- 发现现有设计不成立时先更新设计决定，不用代码静默改变产品方向。

### 1.1 协作语言

- 人工创建的 Issue 标题、正文和评论使用简体中文。
- 人工创建的 Pull Request 标题、正文和审查交流使用简体中文。
- 代码标识、API 名称、技术术语、分支名和 Conventional Commit 类型可以保留英文。
- Commit 的类型前缀保留英文，说明部分优先使用中文，例如 `docs: 明确活动漂移语义`。
- Dependabot、GitHub Actions 等第三方自动化生成的标题和内容允许保留英文。
- 引用英文错误信息或上游资料时保留原文，并在需要时补充中文说明。

## 2. 工作跟踪

非琐碎工作应有 GitHub Issue，包括：

- 功能、Bug、测试与可靠性工作。
- 产品或系统设计问题。
- 有明确结论要求的技术调研。
- 会改变公共接口、持久化或运行语义的重构。

拼写修复、同一 PR 范围内的微小整理不强制建立独立 Issue。

较大的工作使用父 Issue 和 sub-issues 拆分。Milestone 表示可验收的产品增量，不用作无期限的想法收集箱。

## 3. 提案与架构决策

会改变产品原则、系统边界、数据所有权或关键运行语义的修改，其结论必须以**一个合并的 PR** 落地：想清楚它的地方不限，但它必须以可点开的形式存在。PR 正文需包含：

1. 问题和现实场景。
2. 备选方案与取舍。
3. 结论，以及被否决的方案。

squash 合并后 `main` 上留下的 `(#NN)` 就是这个决定的引用地址。此后引用它，引用 `(#NN)`，不引用任何对话——**说不清到能写进 PR 正文的程度，就还不算一个决定。**

尚未接受的内容进入 [`docs/proposals/`](./docs/proposals/README.md)，具身方向的未决项以[决策登记册](./docs/proposals/embodiment-decision-register.md)里的 `Dxx` 条目为单元。只有改变模型—身体接口本身的根边界决定才另写 ADR 到 `docs/decisions/`；其余以“已合并的 `Dxx`”存在即可。

## 4. 分支策略

项目使用 GitHub Flow，不建立长期 `develop`、`release` 或 feature 分支。

从最新 `main` 创建短期分支：

```text
feat/<issue>-<name>
fix/<issue>-<name>
docs/<issue>-<name>
refactor/<issue>-<name>
research/<issue>-<name>
```

示例：

```text
feat/18-event-journal
docs/12-decision-contract
fix/31-release-movement-lock
```

一个分支和 PR 应尽量只解决一个清晰问题。合并后删除分支。

## 5. Commit

使用简化的 Conventional Commits：

```text
feat: add companion event journal
fix: release movement lock on cancellation
docs: define activity alignment semantics
test: cover profile-memory reconciliation
refactor: isolate Mineflayer adapter
chore: configure CI
```

提交应保持可理解，不提交明显损坏的中间状态。PR 内可以有多个开发提交，合并时使用 Squash Merge。

## 6. Pull Request

非琐碎改动通过 PR 进入 `main`。PR 应：

- 关联对应 Issue，并在适用时使用 `Closes #<number>`。
- 说明目标、方案、验证和影响。
- 保持范围足够小，便于理解和回滚。
- 同步更新受到影响的设计、接口和测试文档。
- 通过所有自动检查。

只有注释、拼写等不改变行为的极小改动可以由维护者直接提交到 `main`。

## 7. 验证要求

提交 PR 前至少运行：

```powershell
pnpm install --frozen-lockfile
pnpm check
pnpm check:docs
pnpm test
```

涉及 Minecraft 行为时，还应提供相应验证证据：

- 单元测试或状态机测试。
- 可重复的服务器集成测试。
- 使用的 Minecraft/Paper/Mineflayer 版本。
- 动作结果和真实世界验证。
- 失败、取消和清理路径。

不能用模型输出“成功”代替游戏状态验证。

## 8. AI 辅助开发

允许使用 AI 编写和审查代码，但提交者仍对结果负责：

- AI 生成的实现必须经过阅读和实际验证。
- 不把生产密钥、私人聊天、玩家数据或未公开世界文件发送给外部模型。
- 不因 AI 建议而跳过 Issue、设计决定、测试或代码审查。
- 大范围生成改动应拆分成可审查的 PR。
- PR 描述应说明实际验证结果，不写无法证实的结论。

## 9. 文档维护

文档按读者实际会问的五个问题分区，另有四份被所有分区引用的根文档：

| 位置 | 回答的问题 |
|---|---|
| [`docs/architecture/`](./docs/architecture/README.md) | 系统现在如何运行？目标架构是什么？（当前实况与已接受契约分开标识） |
| [`docs/decisions/`](./docs/decisions/README.md) | 哪项长期决定被正式接受？ |
| [`docs/proposals/`](./docs/proposals/README.md) | 现在还在争什么？（未接受方案与已实现但未接受的实验同处一区，靠 `status` 区分） |
| [`docs/guides/`](./docs/guides/README.md) | 怎么运行和验证？接口是什么形状？（代码 schema 仍优先） |
| [`docs/history/`](./docs/history/README.md) | 为什么走到今天？（演进、旧里程碑、调研、档案；不参与当前权威解析） |

| 根文档 | 作用 |
|---|---|
| [`docs/README.md`](./docs/README.md) | 文档总入口与真相优先级 |
| [`docs/product-design.md`](./docs/product-design.md) | 产品体验与范围，唯一的产品北极星 |
| [`docs/current-status.md`](./docs/current-status.md) | 分支、代码能力、测试和 tracker 现实 |
| [`docs/document-register.md`](./docs/document-register.md) | 逐文件的位置、身份与来源 |

文档按[治理规则](./docs/documentation-policy.md)区分决策状态、权威和实现状态，`pnpm check:docs` 会校验元数据、相对链接和登记完整性。历史材料不再从仓库删除，也不能继续使用未加说明的“当前基线”身份。

**硬规则只有一条：`main` 只通过合并 PR 变更。**其余（顺手更新状态页、登记表和相关设计文档）是习惯，不是门。

## 10. 安全报告

不要在公开 Issue 中提交密钥、访问令牌、服务器地址、私人日志或可利用漏洞的敏感细节。安全问题使用仓库的私密漏洞报告渠道。
