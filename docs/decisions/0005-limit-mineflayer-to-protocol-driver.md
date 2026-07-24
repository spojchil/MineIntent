---
status: proposed
authority: informative
implementation: partial
last_verified: 2026-07-23
---

# 0005：限制 Mineflayer 为协议驱动而非认知与行为权威

> 提出日期：2026-07-13；讨论：[#32](https://github.com/spojchil/mineintent/issues/32)；补充 [ADR 0001](./0001-use-mineflayer-as-initial-backend.md)。最新实验已经实现其中部分 source-port、Motor 和依赖边界，但 Issue 仍 open，本 ADR 仍未接受。

## 背景

v0.1 真人联调证明 Mineflayer 适合承担登录、协议、区块、实体、背包和基础物理，但其高层自动化抽象与“接近真人的具身同伴”存在系统性冲突：

- `bot.players` 表示协议实体被追踪，不表示同伴当前看见玩家；
- `bot.findBlock` 可检索当前视野之外的完整已加载区块；
- `mineflayer-pathfinder` 直接使用 `bot.world`，能利用未探索迷宫和遮挡后的精确地形；
- `bot.dig` 会乐观地把本地方块改为空气，Promise 完成不等于服务端接受破坏；
- 高层方法允许先指定世界坐标，再补做朝向和动画，而不是从当前身体视线取得交互目标。

这些行为对传统自动化 Bot 合理，却会让 MineIntent 的语言、记忆与身体表现失去共同事实基础。仅靠提示词或事后过滤不能纠正已经利用全知数据完成的动作。

## 决定

继续使用 Mineflayer，但把它限制为可替换的协议驱动。Mineflayer 不再是认知事实、意图目标、动作成功或普通路线可知性的权威。

系统采用以下边界：

```text
Mineflayer Protocol Driver
├── Protocol State          服务端实际发送的原始客户端状态
├── Safety Control View     下一输入的碰撞、跌落与协议合法性
├── Perception Boundary     FOV、光学遮挡、声音、交互反馈
│        ↓
├── Epistemic Map           当前看见、亲自探索和仍然记得的空间
│        ↓
├── Intentional Planner     只能利用 Epistemic Map 选择普通路线
└── Motor Controller        视角/方向键/左右键/GUI 输入与反馈
```

### 协议驱动允许承担

- 登录、认证、加密、版本协商和重连；
- 区块、方块、实体、声音、聊天、背包和自身状态的协议解码；
- 客户端物理、移动包和必要的协议时序；
- 产生带来源的原始事件，供感知、动作反馈与测试使用。

### 高层自动化限制

- `findBlock` 结果只能在协议驱动内部生成待验证候选，不能直接成为认知发现、玩家指代或动作目标；
- `bot.players` 和实体表只能生成候选，不能证明当前可见；
- `bot.world` 只能对“下一次即将发出的局部输入”做碰撞、跌落和协议合法性校验；不能批量探测远处、参与 A* 展开或路线排序，也不能把拒绝原因写入 Epistemic Map。Safety 不能依据未感知 raw entity 选择躲避方向；
- 挖掘目标必须追溯到合法 Information Read/Grounding，并在 controller 执行时重新验证当前可交互性；controller 可以内部调整视角并复用 `bot.dig`，不能从 loaded world 自行换目标；
- Mineflayer 的本地乐观更新只表示预测，不能生成成功领域事件；
- 普通寻路只能使用当前感知或历史探索形成的 Epistemic Map；未知区域必须通过探索获得；
- 业务层不得直接导入 Mineflayer Bot、World、Entity、Block 或 Pathfinder 类型。

### 模块依赖矩阵

| 模块 | 允许输入 | 禁止事项 |
|---|---|---|
| `src/minecraft/driver/` | Mineflayer/Prismarine 原始类型、协议状态 | 输出未经归一化的 Bot/World/Entity/Block 给上层 |
| `src/perception/` | `ProtocolObservationSource`、只读候选 DTO | 导入 Mineflayer/Prismarine 原始类型；把完整区块当作观察 |
| `src/grounding/` | 消息话语角色、Cognitive Observation、活动目标和有来源记忆 | 用固定关键词表冒充语言理解；用 raw entity/world 补全位置；输出无证据句柄 |
| `src/information/` | driver DTO、UI context、显示规则、字段注册表 | 输出界面没有展示的精度；绕过 Help/Read；把 raw 对象交给上层 |
| `src/behavior/` | grounded embodied intent、合法 Information Read、对象可供性、Epistemic Map、身体状态 | 接收 raw protocol；用字符串匹配命令；把 controller/协议目录暴露给主模型 |
| `src/motor/` | 有作用域的 controller request、grounded handle 的当前局部空间解、取消和反馈 DTO | 搜索新目标；读取完整世界替普通规划；把精确实现数据回流为认知事实 |
| `src/navigation/` | Epistemic Map；仅单步调用 `SafetyProbe` | 读取 `bot.world`；批量/远程安全探测；用探测结果扩展路线 |
| `src/actions/` | Behavior 生成的内部计划引用与 controller 资源契约 | 导入原始客户端类型；作为主模型命令目录；通过 `findBlocks` 或 Pathfinder 绕过信息边界 |
| `src/skills/` | 无合法职责；删除原型目录 | 注册或保留对象专用 model-facing skill |
| `src/companion/`、`src/context/`、`src/models/`、`src/memory/`、`src/speech/` | Cognitive Observation、已验证领域事件和有来源记忆 | 接收 Protocol/Control 原始状态 |

Mineflayer 适配实现集中在 `src/minecraft/driver/`。依赖方向通过 ESLint import restriction 和架构测试强制；项目不为当前原型保留旧适配器或第二个合法入口。

v0.2 先固定[合法信息接口、Help 发现与 UI 会话](../architecture/information-access-and-ui.md)。模型面对的是可发现的信息接口、当前合法读取、身体可供性、限制与证据要求，不是 raw Mineflayer 状态或协议事务枚举。

控制层在 v0.3 重新设计。`lookAt`、`dig`、Pathfinder 等高层 API 可以在目标已合法选择、用途受限、可取消且结果独立验证的 controller 内被复用；不能因为 API 参数含坐标就一律禁止，也不能因为它方便就允许搜索隐藏目标。`look_at_player`、`follow_player`、`remove_object` 等名称是否包含过多目标选择或规划，需要按信息来源和职责逐项审查，而不是机械套用“只能键鼠输入”的规则。

### Safety Control 非泄漏契约

`SafetyProbe` 只接受 Motor Controller 已经选定、即将执行的一个局部动作，并只返回执行所需的最小 `allow / deny / risk` 结果。它不得：

- 接受候选路线集合、远端坐标或可被调用方用于扫描地图的批量查询；
- 被 Intentional Planner 用于 A* 节点展开、启发式估值、路线排名或可达性预判；
- 返回隐藏方块类型、精确几何、替代通路或拒绝动作之外的世界信息；
- 因拒绝某一步而给 Epistemic Map、语言、活动判断或记忆新增“那里有墙/悬崖”等知识。

安全拒绝只会终止或要求重新观察当前动作。角色若要理解原因，必须通过正常感知取得证据。

### 结果权威

动作结果分为四层：

1. `commanded`：Motor Controller 已发出协议动作；
2. `client_predicted`：客户端库产生本地预测；
3. `server_observed`：收到服务端来源的状态变化；
4. `outcome_verified`：由正式客户端可观察证据证明动作特定后置条件成立，例如服务端来源方块状态、掉落物状态与背包增量在受控关联窗口内一致。

语言、共同活动和记忆只能把 `server_observed` 或 `outcome_verified` 表达为已发生事实。正式产品的 `outcome_verified` 不依赖 Paper、OP、RCON 或服务器文件。开发和 CI 可以使用 Paper 测试裁判独立核对生产判定，但裁判证据不进入同伴认知，也不参与生产结果计算。

测试裁判不能仅靠时间戳猜测关联。测试在动作开始前用 `scenario_run_id + action_id + player_uuid + target` 注册预期观察窗口，Paper artifact 回传相同关联字段；坐标、物品与有限时间窗只用于交叉校验和诊断。

## 理由

- 保留 Mineflayer 可避免重写完整 Minecraft Java 客户端。
- 收紧边界能继续复用已经验证的生命周期、DTO、聊天和背包基础。
- 感知、认知地图和 Motor Controller 是产品语义，应由 MineIntent 掌握。
- 底层安全与角色知识分离后，既能避免明显危险，也不会用原始区块直接破解迷宫。

## 后果

### 正面

- 语言、动作和记忆可以共享同一套带证据事实。
- 更换 Mineflayer、局部自定义插件或未来 Fabric Backend 不影响上层协议。
- 未探索环境会产生合理探索行为，曾探索环境才允许形成路线记忆。
- 客户端预测、服务端状态与最终业务结果不再混为一个事件。

### 代价

- 不能直接依赖 Mineflayer 高层方法快速增加行为能力。
- 需要实现虚拟第一人称感知、Epistemic Map、Motor Controller 和受限规划。
- 某些 Mineflayer 插件会污染本地状态，可能需要替换插件或维护小型适配补丁。
- 人类式未知性会降低传统任务 Bot 的最短路径成功率，这是有意的产品取舍。

## 备选方案

### 继续直接包装高层 Mineflayer API

拒绝。它能隐藏类型依赖，却不能阻止 pathfinder 和 dig 在包装内部使用全知或乐观状态。

### 基于 `minecraft-protocol` 全量重写客户端

暂不采用。控制最完整，但登录、区块、物理、背包和版本兼容成本过高。若 Mineflayer 内部状态持续无法隔离，再重新评估。

### Fabric 完整客户端 Mod

保留为未来 Backend。它更接近原版客户端交互，但无头部署、资源占用、版本和 Mod 兼容复杂度更高。

### Fork Mineflayer

只在无法通过自定义 Motor/Perception 插件隔离乐观状态时采用最小分支，不把业务策略合入分支。

## 验证与复审条件

- 玩家在身后时，同伴不能把 `tracked` 表达为“看见”；转头后必须有视线证据。
- 新迷宫不能利用未探索区块直接规划出口；走过一次后可以使用记忆路线。
- 遮挡后的方块不能仅因已加载或距离足够而成为挖掘目标。
- `dig` Promise 完成、服务端方块变化和背包拾取分别记录。
- 如果上述边界仍需大量侵入 Mineflayer 内部私有 API，重新比较最小 fork、`minecraft-protocol` 和 Fabric Backend。
