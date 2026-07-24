---
status: accepted
authority: normative
implementation: not-applicable
last_verified: 2026-07-23
---

# 文档治理规则

本文经 PR #72 接受。按本文自己的规则，这就是它的引用地址。

本规则解决一个具体问题：MineIntent 同时有长期愿景、已接受决定、已实现代码、实验分支、设计反思和历史资料。它们都值得保存，但不能都自称“当前权威”。

规则**只有一条是硬的**：`main` 只通过合并 PR 变更。其余都是低成本的记账习惯，靠 `pnpm check:docs` 自动执行，不依赖自觉。

## 必填元数据

`docs/` 下的项目文档在标题前使用 YAML front matter：

```yaml
---
status: proposed
authority: informative
implementation: partial
last_verified: 2026-07-23
---
```

若文档只描述特定基线，还须补一个来源字段：

| 字段 | 用于 | 例 |
|---|---|---|
| `applies_to` | 文档描述**某个分支/提交上的系统状态**，基线前进后需要重新核对 | `applies_to: codex/trustworthy-passive-context@57d438e` |
| `source_commit` | 文档正文**写于某个提交**，此后不随基线自动失效（反思、登记册一类） | `source_commit: e50c8f9` |

`check:docs` 强制两条，不多不少：

- **`status: experimental` 的文档必须有 `applies_to`。**实验的全部意义就是“在某个基线上跑通了”，不写基线的实验声明无法核对。
- 两个字段一旦出现，格式必须是 `分支@短提交` 或 `短提交`（`main@53ebc57`、`e50c8f9`）。

其余文档写不写由作者判断——描述稳定契约的 ADR 和导航 README 不需要基线。**规则写多少就强制多少**：政策里不放没人检查的“必须”。

## `status`

| 值 | 含义 |
|---|---|
| `accepted` | 已经过明确决策流程，当前仍有效 |
| `proposed` | 候选方案或开放问题，尚不能约束实现 |
| `experimental` | 已在代码或测试中试做，但尚未成为接受基线 |
| `historical` | 保存过去的事实或方案，不描述当前要求 |
| `reference` | 操作手册、状态快照、调研或接口的人类可读视图 |

**合并的 PR 是接受证据。**直接提交到分支证明“实现存在”，不证明方向已接受；若 PR 自己声明仍待决策，仍应标为 `proposed`。

## `authority`

| 值 | 含义 |
|---|---|
| `normative` | 在自身范围内规定产品或架构必须怎样 |
| `informative` | 报告事实、解释候选方案或保存证据 |

只有 `accepted + normative` 可以作为长期约束。`proposed`、`experimental` 和 `historical` 文档必须是 `informative`。

## `implementation`

| 值 | 含义 |
|---|---|
| `current` | 所述能力在指定基线上存在并已核对 |
| `partial` | 只有文档的一部分被实现 |
| `planned` | 已接受，但实现尚未开始或尚未达到契约 |
| `stalled` | 已接受计划因依赖、优先级或路线重开而停滞 |
| `diverged` | 已接受文档和当前实现发生实质漂移，尚未正式协调 |
| `retired` | 过去存在或曾被提出，现在不再是活动实现路线 |
| `not-applicable` | 愿景、治理或纯研究材料没有实现状态 |

## 唯一的硬规则

**`main` 只通过合并 PR 变更，不强推。**

这条替代了原先的四步流程（先开 Discussion、再写提案、再写 ADR、再同步文档）。预登记的收益在单人项目里最低，成本最高，因此取消。剩下的要求是：一件事可以在任何地方想清楚，但它必须以一个可点开的 PR 落地。

`main` 的分支保护已按此规则配置，规则由 GitHub 强制，不依赖执行者的记性：

| 设置 | 值 |
|---|---|
| 必须经 PR 合并 | 是，要求 0 个 approval |
| force push | 禁止 |
| 删除分支 | 禁止 |
| 必需状态检查 | `Verify` |
| 对管理员强制 | 否 |

两处刻意的放宽：`required_approving_review_count` 为 0，因为单人仓库中作者无法批准自己的 PR，设为 1 会使任何 PR 都无法合并；`enforce_admins` 为否，保留一个逃生口。两者都可以在项目转为多人协作时收紧。

## 怎样接受一个决定

决策单元是[具身决策登记册](./proposals/embodiment-decision-register.md)里的 `Dxx` 条目。接受它 = 合并一个 PR，PR 内含：

1. 把该 `Dxx` 从“待决策”填成“已决定”，写清**结论、理由、被否决方案、对代码和文档的影响**。
2. 翻转受影响文档的 front matter `status`。
3. 同步[文档登记表](./document-register.md)。

squash 合并后，`main` 上会留下一行带 `(#NN)` 的提交——那就是这个决定的引用地址。此后引用它，引用 `(#NN)`，不引用任何对话。

**对话是想法成形的地方，不是证据。**结论必须在 PR 正文里重新写一遍；说不清到能写进 PR 正文的程度，就还不算一个决定。

只有改变模型—身体接口本身的根边界决定（`D01`、`D02` 这一档）才另写 ADR。其余以“已合并的 `Dxx`”存在即可。

## 需要顺手更新的地方

改变代码能力、分支、测试或 Issue 事实时，顺带更新：

- [`current-status.md`](./current-status.md)
- [`architecture/current-system.md`](./architecture/current-system.md)
- 相关的实验或操作指南
- [`document-register.md`](./document-register.md)（新增、移动或改变文档身份时）

这些是习惯而非门槛：漏掉不会挡住合并，但会让下一个读者判断错误。

## 冲突处理

- 代码与参考文档冲突：先修参考文档；若代码违背 accepted ADR，同时登记架构漂移。
- 两份 accepted 文档冲突：不能凭日期自行覆盖，必须通过 PR 明确 supersede 或 amend。
- 提案与 accepted 文档冲突：保留冲突，直到决策完成。
- 历史文档使用“当前”“现行”等词时：在文首加醒目历史说明，不必篡改原始正文语气。
- Issue/Milestone 与代码冲突：状态文档同时记录两者，不能用其中一方假装另一方不存在。

## 一份好文档的样子（建议，非门槛）

- 状态和 authority 明确；
- 当前实现与目标设计分开描述；
- 链接没有断裂（这条由 `check:docs` 强制）；
- 已接受、实验、提案和历史材料没有混在同一叙述层；
- 关键结论能够追溯到代码、测试、Issue、PR、ADR 或研究来源；
- 被替代的高价值思考有归档位置和去向说明。
