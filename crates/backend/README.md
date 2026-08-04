# mineintent-backend

MineIntent 自有 Minecraft 协议后端。固定目标 **Paper 26.1.2 / 协议号 775**，
协议执行层用 Azalea `0.16.0+mc26.1`。

## 为什么依赖的是 fork 而不是 crates.io 版本

`Cargo.toml` 里 azalea 指向 `spojchil/azalea` 的
`mineintent/attempt-identity-0.16` 分支并钉死 rev。基座是 0.16.0 的发布提交
`a253702e`，其上只有一个提交 `d0cc847`，解决一个上游没有、而我们必须有的问题：

**Azalea 断线时保留 Bevy `Entity`，重连可能复用同一个。** 因此后端看到的两条轨迹
在结构上无法区分：一条是旧连接 A 的迟到消息在新连接 B 绑定同一 entity 后才被读到，
另一条是 B 自己产生的合法消息。用「当前 epoch」「时间窗」「payload 哈希」贴标都会
在第一条轨迹里把 A 误当成 B。

补丁在**消息产生处**为每次 join 尝试盖一枚不可变 `AttemptToken`，并让
`ReceiveGamePacket` / `Disconnect` / `ConnectionFailed` / `WorldLoaded` /
`SwarmEvent::Disconnect` 全部携带它；同时提供按 `(entity, token)` 精确取消
`CreateConnectionTask` 的 hook，使连接阶段的超时能真正取消在途 socket，
而不是丢弃一个已经开始 poll 的 future 后祈祷它不会绑定下一次尝试。

后端把这枚 token 与自己的 connection epoch 一对一绑定，只接受匹配项——
既不误收迟到的 A，也不误拒合法的 B。

升级 azalea 时必须同时更新此说明与 rev；补丁的逐文件清单在档案分支的
`vendor-patches/azalea-0.16-attempt-identity.md`。

## 层内边界

- 只对外暴露 `facade::MinecraftBackendFacade`（实现契约层的 `MinecraftBackendApi`）；
  Azalea 类型不越过这层。
- 运行时跑在专属线程的 current-thread Tokio runtime + `LocalSet` 上；
  公开 API 从任意调用方运行时进入，经该线程串行化。
- 观察面（snapshot / frame facts / observation source）在**死亡状态下仍可读**，
  动作面只在 Ready 下放行，唯一例外是 `respawn`——死亡时唯一有意义的动作。
