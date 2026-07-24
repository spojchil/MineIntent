---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-23
---

# 开放提案与实验

这里保存值得继续讨论、但尚未成为 MineIntent 接受基线的设计，以及已经写出代码却尚未被接受的实验。提案可以挑战 accepted 文档，却不能在决策完成前静默覆盖它。

“已经有实现”和“方向已被接受”是两件事，因此实验与提案同处一个分区，靠 `status` 区分：`proposed` 是还没写代码的候选方案，`experimental` 是已经跑起来但未被接受的实现。

## 具身接口

- [架构反思与工具接口草案](./embodiment-architecture-reflection.md)：一次完整审视，含证据、根因、一处被证伪的判断与接口草案。
- [矛盾与未决策登记册](./embodiment-decision-register.md)：把上文的事实矛盾、基线冲突和未决选项拆成带编号的条目。**这是决策的入口，也是本分区最该先读的一份。**
- [具身能力与接口清单](./embodiment-interface-inventory.md)：`historical`；事实审计仍有价值，控制契约已撤回。

## 感知与信息

- [信息接口提案与保留原则](./information-interfaces.md)：从五条已关闭、未合并 PR 中提炼的耐久原则与待决项。
- [2026-07-14 五份未接受设计档案](../history/archive-2026-07-14-information/README.md)：上文的原始材料。

## 实验

- [可信注视实验](./trustworthy-gaze.md)：`experimental`；`codex/trustworthy-passive-context@57d438e` 上真实跑通的能力边界。

认知感知和记忆的长期方向已经被接受，但实现仍不完整，分别见[认知感知模型](../architecture/cognitive-perception.md)和[记忆模型、档案版本与冲突协调](../architecture/memory-model-and-profile-versioning.md)。这里保存的是对它们的修订建议，而不是重复一份“当前设计”。

## 从提案形成决定

决策单元是[登记册](./embodiment-decision-register.md)里的一个 `Dxx` 条目，接受证据是一个**合并的 PR**：

1. 在 PR 里把该 `Dxx` 从“待决策”填成“已决定”，写清结论、理由、被否决方案和影响。
2. 同一个 PR 翻转受影响文档的 front matter `status`，并同步[文档登记表](../document-register.md)。
3. squash 合并进 `main`。此后引用这个决定，引用的是 `(#NN)`。

只有改变模型—身体接口本身的根边界决定（`D01`、`D02` 这一档）才另写 ADR。其余以“已合并的 `Dxx` 条目”存在即可——四十项各写一份 ADR，只会让登记册变成永久停车场。

对话是想法成形的地方，但结论必须在 PR 正文里重新写一遍：**说不清到能写进 PR 正文的程度，就还不算一个决定。**
