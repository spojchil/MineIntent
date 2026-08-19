# 合法寻路：裁决与设计

> 2026-08-19 裁定。问题一句话：**寻路当前在开挂**——它读服务端推过来的全部已加载
> 区块，包括同伴从没看过的地方。本文记下裁定、查证过的事实、以及为什么这么改。

## 1. 问题

`go_to` 走 azalea 的 pathfinder，它读的是 ECS 里的 `azalea_world::World`——服务端推
过来的每一个区块。**loaded ≠ observed**。于是同伴会：

- 穿墙规划（墙后的地形它「知道」）
- 规划地下的路（从没下去过）
- 绕开身后从没回头看过的障碍

而 `BlockMemory` 那一整套可见性判据（视锥、`ExposedFace`、遮挡）存在的理由，正是
**不让模型知道它不该知道的**。结果寻路从后门把整个世界拿走了。

这与合法信息边界那条线（PR #42/#62）直接冲突。

## 2. 学术与前人

「在未知地形里导航到目标」的标准解法是 **D\* Lite / LPA\***（Koenig & Likhachev），
三件套：

1. **freespace assumption**——未知格假设可通行，按此求最短路；
2. 走，观察到与假设矛盾时把新信息加进地图；
3. **增量修复**局部路径，不从头重算（解质量等同从头 A\*）。

论文自己写明边界：D\* Lite **不推理不确定性，对未知一律乐观**。

Minecraft 侧的前人是 **Baritone**：区块压成 2 bit（AIR/SOLID/WATER/AVOID）常驻内存；
目标在未加载区块时 `GoalBlock` 降级成 `GoalXZ`；计算线程发现最优节点落在已加载边缘
超过 N 次就早停。

⚠ Baritone 还用**世界种子预测地形**。对我们这是**越界**——同伴拿不到种子。不采用。

## 3. 查证：azalea 已经有什么（2026-08-19，fork `d0cc847`）

| 要什么 | azalea 现状 |
| --- | --- |
| 可替换的世界抽象 | **没有 trait**。`CachedWorld::new(world_lock, origin)` 直接吃 `Arc<RwLock<World>>` |
| 有没有缝 | **有**。`calculate_path(ctx)` 是 `pub`，`ctx.world_lock` 是 pub 字段 |
| 能否造合成世界 | **能**。`ChunkStorage::default()` + `PartialWorld.chunks.set_block_state` + `.into()`（azalea 自己的测试就这么造） |
| 未加载区块 | `with_section(..).unwrap_or_default()` → 空气。**已经是 freespace 乐观** |
| 「够得着」的 goal | **`ReachBlockPosGoal` 有**（默认 4.5 格），`success()` 里做 **raycast**（`pick_block`） |
| 增量修复 | **有**。`check_for_path_obstruction` + `patch_path`（局部重算约 20 节点） |

**三件苦活 azalea 全做了**（乐观假设、增量修复、reach goal）。缺的只有一样：让它看
我们的地图，而不是服务端的全量世界。

## 4. 裁定

### 裁定一：做合法寻路

代价明说：合法寻路**必然更笨**——会走进死胡同、会掉头、会「我以为那边通」。那是玩家
行为，不是 bug。

### 裁定二：改 fork

`github.com/spojchil/azalea` 本来就是为改而 fork 的。不绕路、不在上层糊补丁。

### 裁定三：未知格的两个问题，假设方向相反

azalea 对每格问两个不同的问题，此前一律按「空气」回答。**分开：**

| | 未知格 | 理由 |
| --- | --- | --- |
| `is_block_passable` | **乐观 = 是** | 撞上了再修，`patch_path` 自带 |
| `is_block_standable` | **悲观 = 否** | 只踩见过的方块。凭空假设地板 = 规划出走进空中的路，真走进去是摔下去 |

**这是本文最要紧的一条。**「未知视为空气」对通行成立，对站立不成立——人得站在方块上。

**这样还走得动**，因为地面基本都是已观察的：视锥往前看，看到的恰是地面的**顶面**。
未观察的主要是三类：

- **脚下这一格**——机器层从 `on_ground` 就知道有支撑。这是本体感觉不是视觉，不进模型
  可见面（`standingOnBlock` 当年被删说的是「别给模型白拿」，寻路不是给模型看的）；
- **已观察表面之下**——不需要站上去；
- **遮挡后面**——确实走不过去。**这正是「合法」该有的样子。**

副作用：**刚进服、还没扫过一帧时哪儿都站不了**。帧 250ms 一次，所以是「先看一眼再动」。

### 裁定四：方块记忆一个字不动

原本的担心是「没条目」既是**没见过**又是**见过是空气**，分不开。**站立取悲观之后不需要
分**——两种「没条目」的处置完全相同：

```
有条目 → 用真实状态，照 azalea 原有判定（solid / passable / standable）
无条目 → passable = true，standable = false
```

歧义消失，增量那条回路一个字不改。

### 裁定五：不造影子世界，改取块口

原方案是造一个只含已观察方块的合成 `World` 喂给 pathfinder。**放弃**：

- 合成 World 要把几万格拷成 chunk section，**记忆每变一次就得重建**；
- 而 `BlockMemory` 本身是 `HashMap<[i32;3], BlockFact>`，**直接查就行**。

fork 里的改动：给 `CachedWorld` 的取块口加一层可插拔来源，传一个 `BlockMemory` 支撑的
实现。三个入口（`is_block_passable` / `is_block_standable` / `get_block_state`）都经
`get_block_state_at_pos`，**改一处即可**。

⚠ 代价未测：每次查块过一次 `HashMap`，比 section 直取慢。`CachedWorld` 自带两级缓存
（bounded/unbounded chunk cache + `cached_blocks`）应能吸收，但**这是开工后的第一个数**。

### 裁定六：走近用 `ReachBlockPosGoal`

它的 `success()` 做 raycast，要求从落脚点**真的看得见**那块——比我们的 `check_reach`
（纯距离，对齐服务端）严。

顺带效果：**堵住「隔墙挖」**。今天查证服务端 `handleBlockBreakAction` 只有四道守卫
（`isWithinBlockInteractionRange` 纯距离 / `too high` / spawn protection / `mayInteract`），
**一次射线都没有**——原版玩家挖不到被挡的方块，是客户端射线选中拦的，不是服务端。
azalea 的 `start_mining` 绕过射线直接对坐标发包，所以同伴能隔墙挖。

⚠ 这道堵是**隐式**的（换个 goal 就漏）。要不要在动作层显式补一道，**未裁**。

## 5. 放弃的东西

**Baritone 式「早停在已知边界」拿不到**——站立悲观之后分不出未知和已知空气，边界不可
观测。替代品更简单也更好：**A\* 直接找不到路** = 「按我知道的，走不过去」。这本身就是
合法且可行动的终局。

## 6. 顺带的一个后果

合法寻路一旦落地，**它自己会驱动探索**：想去没见过的地方，就得先去看。这和 2026-08-18
长跑「424 个动作里 196 个是 scan/look」是同一个洞的两面——现在是模型手动补感知，之后
是寻路主动要求感知。
