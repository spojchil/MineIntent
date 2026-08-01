# Minecraft Java 客户端能力全景

- 状态：版本化研究基线（参考材料，无产品权威）
- 来源：2026-08-01 迁自维护者早前的 Go 客户端仓库（本地 `codex/MineIntent`，HEAD `2082785`）；原文「关联：Issue #32」指该仓库的 Issue 编号，不是本仓库的 Issue
- 基线：Minecraft Java Edition 26.1.2，protocol 775，原版服务端
- 调研日期：2026-07-19

本文盘点一个无头 Minecraft Java 客户端可以观察、维护、推导和执行的能力。它是后续里程碑的能力目录，不是 v0.2.0 的范围承诺，也不是实现任务清单。

## 1. 研究方法与来源

本清单以 Mojang 26.1.2 原版客户端中的协议类、调试条目和按键映射为版本真值，再使用成熟客户端库核对数据包覆盖与高层语义。

| 来源 | 调研快照 | 用途 | 限制 |
| --- | --- | --- | --- |
| Mojang 26.1.2 `client.jar` | SHA1 `4e618f09a0c649dde3fdf829df443ce0b8831e65` | 核对原版协议类、F3 条目和按键映射 | 不提供现成的 Agent 语义接口 |
| Mojang 26.1.2 `server.jar` | SHA1 `97ccd4c0ed3f81bbb7bfacddd1090b0c56f9bc51` | 核对原版服务端接受、发送和超时语义 | 不代表第三方服务端实现 |
| MCProtocolLib | `19783c29ece24bc3f07f8ff08628549527e3de20` | 核对 protocol 775 双向数据包及字段 | 高层世界模型不是其主要目标 |
| Mineflayer | `7368ac8e9cc8dc9800d611bd46b2548c8b8fe48d` | 核对状态模型和高层行为集合 | 当前最高支持 1.21.11，不能作为 26.1.2 协议真值 |
| node-minecraft-protocol | `ee40987dde124cd2f560bb944cfffb29946fef81` | 核对底层数据包和跨版本组织方式 | 当前最高支持 1.21.11 |
| go-mc | `539b4a3a7f030332eb58b8a946116ae7907630d2` | 核对 Go 客户端的模块边界 | 当前版本较旧，只作为补充 |
| mineflayer-pathfinder | `d1f4d7fdbebc452f390a9bc8b64e9d8ebfdb9f95` | 核对目标、移动成本、动态重规划与路径事件 | 不支持 26.1.2，不能作为协议真值 |
| Baritone | `054092e44eec61f6ef3818a2b4b7c56df90daf76` | 核对方块查询、目标、可达性及挖掘/放置路径 | 面向带完整原版模型的客户端 Mod |
| Azalea | `c35b57ebf82fa8b26ada77ab9eb795e3827d6c16` | 核对无头客户端的物理、世界、寻路与动作边界 | 仍有未完成能力和破坏性变更 |

官方版本元数据：<https://piston-meta.mojang.com/v1/packages/6f2c56190a59d4920b82d73ebbf7969c260f3f36/26.1.2.json>

第三方仓库：

- <https://github.com/GeyserMC/MCProtocolLib>
- <https://github.com/PrismarineJS/mineflayer>
- <https://github.com/PrismarineJS/node-minecraft-protocol>
- <https://github.com/Tnze/go-mc>
- <https://github.com/PrismarineJS/mineflayer-pathfinder>
- <https://github.com/cabaletta/baritone>
- <https://github.com/azalea-rs/azalea>

MCProtocolLib 对 26.1 / protocol 775 有直接支持。其数据包类与原版客户端的 Play、Configuration、Common、Login 和 Status 协议类基本对应；少数名称差异来自基类、组合数据和按位置/旋转拆分的表示方式，未发现因此缺失的主要语义领域。

### 1.1 快照复现

JAR 下载地址来自上述官方版本元数据，不提交到 MineIntent 仓库。最小复现流程为：

```powershell
Get-FileHash .\client.jar -Algorithm SHA1
Get-FileHash .\server.jar -Algorithm SHA1
jar tf .\client.jar | Sort-Object > client-classes.txt
jar tf .\server.jar | Sort-Object > server-classes.txt
git -C .\MCProtocolLib checkout 19783c29ece24bc3f07f8ff08628549527e3de20
rg "register(Clientbound|Serverbound)Packet" .\MCProtocolLib\protocol\src\main\java
```

对协议字段的判断以固定提交中的 packet 类为交叉证据；对原版时序和界面语义的判断以固定 JAR 反编译结果为证据。附录给出 packet/class 到能力域的覆盖索引，避免只凭名称人工抽样。

### 1.2 范围声明

本研究覆盖连接原版 Java 服务端所需的线协议、MC 状态模型、客户端推导与 Agent 可用动作。下列产品表面不在本次能力清单范围内：

- Microsoft/Xbox OAuth 登录界面与账号获取流程；但使用调用方提供的令牌完成 session server join 属于在线服务器连接能力。
- Realms 发现、邀请、购买和管理。
- 皮肤/Profile 的上传与管理；但登录及玩家列表中收到的 Profile 属性属于会话数据。
- 聊天举报提交、社交屏蔽 UI 和 Mojang 遥测；安全聊天的密钥、签名、确认与验证仍属于协议能力。
- 画面渲染、音频播放和原版 GUI 复刻。

## 2. 能力来源分类

原 P/M/D/C/L 记法混合了来源、处理阶段、可用条件和执行位置，不能作为稳定目录模型。后续目录按独立维度描述能力：

| 维度 | 取值示例 | 回答的问题 |
| --- | --- | --- |
| `origin` | `protocol` / `local` / `external-service` | 原始事实从哪里产生？ |
| `state_requirement` | `event` / `maintained-model` / `derived` | 是瞬时事件、持续状态，还是计算结果？ |
| `authority` | `server-authoritative` / `client-predicted` / `heuristic` | 谁对结果拥有最终裁决权？ |
| `availability` | 加载范围、权限、模式、配置、在线身份 | 在什么条件下存在？ |
| `lifetime` | packet / tick / connection / dimension / session | 数据在何时失效？ |
| `confirmation` | 响应包、状态更新、超时、无确认 | 如何知道请求是否生效？ |
| `failure_modes` | 拒绝、纠正、断线、部分结果、过期 | 失败如何显现？ |
| `side_effects` | 移动、写槽、下载、转服、公开聊天 | 调用会改变什么？ |
| `sensitivity` | token、cookie、聊天、资源包、位置 | 是否需要脱敏或策略许可？ |

下列表格为保持可读性，只在“形成链”中紧凑列出 `origin` 与 `state_requirement`；`authority` 默认是服务端权威，例外会写在限制中，`availability` 等维度也由“主要限制”展开。例如“看向的方块”是 `protocol · maintained-model · derived`；客户端预测命中不是服务端真值。Status 延迟是 `protocol · derived`，因为它来自 Ping/Pong 再叠加本地单调时钟，而不是服务端直接提供的字段。

## 3. 可观察能力

### 3.1 连接、配置与服务器

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 服务器状态 | 版本、协议号、MOTD、在线与最大人数 | protocol · event | Status 与 Play 是不同连接流程；均为服务端自报信息 |
| 状态探测延迟 | Status Ping/Pong 往返时间 | protocol · derived | 依赖本地单调时钟，不是服务端直接提供的值 |
| 会话状态 | 登录阶段、进入 Play、断线原因、重连、Play 延迟 | protocol · maintained-model · derived | 逻辑重连由 MineIntent 管理；延迟需本地计时 |
| 服务端元数据 | 服务端品牌、图标、描述、是否安全聊天 | protocol · event | 均为服务端自报信息，不构成可信身份；品牌通常来自自定义载荷 |
| 在线身份与加密 | Encryption Request/Response、session server join、Profile Key | protocol · external-service · maintained-model | 令牌与私钥是敏感数据；离线模式不执行这条链 |
| 配置注册表 | Registry、Tag、启用功能、Known Pack | protocol · maintained-model | 是解释物品、实体和世界数据的基础 |
| 动态重配置 | Play → Configuration、确认、重新进入 Play | protocol · maintained-model | Registry、Tag、功能集及依赖模型必须按新 epoch 失效并重建 |
| 客户端设置与隐私 | 语言、视距、聊天可见性、聊天颜色、主手、皮肤部件、文本过滤、服务器列表可见性、粒子级别 | local · protocol · maintained-model | 由 `ClientInformation` 发送；默认值和更改必须可配置、可审计 |
| 服务器规则入口 | 难度、硬核、游戏模式、模拟距离、视距 | protocol · maintained-model | 部分值会在重生或配置切换时更新 |
| 资源包 | URL、哈希、是否必须、提示、压入与弹出 | protocol · maintained-model | 音频和纹理内容需要另行下载解析 |
| 服务端导航 | Server Links、Transfer 目标 | protocol · event | Transfer 会结束当前物理连接 |
| Cookie | 服务端请求、保存和后续登录回传 | protocol · maintained-model | 必须限制大小并避免泄漏隐私 |
| 行为规范与对话框 | Code of Conduct、服务端 Dialog | protocol · maintained-model | 属于新版协议的交互表面 |
| 扩展频道 | Custom Payload 与自定义点击动作 | protocol · event | 不是原版稳定语义，必须显式声明支持 |

### 3.2 自身玩家状态

| 能力 | 可获得内容 | 形成链 | 备注 |
| --- | --- | --- | --- |
| 身份 | 实体 ID、UUID、用户名、玩家列表资料 | protocol · maintained-model | 离线模式 UUID 与正版身份语义不同 |
| 空间状态 | XYZ、Yaw、Pitch、头部朝向、维度 | protocol · maintained-model | 还要处理服务端相对位移和传送确认 |
| 运动状态 | 速度、是否落地、碰撞后的实际位置 | protocol · maintained-model · derived | 服务端修正优先于本地预测 |
| 生命状态 | 生命值、死亡、复活、伤害事件 | protocol · maintained-model | 生命值可以超过默认上限 |
| 生存状态 | 饱食度、饱和度、氧气、着火、冰冻 | protocol · maintained-model | 氧气等部分状态来自实体元数据 |
| 经验 | 等级、总经验进度、经验条进度 | protocol · maintained-model | 由独立更新包维护 |
| 姿态 | 站立、潜行、游泳、睡眠、滑翔等 Pose | protocol · maintained-model · derived | 输入意图与服务端确认状态应分开 |
| 能力 | 可飞行、正在飞行、无敌、创造放置、飞行和行走速度 | protocol · maintained-model | 由服务端控制能力边界 |
| 属性 | 最大生命、移动速度、攻击力、护甲等属性及修饰符 | protocol · maintained-model | 属性集合会随版本与注册表扩展 |
| 状态效果 | 效果类型、强度、持续时间、显示标志 | protocol · maintained-model | 需要处理添加、更新和移除 |
| 装备 | 主手、副手、头盔、胸甲、护腿、靴子 | protocol · maintained-model | 与背包槽状态需要保持一致 |
| 冷却 | 物品或冷却组、剩余时间 | protocol · maintained-model | 影响动作是否可执行 |
| 出生与重生 | 默认出生位置、角度、重生、床失效 | protocol · maintained-model | 维度切换也会触发重建状态 |

### 3.3 背包、容器与物品

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 玩家背包 | 主背包、快捷栏、盔甲、副手 | protocol · maintained-model | 必须跟踪状态 ID 和服务端纠正 |
| 当前选择 | 快捷栏槽、主手物品、Cursor Item | protocol · maintained-model | 选中槽与槽内容分别更新 |
| 物品数据 | 类型、数量、数据组件、耐久、附魔、名称等 | protocol · maintained-model | 26.1.2 以组件化物品数据为核心 |
| 打开窗口 | 类型、容器 ID、标题 | protocol · maintained-model | 只能观察已打开并加载的容器 |
| 容器内容 | 所有槽、单槽更新、可用槽状态 | protocol · maintained-model | 不可读取未打开箱子的内容 |
| 容器属性 | 熔炉进度、附魔种子、铁砧成本等窗口数据 | protocol · maintained-model | 属性含义依赖窗口类型 |
| 村民交易 | 商品、输入、输出、次数、经验、价格修正 | protocol · maintained-model | 需要先与村民交互并打开窗口 |
| 配方 | 服务端可用配方、配方书、Ghost Recipe | protocol · maintained-model | 还需要物品注册表才能解释材料 |
| 地图物品 | 地图颜色像素、图标、缩放与锁定状态 | protocol · maintained-model | 只包含服务端发来的地图更新 |

### 3.4 世界、区块与方块

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 区块 | 已加载区块、Section、调色板和高度范围 | protocol · maintained-model | 只覆盖服务端发送的加载范围 |
| 方块状态 | 坐标对应的方块、状态属性 | protocol · maintained-model | 需要 Registry/Tag 数据解释 ID |
| 方块更新 | 单方块、Section 批量更新、区块卸载 | protocol · maintained-model | 更新顺序影响世界模型一致性 |
| 方块实体 | 告示牌、容器外观、信标等 NBT/组件数据 | protocol · maintained-model | 不代表可以读取容器内部物品 |
| 生物群系 | 区块中的 Biome | protocol · maintained-model | 名称来自动态注册表 |
| 光照 | 天空光、方块光、Light Section | protocol · maintained-model | 可以进一步推导指定位置亮度 |
| 高度图 | 各种 Heightmap | protocol · maintained-model | 仅限已加载区块 |
| 方块事件 | 活塞、箱盖、音符盒等 Block Event | protocol · maintained-model | 需要方块类型解释事件参数 |
| 破坏进度 | 实体正在破坏的方块和破坏阶段 | protocol · maintained-model | 是可见动画进度，不等同于剩余时间 |
| 世界边界 | 中心、大小、渐变、警告距离与时间 | protocol · maintained-model | 影响移动和规划边界 |
| 世界时间 | Game Time、Day Time、昼夜循环 | protocol · maintained-model · derived | 天数和月相是推导值 |
| 天气 | 雨、雷、强度变化 | protocol · maintained-model | 原版通过 Game Event 等状态维护 |
| 难度与规则 | 难度、锁定、服务端同步的游戏规则 | protocol · maintained-model | 并非所有服务端规则都对客户端公开 |
| 爆炸 | 中心、半径、方块数量、可选击退、粒子和声音 | protocol · event | protocol 775 不携带受影响方块坐标；破坏结果来自后续方块或区块更新 |
| 世界事件 | Level Event、粒子、测试标记等瞬时事件 | protocol · maintained-model | 不应全部进入长期世界快照 |

### 3.5 实体与玩家

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 实体生命周期 | 生成、移除、类型、实体 ID、UUID | protocol · maintained-model | 只包含追踪范围内实体 |
| 实体空间 | 位置、相对移动、旋转、头部旋转、传送 | protocol · maintained-model | 相对更新需要基于既有状态合并 |
| 实体运动 | 速度、矿车移动、载具修正 | protocol · maintained-model · derived | 客户端物理预测不是服务端真值 |
| 实体元数据 | Pose、旗标、特定生物状态和可选字段 | protocol · maintained-model | 元数据索引与实体类型、版本相关 |
| 实体装备 | 各装备槽物品 | protocol · maintained-model | 只表示服务端公开的当前装备 |
| 实体属性 | 生命上限、速度、攻击等属性 | protocol · maintained-model | 通常只对需要追踪的实体发送 |
| 实体效果 | 状态效果添加、更新、移除 | protocol · maintained-model | 不一定包含服务端所有内部效果 |
| 动画与事件 | 挥手、受伤、死亡、暴击、进食等 | protocol · maintained-model | 多数是瞬时事件 |
| 战斗 | 伤害事件、进入/结束战斗、击杀信息 | protocol · maintained-model | 精确伤害计算仍可能需要本地规则模型 |
| 实体关系 | 乘客、载具、拴绳、实体链接 | protocol · maintained-model | 必须处理任一实体先移除的情况 |
| 玩家列表 | 加入、离开、显示名、游戏模式、延迟、皮肤信息 | protocol · maintained-model | Tab 列表和世界实体并非同一生命周期 |
| 收集物品 | 哪个实体拾取哪个物品实体 | protocol · maintained-model | 背包最终状态仍由槽更新确认 |
| Waypoint | 服务端追踪点和更新 | protocol · maintained-model | 新版、服务端控制的能力 |
| 强制相机 | 当前 Camera 实体 | protocol · maintained-model | `SetCamera` 可让视角脱离自身玩家；相机实体移除时必须恢复 |
| 强制看向 | 服务端要求看向的坐标或实体锚点 | protocol · event | `PlayerLookAt` 需要结合目标实体模型；不等于持续锁定 |
| 强制旋转 | 服务端下发的玩家 Yaw/Pitch | protocol · maintained-model | `PlayerRotation` 属于服务端权威修正，应与客户端视角意图区分 |

### 3.6 聊天、命令与界面语义

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 玩家聊天 | 正文、发送者、签名和验证相关数据 | protocol · maintained-model | 在线与离线模式的签名能力不同 |
| 安全聊天会话 | Profile 公钥、`ChatSessionUpdate`、消息索引、签名链和验证结果 | protocol · external-service · maintained-model | 密钥有生命周期；重连或轮换后旧签名上下文不可复用 |
| 系统聊天 | 系统消息及 Overlay 位置 | protocol · event | 应与玩家聊天分开建模 |
| 伪装聊天 | 服务端按聊天类型展示的非签名消息 | protocol · maintained-model | 需要聊天类型注册表 |
| 消息生命周期 | 删除消息、Last Seen、确认 | protocol · maintained-model | 维护错误可能导致断线 |
| 命令树 | 命令、参数节点、建议提供器 | protocol · maintained-model | 不意味着客户端有执行权限 |
| 命令建议 | 补全结果及范围 | protocol · maintained-model | 必须针对一次请求关联响应 |
| 自定义补全 | 服务端追加或移除聊天补全项 | protocol · maintained-model | 与命令建议不同 |
| 标题类信息 | Title、Subtitle、Action Bar、显示时间、清除 | protocol · maintained-model | 属于短期展示事件 |
| Tab 列表 | Header 与 Footer | protocol · maintained-model | 玩家条目由另一组包维护 |
| 打开界面 | 容器、书、告示牌编辑器、坐骑窗口、Dialog | protocol · maintained-model | “显示界面”不等同于必须实现 GUI |

### 3.7 声音、粒子与瞬时感知

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 定位声音 | 声音 ID、类别、XYZ、音量、音高、随机种子 | protocol · maintained-model | 声音资源本体不在数据包里 |
| 实体声音 | 声音 ID、类别、来源实体、音量、音高、种子 | protocol · maintained-model | 位置需要从实体模型取得 |
| 停止声音 | 按声音 ID、类别或两者停止 | protocol · maintained-model | 对事件流有意义，对状态快照意义较弱 |
| 可听判断 | 距离衰减、类别音量、遮挡近似 | local · derived | 需要实现客户端声学规则；原版不是完整物理声学 |
| 音符盒 | 方块事件与声音组合出的音符、乐器、音高 | protocol · maintained-model · derived | 高层语义需要组合事件和方块状态 |
| 粒子 | 类型、位置、偏移、速度、数量和专用数据 | protocol · maintained-model | 无头客户端通常保留语义事件而不渲染 |
| 爆炸与受击反馈 | 爆炸、伤害动画、Hurt Animation | protocol · maintained-model | 视觉表现不属于 MC 语义接口 |

### 3.8 游戏进度与公共状态

| 能力 | 可获得内容 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 统计 | 服务端发来的统计项和值 | protocol · maintained-model | 可能在用户请求统计界面后才完整发送 |
| 进度 | Advancement 定义、进度、选中 Tab | protocol · maintained-model | UI 排版不是无头客户端职责 |
| 配方书 | 已知配方、Highlight、筛选设置 | protocol · maintained-model | 与可实际合成仍是两个问题 |
| 计分板 | Objective、Score、显示槽 | protocol · maintained-model | 名称和显示组件需正确解析 |
| 队伍 | 成员、颜色、前后缀、碰撞和友伤规则 | protocol · maintained-model | 同时影响实体显示与交互规则 |
| Boss Bar | 标题、进度、颜色、样式和环境标志 | protocol · maintained-model | 可能同时存在多个 Bar |
| Tick 状态 | Tick Rate、冻结、单步信息 | protocol · maintained-model | 与服务端权限和版本能力有关 |
| Projectile Power | 投射物实体及服务端同步的动力值 | protocol · maintained-model | 新版实体状态；只对已追踪实体可用 |
| Low Disk Space Warning | 服务端低磁盘空间警告 | protocol · event | 属于服务端运维提示，不是本机磁盘指标 |
| Custom Report Details | 服务端提供的自定义报告上下文 | protocol · maintained-model | 可能包含敏感服务器上下文，不应默认暴露给 Agent 或日志 |

### 3.9 协议维护与客户端就绪

这些能力通常不直接暴露给 Agent，但决定客户端能否按原版节奏持续参与会话。

| 能力 | 语义 | 形成链 | 生命周期与回应 |
| --- | --- | --- | --- |
| 客户端 Tick 边界 | 每个客户端 tick 结束时发送 `ClientTickEnd` | local · protocol · event | 由连接 epoch 内的客户端时钟驱动 |
| 世界加载完成 | 世界模型达到原版要求的就绪点后发送 `PlayerLoaded` | protocol · maintained-model | 不能仅以收到首次位置包作为就绪条件 |
| 方块预测确认 | 关联本地交互序号与 `BlockChangedAck` | protocol · maintained-model | 服务端确认前保持预测状态；拒绝或纠正时回滚 |
| Keep Alive 与 Ping/Pong | 在时限内原样回应标识，并测量往返 | protocol · derived | 必须绕过慢事件消费者，由自动响应器处理 |
| Chunk Batch | 接收 Start/区块流/Finish，并反馈期望批速率 | protocol · maintained-model | 影响区块流控与世界模型加载 |
| 聊天确认 | 维护 Last Seen、消息索引与 Chat Ack | protocol · maintained-model | 错序或遗漏可能导致断线 |
| Configuration 确认 | 确认 Play → Configuration 并完成重新配置 | protocol · maintained-model | 旧配置相关模型失效；完成后才恢复 Play 动作 |

### 3.10 Agent 世界查询与导航基础

这些不是服务端直接发送的“高级能力”，而是从有限加载范围内的世界模型构建的客户端推导。mineflayer-pathfinder、Baritone 和 Azalea 都把它们作为独立于线协议的高层模块。

| 能力 | 输出 | 形成链 | 主要限制 |
| --- | --- | --- | --- |
| 附近实体查询 | 按距离、类型、状态和谓词筛选实体 | protocol · maintained-model · derived | 只覆盖服务端追踪范围 |
| 附近方块查询 | 按方块、Tag、状态和范围搜索 | protocol · maintained-model · derived | 只覆盖已加载区块；大范围扫描需要预算 |
| 射线与视线 | 首个命中方块/实体、可见性和命中面 | protocol · maintained-model · derived | 依赖碰撞形状、流体和眼睛位置；是客户端计算 |
| 碰撞形状 | 方块和实体的碰撞/交互体积 | protocol · maintained-model · derived | 依赖版本规则与动态状态 |
| 可达性 | 在当前能力、装备和规则下是否存在候选路线 | local · maintained-model · derived | 是预测，不保证服务端最终接受 |
| 路径规划 | 路点、移动类型、成本、部分路径和失败原因 | local · maintained-model · heuristic | 必须设定计算预算，并在区块/实体变化后重规划 |
| 动态避障 | 识别实体、危险方块和临时阻塞并调整路径 | protocol · maintained-model · heuristic | 对未知区块和未来移动只能启发式处理 |
| 改造环境的路径 | 将挖掘、搭桥、放置和取材纳入路径动作 | local · maintained-model · heuristic | 有显著副作用，必须受策略、物品和权限约束 |

## 4. 可执行能力

动作清单描述服务端可理解的游戏语义，不以键盘按键作为上层接口。

### 4.1 移动、姿态与视角

| 能力 | 动作 | 形成链 | 关键依赖 |
| --- | --- | --- | --- |
| 方向输入 | 前、后、左、右及组合 | protocol · maintained-model | 需要固定频率发送输入和位置 |
| 跳跃 | 按下与释放跳跃 | protocol · maintained-model · derived | 需要本地物理和服务端位置纠正 |
| 潜行 | 按下与释放 Shift | protocol · maintained-model | 26.1.2 玩家输入包直接包含 Shift 状态 |
| 冲刺 | 输入状态与冲刺开始/停止 | protocol · maintained-model | 饱食度、碰撞和服务端规则会影响结果 |
| 视角 | 设置 Yaw/Pitch、看向坐标或实体 | protocol · derived | 需要角度计算与规范化 |
| 精确位置 | 发送位置、旋转、On Ground 状态 | protocol · maintained-model · derived | 不能绕过服务器移动校验 |
| 飞行 | 开关飞行、上升、下降 | protocol · maintained-model | 需要服务端授予飞行能力 |
| 鞘翅 | 请求开始滑翔并维持飞行输入 | protocol · maintained-model | 需要装备、空间和物理条件 |
| 睡眠 | 使用床、离床 | protocol · maintained-model | 进入睡眠由服务端确认 |
| 载具移动 | 载具方向、跳跃强度、船桨状态 | protocol · maintained-model | 不同载具具有不同输入与物理规则 |
| 上下车 | 与实体交互上车、潜行下车 | protocol · maintained-model | 结果由乘客关系更新确认 |

### 4.2 方块、实体与物品交互

| 能力 | 动作 | 形成链 | 关键依赖 |
| --- | --- | --- | --- |
| 攻击实体 | Attack、挥手、命中目标 | protocol · maintained-model · derived | 需要距离、视线、冷却和目标实体状态 |
| 普通交互 | 对实体交互、指定命中位置交互 | protocol · maintained-model | 主手/副手和实体类型影响语义 |
| 使用物品 | 吃喝、拉弓、盾牌、投掷、望远镜等 | protocol · maintained-model | 开始、持续和释放需要状态机 |
| 对方块使用 | 指定方块、面、命中点和手 | protocol · maintained-model · derived | 放置、开门、按钮、工作站共用底层动作 |
| 挖掘 | 开始、取消、完成方块破坏 | protocol · maintained-model · derived | 破坏时间依赖方块、工具、效果和环境 |
| 放置 | 选择物品并对方块面使用 | protocol · maintained-model · derived | 最终方块状态由服务端更新确认 |
| Pick Block | 从方块或实体选择对应物品 | protocol · event | 生存和创造模式行为不同 |
| 挥手 | 主手或副手 Swing | protocol · event | 可独立于攻击和使用发送 |

### 4.3 背包、容器与工作站动作

| 能力 | 动作 | 形成链 | 关键依赖 |
| --- | --- | --- | --- |
| 快捷栏选择 | 选择 0–8 槽 | protocol · maintained-model | 需要同步当前手持物 |
| 副手交换 | 主手与副手交换 | protocol · maintained-model | 服务端槽更新确认结果 |
| 丢弃物品 | 丢一个或丢整组 | protocol · maintained-model | 需要确认服务端背包变化 |
| 点击窗口 | 左键、右键、Shift、数字键、双击、拖拽 | protocol · maintained-model | 必须准确模拟槽变化和状态 ID |
| 移动物品 | 在玩家背包和容器间转移 | protocol · maintained-model · derived | 是窗口点击序列，不是单一万能包 |
| 关闭窗口 | 关闭当前容器 | protocol · maintained-model | 服务端也可能主动关闭 |
| 放置配方 | 请求 Recipe Placement | protocol · maintained-model | 仍需处理服务端生成的槽更新 |
| 合成 | 摆放材料并取出结果 | protocol · maintained-model · derived | 可由配方放置或通用点击完成 |
| 村民交易 | 选择交易、放入物品、取出结果 | protocol · maintained-model | 价格和次数可能动态变化 |
| 编辑书 | 修改书页与标题 | protocol · maintained-model | 服务端会校验长度和物品槽 |
| 告示牌 | 提交正面或背面文本 | protocol · maintained-model | 需要先收到编辑器打开请求 |
| 重命名 | 提交铁砧名称 | protocol · maintained-model | 最终成本和结果由容器状态确认 |
| 工作站设置 | 信标、附魔、熔炉、铁砧等操作 | protocol · maintained-model | 多数由按钮点击和窗口槽组合完成 |
| Bundle 选择 | 切换 Bundle 内选中项目 | protocol · maintained-model | 依赖物品组件和版本语义 |

### 4.4 通信、生命周期和特殊动作

| 能力 | 动作 | 形成链 | 关键依赖 |
| --- | --- | --- | --- |
| 发送聊天 | 普通消息 | protocol · maintained-model | 正版服务器涉及签名、时间戳和 Last Seen |
| 执行命令 | 普通或签名命令 | protocol · maintained-model | 权限由服务端决定 |
| 更新安全聊天会话 | 发送 `ChatSessionUpdate` 并轮换签名上下文 | protocol · external-service · maintained-model | 需要有效 Profile Key；密钥材料不得进入普通日志 |
| 请求补全 | 发送命令/聊天建议请求 | protocol · maintained-model | 需要按事务 ID 匹配响应 |
| 确认聊天 | Chat Ack、Last Seen 更新 | protocol · maintained-model | 属于协议维护，不应依赖 Agent 消费事件 |
| 结束客户端 Tick | 发送 `ClientTickEnd` | local · protocol · event | 由协议时钟自动执行，不是 Agent 工具 |
| 声明玩家已加载 | 发送 `PlayerLoaded` | protocol · maintained-model | 必须等待世界模型达到就绪条件 |
| 确认动态重配置 | 发送 `ConfigurationAcknowledged` 并切换状态机 | protocol · maintained-model | 由协议生命周期自动执行；暂停不适用于 Configuration 的 Play 动作 |
| 接受资源包 | 接受、下载完成、失败或拒绝 | protocol · maintained-model | 强制资源包可能决定能否继续连接 |
| 重生 | 请求 Respawn | protocol · maintained-model | 只在死亡或允许状态下有效 |
| 主动断开 | 优雅关闭会话 | protocol · maintained-model | 由 MC Runtime 管理生命周期 |
| 自定义点击 | 执行服务端 Dialog 的 Custom Click Action | protocol · event | 必须限制为显式支持的动作 |
| 观察者动作 | 观察实体、传送到实体 | protocol · event | 需要观察者模式或权限 |
| 创造模式背包 | 直接设置或清除创造模式槽 | protocol · maintained-model | 服务端会校验游戏模式与权限 |
| 管理类操作 | 难度、游戏模式、游戏规则、命令方块、结构方块等 | protocol · event | 不属于普通 Agent 的默认能力 |

### 4.5 UI 驱动请求

无头客户端不需要绘制对应界面，但如果希望覆盖原版用户可触发的协议动作，应保留下列显式请求。它们不能被误判为服务端主动持续推送的完整状态。

| 能力 | 动作 | 形成链 | 关键依赖 |
| --- | --- | --- | --- |
| 请求统计 | 请求服务端发送当前统计数据 | protocol · event | 结果通过后续统计更新到达 |
| 配方书设置 | 修改配方书筛选/打开设置并标记配方已查看 | protocol · maintained-model | 与客户端本地偏好和服务端配方状态关联 |
| 查看进度页 | 打开、切换或关闭 Seen Advancements Tab | protocol · maintained-model | 是 UI 意图，不授予任何进度 |
| 容器槽状态 | 通知服务端特定容器槽的 UI 状态变化 | protocol · maintained-model | 依赖当前容器 ID 和槽语义，不等同于点击槽 |

## 5. 原版按键全景

按键是人类输入表面，不是 Agent 接口，但它可以帮助检查是否遗漏玩家可执行的动作。26.1.2 原版 `Options` 中的按键可分为以下几类。

### 5.1 有服务端游戏语义

- 前、后、左、右
- 跳跃、潜行、冲刺
- 攻击、使用、Pick Block
- 丢弃物品、交换副手
- 快捷栏 1–9
- 鼠标移动产生的视角变化

### 5.2 打开本地界面，但界面中的操作可能产生协议动作

- 背包
- 聊天与命令
- 玩家列表
- 进度
- 社交互动
- Quick Actions
- 观察者快捷栏

这些按键本身通常不需要发送数据包；真正的协议动作发生在用户进一步点击、提交或选择时。

### 5.3 纯本地功能

- 截图
- 切换第一/第三人称视角
- 平滑镜头
- 全屏
- 隐藏 GUI
- 调试覆盖层和大多数调试组合键
- 切换观察者着色效果

### 5.4 条件性功能

- 创造模式保存/加载快捷栏
- 观察者轮盘与轮廓
- 调试切换游戏模式
- 高权限调试订阅和服务端可视化

## 6. F3 能力拆分

F3 是原版客户端把协议状态、世界模型、推导结果和本机运行指标组合成的视图，不能定义成单一的 `GetF3()` 能力。

26.1.2 原版客户端注册的调试条目包含下列领域。

### 6.1 可由协议和 MC 状态模型提供

- 玩家位置、区块坐标、Section 坐标和方块坐标
- 视角方向、维度和世界信息
- 生物群系
- 天空光、方块光和高度图
- 看向的方块状态、方块 Tag、流体状态、流体 Tag
- 看向的实体及实体 Tag
- 天数和月相
- 已加载区块与客户端实体统计
- 声音 Mood 的部分状态

其中坐标换算、朝向名称、射线命中和月相属于本地推导，而不是服务端直接发送的“F3 字段”。

原版 26.1.2 的 `DebugEntryLocalDifficulty` 仅在 `Level` 是 `ServerLevel` 时显示。普通远程客户端持有的是 `ClientLevel`，协议也不提供计算服务端真实局部难度所需的区块 `inhabitedTime`，因此不能准确推导该值。集成服务端可以直接取得；远程连接只有在服务端扩展主动提供时才可获得。

### 6.2 条件性服务端调试数据

- TPS 和远端性能采样
- 局部难度与区块 `inhabitedTime`
- 生物生成计数
- 服务端区块来源与生成状态
- 服务端路径、POI、Village Section、实体 Brain 等调试值
- “服务端眼中的区块”等可视化

这些能力可能需要集成服务端、权限、调试订阅或特定配置。普通原版多人连接不得默认声明支持。

### 6.3 纯本地运行与渲染数据

- FPS、帧时间和性能影响因素
- JVM 内存、详细内存和分配速率
- CPU、GPU、显示器、驱动和操作系统信息
- GPU 利用率
- 已渲染区块、实体和粒子统计
- 声音缓存
- 后处理效果
- 碰撞箱、区块边界、Octree、光照、Heightmap 等渲染可视化开关

无头 Agent 通常不需要这些能力。若未来需要进程运行指标，应由独立 Runtime Metrics 接口提供，而不是伪装成 Minecraft 世界状态。

## 7. 明确不可直接获得的内容

- 未加载区块、未打开容器和不在追踪范围内的实体。
- 服务端插件、数据包或实体 AI 的内部私有状态，除非服务端通过协议主动公开。
- 声音的原始音频波形；协议只提供声音事件和播放参数。
- 原版客户端渲染后的屏幕像素、FPS、遮挡结果和材质外观，除非另外实现资源与渲染系统。
- 服务端绝对真实的连续物理过程；客户端只能预测并接受服务端修正。
- 任意方块或实体的完整 NBT。Tag Query 等请求受权限、模式和服务端校验限制。
- 命令树中存在但当前玩家无权执行的命令能力。
- 服务端没有发送给客户端的作弊检测、路径规划、红石调度或区块票据内部状态。
- 普通远程连接上的服务端真实局部难度和区块 `inhabitedTime`。
- 由品牌、MOTD、图标或 secure-chat 标志推导出的可信“服务器身份”；这些字段都可由服务端自行声明。

## 8. MineIntent 三层归属

| 内容 | 所有者 | 说明 |
| --- | --- | --- |
| 数据包 ID、字段、Registry 编码、NBT、压缩与协议状态机 | Protocol | 精确绑定 26.1.2 / protocol 775 |
| 玩家、世界、实体、背包状态模型 | MC Runtime / Compatibility | 向上提供稳定语义，隐藏数据包细节 |
| `Move`、`UseItem`、`Attack` 等动作编排 | MC Runtime / Compatibility | 转换成版本相关数据包序列并确认结果 |
| 客户端协议时钟、及时回应和连接 epoch | Protocol / MC Runtime | 不依赖 Agent 或事件消费者；重连后旧 epoch 不得写入新连接 |
| 目标、计划、工具选择、记忆 | Agent | 只依赖 MC 层语义能力 |
| FPS、GPU、截图、第三人称视角 | 不属于默认三层接口 | 除非未来单独引入渲染或运行指标子系统 |

协议维护类行为，例如 Client Tick End、Keep Alive、Teleport Confirm、Chat Ack、Chunk Batch Ack 和 Configuration Acknowledged，必须由协议或 MC Runtime 自动及时处理，不能成为 Agent 工具。资源包、行为规范和 Transfer 等需要用户策略的请求不能由自动响应器无条件接受。

## 9. 能力目录条目格式

后续若将本研究转换为机器可读或可追踪目录，每项能力至少应包含：

```text
id                 稳定语义标识，例如 observe.self.vitals
name               人类可读名称
direction          observe / act / bidirectional / maintenance
origin             protocol / local / external-service
state_requirement  event / maintained-model / derived
authority          server-authoritative / client-predicted / heuristic
availability       权限、游戏模式、加载范围、配置和身份条件
lifetime           packet / tick / connection / dimension / session
confirmation       响应、状态更新、超时或无确认
failure_modes      拒绝、纠正、断线、部分结果、过期等
side_effects       移动、写槽、下载、转服、公开聊天等
sensitivity        token、cookie、聊天、资源包、位置等
protocol_support   对应协议版本和数据包覆盖状态
model_dependency   需要维护的 MC 状态
agent_exposure     是否适合暴露为 Agent Observation 或 Tool
verification       单元、协议集成或原版端到端证据
status             unknown / researched / implemented / verified
```

建议的 ID 命名空间包括：

```text
observe.connection.*
observe.self.*
observe.inventory.*
observe.world.*
observe.entity.*
observe.chat.*
observe.sound.*
observe.progress.*
act.movement.*
act.look.*
act.block.*
act.entity.*
act.item.*
act.inventory.*
act.chat.*
maintenance.protocol.*
local.runtime.*
```

## 10. packet/class → capability 覆盖附录

本附录用于确认 protocol 775 的主要 packet/class 家族都落入了能力域。完整注册顺序以固定提交的 [`MinecraftCodec.java`](https://github.com/GeyserMC/MCProtocolLib/blob/19783c29ece24bc3f07f8ff08628549527e3de20/protocol/src/main/java/org/geysermc/mcprotocollib/protocol/codec/MinecraftCodec.java) 为准；表格不是 MineIntent 已实现声明。

| packet/class 家族或代表类 | capability 域 | 关键语义 |
| --- | --- | --- |
| Status Request/Response、Ping/Pong Response | `observe.connection.status` | 服务端自报元数据与本地 RTT 推导 |
| Login Hello/Key/Compression/Finished、Login Acknowledged | `maintenance.login.*` | 身份、加密、压缩及登录状态迁移 |
| session server join、Profile Key | `maintenance.identity.*` | 外部身份服务与敏感密钥生命周期 |
| Registry Data、Update Tags、Known Packs、Finish Configuration | `observe.configuration.*` | 解释后续世界数据的配置模型 |
| Start Configuration、Configuration Acknowledged | `maintenance.reconfiguration.*` | Play → Configuration 动态迁移与模型失效 |
| Client Information | `act.client-settings.*` | 客户端设置及隐私偏好 |
| Keep Alive、Ping/Pong、Client Tick End、Player Loaded | `maintenance.protocol.*` | 时限回应、客户端节奏与世界就绪 |
| Player Position、Accept Teleportation、Move Player、Player Input | `observe.self.spatial`、`act.movement.*` | 服务端修正、确认、输入和位置更新 |
| Level Chunk With Light、Forget Chunk、Block/Section Update | `observe.world.*` | 有限加载范围内的世界模型 |
| Chunk Batch Start/Finished/Received | `maintenance.chunk-batch.*` | 区块流控与批次确认 |
| Add/Remove/Move/Teleport Entity、Entity Data/Attributes/Equipment | `observe.entity.*` | 实体生命周期及状态模型 |
| Player Inventory、Container Content/Slot/Data、Container Click | `observe.inventory.*`、`act.inventory.*` | 状态 ID、槽同步与动作确认 |
| Recipe Book、Place Recipe、Statistics、Seen Advancements | `observe.progress.*`、`act.ui-request.*` | UI 驱动请求和后续状态更新 |
| Player/System/Disguised Chat、Chat Session/Ack/Delete | `observe.chat.*`、`act.chat.*`、`maintenance.chat.*` | 安全聊天、签名链和 Last Seen |
| Sound、Entity Sound、Stop Sound、Particles、Level Event | `observe.sound.*`、`observe.event.*` | 瞬时感知事件，不默认进入长期快照 |
| Set Camera、Player Look At、Player Rotation | `observe.camera.*` | 服务端强制相机与视角修正 |
| Block Changed Ack | `maintenance.prediction.*` | 客户端方块交互预测确认 |
| Explode | `observe.event.explosion` | 中心、半径、数量、击退、粒子和声音；[不含受影响方块坐标](https://github.com/GeyserMC/MCProtocolLib/blob/19783c29ece24bc3f07f8ff08628549527e3de20/protocol/src/main/java/org/geysermc/mcprotocollib/protocol/packet/ingame/clientbound/level/ClientboundExplodePacket.java) |
| Projectile Power、Low Disk Space Warning、Custom Report Details | `observe.entity.projectile-power`、`observe.server.warning`、`observe.report-context` | 26.1 新增状态与提示 |
| Resource Pack、Cookie、Code of Conduct、Dialog、Transfer | `maintenance.policy.*`、`observe.server-interaction.*` | 需要安全边界或显式策略，不应无条件自动接受 |

## 11. 对后续规划的使用方式

本清单通过审查后，后续里程碑应从中选择一个可端到端验证的能力切片，而不是按数据包数量或按键数量制定范围。每个切片需要同时列出：

1. Agent 或 CLI 能感知的产品价值。
2. MC 层需要公开的观察和动作契约。
3. Protocol 层需要补齐的数据包。
4. 需要长期维护的状态模型。
5. 在 26.1.2 原版服务端上的端到端验收。

例如，“观察自身状态并完成一次可确认的移动”会同时涉及玩家状态、输入、位置预测、服务端修正和动作结果，而不是单独实现一个移动包。

本清单是规划输入而不是范围承诺。v0.2.0 可以包含多个 Proposal；每个 Proposal 应从这里选择可端到端验证的能力切片，并通过 Design 明确协议时序、状态模型和 Agent 边界。
