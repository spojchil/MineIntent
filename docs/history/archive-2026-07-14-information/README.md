---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
---

# 2026-07-14 信息接口设计档案

本目录从五个保留的远端分支恢复。它们曾被写成“实现就绪基线”，但对应 PR 均未合并，不能作为当前接受设计。

| 文档 | 来源提交 | PR | 处置 |
|---|---|---|---|
| [玩家状态、物品与调试](./player-state-information.md) | `a213690` | [#64](https://github.com/spojchil/maineintent/pull/64) | closed, unmerged |
| [Screen 与 Overlay](./screen-and-overlay-information.md) | `079de7b` | [#65](https://github.com/spojchil/maineintent/pull/65) | closed, unmerged |
| [声音与生命周期](./sound-and-lifecycle-information.md) | `c31a814` | [#66](https://github.com/spojchil/maineintent/pull/66) | closed, unmerged |
| [第一人称视口](./viewport-information.md) | `ccae567` | [#68](https://github.com/spojchil/maineintent/pull/68) | closed, unmerged |
| [合法信息验收矩阵](./information-acceptance-matrix.md) | `7b89da6` → `a23823b` → `f93cf7e` | [#69](https://github.com/spojchil/maineintent/pull/69) | closed, unmerged |

2026-07-21 的统一关闭理由是：这些 PR 只有设计文档，依赖尚未实现的 Issue #63，与当时 v0.1 可运行能力无关；项目决定优先玩家可观察的功能，同时保留分支供未来重开。

## 如何使用这些材料

可以复用：

- source/projection/provider 的职责拆分；
- UI session、input ownership 和真实像素呈现的区别；
- 声音与视觉遮挡不能共用同一模型；
- scope/revision/cursor 的竞态分析；
- O0/O1a/O1b/O2 的测试裁判隔离；
- maximum-legal 与 oversize-rejected 成对边界测试。

不能直接复用：

- 当年的字段全集、revision 数字和文件布局；
- “实现就绪”“已冻结”等状态声明；
- 依赖 Issue #63 的 pagination 方案；
- 假定 Catalog → Help → Read 已经 model-facing 的叙述；
- 与当前简化 provider 或可信注视实验冲突的接口细节。

已抽取的跨方案原则见[信息接口提案入口](../../proposals/information-interfaces.md)。原分支上的公共文件修改没有强行叠加到当前架构文档；需要时应查看对应 PR diff，而不是静默合并两代契约。
