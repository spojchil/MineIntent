---
status: historical
authority: informative
implementation: retired
last_verified: 2026-07-23
source_commit: 2ee231c
---

# MineIntent 早期自主 Agent 交接文档

> **历史警告：** 以下正文原本用于 2026-07-12 的新会话交接，并自称“当前决策依据”。产品随后从“自主完成长期目标的 Agent”转向“共同生活的长期同伴”；本文现在只保存早期问题意识和工程原则。当前入口见[文档首页](../README.md)，演进说明见[项目演进史](../history/project-evolution.md)。

## 1. 项目愿景

MineIntent 是一个面向 Minecraft Java Edition 原版生存模式的自主 AI 玩家。

用户只给出一次长期目标，例如：

```text
获得鞘翅
```

此后 AI 自己观察环境、形成方案、执行操作、检查结果、处理失败并继续规划。系统不预设完成目标的固定路线。AI 可以击败末影龙，也可以搭桥、制造飞行机器或采用其他符合原版机制的方案。

项目的核心原则是：

> 用户规定意图和成功条件，AI 自己选择路线；程序提供可靠、通用、可组合的玩家能力。

## 2. 当前范围

当前明确限定为：

- Minecraft Java Edition。
- 目标版本暂定为 `1.21.1`。
- 只考虑原版游戏，不考虑模组内容兼容。
- 使用无界面的协议客户端，而非 Minecraft MOD 或完整图形客户端。
- 开发及评测环境允许关闭正版验证，第二个 Microsoft/Minecraft 账号不是当前阻塞项。
- 不使用截图作为主要感知来源。
- 不用大模型逐 tick、逐按键遥控玩家。
- 长期验收目标是从生存环境中自主获得鞘翅。

“只发送一次提示词”指用户只下达一次长期目标，不代表整个任务只能调用一次大模型。系统内部可以在检查点、失败和重大事件发生时反复调用模型。

## 3. 已否决或暂时放弃的路线

### 3.1 Fabric MOD 内的假玩家

曾考虑通过 Fabric 创建 `ServerPlayer` 假玩家。研究了 Fabric Carpet 1.4.147 的实现，包括：

- `EntityPlayerMPFake`
- `FakeClientConnection`
- `NetHandlerPlayServerFake`
- `EntityPlayerActionPack`

Carpet 通过 `ServerPlayer` 子类、假的 Netty 连接、假的服务端包监听器和 Mixin 补齐假玩家生命周期。这种方案部署简单，但并非真实客户端连接，可能出现网络状态释放、模组握手、重生、换维度和兼容性问题。

由于项目现已限定为原版，假玩家 MOD 的优势不足，暂不采用。

### 3.2 完整客户端加截图、鼠标和键盘模拟

这是最接近真人的方案，但存在：

- 图像模型响应慢、token 成本高。
- 鼠标、键盘和窗口焦点容易失控。
- 无法满足 Minecraft 20 tick/s 的实时控制需求。
- 完整客户端占用大量内存和显卡资源。

因此不作为当前主体方案。

### 3.3 面向任意模组整合包的协议 Bot

研究 Applied Energistics 2 后确认，复杂模组拥有专用客户端状态和自定义网络协议。例如 AE2 终端不是普通容器槽位，而是通过服务端同步条目序列号，再由客户端发送 `MEInteractionPacket` 操作虚拟物品。

纯协议客户端若要兼容任意内容模组，必须逐个实现模组客户端协议，成本会迅速失控。因此当前放弃 MOD 兼容，只做原版。

## 4. 技术框架决定

主体选择：

```text
TypeScript / Node.js
+ Mineflayer
+ node-minecraft-protocol
+ 自己实现的 AI、技能、记忆和执行框架
```

理由：

- Mineflayer 支持 Minecraft 1.21.1。
- 提供协议连接、世界缓存、实体、背包、容器和基础玩家操作。
- `node-minecraft-protocol` 负责认证、加密、压缩、登录、保活和数据包。
- 社区已有 Pathfinder、Tool、CollectBlock、PVP、AutoEat 等插件。
- 相比 Java MCProtocolLib，不需要从零实现完整玩家框架。

初期可评估的依赖：

- `mineflayer`
- `mineflayer-pathfinder`
- `mineflayer-tool`
- `mineflayer-collectblock`
- `mineflayer-pvp`
- `mineflayer-auto-eat`
- `minecraft-data`

第三方插件只能作为底层能力，不应直接决定 AI 的任务路线。关键能力需要我们自己的包装、超时、取消、验证和测试。

## 5. 总体架构

```text
用户长期目标
      ↓
AI Planner
├── 选择短期意图
├── 编写短流程
├── 根据结果重新规划
└── 维护长期目标
      ↓
MineIntent Flow
├── 类型检查
├── 参数验证
├── 时间和资源预算
├── 取消及失败策略
└── 编译为内部 AST
      ↓
Skill Runtime
├── navigate
├── explore
├── collect
├── craft
├── smelt
├── transfer_items
├── fight
├── flee
└── build_blueprint
      ↓
Action / Reflex Layer
├── look
├── move
├── jump
├── sneak
├── attack
├── use
├── dig
├── place
├── equip
├── click_slot
└── 本地危险反射
      ↓
Mineflayer / Minecraft 协议
```

配套模块：

- `perception`：把世界状态压缩为模型可理解的观察。
- `memory`：保存地图、地点、资源、计划、失败和关键事实。
- `knowledge`：按需查询原版配方和游戏机制。
- `evaluation`：由程序判断目标是否完成。
- `telemetry`：记录决策、技能事件、状态变化和失败。

### 5.1 解耦原则与可能的运行形态

核心设计应保持适度解耦，以便未来可能出现不同的控制入口、Minecraft 后端和部署形态：

```text
控制入口
├── CLI
├── MCP Server
├── HTTP / WebSocket
└── 可选 Minecraft MOD 界面
          ↓
MineIntent Core
├── 目标与 Agent
├── Flow AST 与执行器
├── Skills
├── Perception / Memory
└── Evaluation / Telemetry
          ↓
Minecraft Backend
├── Mineflayer 协议客户端（第一版）
└── 完整客户端 / Fabric MOD 控制后端（未来可能）
```

需要保持稳定的边界包括：

- `MinecraftBackend`：连接、状态快照、事件和原子动作。
- `Skill`：可取消、带超时、返回结构化结果的通用能力。
- `FlowRuntime`：与具体模型、CLI 或 MCP 无关。
- `ModelProvider`：隔离不同大模型服务。
- `Observer/TelemetrySink`：调试界面、日志和录像可以独立订阅。

CLI、MCP 和 MOD 只是不同适配器：

- CLI 适合第一版启动、配置和输入长期目标。
- MCP 可以在未来把观察、动作、技能和 Flow 暴露给外部 Agent。
- MOD 可以在未来承担真实客户端控制、游戏内调试界面或视觉后端，但不应成为核心逻辑的宿主。

解耦不等于一开始建立复杂的插件系统。第一版只实现：

```text
Mineflayer Backend + CLI + 单一模型适配器
```

只在代码中保留清晰模块边界和少量必要接口，不为尚未验证的 MCP、MOD 或多后端实现编写大量抽象层。当第二个真实实现出现时，再提取共同接口，避免过早设计。

### 5.2 原版目标与优化 MOD

MineIntent 支持的是原版内容、协议和游戏机制。玩家或测试服务器可能安装不改变玩法语义的优化 MOD，例如客户端渲染优化或服务端性能优化；这些通常不会改变 Bot 的目标和高级接口，但可能改变时序、区块加载速度或边缘行为。

第一阶段必须在纯原版 1.21.1 环境中开发和建立基线，不把任何优化 MOD 作为依赖，也不承诺一开始兼容所有优化组合。核心测试通过后，可选择常见优化环境做兼容测试。纯客户端渲染优化只影响人类观察客户端，不会给协议 Bot 增加真实图像能力。

## 6. 三种时间尺度

大模型太慢，不能一次只发一个操作。系统分三层运行：

### 6.1 Tick 级本地控制

以游戏速度执行，不调用大模型：

- 移动和视角控制
- 碰撞与避障
- 挖掘时序
- 攻击冷却
- 拾取掉落物
- 防止跌落
- 低血量紧急撤退
- 服务端位置纠正

### 6.2 数秒到数分钟的短流程

AI 一次提交一段可验证、可取消的流程，由本地连续执行。

示意语法：

```text
target = find_block(tag="minecraft:logs", visible=true)
|> nearest()

navigate(to=target, range=3)
|> look_at(target)
|> equip_best_tool(target)
|> dig(target)
|> collect_drops(timeout=5s)
|> assert_inventory(tag="minecraft:logs", at_least=1)
```

### 6.3 事件驱动的 AI 规划

仅在以下时机重新调用模型：

- 流程完成。
- 前置条件不满足。
- 技能失败或超时。
- 出现重大危险。
- 世界发生重要变化。
- 当前方案需要调整。

## 7. MineIntent Flow

管道语言是核心架构，而不是附加功能。

目标：让 AI 编写短期行动程序，而非逐按键遥控或一次规划整个长期任务。

语言需要支持：

- 顺序管道。
- 变量和步骤结果引用。
- 有上限的 `repeat` / `until`。
- `if` 条件。
- `timeout`。
- `on_fail`。
- 前置条件和后置断言。
- 流程取消。
- 执行预算。

不要执行模型生成的任意 JavaScript。模型可使用易读的文本 DSL，但执行前必须编译成类型化 JSON/AST，例如：

```json
{
  "type": "pipeline",
  "timeoutMs": 30000,
  "steps": [
    {
      "id": "target",
      "skill": "find_block",
      "args": {
        "tag": "minecraft:logs",
        "visible": true
      }
    },
    {
      "skill": "navigate",
      "args": {
        "target": { "ref": "target" },
        "range": 3
      }
    },
    {
      "skill": "dig",
      "args": {
        "target": { "ref": "target" }
      }
    }
  ]
}
```

所有循环必须有上限，所有技能必须能取消，所有流程必须有时间或资源预算。

## 8. 目标自由与能力边界

系统只固定：

- 用户目标。
- 世界规则。
- 可执行动作。
- 安全和资源限制。
- 成功判定。

系统不固定获得鞘翅的任务树，也不应内置：

```text
beat_dragon
reach_end_islands
get_elytra
```

适合提供的通用技能包括：

```text
navigate
explore
collect
craft
smelt
fight
flee
build_blueprint
```

AI 可以把这些能力组合成传统路线、搭桥路线、飞行机器路线或其他方案。

原版机制知识应通过查询接口提供，例如配方、方块硬度、生物掉落和维度规则，而不是直接返回固定攻略。

### 8.1 框架不内置玩家社会交互逻辑

MineIntent 框架只提供观察和动作接口，不负责定义玩家之间的协作、敌对、信任、所有权或社会关系。即使以后运行在多人世界中，也不需要为以下概念编写固定决策模块：

- 理解玩家意图和模糊聊天。
- 跟随、分工与协作。
- PvP、偷袭和敌我判断。
- 共享资源、物资归属和任务交接。
- 信任、欺骗、承诺和关系记忆。
- 多个玩家或 Agent 之间的资源竞争。
- 对其他玩家造成的世界变化作出策略判断。

这些属于大模型基于目标、上下文和记忆作出的决策，不属于框架预先编码的流程。框架只需中立地提供事实与能力，例如：

```text
observe_players()
observe_chat()
observe_teams()
observe_world_events()
send_chat(message)
look_at(entity)
follow(entity)
attack(entity)
give_item(entity, item, amount)
interact(entity)
```

例如，框架可以报告：

```json
{
  "event": "player_opened_container",
  "player": "Alex",
  "position": [12, 64, -8]
}
```

但不应自行推断：

```text
Alex 正在偷窃
Alex 是敌人
应该攻击 Alex
```

这些语义和行动选择交给 AI。框架负责保证接口真实、及时、可验证，并把 AI 选择的合法动作可靠地转换为协议操作。除非用户明确配置安全规则，否则框架不应把某种社交策略硬编码为默认流程。

## 9. 感知设计

不应把整个已加载世界直接塞进上下文。观察需要按重要性压缩：

```json
{
  "situation": "位于洞穴入口，夜晚",
  "threats": ["右前方8格有骷髅"],
  "resources": ["可见铁矿3块"],
  "inventory_summary": {
    "food": 4,
    "torches": 2
  },
  "warnings": ["食物不足"]
}
```

需要区分：

- Bot 协议层已经收到的数据。
- 本地控制器为移动和碰撞使用的数据。
- 角色按视野、遮挡和记忆规则应该知道的数据。
- 最终需要通知 AI 的重要信息。

当前只做原版，但仍应避免把协议客户端自然获得的整个区块数据直接变成“角色透视能力”。

### 9.1 协议客户端暂时放弃真实图像感知

选择无界面协议客户端意味着暂时放弃与完整 Minecraft 客户端一致的第一人称图像。协议客户端接收区块、方块、实体、粒子或声音事件等网络数据，但它不运行 Minecraft 的完整渲染流水线，通常不具备：

- 原版方块和实体渲染器。
- 纹理、模型和资源包渲染。
- 光照、雾、透明、粒子和天气画面。
- 相机、后处理、GUI 和最终帧缓冲区。
- 与真实客户端完全一致的截图。

Prismarine Viewer 或自建三维查看器可以根据协议世界缓存生成简化画面，适合人类调试，但这不是原版客户端的真实视觉，也不应被描述为 Bot 的原生视觉能力。

核心感知因此采用结构化信息：

```text
方块和碰撞
实体、位置和状态
视线射线与遮挡
背包和容器
聊天、声音及世界事件
空间记忆
```

若以后确实需要真实图像，可增加独立的“完整客户端视觉后端”，由真实 Minecraft 客户端渲染 Bot 视角并提供截图。该后端应是可选传感器，不改变上层观察、动作、Flow 和 AI 接口，也不应成为当前协议 Bot 的开发依赖。

同服观察客户端、附身视角和录像目前只用于人类观看与调试；除非未来明确启用视觉后端，它们生成的画面不会自动提供给 AI。

## 10. 执行结果协议

正常执行不返回每个 tick，只返回关键变化：

```json
{
  "status": "completed",
  "durationMs": 8420,
  "effects": {
    "inventory_added": {
      "minecraft:oak_log": 1
    }
  },
  "observations": [
    "附近仍可见两块原木"
  ]
}
```

失败必须提供可用于重新规划的真实原因：

```json
{
  "status": "failed",
  "failed_step": "navigate",
  "reason": "unreachable",
  "details": {
    "obstacle": "ravine",
    "missing_resource": "scaffolding_block"
  }
}
```

AI 不能自行宣称执行成功。技能结果和最终目标必须由程序根据真实游戏状态验证。

## 11. 长期记忆

不能只保存聊天记录。至少需要结构化持久化：

- 长期目标。
- 当前方案和短期意图。
- 已确认事实。
- 已探索区域。
- 关键地点和传送门。
- 容器位置及其中的重要物资。
- 死亡地点。
- 失败方案和原因。
- 当前技能与检查点。
- 重启后必须重新验证的过期信息。

服务端断线、Bot 死亡、进程崩溃和模型超时都应当作为正常事件处理。

## 12. 主要技术难点

按风险大致排序：

1. 长时间任务中的状态一致性和失败恢复。
2. 通用技能的可靠性、取消和清理。
3. 感知压缩、注意力和空间记忆。
4. 未知区块中的探索与避免绕圈。
5. 危险环境下的寻路、战斗和实时反射。
6. 背包、容器、菜单 ID、状态 ID 和槽位同步。
7. AI 对游戏版本、机制和可用工具的幻觉。
8. Mineflayer 插件在 1.21.1 边缘机制上的正确性。
9. 死亡、换维度、断线和重启恢复。
10. 长时间运行的成本、日志和问题复现。

“支持 1.21.1”只代表库能够连接和解析协议，不代表所有行为在该版本上完全可靠。关键能力必须写集成测试。

## 13. 开发阶段

不要直接编写固定的“获得鞘翅”流程。按能力逐步验收：

### 阶段 A：协议和状态

- 连接关闭正版验证的 1.21.1 服务器。
- 稳定维持连接。
- 正确获得位置、维度、背包、区块和实体状态。
- 正确处理断线和重连。

### 阶段 B：原子动作

- 走到指定位置。
- 转向指定方块或实体。
- 挖掘、放置、拾取、攻击和使用。
- 打开、操作并关闭箱子。
- 验证服务端最终状态。

### 阶段 C：最小短流程

第一个完整闭环：

```text
输入目标：获得一根原木
观察 → AI 生成短流程 → 本地执行 → 验证背包 → 完成
```

必须验证失败时 AI 能根据结构化原因改变方案。

### 阶段 D：通用物品目标

- 获取任意已知自然资源。
- 合成多级物品。
- 使用工作台、熔炉和箱子。
- 管理食物、工具和背包空间。

### 阶段 E：长期自主性

- 探索未知区域。
- 建立和使用空间记忆。
- 生存、战斗、死亡恢复。
- 跨维度。
- 进程重启后继续长期目标。

### 阶段 F：最终压力测试

从原版生存世界出发，仅输入一次“获得鞘翅”，不限制 AI 选择的路线，最终由程序检查背包中是否存在：

```text
minecraft:elytra
```

## 14. 建议的新项目结构

```text
mineintent/
├── package.json
├── tsconfig.json
├── src/
│   ├── minecraft/       # Mineflayer连接、事件与状态
│   ├── actions/         # 原子动作
│   ├── skills/          # 通用可组合技能
│   ├── flow/            # DSL、AST、验证和执行器
│   ├── perception/      # 观察提取与压缩
│   ├── memory/          # 结构化长期记忆
│   ├── knowledge/       # 原版机制查询
│   ├── agent/           # 模型适配和规划循环
│   ├── evaluation/      # 目标判定
│   └── telemetry/       # 日志、轨迹和调试
├── tests/
│   ├── unit/
│   └── integration/
└── worlds/              # 固定种子的评测世界配置，不提交大型存档
```

模型供应商应通过接口隔离，避免系统绑定某一家 API。

## 15. 项目名称

当前名称：**MineIntent**。

命名含义：用户提供 Minecraft 中的意图，AI 自己选择并执行路线。

初步可用性调查：

- 未发现明确同名软件或 Minecraft 项目。
- GitHub 搜索无同名项目。
- npm 包 `mineintent` 暂未占用。
- npm scope `@mineintent/bot` 暂未占用。
- `mineintent.com` 已注册但仅为域名停放页。
- `.dev`、`.ai`、`.io` 当时未发现 DNS 记录，但这不等于注册商确认可购买。

建议技术命名：

```text
项目：MineIntent
仓库：mineintent
npm scope：@mineintent
核心包：@mineintent/bot
CLI：mineintent
```

尚未进行正式商标法律检索。

## 16. 已查阅的项目

- Mineflayer：https://github.com/PrismarineJS/mineflayer
- node-minecraft-protocol：https://github.com/PrismarineJS/node-minecraft-protocol
- mineflayer-pathfinder：https://github.com/PrismarineJS/mineflayer-pathfinder
- mineflayer-collectblock：https://github.com/PrismarineJS/mineflayer-collectblock
- Fabric Carpet：https://github.com/gnembon/fabric-carpet
- Applied Energistics 2：https://github.com/AppliedEnergistics/Applied-Energistics-2

旧工作区中曾浅克隆 Carpet 1.4.147、AE2 1.21.1 和 AE2 Fabric 1.20.1，仅作为研究材料。新项目不需要携带这些仓库即可开始。

## 17. 可选的观看、观测与调试层

核心 Bot 是无界面协议客户端，但开发和演示时必须保留方便人类观察其行为的途径。调试工具可以使用客户端 MOD、服务端插件或独立界面；“只考虑原版”限制的是 Bot 的生存目标和游戏机制，不禁止使用不改变 Bot 决策的开发工具。

这些工具应是可选旁路，不参与 AI 决策，也不成为 MineIntent 正常运行的前置依赖：

```text
Minecraft Server
├── MineIntent 协议 Bot
├── 人类观察客户端
└── 可选调试服务 / 客户端 MOD / 服务端插件
```

可采用的观察方式：

- 人类使用真实 Minecraft 客户端加入同一个服务器，直接观察 Bot 的第三人称行为。
- 使用旁观模式跟随 Bot；在允许作弊或拥有相应权限的测试服务器上，可以附身到玩家视角，查看 Bot 的第一人称朝向和实际操作。
- 使用客户端调试 MOD，将摄像机锁定到 Bot、显示其路径、目标方块、视线、当前技能和计划。
- 使用服务端插件或调试接口查看 Bot 的背包、装备、生命、饥饿、状态效果、位置和当前容器。
- 使用 Mineflayer/Prismarine Viewer 或自建 Web 面板显示已加载区块、实体、路径、背包和技能状态。
- 记录关键事件和动作轨迹，必要时结合服务器录像或 Replay 类工具回放问题。

“附身观看”只改变观察者的摄像机，不应接管 Bot、改变 Bot 可见信息或为它提供额外世界数据。若使用客户端 MOD，应优先显示：

```text
长期目标
当前短期意图
正在执行的 Flow
当前步骤和进度
路径与目标位置
本地危险判断
背包和装备摘要
最近一次模型决策
技能失败原因
模型调用与执行耗时
```

建议 MineIntent 从一开始就暴露只读调试状态，例如本地 WebSocket/HTTP 接口：

```json
{
  "goal": "获得鞘翅",
  "intent": "寻找可采集的木材",
  "flow": "collect_nearby_logs",
  "step": "navigate",
  "position": [12.4, 64.0, -8.1],
  "health": 18,
  "food": 15,
  "target": {
    "type": "block",
    "id": "minecraft:oak_log",
    "position": [19, 65, -3]
  }
}
```

调试层还应支持人工暂停、继续、单步执行和安全终止当前 Flow，但人工干预必须被记录，以免把被干预的运行误判为自主成功。

## 18. 新会话的建议开场任务

将本文放入空项目后，可以向新的编码 Agent 提出：

```text
阅读 MINEINTENT_HANDOFF.md。以其中的范围和决策为准，创建 MineIntent 的 TypeScript 项目骨架。先不要接入大模型，也不要实现固定任务流程。第一阶段只实现并测试：连接 Minecraft 1.21.1 离线验证服务器、记录基础状态、行走到给定坐标、挖掘指定方块，并用真实背包状态验证结果。所有玩家能力必须通过可取消、带超时、返回结构化结果的接口暴露。
```

第一阶段开始前，应确认本机 Node.js 版本、包管理器选择、测试服务器启动方式和 Mineflayer 对具体 1.21.1 数据包/物理行为的实际表现。
