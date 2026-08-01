# MineIntent 文档入口

## 按任务阅读

| 任务 | 阅读路径 |
|---|---|
| 判断产品方向 | [产品](../产品.md)；若文字不清楚或彼此冲突，询问维护者 |
| 运行当前代码 | [运行指南](./guides/run.md) |
| 检查、测试或验证 | [验证指南](./guides/validation.md) |
| 理解当前实现 | [当前实现结构](./architecture.md)，再阅读其中链接的代码产生方 |
| 追溯历史 | [历史来源与旧路径迁移表](./history/index.md)，再进入不可变 commit、Issue 或 PR |
| 了解原版客户端能力范围（P04 参考） | [Minecraft Java 客户端能力全景](./minecraft-client-capability-panorama.md) |
| 准备贡献 | [贡献流程](../CONTRIBUTING.md) |

## 文件权威等级

| 文件或来源 | 用途 | 产品权威 |
|---|---|---|
| [`产品.md`](../产品.md) | 产品判断与由其导出的候选架构 | 以文件内每条文字的状态为准 |
| 带“提案接受”标签的 GitHub Issue | 已接受提案入口 | 是否具有同等权威，以 `产品.md` 的 G03 状态为准 |
| [`docs/architecture.md`](./architecture.md) | 绑定到指定版本的当前实现说明 | 无 |
| [`docs/guides/run.md`](./guides/run.md) | 安装、配置、运行和排障 | 无 |
| [`docs/guides/validation.md`](./guides/validation.md) | 检查、测试和验证边界 | 无 |
| [`docs/history/index.md`](./history/index.md) | 历史证据和旧路径导航 | 无 |
| [`docs/minecraft-client-capability-panorama.md`](./minecraft-client-capability-panorama.md) | 原版客户端能力调研基线（26.1.2/775），P04 的参考材料 | 无 |
| [`README.md`](../README.md) | 项目导航和最短启动 | 无 |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | 人类贡献工作流 | 无 |
| [`CLAUDE.md`](../CLAUDE.md) | 开发代理工作规则 | 无 |
| 组件目录中的 README | 对应组件的局部使用说明 | 无 |
| 代码、类型、测试和运行结果 | 当前实现的产生方与证据 | 不产生产品权威 |

“无产品权威”不等于内容可以随意失真；它表示这些文件只能描述入口、实现、操作、证据或工作流，不能替
`产品.md` 增加产品判断。

## 维护边界

- 产品文字只在 `产品.md` 中修改；其他文件引用条目，不复制整段产品定义。
- 当前实现说明必须写明适用的分支和提交；过期时更新版本或明确标为历史。
- 操作指南只保留可以执行的步骤，并把每项检查能够证明和不能证明的范围写清楚。
- 历史正文不复制回活文档；使用不可变链接，并在路径变化时更新迁移表。
