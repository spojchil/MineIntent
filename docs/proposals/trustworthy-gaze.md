---
status: experimental
authority: informative
implementation: current
last_verified: 2026-07-23
applies_to: codex/trustworthy-passive-context@57d438e
---

# 可信注视纵向实验

## 假设

原假设是：如果模型只能使用合法第一人称观察，语义指代被绑定到本轮证据，身体控制短、有界且必须通过新观察验证，那么“看向我”可以做到不借用 tracked entity 坐标、不伪造完成，并在目标暂时不在视野中时自然扫描。代码审计确认前三段大体存在，但“新观察”目前没有 revision guard，因此该假设只被部分验证。

## 实现切片

相关提交：

- [`e9616c1`](https://github.com/spojchil/maineintent/commit/e9616c1)：签发 viewport refs；
- [`ecd13cf`](https://github.com/spojchil/maineintent/commit/ecd13cf)：Grounding；
- [`08bf5a6`](https://github.com/spojchil/maineintent/commit/08bf5a6)：Behavior Synthesizer；
- [`893975b`](https://github.com/spojchil/maineintent/commit/893975b)：视觉 controller 与结果验证；
- [`f329ba5`](https://github.com/spojchil/maineintent/commit/f329ba5)：运行时接线；
- [`fedab47`](https://github.com/spojchil/maineintent/commit/fedab47)：Paper 场景；
- [`0084a98`](https://github.com/spojchil/maineintent/commit/0084a98)：ref 与本轮 Read 绑定；
- [`a97a5fe`](https://github.com/spojchil/maineintent/commit/a97a5fe)：保守可见性边界。

链路：

```text
player message / viewport ref
  → semantic embodied intent
  → scoped grounding handle
  → self.attention_includes operator
  → bounded visual plan
  → gradual look or identity scan
  → current first-person visibility re-check
      (fresh revision not enforced)
  → outcome_verified
  → dependent terminal speech
```

## 已证明

- 模型不能提交绝对坐标、协议实体 ID 或 Mineflayer 对象。
- 已知目标会在执行时重新解析和复核。
- 只有身份、没有方向时，可以在有限预算内连续扫描。
- Motor 完成后还会复查当前可见性，复查通过才产生名为 `outcome_verified` 的事件。
- 取消、scope 变化和 deadline 会终止 controller 并释放输入。
- 不支持的语义不会回退到旧 skill。

## 没有证明

- 该 Grounding/Behavior 分层优于短动作 Tool Loop。
- 对方块、复杂实体、多目标或长时行为同样自然。
- 真实 DeepSeek 能稳定生成所有必需结构；Paper 场景使用的是确定性 scenario model。
- locomotion、交互、背包选择、战斗或采集能力存在。
- Claim Policy 能验证模型生成的自然语言内容。
- 同伴会主动发现机会或维持长期共同活动。
- 结果证据一定来自比动作前更新的 perception revision；当前实现不比较 revision，目标起初已对准时甚至可能 0 次 `look` 完成。
- `message_referent` 能解析消息中的任意对象；它目前只校验子串，再一律绑定消息发送者。

## 当前验证结果

- Node 24：相关测试包含在 123/123 通过结果中。
- Node 22.23.1：deadline 测试会因 `AbortSignal.timeout()` 不保持空事件循环而 cancelled，完整测试进程退出 1。
- 最新实验分支没有 GitHub Actions run，也没有正式 Paper + DeepSeek 端到端记录。

## 决策用途

本实验应作为具身接口 A/B 的一个基线，而不是默认胜者。对比候选 Tool Loop 时至少测量：结构化输出有效率、静默失败率、恢复轮次、调用数、延迟、成本、隐藏信息泄漏、无依据话术和动作自然度。
