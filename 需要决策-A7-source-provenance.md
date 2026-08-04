# NEW-13：A7 连接 source provenance 需要决策

> 更新日期：2026-08-03  
> 状态：BLOCKED（实现与门禁已通过；是否允许集成仍需裁定）

## 结论先行

A7 的实体事件实现已通过以下六道门禁：

- `cargo test -p mineintent-backend entity_events --lib --locked --offline`：37/37；
- `cargo test -p mineintent-backend --lib --locked --offline`：156/156；
- `cargo check --workspace --all-targets --locked --offline`；
- 默认 `cargo fmt --all -- --check`；
- `cargo +stable fmt --all -- --check`；
- `git diff --check`（仅既有 CRLF 提示）。

Spawned/Moved/Removed/Updated/Hurt 的逐包 post-state、同批次顺序、角度单位、
速度 residual、Remove/refresh、Login/Respawn 世界边界和 publication race 均已补齐
确定性回归。当前唯一阻断不是测试失败，而是 Azalea source 事件缺少不可变的
连接身份。

## 为什么 backend-only 无法同时安全且完备

Azalea 0.16 当前事件只携带：

- `ReceiveGamePacketEvent { entity, packet }`；
- `DisconnectEvent { entity, reason }`；
- `ConnectionFailedEvent { entity, error }`；
- `WorldLoadedEvent { entity, name, world }`；
- `StartJoinServerEvent`、高层 `Event::Init` 和无 entity 的
  `SwarmEvent::Disconnect` 也没有 attempt token。

Azalea 断线时保留 Bevy `Entity`，重连可能复用它。因此 backend 看到的以下两条
轨迹完全相同：

1. A(epoch 1) 的消息已产生，B(epoch 2) 重绑同一 entity 后才被 backend 读取；
2. B(epoch 2) 重绑后产生字段相同的合法消息，再被 backend 读取。

用“当前 epoch”、时间窗、payload hash、通常会先排空或 schedule 邻接来贴标，
都会在轨迹 1 中把旧 A 伪装成 B。backend 没有足够信息构造既不误收 A、又不
误拒 B 的判别函数。

## 当前分支的保守行为及代价

当前 A7 worktree 使用 fail-closed `EntitySourceFence`：同一 Bevy entity 首次被
重连复用后，`ambiguous` 永不清除；其后所有无 token 的 packet、Login/Respawn、
WorldLoaded、disconnect/failure、Init 与 swarm disconnect 都拒绝。

它能保证旧 A 不污染 B，但也会永久拒绝合法 B，等价于该 RuntimeSession 重连后
实体 producer 和一部分 lifecycle 功能失效。因此目前没有提交/cherry-pick，
也不会把它表述为生产可接受的完整实现。

## 选项

### A（建议）：在 Azalea/vendor source 边界增加不可变 generation

为每次 `RawConnection`/join attempt 分配 `connection_generation`（或同义 token），
并在消息**产生处**盖章，而不是 backend 读取当前 entity 时补盖。最小覆盖：

1. `ReceiveGamePacketEvent`、`DisconnectEvent`、`ConnectionFailedEvent`、
   `WorldLoadedEvent`；
2. `StartJoinServerEvent` / reconnect return handoff；
3. 高层 `Event::Init`/`Event::Login` 与 `SwarmEvent::Disconnect`，或为它们提供
   一个由上述 canonical source 消费的、带 token 的有限 handoff。

backend 再把 source token 与自己的 epoch 一对一绑定，仅接受匹配项。这样既能
拒绝迟到 A，也不会牺牲合法 B。若你愿意更新 supplies/vendor，我可以给出最小
patch 设计并继续完成接线。

### B：接受当前 fail-closed 降级并集成

保留现实现，明确文档化“同 entity 重连后无 token source 永久禁用”。这在安全性
上成立，但重连功能明显残缺；我不建议作为迁移完成态。

### C：强制每次连接使用新的 Bevy Entity

若 Azalea 能提供受支持的配置/API，确保旧 entity 永不复用，也可消除这类歧义。
直接在 backend 强行销毁/替换 entity 会牵涉 UUID/world 索引、事件复制任务和
组件生命周期，当前证据不足，不建议静默采用。

## 明确排除的伪解法

不建议选择“继续按当前 writer epoch 给无 token 事件盖章”。它测试容易全绿，
但无法排除旧连接事件跨重绑污染新连接，违反 A→B 严格归属要求。

## 请回复

可只写：

```text
NEW-13: A
```

若选择 A，也请说明由我直接修改当前忽略的 vendor/supplies 并同步可追踪依赖，
还是你先提供带 generation 的 supplies 更新。
