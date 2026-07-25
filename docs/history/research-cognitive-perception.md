---
status: reference
authority: informative
implementation: not-applicable
last_verified: 2026-07-12
---

# Minecraft 认知感知源码调研

> 第一轮源码调研，对应 Issue [#24](https://github.com/spojchil/mineintent/issues/24)。本文记录当时锁定版本的事实和启发，不替代正式设计；复用结论前应重新核对第三方版本。

## 1. 调研问题

本轮不研究“怎样把附近数据全部交给模型”，而研究：

1. Mineflayer 实际持有什么世界与实体数据？
2. Prismarine 的 raycast、方块形状和实体尺寸能复用到什么程度？
3. 哪些高层插件会使用超出角色视觉的全量数据？
4. Sodium 的渲染可见性有哪些可借鉴的分层方法？
5. 怎样证明 Control View 没有泄漏到 Cognitive Observation？

## 2. 仓库与版本

仓库浅克隆于 `research/repos/perception/`，不进入 MineIntent Git 历史。

| 仓库 | HEAD | 本轮用途 |
|---|---|---|
| PrismarineJS/mineflayer | `7368ac8e9cc8` | 方块、实体、协议事件与 cursor raycast |
| PrismarineJS/prismarine-world | `e1296ec37029` | 世界缓存、DDA raycast 和区块生命周期 |
| PrismarineJS/prismarine-chunk | `7d39d3187a1d` | 已加载区块的存储边界 |
| PrismarineJS/prismarine-block | `c52fba46b794` | 方块状态、碰撞形状、透明属性 |
| PrismarineJS/prismarine-entity | `4f2678f704d3` | 实体位置、速度、朝向、尺寸和装备 |
| PrismarineJS/mineflayer-pathfinder | `d1f4d7fdbebc` | Control View 的空间使用方式 |
| PrismarineJS/mineflayer-collectblock | `2e3a79cc1ba6` | 全量方块搜索到动作的链路 |
| PrismarineJS/prismarine-viewer | `bead85c57a23` | 完整世界调试视图与认知视图的对照 |
| CaffeineMC/sodium | `2cfb93d33279` | 视锥、分区、遮挡图与粗到细裁剪 |

## 3. Mineflayer 世界数据

### 3.1 `findBlocks` 是缓存查询，不是视觉

`mineflayer/lib/plugins/blocks.js` 的 `findBlocks`：

- 从角色所在 section 开始用八面体迭代器遍历已加载 section。
- 利用 palette 做粗筛，然后遍历 section 内方块。
- 按空间距离排序。
- 不检查视野方向或遮挡。

因此它能快速找到墙后、地下或背后的目标。它适合寻路、技能候选生成和测试布置，不适合作为认知观察。`findBlock` 只是取其第一项，也不具备“看见”的含义。

`blockAt` 在区块未加载时返回 `null`；区块已加载只代表协议缓存可用，不代表角色观察过该位置。

### 3.2 区块事件只描述缓存生命周期

Mineflayer 转发 Prismarine World 的：

- `chunkColumnLoad`
- `chunkColumnUnload`
- `blockUpdate`

`chunkColumnLoad` 不能生成“发现一片地形”的认知事件。否则加入服务器或移动时会瞬间知道整个加载范围。`blockUpdate` 也只证明客户端收到变化；只有变化位置通过视觉、听觉或直接交互证明时，才能形成相应认知观察。

### 3.3 实体表是协议跟踪表

`mineflayer/lib/plugins/entities.js` 根据协议包维护 `bot.entities`，并发布：

- `entitySpawn`
- `entityMoved`
- `entityUpdate`
- `entityGone`
- `entityHurt`
- 装备、姿态、睡眠、手臂动作等派生事件

实体表含位置、速度、yaw/pitch、width/height、装备和 metadata。它足以支持一个认知跟踪器，但这些事件本身仍不是视觉事件。

尤其是 `entityGone`：源码在 destroy packet 后将实体标为无效并删除。原因可能是死亡、despawn、超出追踪范围、区块卸载或生命周期切换，不能把它直接翻译为“同伴看见实体离开”。

### 3.4 Mineflayer cursor raycast 的能力与局限

`mineflayer/lib/plugins/ray_trace.js`：

- 从实体眼睛位置和 yaw/pitch 得到视线方向。
- `blockAtCursor` 使用 world raycast。
- `entityAtCursor` 先找最近方块交点，再以实体 width/height 构造 AABB，选择方块前最近实体。

这适合“准星正对什么”的精确交互，不等于完整视野：

- 只有一条中心射线。
- 不覆盖周边视野。
- 实体检测使用简单竖直 AABB。
- 没有注意、时间连续性和观察置信度。
- 默认最大距离偏向 API 能力，不是合理认知距离。

`canSeeBlock` 同样只从眼睛射向方块中心。中心被挡而边缘可见时会产生假阴性；细小可见面也可能被误判。

## 4. Prismarine raycast 与形状

### 4.1 可复用的几何基础

`prismarine-world/src/iterators.js` 的 `RaycastIterator` 使用三维 DDA 穿越体素，并支持射线与一组 AABB shape 相交。`WorldSync.raycast` 逐体素读取方块并返回第一个匹配交点。

可复用内容：

- 从眼睛到目标采样点的体素遍历。
- 与方块/实体 AABB 求交。
- 交点距离与进入面。
- 未加载区块返回未知，而不是自动透明。

### 4.2 碰撞形状不等于视觉遮挡形状

`prismarine-block` 从 registry 加载 `blockCollisionShapes`，并提供 `shapes`、`boundingBox` 和 `transparent`。这些数据主要服务碰撞、挖掘和交互：

- 玻璃具有完整碰撞体，但视觉上透明。
- 树叶、流水、栅栏和门的视觉透过程度不同。
- 一些方块模型会超出单格体积。
- `transparent` 是有用提示，但不足以表达光学衰减和部分遮挡。

因此 MineIntent 可以复用几何相交，但必须维护版本化 `VisionMaterialPolicy`，把方块分为 opaque、partial 和 transparent，并为 partial 累积遮挡代价。不能直接把 `block.shapes` 命中等同于完全不可见。

## 5. 高层插件揭示的边界

### 5.1 Pathfinder

Pathfinder 读取附近方块、碰撞、可破坏性和动态 `blockUpdate` 来规划整条路径。它是典型 Control View：角色可以沿规划行动，但同伴语言不能据此宣称看见路径背后的房间、矿物或实体。

### 5.2 CollectBlock

CollectBlock 示例直接使用 `findBlocks` 找资源，再交给 Pathfinder、工具选择与挖掘逻辑。这条链路适合动作候选，但如果模型收到原始命中坐标，就形成透视。

正确边界是：

```text
全量缓存生成行动候选
→ 技能内部规划
→ 只有通过认知可见性或动作真实发现的内容进入同伴观察
```

技能可以因为内部查询决定“去某处尝试”，但在真正观察前不得产生“我发现了钻石”之类语言或记忆。

### 5.3 Mineflayer 声音事件

`mineflayer/lib/plugins/sound.js` 将两类协议包规范化：

- `named_sound_effect`：提供资源名、以 1/8 block 编码的位置、volume 和 pitch。
- `sound_effect`：提供 registry sound 或 hardcoded sound ID、category、位置、volume 和 pitch。

Mineflayer 对外发布 `soundEffectHeard` 和 `hardcodedSoundEffectHeard`。这意味着声音不是通过实体或方块变化猜测，而是第三种独立的外界感知输入，并且具有可用的声源位置。

同时需要注意：

- 包到达只证明服务端向客户端发送了声音，仍要做世界/维度、距离和异常值校验。
- sound name 可以支持“像僵尸声”“挖掘声”的类别判断，但不能自动绑定某个实体或具体方块。
- 协议位置可以用于计算相对方向和距离，不应默认把精确坐标交给模型。
- volume/pitch 是渲染参数，不直接等于现实声压或来源身份。
- named sound 和兼容 hardcoded event 可能表示同一次声音，必须按短时间、位置和参数去重。

因此 Backend 应保留一个规范化 raw sound event，Perception 层再产生有类别、方向、距离带和置信度的 `SoundObservation`。

## 6. Prismarine Viewer

`prismarine-viewer/viewer/lib/worldView.js` 会：

- 按 view distance 加载并发送完整 chunk JSON。
- 将 `bot.entities` 中全部实体发送给 viewer。
- 转发所有实体移动和方块更新。

它忠实展示 Protocol State，适合作为调试基准。它不做角色级视锥和遮挡过滤，不能作为 Cognitive Observation 实现。

最有价值的用法是双层调试：

```text
左侧：Protocol / Control View（仅开发者）
右侧：Cognitive Observation 覆盖层
```

这样可以直观看出墙后实体、缓存矿物或过期目标是否错误进入认知。

## 7. Sodium

### 7.1 值得借鉴的分层

Sodium 的渲染可见性不是对所有方块逐个射线，而是：

1. `Viewport` / `SimpleFrustum` 用相机和包围盒做视锥粗筛。
2. 以 16×16×16 render section 组织场景。
3. `VisibilityEncoding` 表达一个 section 六个方向之间是否连通可见。
4. `OcclusionCuller` 从相机 section 向外遍历可见连接，并结合距离、角度和 frustum。
5. `RayOcclusionSectionTree` 对较远 section 做有限步进的额外遮挡验证。

这支持 MineIntent 采用“候选生成 → 视锥粗筛 → 精细射线 → 时间聚合”，而不是全世界逐方块检查。

### 7.2 不能直接移植

Sodium 依赖 Minecraft 客户端渲染模型、occlusion shape、mesh、VisibilitySet 和相机矩阵。Mineflayer 没有这些渲染产物。渲染目标也与认知不同：

- 渲染宁可保守多画，认知宁可保守少说。
- section 可渲染不代表 section 内每个方块都被角色注意。
- 透明排序、面剔除和动画优化不是认知判断。
- Sodium 的第三人称或自由相机不应改变同伴的第一人称身体认知。

因此只借鉴层次、数据局部性与粗到细策略，不复制 Java/OpenGL 实现。

## 8. 结论

### 8.1 可直接复用

- Mineflayer 的实体和方块协议事件作为原始信号。
- Prismarine 的 DDA、Vec3 和 AABB 相交。
- Prismarine Block 的状态、碰撞 shape 与透明提示。
- Viewer 作为 Protocol State 调试视图。

### 8.2 必须新增

- 虚拟第一人称 FOV 与注意区域。
- 光学材质和部分遮挡策略。
- 实体多点可见性与时间连续状态机。
- 方块候选生成、可见表面采样和资源聚类。
- 声音规范化、距离判断、遮挡衰减、重复聚合和来源关联边界。
- 玩家行为证据窗口与不确定活动假设。
- 当前观察、最近观察和记忆的显式来源区分。
- 有界采样、显著性、去重和上下文压缩。
- 防止 Control View 泄漏的自动测试。

### 8.3 最重要的实现约束

`bot.entities`、`bot.world`、`bot.findBlocks` 和 Pathfinder 路径不得离开 `minecraft/`、`actions/`、`skills/`、`perception/` 的受控实现边界。Companion Runtime、Context Composer 和模型层只能接收版本化 Cognitive Observation 或明确标为 Memory 的内容。
