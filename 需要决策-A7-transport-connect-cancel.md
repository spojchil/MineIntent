# A7 transport pre-Init connect cancellation：需要决策

日期：2026-08-03
来源工作树：`backend-producers`

## 结论

当前锁定的 Azalea 0.16 公共 API 不能证明 backend 能在 transport `Init` 之前安全取消一个
已经开始 poll 的连接，并让同一进程中的下一 attempt 独立启动而不让旧连接迟到绑定。
因此本文件阻塞的是**生产 connect deadline 的 pre-Init 取消/下一 attempt 安全证明**，不是
独立的 backoff、retry ordinal、Init 后 login/spawn deadline、generation 门控或 stop watchdog
语义。

## 精确源码/API 证据

- `vendor/azalea/src/swarm/builder.rs`：`SwarmBuilder::start` 转发到
  `start_with_opts`；后者在 LocalSet 内部 `task::spawn_local`，调用
  `swarm.add_with_opts(...).await`。该在途 `add_with_opts` future 的 handle 没有返回给调用者。
- `vendor/azalea/src/swarm/mod.rs`：`Swarm::add_with_opts` 等待
  `Client::start_client(...).await`，取得 `Client` 后才向 client ECS entity 写入 state 并启动
  event-copying task。调用方在 await 返回前没有 `Client` 可调用。
- `vendor/azalea/src/client_impl/mod.rs`：`Client::start_client` 通过
  `StartJoinServerEvent` 启动连接并等待 callback；公开 `Client::disconnect()` 发送
  `DisconnectEvent`，`Client::exit()` 发送 `AppExit`，二者都要求已有 `Client`，不能覆盖
  尚未返回 Client 的 pre-Init 阶段。
- `vendor/azalea-client/src/plugins/join.rs`：join handler 为 ECS entity 启动
  `CreateConnectionTask`，其顺序是 `Connection::new/new_with_proxy`、发送 intention、再
  `conn.login()`；只有 task 完成后才插入 `RawConnection`/`InLoginState`。
- `vendor/azalea-client/src/plugins/disconnect.rs`：Disconnect 清理 bundle 包含已建立的
  `RawConnection` 等资源，但不包含 `CreateConnectionTask`。
- `vendor/bevy_tasks/src/task.rs`：`Task` 的 drop/cancel 具备 future 取消语义；但当前
  `Swarm::add_with_opts` 不把对应 ECS entity 暴露给 backend，因此仅丢弃已经 poll 的
  `add_with_opts` future 不是旧 socket 不会继续连接/绑定的证明。

## 最小可行 seam（需要维护者裁决）

1. 不改 wire DTO 的情况下，让 backend-owned Azalea plugin 在
   `StartJoinServerEvent` 被处理后捕获 `(entity, attempt token)`，并把这个 identity 交给
   runtime。
2. connect deadline 在 `command_admission` 下精确校验 entity、epoch、retry ordinal 与
   phase generation；锁外取消该 entity 的 `CreateConnectionTask`，并使旧
   `add_with_opts` 返回后的 Client 只能走 stale/disconnect 路径，不能绑定下一 attempt。
3. 用一个 runtime-shaped test 证明 A 的 task/socket 在 timeout 后被取消，B 的
   `ConnectionRequested`/`Init` 只能消费 B token。

这通常需要 vendor 暴露/调整一个最小 cancellation hook（例如返回 add task/entity 或提供按
`StartJoinServerEvent` token 取消的 API）；本轮约束禁止修改 vendor、manifest、lock，故不自行
实现该 seam，也不把事件发布伪装成生产 connect timeout 已完成。

## 当前可继续的范围

可以继续并验证：完整 `RunConfig` 映射、独立 epoch/retry ordinal、确定性 backoff 与 stable
reset、`Init -> Login -> Ready` 的 login/spawn deadline、phase/stable/reconnect 的
generation/late-callback 门控，以及与 `OperationControl` 分开的 stop cleanup watchdog。

尚未验证且必须明确保留为 blocked：Paper 真实断网、黑洞 socket、慢登录、慢 spawn，以及
`begin -> Init` pre-Init connect deadline 的真实取消/再连接时序。
