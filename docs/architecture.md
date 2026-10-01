# 当前实现结构

> 无产品权威。绑定 `feat/keymouse-actions` 当前工作树与
> Azalea fork `4e29bc2eeee77bf81c21a0a803dfb6839d8371ae`、midturn
> `bf8bc7a7126dc2943145b03a0b4a3481de8177e1`。
>
> 本仓只有一条线。此前并存的两套实现都已移出：
> TypeScript 原型（`src/`、`agent-service/`、`mcserver/`）已随本线落 `main` 删除，
> 代码快照见 tag `typescript-prototype`（`e9b18c4`）；
> 早期 Rust 移植（`crates/{app,backend,middle,contracts,toolloop}`）已删除，
> 证据见 git 历史。

## 0. 一句话

`companion` 是唯一组合根、唯一可执行。它把**接入世界**、**工具编排**、
**上下文策略**、**模型内核**四件事接在一起，其余 crate 各自只做一件事。

## 1. 依赖图

```
                      companion（组合根，唯一知道所有人的地方）
                          │
   ┌────────┬─────────┬───┴────┬────────┬───────┬────────┬─────────┬──────┐
   ▼        ▼         ▼        ▼        ▼       ▼        ▼         ▼      ▼
context perception screens   input    hand    jobs   presence   memory  wait
   │        │  │      │  │      │  │    │  │    │  │     │  │      │      │
   │        │  └───┐  │  │      │  │    │  │    │  │     │  │      │      │
   ▼        ▼      ▼  ▼  ▼      ▼  ▼    ▼  ▼    ▼  ▼     ▼  ▼      ▼      ▼
 render ──→ world        dispatch ────────────────────────────────────→ agent
   │                         │                                        ▲
   └─────────────────────────┴────────────────────────────────────────┘
```

两条读法上要注意的：

- **`agent` 不认识 MineIntent。** 它是零项目依赖的模型—工具循环内核，
  本体是独立仓库 midturn 的 git 依赖（见 §5）。
- **`world` 不认识模型。** 它只做「原版连接与感知」，出口是 tick 快照。

## 2. 各 crate 一句话

| crate | 职责 | 关键约束 |
|---|---|---|
| `world` | 接入 azalea、tick 快照、成像用的方块与光照拷贝 | **直译无损、政策外置**：不过滤、不判重要性、不做丢弃决策 |
| `render` | 快照 → 模型可读文字 | **全部纯函数**；呈现选择归此处，事实归快照 |
| `vision` | 已有方块状态 + 本地客户端资源 → PNG 图片原型 | CPU 三角形光栅化、视锥裁剪、深度缓冲与透明合成，按需成像；无窗口或游戏模拟；配置资源后供 `view` 工具调用，限制见 [vision](../crates/vision/README.md) |
| `perception` | 按需图片（`view`） | 图片通过 `PictureDoor` 获取，保留为原生图片回执；模型看世界只经画面 |
| `screens` | 界面互斥域（`chat_box`/`inventory`/`container`） | 屏的状态转换在此，占用账本在 dispatch；容器屏真相在服务端，组合根随屏事实翻转 |
| `input` | 键鼠（`input`）：WASD/空格/Shift/Ctrl、左右键、鼠标相对转动 | 一次调用按住若干秒后全部松开，松开后才返回回执；左右键作用于准星所指，**不收坐标**；合法性由原版物理与服务端自我仲裁 |
| `hand` | 瞬时键（`hand`）：快捷栏、丢弃、主副手对调 | 发出即完，结果由物品栏变化证实 |
| `jobs` | 任务表（`list`） | 只读、`ToolClass::Free`；槽位是唯一真相源，不另建镜像 |
| `presence` | 生死去留（`respawn`） | 死亡时唯一还放行的一类（`ToolClass::Vital`）；自动重生已关，起不起来是模型自己的事 |
| `memory` | 单文件长期记忆 | 一个文件、两张脸（工具面 `remember` 与策略面落盘）、一个出口 |
| `wait` | 让时间过去（`wait`） | `ToolClass::Free`；**永远可打断**，判据由组合根的门铃给（见下） |
| `context` | 提示装配与压缩 | 受保护**两段**（人设、记忆）每轮现拉、永不参与压缩；处境不在前缀里，随帧追加 |
| `dispatch` | 工具编排 | 互斥域账本归此层；工具模块只做状态转换 |
| `agent` | 模型—工具循环内核 | 零项目依赖；见 §5 |
| `companion` | 组合根 | 唯一 `main`；唤醒脚手架也在这里 |

`wait` 的打断判据不在 `wait` 里：组合根的 `doorbell.rs` 同时是 `agent::Observer` 和
`wait::Interruptions`。判据是两个计数的差——唤醒每投递一批敲一次铃，每次模型请求开始
拍一张快照，**计数比快照大 = 有模型还没看见的唤醒**，那就一秒都不等。这补上了「模型
决定要等」到「等真的开始」之间那次推理（实测 3–4 秒）的窗口。

**铃由组合根的唤醒投递点敲，帧那一侧不敲。**两个投递点都走 `NextModelRequest`，投递
类别分不出谁是谁；而帧的发车闸门是「有非终局进展」，只要同伴在动就一直来。把帧算作
打断，等待会在它最主要的用途上当场失效：开始挖、`wait`、进展帧、立刻醒、再 `wait`
——正是这件工具要消灭的轮询。判据钉在唯一知道区别的地方，比钉在一个已经不承载这个
区分的投递类别上结实：后者在类别改动时会静默失效，而且单测照样绿。

## 3. 数据怎么到模型

**世界长什么样以状态到达**（拉取，最新赢）；只有**在状态里不留痕**的瞬时事
才走事件通道。

```
azalea ECS ──每 tick──→ TickSnapshot (latest-wins, Arc)
                            │
                            ├─ self/entities/players/world_meta  ← 状态
                            └─ chat/damage/jobs: Window<T>        ← 时间窗
                                    │
                                  render（纯函数）
                                    │
                       companion::situation（随帧追加，不进前缀）
                                    │
                                  agent → 模型
```

方块**不在快照里**——最深最重的嵌套留在 azalea 世界模型原地。模型看方块只经画面
（`view`）。

图片另有 `Module::capture_view`：按原版口径取视距内全部区块、全世界高度。视距是
客户端默认视距 12 区块（登录前随客户端信息上报）与服务端视距（登录包与
`SetChunkCacheRadius`）的较小者。世界读锁里把各区块的分段原样克隆出来（冻结成
同一时刻的副本），随即放锁，逐格解码与成像都在锁外；在锁内逐格解码会饿住客户端
循环，心跳超时被踢。和原版只建视锥里的区块段一样，调用方用同一份位姿快照与视距
给出区块段判据（`vision::section_filter`，与成像剔除区块段是同一个视锥；世界层不知道
相机），锁外只多线程解码判据内的区块段、贴着它们的一圈（表面判定与平滑光照要查
邻格）和脚边一圈（未加载计数），状态按区块段连续存放。只交判据内的**表面**方块
（按 y、z、x 排回整体扫描序）
（`RegionBlock`：六面里至少一面没被完整不透明方块挡住；流体另算同种流体相邻），
每块附六面遮挡位供成像剔面；身边 16 格内有未加载格时报出数量，更远处未加载的区块和
原版一样不画。另附地平线高度（超平坦是世界底，其余 63）与主世界时钟累计 tick。

生物群系：同在读锁里取注册表的生物群系名表与各段的生物群系调色板（4×4×4 一格的
「四分格」），锁外只交成像用得到的格——每个表面方块的原版模糊取样（`BiomeManager`
八角）与 5×5 染色混合所及的格，加相机眼睛周围 6³ 格（环境属性的高斯取样）；
另附登录/重生包里服务端已哈希的 `seed`（模糊取样的扰动种子，`state.rs` 的
`biome_zoom_seed`）与世界的四分格高度范围。

光照：azalea 解析光照包但不存，`machine/light.rs` 的 `LightStore` 按原版
`applyLightData` 口径自存每柱天空光与方块光（区块包整柱重来、光照更新包逐段覆盖），
随区块卸载与换维度清掉。`capture_view` 同在世界读锁里克隆光照段（`Arc`，只加引用
计数），锁外为表面方块周围 3×3×3、未遮挡面外两格的侧向格与实体眼睛所在格附上
`LightCell`：存储的天空光/方块光加该格方块的原版渲染属性（发光、透光、
`isViewBlocking`、`isSolidRender`、`emissiveRendering`、碰撞箱是否完整方块）。
这些属性 azalea 不提供，由 `crates/world/data/BlockRenderDump.java` 对 26.1.2 服务端
导出成按 `state_id` 排列的二进制表随源码入库。姿态取最新 tick 快照，与
方块复制不是原子化的同一服务端 tick。`vision` 的可选 `live` feature 只供连接
探针使用；纯渲染默认不依赖 world/Azalea。画面含方块、准星与实体：组合根把同一份
快照里的实体（种类、脚底位置、朝向、玩家 UUID、掉落物的物品名）交给 `vision`，
几何按 26.1.2 客户端模型类转录，深度缓冲决定遮挡——墙后的实体不会出现在画面里。

配置 `MINEINTENT_CLIENT_JAR` 后，组合根注册 `perception::PictureTools` 的 `view {}`
工具（Free 类）。`companion::picture::ModulePictureDoor` 在阻塞池里采集视距内表面方块、
检查身边已加载、调用 `vision` 渲染并编码 PNG；资源在进程内复用。图片回执
只含图片及通用限制，不发送原始方块清单或包含被遮挡方块名称的渲染报告，也不据此
把整个采集区域写入观察记忆。朝向由 `input` 的 `turn` 改变，`view` 不接受任意相机坐标；
准星即画面正中央。

`MODEL_PROTOCOL` 选择 Chat Completions / Responses / Anthropic（默认仍是 Chat）。
`view` 需要后两者的原生图片工具回执；不兼容的配置在连接服务器前报错。
`ContentPart::Image` 在框架内保留结构化图片，经 HTTP adapter 编码；诊断轨迹仅记
`[图片]`，不展开 Base64。图片随框架会话历史保留，当前上下文压缩仍是空实现。

时间窗不是队列：条目自带 `tick` 与单调 `seq`，「取某 seq 之后的条目」
是读方一行过滤。逐出用原版常量（聊天 100 行、声音 60 tick）。

## 4. `world/machine` 的模块

按事情拆开，各自显式声明依赖（生产码零 `use super::*`）：

| 模块 | 做什么 |
|---|---|
| `mod.rs` | `Module` 公开面、`ConnectionConfig` |
| `state.rs` | `Inner`：共享状态、时间窗、写口队列——**可脱离 azalea 单测** |
| `capture.rs` | ECS → `TickSnapshot` 直译 |
| `job.rs` | `JobSlot`：后台任务的形状，**每个任务恰好一条终局** |
| `input.rs` | 键鼠输入的时序：起手转向、下一 tick 按键、按满或提前结束后全部松开，回执经一次性通道送回 `Module::input` |
| `mining.rs` | 坐标式挖掘的判定表与轮询（已无模型面，见 §4a） |
| `connect.rs` | azalea 接入、客户端回调、停机 |
| `door.rs` | `DoorCommand` 与 tick 内执行 |
| `blocks.rs` | 成像拷贝的方块解码、渲染分类与原版光照属性表 |
| `light.rs` | 服务端光照包的自存（azalea 只解析不存） |

## 4a. 挖掘请求与结果

本节描述坐标式挖掘队列（`DoorCommand::Mine`），已无模型面，只剩探针在用。
模型挖掘是 `input` 按住左键：Azalea 的 `LeftClickMine` 每 tick 挖准星下的方块，
`machine::input` 记下上一 tick 准星下的方块，它变成空气（客户端所见，与玩家屏幕一致）
即算挖碎并提前松开全部按键。

- fork 的 `Client::start_mining` 在同一 ECS 写锁内直接写入 `MiningQueued`；MineIntent
  每 tick 再用同一读锁核对 `Mining`、`MiningQueued`、`MineBlockPos`、`MineProgress`
  与 `MineTicks` 的目标一致性，并查询目标格是否还有 `BlockStatePredictionHandler`
  的待确认预测。因此 queued→active 的调度窗口不会被误判成中断、反复重发清零
  进度；本地预测出的空气也不会在服务端确认前冒充成功。挖碎只由已收敛的目标变空气
  证明；目标读不到产生独立终局，不能默认成实心。预测从首次 pending 起超过协议确认
  边界会单独报告 `PredictionNotSettled`，不冒充成功、不可挖或进度停滞。queued 长时间
  未被消费单独报告 `DispatchNotObserved`；queued 在进入匹配 Active 前消失报告
  `RequestEnded`，不再用
  无进展窗口或循环补发掩盖调度/权限拒绝。只有曾进入匹配 Active 才允许一次补发；
  第二次仍未进入 Active 就终局。`Blocked` 只表示目标仍为实心且 Active 状态下的
  `MineProgress` 连续一个窗口没有严格增长，总工期本身不设上限。

## 4d. 任务生命周期

- 连接结束后不再有 tick，因此 client 断线、swarm 断线、连接线程退出与显式
  `Module::stop` 都在生命周期边界幂等收掉移动/挖掘槽，落独立 `ConnectionEnded`
  终局；它不冒充取消、超时或目标失败。

## 5. `agent` 是 git 依赖 midturn

模型内核不在本仓：它是独立仓库
[midturn](https://github.com/spojchil/midturn)（曾用名 agent/toolturn），
与 azalea 同款接法——根 `Cargo.toml` 的 `[workspace.dependencies]` 钉死 rev，
依赖键保持 `agent`（`agent = { package = "midturn", git = …, rev = … }`），
源码里仍写 `agent::`。

升级流程：上游更新 → 本地验收（上游测试 + 对抗读）→ 改根 Cargo.toml 的
rev → 修下游破口 → 推送。内核自己的测试在上游仓跑，不占本仓 CI。
（`crates/agent` 目录已删除，见 git 历史。）

## 6. 线程模型

- `world::machine` 独占一个线程（tokio current_thread + LocalSet，azalea 需要）。
  ECS 只在这个所有者线程的客户端回调与 Bevy schedule 内触碰；外部调用不跨线程直写。
- 挖掘槽及其副作用同样由该线程单写。`Module::stop` 只记录不可逆请求并唤醒
  owner；已领取的 tick/命令先完成，owner 销毁 LocalSet/runtime、
  收掉任务并报告完成后，外部才发布 `Stopped`。超时只返回合流失败，不虚称已经停止。
- 对外全部经共享状态：快照 latest-wins（外部持旧 `Arc` 用多久都行），
  写口走队列在 tick 内执行。

**已知上游隐患**：azalea 在 `AppExit` 清空 ECS 之后，残留事件处理可能在
持 ECS 写锁时重入读锁而自死锁。此时机器线程卡死，`Module::stop` 的合流
超时会如实报错且不会发布 `Stopped`，进程退出时由操作系统回收。

## 7. 验证口径

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
```

无整体放行的 lint。`[workspace.lints]` 的 `unsafe_code = "deny"` 与
`str_to_string = "warn"` 由 11 个 crate 全部接上。
