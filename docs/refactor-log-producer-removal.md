# 施工记录：摘除自建生产者，世界信息改走状态

> 无产品权威。本文是**进行中**工作的记录，随施工更新。
>
> 判据与推导见[世界信息如何到达模型](./world-information-design.md)。
> 分支：`refactor/panic-supervision`。姊妹篇：[panic 接管层施工记录](./refactor-log-panic-supervision.md)。

## 0. 目标

把 azalea **有意不作为事件暴露**的状态变化（实体、方块、位置回拉）停止转成后端
事件。世界长什么样经视口拉取到达；事件通道只留「状态里不留痕」的那一类。

预期结果：参与者队列的输入量降三到四个数量级，四层队列的次生问题（omission 计数
不可合成、控制车道死锁环）随之自动消解。

## 1. 切口：三个插件都是**混的**

动手前逐个 system 核过。没有一个插件可以整体删除：

| 插件 | 要摘的 | **承重必须留的** |
|---|---|---|
| `EntityProducerPlugin` | `produce_entity_packet_events` 里的实体发布 | 三个连接准入 system——**正是我们 fork 的 AttemptToken 存在的理由**（「高层 Event 通道在重连复用实体时没有 attempt token，所以生命周期准入要在那些 listener 之前做」） |
| `BlockSoundProducerPlugin` | 全部 | 无（插件整个去掉） |
| `ServerPositionCorrectionPlugin` | `record_server_position_corrections` | `reset_spawn_marker_on_world_loaded`、`record_respawn_packet`——26.1 跨维度/重生走 `WorldLoadedEvent`，删了新维度不再产生 `Spawn` |

## 2. 动手前验到的三个地雷

**任何一个没查就动手，同伴都会瞎或死。**

### 2.1 光照缓存只由归约器喂

`apply_light_packet` 全仓**只有两个调用点**，都在 `produce_entity_packet_events`
内部。删掉该函数或它的读包逻辑，`frame.light` 立刻永久缺席。

⇒ 归约器**必须保留**，只从里面摘职责。

### 2.2 方块 system 自己写世界

`produce_block_update_events` 不只发事件：

```rust
let updates = std::mem::take(&mut queued.list);          // 夺走 azalea 的队列
...
let prediction_consumed = prediction_handler.update_known_server_state(position, block_state);
if !prediction_consumed {
    world.chunks.set_block_state(position, block_state);  // 自己写世界
}
```

原注释写着「Match Azalea's vendor handler exactly」，并跑在 vendor handler **之前**
把队列清空。

⇒ 删掉是安全的，**前提是 vendor handler 仍注册**。已确认：`driver.rs` 只
`disable` 了 `AutoRespawnPlugin` / `AcceptResourcePacksPlugin` / `AutoReconnectPlugin`
三个，方块插件没碰。删后 vendor handler 拿到满队列照常处理——**职责交还 azalea**。

### 2.3 `EntityProducerRuntimeState` 装着承重状态

它不只是影子缓存：

```rust
pub(super) struct EntityProducerRuntimeState {
    owner: Option<(Entity, u64)>,
    scope_generation: u64,              // ← refresh_snapshot 在读
    attempt: AttemptAdmissionState,     // ← 我们 fork 的 AttemptToken 机制
    pending_connection_failure: ...,    // ← pre-Init 取消的交接
    source_fence: EntitySourceFence,
    source_token_bindings: SourceTokenBindings,
    cache: EntityProducerCache,         // ← 只有这个是实体事件用的
}
```

⇒ 摘实体发布时**不能**连带删这个结构，只有 `cache` 会成为孤儿。

### 2.4 视口不依赖影子缓存（可以放心）

`capture_tracked_entities_impl` 直接
`ecs.query::<(Entity, MinecraftEntityId, LoadedBy, Position, Physics, LookDirection, …)>()`。
与生产者的影子无关。

## 3. 逐刀记录

### 第一刀 ✅ 位置回拉不再产生事件

提交 `dc382ff`。净减 45 行。

- 删 `record_server_position_corrections`（49 行），它是 `SelfState` 的**唯一**
  生产者
- 插件改名 `ServerPositionCorrectionPlugin` → `RespawnBoundaryPlugin`
- `SelfState` 契约变体暂留；覆盖全部 `BackendEventKind` 的夹具仍需构造它

验证：629 通过 0 失败，fmt 干净，clippy 零 error。

### 第二刀 ✅ 方块与区块不再产生事件

提交 `8575092`。净减 **669 行**。

- 删 `BlockSoundProducerPlugin` 整个（名字里的 Sound 是误导，两个 system 都是方块，
  声音在归约器里）
- 删 `produce_block_update_events`(80)、`produce_chunk_loaded_events`(62)、
  `attach_canonical_packet_source_metadata` + `CanonicalPacketSourceMetadata`、
  `record_canonical_packet_source_metadata`(29)
- 删归约器里的 `Block(ChunkUnloaded)` 发布，**保留** `remove_light_chunk`
- 删 `tests/block_sound.rs` 整个（168 行，两个测试都是方块，无声音断言）
- 删 `raw_reducer` 的两个方块测试（162 行）
- **写世界的职责交还 azalea**（见 §2.2）

验证：625 通过 0 失败（原 629，删掉 4 个方块测试），fmt 干净，clippy 零 error，
新增 dead-code 警告 0。

### 第三刀 ⏳ 实体不再产生事件（最大且最需小心）

从 `produce_entity_packet_events`（371–770，约 400 行）里摘掉 14 处
`emit_entity_input*` / `emit_entity_motion_residual`，**保住**：

- `emit_canonical_sound`（声音发布）
- `apply_light_packet`（光照缓存，见 §2.1）
- 作用域/维度记账

连带成为死代码的预估：

```
crates/backend/src/entity_events.rs                       1536
crates/backend/src/runtime/entity_events_owner_tests/     2083
crates/backend/src/runtime/entity.rs 的 emit_entity_*        ~80
producers.rs 里实体那半                                    ~250
                                                        ─────
                                                       约 3900 行
```

### 第四刀 ⏳ 死代码清理

契约侧 `ProtocolEntityEvent` / `BackendEventPayload::{Entity,Block,SelfState}` /
`BackendEventKind::{Entity,Block,SelfState}` 的去留。这三个变体现在都没有生产者，
但移除是 wire 变更，单独一刀。

### 第五刀 ⏳ 事件通道取「丙」

折叠视图（此刻该知道什么）+ 轮间摘要（错过了什么）。设计见
[世界信息设计](./world-information-design.md) §5。

**声音的呈现窗口维护者已明确暂缓**，本刀不含声音形态改造。

## 4. 测试策略：主题保住，载体更换

删掉生产者会让一批测试失去驱动，但**它们的主题往往不是被删的那个东西**。

典型：`stamped_identity.rs` 的三处断言在「AttemptToken 跨同实体重连的绑定」测试里
——那是我们 fork 存在的理由，主题是**绑定**，方块包只是「一条被发布的观察事实」的
载体。处理方式是换载体（改用声音包驱动，新增 `queue_production_sound_packet` /
`sound_events`），**覆盖不变**。

另一种情况：`emit_canonical_observation_event` 生产侧已无调用者，但两处测试要的正是
「在某个来源上发布一条**载荷无关**的事实」这个原语（验证晚到作用域 fail-closed 与
跨 attempt 拒绝）。保留并标 `#[cfg(test)]`，注释写明原因——**它不是死代码，是测试
专用 API**。

判据：**先问这个测试在测什么**，再决定删除还是换载体。

## 5. 每刀的验证口径

```sh
export PATH="$HOME/.rustup/toolchains/nightly-x86_64-apple-darwin/bin:$PATH"
cargo fmt --all --check
cargo clippy --workspace --all-targets      # error 必须为 0
cargo test --workspace --all-targets --no-fail-fast
```

外加：**新增 dead-code 警告必须为 0**——摘完之后留下的孤儿要么删、要么写明为什么
保留。分支既有的 4 条（`field id` ×2、`value_at`、`DispatchBehavior::Panic`）不在
本次范围内。
