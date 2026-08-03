# MineIntent 历史技术资产、来源与旧路径

> 本页用于寻找可复用的历史资产和旧文件，不裁决产品，也不作为当前实现规范。产品文字见
> [《产品》](../../产品.md)，当前实现见[绑定版本的架构说明](../architecture.md)。

## 使用规则

- 早期记录对恢复思想来源有价值，但“更早”“已合并”或“被反复引用”都不自动赋予当前权威。
- 来源能否公开复核，与它是否拥有产品权威是两个问题。
- 下列分类用于定位和复用前审计；需要判断原文时，优先阅读不可变 commit。Issue/PR 的当前正文和评论
  用于追踪讨论与更正，但它们仍可被编辑。
- 旧正文固定在不可变快照中，不复制回活文档，也不因进入本索引而恢复原有状态。

## 产品方向的历史来源

| 来源 | 记录了什么 | 在本索引中的用途 |
|---|---|---|
| 维护者当前澄清（2026-07-29，未复制私人会话原文） | 初始方向发生过变化；产品不替 AI 选择具体社会与行动结果 | 当前 `产品.md` 文字的直接输入，仍以文件内逐条状态为准 |
| [`2ee231c`](https://github.com/spojchil/MineIntent/commit/2ee231c) | 最早 handoff 以一次长期目标和自主交付结果为中心 | 技术前身 |
| [`078f525`](https://github.com/spojchil/MineIntent/commit/078f525) | 首次完整表述长期同伴、共同经历、陪伴、可信参与和持续关系 | 初始问题框架的历史证据 |
| [Issue #108](https://github.com/spojchil/MineIntent/issues/108) | 追踪“安静真人”从候选路线和验收代理升格为产品目标的偏离与更正 | 产品层级混淆的审计记录 |
| [Issue #109](https://github.com/spojchil/MineIntent/issues/109) | 展开无主要玩家、自主关注、文本记忆和单多人统一方案 | 多项判断已进入当前产品文字；其中“专用外部管理入口”已被后续澄清取代 |

## 历史技术资产审计（2026-07-30）

本轮查阅了当时 GitHub 中可见的 63 个 Issue、45 个 PR、关键不可变提交和当前代码
（本地基线为 14afb64）。这里不回答“恢复哪一代架构”，而是拆出仍可复核的研究材料、问题定义、
算法、实现机制和验证方法。

表中状态可以组合，不是一组互斥标签：

- **现行资产**：当前代码仍在使用，并有自动检查或窄范围实验证据；这不使它自动成为产品决定。
- **候选资产**：问题和推理仍有价值，但必须依据当前《产品》重新设计、实现和验证。
- **研究线索**：适合作为下一轮阅读目录；使用前需重新核对原始来源和版本。
- **不继承**：旧整体或旧前提因当前产品未选择、后续已替代/撤回或窄实验已证伪而不能直接导入；这不表示其中每个局部思想都已被经验证伪。
- **实验证据**和**需复验**是证据限定：前者只回答被测的窄问题，后者表示版本、场景或实现条件已变化。

### 研究与实验资产

| 资产 | 来源 | 可保留的内容 | 证据与边界 |
|---|---|---|---|
| 固定版本的认知感知源码调研 | [Issue #24](https://github.com/spojchil/MineIntent/issues/24)、[PR #25](https://github.com/spojchil/MineIntent/pull/25)、[不可变调研](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/research-cognitive-perception.md) | 固定 Mineflayer、Prismarine、Pathfinder、Viewer、Sodium 等源码版本；证明 tracked entity、chunk cache、findBlocks 与视觉认知不是同一件事；记录 DDA、AABB、粗到细筛选和声音去重线索 | **研究线索；部分机制为现行资产**。固定版本上的源码事实可复核；依赖升级后必须重验，旧 FOV、距离和相对坐标不是产品结论 |
| Agent 与具身智能相关工作地图 | [078f525 的系统调研](https://github.com/spojchil/MineIntent/blob/078f525364b28c62c037189e86ede2b61575e417/research/SYSTEM_DESIGN_RESEARCH.md)、[Issue #60](https://github.com/spojchil/MineIntent/issues/60) | OpenClaw、Mindcraft、Voyager、MineDojo、STEVE-1、JARVIS-1、SayCan、Inner Monologue、POMDP、Generative Agents、Letta/memU 等阅读入口；可比较原始 API、合法观察边界、证据治理三组实验 | **研究线索**。早期系统调研没有完整外部版本锚点，#60 也未完成综述和消融；具体结论进入实现前应回到一手论文或源码复核 |
| 可重复 Paper 场景框架 | [Issue #8](https://github.com/spojchil/MineIntent/issues/8)、[PR #30](https://github.com/spojchil/MineIntent/pull/30)、[PR #31](https://github.com/spojchil/MineIntent/pull/31) | setup、companion、assertion、cleanup 分段；独立复制世界；总期限、取消和清理；JSONL、服务器日志和真实方块、背包、位移断言 | **现行资产**。早期纵向切片使用确定性关键词模型，证明管线能闭合，不证明真实模型理解、人格或产品体验 |
| 分层裁判与防泄漏测试 | [Issue #40](https://github.com/spojchil/MineIntent/issues/40)、[Issue #45](https://github.com/spojchil/MineIntent/issues/45)、[PR #69](https://github.com/spojchil/MineIntent/pull/69) | 把服务器真值、可信内部投影、Provider 输出和模型实际收到的信息分层；测试真值只用于布置与断言；隐藏输入变化时公开输出和普通 trace 应保持不变 | **候选资产**。现行 Paper 骨架保留隔离方法，但旧 Paper 插件、动作关联和整套验收矩阵没有实现 |
| D40 真实模型短动作实验 | [PR #73](https://github.com/spojchil/MineIntent/pull/73)、[PR #77](https://github.com/spojchil/MineIntent/pull/77)、[PR #78](https://github.com/spojchil/MineIntent/pull/78)、[归档头 f2aada3](https://github.com/spojchil/MineIntent/commit/f2aada361d314b2fa12896e1bf29ac0d741c1601) | 固定 Paper 场景、旁观 Bot、工具轨迹、服务器真值三路交叉；7 次 live run 暴露出视锥、移动过冲、身体未稳定和结果虚报问题 | **实验证据**。五份可评价最终报告中四份不实；运行跨越多个演进提交，未在归档头完整重跑，也没有证明长活动、多人、记忆或工具循环是唯一架构 |
| 可见性判据与独立 oracle | [Issue #82](https://github.com/spojchil/MineIntent/issues/82)、[PR #100](https://github.com/spojchil/MineIntent/pull/100) | 平地样本中中心射线召回 0/900，暴露面判据 900/900；section 预筛跳过 100 个 section 中的 76 个；用闭式 oracle、脚本和生产变异交叉验证 | **现行资产 / 实验证据**。当前已有矩形视锥、实体 AABB 采样、暴露面、section 粗筛、预算与取消；材质光学仍是粗近似，不等于渲染器 |
| 模型供应商兼容探测 | [PR #78](https://github.com/spojchil/MineIntent/pull/78)、[PR #93](https://github.com/spojchil/MineIntent/pull/93) | 标准 tool calls、响应格式竞争、缓存统计和逐轮消息保存的探测方法 | **实验证据；需复验**。供应商、模型和协议会变化；旧矩阵只能说明被测版本，不能作为长期兼容承诺 |
| 长期第三方行为观察 | [Issue #70](https://github.com/spojchil/MineIntent/issues/70)、[PR #71](https://github.com/spojchil/MineIntent/pull/71)、[Issue #108 的更正](https://github.com/spojchil/MineIntent/issues/108#issuecomment-5100507322) | 长时间观察可发现身份漂移、动作—语言不一致、事件遗漏和注意机制缺陷 | 观察方法是**候选资产**；旧完成标准**不继承**。N02/N03 已否定“安静真人不可区分”和刻意模拟人类缺陷作为产品完成标准 |

### 架构、实现与算法资产

| 资产簇 | 来源 | 可复用核心 | 当前状态与边界 |
|---|---|---|---|
| 三种时间尺度与有界长动作 | [2ee231c](https://github.com/spojchil/MineIntent/commit/2ee231c)、[078f525](https://github.com/spojchil/MineIntent/commit/078f525)、[Issue #104](https://github.com/spojchil/MineIntent/issues/104) | tick 级身体控制、秒到分钟的有界流程、事件驱动的模型规划分层；循环、资源、期限和取消显式；不执行模型生成的任意代码；长动作可建成可查询、可取消的后台 job | **候选资产**。早期 Flow DSL 和“一次目标交付”产品外壳已被标准工具循环替代；后台 job 未实现，停止权仍受 Q04 约束 |
| Mineflayer Backend 生命周期 | [Issue #14](https://github.com/spojchil/MineIntent/issues/14)、[PR #26](https://github.com/spojchil/MineIntent/pull/26)、[PR #27](https://github.com/spojchil/MineIntent/pull/27) | connect、login、spawn、ready 分离；connection epoch 隔离旧事件；唯一 close outcome；death、respawn、dimension 分开；同步复制 plain DTO；可取消退避和幂等清理 | **现行资产**。主体至今保留且有测试/Paper 记录；Minecraft、Mineflayer 或协议版本升级后需重验事件顺序和字段 |
| 模型提议与真实执行边界 | [Issue #10](https://github.com/spojchil/MineIntent/issues/10)、[Issue #11](https://github.com/spojchil/MineIntent/issues/11)、[PR #22](https://github.com/spojchil/MineIntent/pull/22)、[PR #29](https://github.com/spojchil/MineIntent/pull/29) | 模型输出是提议，不是已发生事实；能力有 schema；动作绑定 scope、期限、取消和物理资源；迟到结果不得跨 run/world 提交；清理不抹掉已发生副作用 | 核心是**现行资产**，扩展部分是**候选资产**。旧决策信封、依赖图和完整 Action Runtime 不应恢复 |
| 事件、run 与持久会话分离 | [Issue #12](https://github.com/spojchil/MineIntent/issues/12)、[Issue #13](https://github.com/spojchil/MineIntent/issues/13)、[PR #20](https://github.com/spojchil/MineIntent/pull/20)、[PR #21](https://github.com/spojchil/MineIntent/pull/21) | raw signal、command、domain event、projection、effector 分开；命令不能冒充事件；correlation 不等于因果；重放不能重发聊天或动作；observe、collect、steer、interrupt 是并发语义 | **候选资产**。当前 JSONL journal 没有旧设计的 projection、checkpoint、upcaster 和安全 replay；主要玩家与共享活动状态机不随之继承 |
| 合法信息 Runtime 内核 | [Issue #32](https://github.com/spojchil/MineIntent/issues/32)、[Issue #53](https://github.com/spojchil/MineIntent/issues/53)、[PR #42](https://github.com/spojchil/MineIntent/pull/42)、[PR #62](https://github.com/spojchil/MineIntent/pull/62)、[Issue #63](https://github.com/spojchil/MineIntent/issues/63) | Provider 声明 schema、来源、精度、scope 和预算；Runtime 用解析值重建结果，避免未知嵌套字段泄漏；读前后复核 scope；revision 一字段一语义；有契约测试和窄 source port | **现行资产**。四个 Provider 仍经核心读取；Catalog/Help/ref/cursor 大多没有生产消费者，分页启用前须修 #63 的 scope/UI revision 绑定缺陷，否则应删除休眠路径 |
| 动作结果与事实归因 | [Issue #33](https://github.com/spojchil/MineIntent/issues/33)、[#36](https://github.com/spojchil/MineIntent/issues/36)、[#37](https://github.com/spojchil/MineIntent/issues/37)、[#38](https://github.com/spojchil/MineIntent/issues/38)、[#43](https://github.com/spojchil/MineIntent/issues/43)、[#52](https://github.com/spojchil/MineIntent/issues/52) | commanded、client-predicted、server-observed、outcome-verified 是不同阶段；破坏、掉落、拾取分开；并发效果无法唯一归因时返回 unknown；旧目标失效不能静默换目标；安全探测不能成为隐藏地图 oracle | **候选资产**。原 Grounding、Behavior、Epistemic、Action Runtime 整包已被删除；只保留局部不变量，按新能力重新设计 |
| 标准工具循环与上下文稳定性 | [Issue #39](https://github.com/spojchil/MineIntent/issues/39)、[PR #93](https://github.com/spojchil/MineIntent/pull/93) | 标准 tool call/replay 和 call-id 配对；稳定前缀加追加帧；旧帧不重渲染；finish reason、轮数和调用数有界；资源冲突作为真实失败结果而非传输错误；待发事实绑定 scope | **现行资产**。旧自定义 JSON 决策信封已删除；绝对坐标已实现，但其产品理由和身体表征仍按 W09 持续记录 |
| run、round、call 的宿主与标识 | [Issue #98](https://github.com/spojchil/MineIntent/issues/98)、[PR #103](https://github.com/spojchil/MineIntent/pull/103) | 能看到 world scope 的宿主铸造 round id，外部服务只透明回显；拒绝旧 run、跨 run 和过期 id；派生状态的寿命不得超过其上下文 | **现行资产**。最初“每轮只准一个身体动作”已撤回；PR 还发现并修复了 FakeModel handler 吞掉测试断言的问题 |
| 身体键集、物理资源与实测效果 | [PR #105](https://github.com/spojchil/MineIntent/pull/105) | 一次调用表达同时按键集；动作结果由前后姿态测量；中间层可计算按键贡献，但不得把计算值伪装成实际位移 | **现行资产**；相反键行为**需复验**。真实 Paper 中尚未测量；低层按键不是未来高级移动或 pathfinding 的永久上限 |
| 能力自带契约 | [Issue #104](https://github.com/spojchil/MineIntent/issues/104)、[PR #106](https://github.com/spojchil/MineIntent/pull/106) | 名称、说明、schema、资源和执行逻辑归同一能力；注册目录和 dispatch 同源；非法资源类型在编译期失败；模型可见但不可执行在启动时失败；依赖注入保持窄 | **现行资产**。可深度建模世界与身体，但中间层不替 AI 决定想要什么、选择谁或如何组织社会关系 |
| 视觉读取语义 | [Issue #88](https://github.com/spojchil/MineIntent/issues/88)、[Issue #94](https://github.com/spojchil/MineIntent/issues/94)、[Issue #96](https://github.com/spojchil/MineIntent/issues/96)、[PR #107](https://github.com/spojchil/MineIntent/pull/107) | 全量结果被截断时，遗漏表示 unknown；定向读取不可泄漏当前不可见坐标；增量读取必须说明 baseline、freshness 和不再报告原因；projection revision 不是缓存 token；mirror 只随源上下文收缩 | 全量 view 是**现行资产**，其他模式是**候选资产**。增量、定向读取、廉价变化唤醒和自动视口 A/B 尚未实现 |
| 到达制度与按需世界信息 | [Issue #99](https://github.com/spojchil/MineIntent/issues/99) | 区分 run 间聊天、round 内工具结果和工具后的 frame；FIFO 保留事实顺序；队列溢出显式；便宜稳定事实可推送，随世界复杂度增长的信息按需读取；帧说明“为什么现在到达” | **候选资产**。到达联合类型、主动 run、lifespan 和预算未实现；动态能力目录、第四类到达及“像人所以 30 秒”已撤回 |
| UI Context 与声音投影 | [Issue #54](https://github.com/spojchil/MineIntent/issues/54)–[#59](https://github.com/spojchil/MineIntent/issues/59)、[PR #64](https://github.com/spojchil/MineIntent/pull/64)–[#69](https://github.com/spojchil/MineIntent/pull/69) | screen session/revision、原子 projection、unknown mod screen、UTF-8 字节边界；声音使用事件时 listener pose，先去重再聚合，无位置声音不继承旧位置，TTL 不证明声源消失 | **候选资产**。多数 PR 是关闭未合并的设计稿；现行 SoundHistory 只实现了其中少量机制，不能把旧设计测试通过写成模块已实现 |
| 单文本记忆的编辑与存储纪律 | [Issue #4](https://github.com/spojchil/MineIntent/issues/4)、[PR #23](https://github.com/spojchil/MineIntent/pull/23)、[Issue #87](https://github.com/spojchil/MineIntent/issues/87)、[Issue #109](https://github.com/spojchil/MineIntent/issues/109) | “过去观察到”不等于“现在仍成立”；事实绑定世界和时间；存储层可原子写入、备份、审计和恢复；文本编辑可用逐字匹配、唯一性检查和明确字符预算 | **候选资产**。M01–M10 已排除独立档案、固定记录类型、关系图和专用管理接口；标签块和具体编辑协议仍需实验，当前结构化 FileMemoryStore 只是历史实现残留 |
| 测试的探测能力 | [Issue #89](https://github.com/spojchil/MineIntent/issues/89)、[PR #100](https://github.com/spojchil/MineIntent/pull/100)、[PR #103](https://github.com/spojchil/MineIntent/pull/103)、[PR #106](https://github.com/spojchil/MineIntent/pull/106) | fixture 来自真实契约；全称测试名覆盖边界；先证明新测试能在旧实现失败；只对静默错误热点做变异；编译期约束也是测试资产 | 方法已有现行实例，完整纪律仍是**候选资产**。当前 Python 测试仍有手写空 schema 等问题；测试是防回退棘轮，不会自动发现未知缺陷 |
| 产品层级与证据来源审计 | [Issue #108](https://github.com/spojchil/MineIntent/issues/108)、[PR #101](https://github.com/spojchil/MineIntent/pull/101)、[PR #112](https://github.com/spojchil/MineIntent/pull/112) | 把产品目的、产品假设、技术路线、风险策略和验收代理分开；区分代码产生方锚点与展示层摘要；否定式能力主张需要更强查证；明确记录后来撤回 | 分类方法已有现行实例，自动化工具仍是**候选资产**。PR 状态和多个 AI 得出相同结论都不是独立复核；该分类也不替代《产品》的逐条权威 |
| 延迟抽象与决定可追溯性 | [Issue #61](https://github.com/spojchil/MineIntent/issues/61)、[Issue #111](https://github.com/spojchil/MineIntent/issues/111) | 只有第二个真实环境、重复实现或类型污染出现时才抽象通用环境；可审计“拒绝完整合法输入”的代码点是否有产品依据 | **候选资产**。MineIntent 当前只针对 Minecraft；决定依据类型尚未实现，也不能覆盖省略型假设和所有 early return |

### 从已删除架构中保留的问题不变量

| 不变量 | 历史问题 | 复用方式 |
|---|---|---|
| 协议跟踪不等于看见 | [#24](https://github.com/spojchil/MineIntent/issues/24)、[#32](https://github.com/spojchil/MineIntent/issues/32)、[#34](https://github.com/spojchil/MineIntent/issues/34) | 任何新感知源先写清“系统收到、AI 可得、当前可见、测试真值”的层级 |
| 命令或客户端预测不等于服务器结果 | [#33](https://github.com/spojchil/MineIntent/issues/33)、[#36](https://github.com/spojchil/MineIntent/issues/36) | 新增 dig、use、attack、容器或长动作时，以真实效果和不可归因状态设计结果 |
| 身份已知不等于位置已知；未看见不等于不存在 | [#37](https://github.com/spojchil/MineIntent/issues/37)、[#38](https://github.com/spojchil/MineIntent/issues/38)、[#94](https://github.com/spojchil/MineIntent/issues/94) | 用 unknown、stale、occluded、unloaded、truncated 分开表达，禁止静默补全或换目标 |
| 世界事实必须绑定 scope、时间和 revision | [#53](https://github.com/spojchil/MineIntent/issues/53)、[#63](https://github.com/spojchil/MineIntent/issues/63)、[#96](https://github.com/spojchil/MineIntent/issues/96)、[#98](https://github.com/spojchil/MineIntent/issues/98) | 派生 ref、cursor、round、frame 和 mirror 的寿命不超过其来源；读前后检查失效 |
| 测试真值不能回流生产认知 | [#40](https://github.com/spojchil/MineIntent/issues/40)、[#45](https://github.com/spojchil/MineIntent/issues/45)、[D40](https://github.com/spojchil/MineIntent/pull/78) | 场景命令、Paper oracle 和旁观 Bot 只负责布置与断言 |
| 运行时必须如实，产品不替 AI 审查语言 | [#43](https://github.com/spojchil/MineIntent/issues/43)、[PR #29](https://github.com/spojchil/MineIntent/pull/29) | W04b/W04c 的边界是工具与中间层不得虚报；AI 得到真实信息后是否说谎属于 AI 自身行为 |

### 不应整包复活的旧方案

| 旧方案 | 为什么不能直接恢复 | 仍可留下什么 |
|---|---|---|
| 一次长期任务、获取鞘翅与 Flow DSL 作为产品核心 | 产品目标很早已变化，当前又采用标准工具调用 | 三种时间尺度、有界循环、取消和禁止任意模型代码 |
| primaryPlayer、主要伙伴控制权和输入过滤 | 与 A01 及关系自主性冲突；[14afb64 的 runtime](https://github.com/spojchil/MineIntent/blob/14afb64b8cf3f6462e03d3f88270c5c1c19c87aa/src/companion/runtime.ts#L262-L274)仍会在 journal 前丢掉其他玩家消息，[#110](https://github.com/spojchil/MineIntent/pull/110)的行为探针复现了传播链 | #110 的传播链和行为探针可作为修复与回归方法 |
| 独立玩家档案、结构化关系/承诺/好感记忆 | 与 M01–M10 的单一 AI 可编辑文本记忆冲突 | stale/current、world scope、存储层版本与恢复 |
| Grounding handle、Behavior Synthesizer、Epistemic Map、旧 Action Runtime 整套拓扑 | [#33 的关闭说明](https://github.com/spojchil/MineIntent/issues/33#issuecomment-5081940029)及后续同簇 Issue 明确说明旧落点已删除 | 上一节的事实、作用域、归因和防 oracle 不变量 |
| Claim Policy 或 Speech Scheduler 强制“诚实” | 会把运行时真实性与 AI 的社会选择混为一谈，违反 W04b | 只约束工具、状态和中间层不得制造假事实 |
| 安静真人不可区分及模拟人为错误 | #108 已纠正代理升格，N02/N03 明确否定 | 长期第三方观察作为可选研究 |
| 中心点射线作为方块可见性判据 | #82/#100 已用 0/900 对 900/900 的 oracle 证伪 | 独立 oracle、暴露面和粗到细预算方法 |
| 自定义 JSON 决策信封和 response-format 修复循环 | 标准 tool calls 已替代，旧 V1/V2 分发被删除 | schema 与示例同源、错误和取消受预算约束 |
| viewport mirror 自行“像人一样遗忘” | #88 证明这会让某些消失永久无法报告 | mirror 必须随上下文和 scope 同寿命 |
| 每轮一个身体动作、永久禁止 pathfinding、动态能力目录 | #98、#99、#104 的后续评论已逐项撤回 | 同时按键契约、合法信息与真实身体约束、中间层不决定欲望 |
| 专用外部 AI 管理者、授权体系和管理 API | A02/M04 已明确：系统外修改来自普通文件、提示词或源码，不是产品接口 | 不把诊断/test audience 误写成产品社会角色 |
| 一次性实现 17 个信息接口或提前抽象通用游戏 Runtime | 多数接口没有真实消费者；#61 要求等待第二个环境或具体重复 | strict schema/scope/budget 内核，以及“按真实触发条件抽象”的判断方法 |

### 复用顺序

1. 先指出当前《产品》的相关条目；没有条目时，问题保持未决，不能让旧设计代替维护者裁决。
2. 从旧方案中抽出一个可独立检验的命题，不以“恢复旧架构”作为工作项。
3. 重新核对 commit 中的不可变原文、后续更正、当前代码和外部一手来源；合并、关闭或 accepted 标签只说明历史状态。
4. 为命题选择窄验证：单元/变异、独立 oracle、Paper 真值或真实模型实验，并写清它不能证明什么。
5. 需要实现时新建面向当前架构的 Issue，链接旧来源；不要重开已经明确删除的整套接口。

触发式优先级：启用分页前处理 #63 或删除休眠 Cursor；新增 dig/use/attack 时恢复实际结果与 Paper
关联；新增 GUI 或深化声音时重新利用 #54–#59；新增长距离导航时重新评估 DDA、地图和 pathfinding；
实现单文本记忆时复验 #87 的编辑保护，而不是恢复旧档案层。

本轮也确认了几条历史问题在 14afb64 的代码中仍可定位：[#84](https://github.com/spojchil/MineIntent/issues/84)
的视口姿态与相对转头符号不一致、[#89](https://github.com/spojchil/MineIntent/issues/89)指出的手写空工具
schema、[#97](https://github.com/spojchil/MineIntent/issues/97)指出的实体自定义名缺失，以及
[#110](https://github.com/spojchil/MineIntent/pull/110)追踪的 primaryPlayer 输入过滤。它们是现行修复或复验
入口，不会使各自所在的整套旧架构重新成立。

常规依赖升级、翻译、语言注册、许可证和纯文档清理仍可从 GitHub 时间线查到，但没有进入本技术资产表。
早期 PR 多由同一维护者快速创建和合并，独立 review 很少：它们对“当时为何这样设计”是强证据，对“方案已经
多人验证”则不是。

## 时间线索引

下表用于快速定位演变节点；它不覆盖上方资产的证据和边界说明。

### 起点与早期实现

| 来源 | 记录了什么 | 当前查阅方式 |
|---|---|---|
| [Issue #12](https://github.com/spojchil/MineIntent/issues/12) / [PR #20](https://github.com/spojchil/MineIntent/pull/20) | 领域事件与事件日志的早期设计 | 作为历史设计阅读；当前接口看代码 |
| [Issue #13](https://github.com/spojchil/MineIntent/issues/13) / [PR #21](https://github.com/spojchil/MineIntent/pull/21) | 持续同伴、共同活动、注意模型和第一版主要玩家 | 用于追踪主要玩家假设的来源 |
| [Issue #10](https://github.com/spojchil/MineIntent/issues/10) / [PR #22](https://github.com/spojchil/MineIntent/pull/22) | 模型提议、世界结果优先和早期决策协议 | 设计来源；当前协议看代码 |
| [Issue #4](https://github.com/spojchil/MineIntent/issues/4) / [PR #23](https://github.com/spojchil/MineIntent/pull/23) | 长期记忆、档案版本和结构化证据方案 | 旧记忆方案来源 |
| [Issue #24](https://github.com/spojchil/MineIntent/issues/24) / [PR #25](https://github.com/spojchil/MineIntent/pull/25) | “客户端收到”与“AI 知道”的感知边界 | 感知问题来源 |
| [PR #26](https://github.com/spojchil/MineIntent/pull/26) / [PR #27](https://github.com/spojchil/MineIntent/pull/27) | Mineflayer Backend 生命周期和首个真实 Paper 验证 | 当前 Backend 的历史来源 |
| [PR #28](https://github.com/spojchil/MineIntent/pull/28) / [PR #29](https://github.com/spojchil/MineIntent/pull/29) / [PR #30](https://github.com/spojchil/MineIntent/pull/30) | 聊天、可取消执行和 Paper 测试的早期闭环 | 工程机制来源 |
| [Issue #17](https://github.com/spojchil/MineIntent/issues/17) / [PR #31](https://github.com/spojchil/MineIntent/pull/31) | 采木、打断、真实结果和重启记忆组成的早期纵向切片 | 原型历史 |

### 后续演变

| 来源 | 记录了什么 | 当前查阅方式 |
|---|---|---|
| [PR #42](https://github.com/spojchil/MineIntent/pull/42) / [PR #62](https://github.com/spojchil/MineIntent/pull/62) | 合法信息边界与 Information Runtime | 当前行为看代码，完整设计看历史快照 |
| [Issue #70](https://github.com/spojchil/MineIntent/issues/70) / [PR #71](https://github.com/spojchil/MineIntent/pull/71) | “安静真人/行为图灵测试”升格为高层目标 | 方向偏离节点，后续更正见 #108 |
| [PR #72](https://github.com/spojchil/MineIntent/pull/72) | 五层文档状态、登记表与大量历史恢复进入主线 | 本次文档收敛的前一结构 |
| [PR #73](https://github.com/spojchil/MineIntent/pull/73) / [PR #78](https://github.com/spojchil/MineIntent/pull/78) | D40 短动作工具循环实验 | 实验来源 |
| [PR #93](https://github.com/spojchil/MineIntent/pull/93) | 标准工具调用、稳定前缀和追加帧上下文 | 当前实现的主要来源 |
| [PR #103](https://github.com/spojchil/MineIntent/pull/103) / [PR #105](https://github.com/spojchil/MineIntent/pull/105) / [PR #106](https://github.com/spojchil/MineIntent/pull/106) / [PR #107](https://github.com/spojchil/MineIntent/pull/107) | round 宿主、键集移动、能力契约和 `view` 工具 | 当前代码演进来源 |
| [Issue #108](https://github.com/spojchil/MineIntent/issues/108) / [Issue #109](https://github.com/spojchil/MineIntent/issues/109) / [Issue #111](https://github.com/spojchil/MineIntent/issues/111) / [PR #112](https://github.com/spojchil/MineIntent/pull/112) | 产品层级纠偏、无主要玩家、决定依据审计和历史分类 | 后续更正与治理方法来源 |
| [Issue #83](https://github.com/spojchil/MineIntent/issues/83) | 状态页、架构说明和登记册腐烂问题 | 最小文档结构的诊断来源 |

## 旧路径迁移

本节覆盖 `main@46bcd4d28630421a4199f0857b973818f1569f92` 中后来删除的全部 44 份 `docs/` 文档。
链接指向不可变原文；“当前入口”只说明今天从哪里继续阅读，不继承旧文档的状态。

迁移表不能使已删除的 GitHub URL 自动跳转。需要旧 URL 本身继续可用时，只能在原路径保留薄墓碑文件；当前结构选择
集中索引，因此旧深链仍会返回 404。

### 产品

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/product-design.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/product-design.md) | [产品](../../产品.md)；未重新确认的内容只作为历史 |

### 架构、状态、接口和旧 ADR

这些旧文件不能整体并入当前架构页。当前仍成立的实现事实必须从目标代码重新核对；旧目标、旧决定和理由留在快照。

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/architecture/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/README.md) | [当前实现结构](../architecture.md) |
| [`docs/architecture/cognitive-perception.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/cognitive-perception.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/companion-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/companion-runtime.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/current-system.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/current-system.md) | [当前实现结构](../architecture.md) |
| [`docs/architecture/decision-contract-and-context.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/decision-contract-and-context.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/domain-events-and-journal.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/domain-events-and-journal.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/information-access-and-ui.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/information-access-and-ui.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/information-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/information-runtime.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/memory-model-and-profile-versioning.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/memory-model-and-profile-versioning.md) | 当前事实看[架构页](../architecture.md)，候选记忆选择看[产品](../../产品.md) |
| [`docs/architecture/minecraft-backend.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/minecraft-backend.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/architecture/target-system.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/target-system.md) | 产品看[产品](../../产品.md)，实现看[架构页](../architecture.md) |
| [`docs/architecture/ui-context.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/architecture/ui-context.md) | 当前事实看[架构页](../architecture.md)，旧设计看快照 |
| [`docs/current-status.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/current-status.md) | [当前实现结构](../architecture.md)与[运行指南](../guides/run.md) |
| [`docs/guides/model-interface.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/guides/model-interface.md) | 模型边界看[架构页](../architecture.md)，配置看[运行指南](../guides/run.md) |
| [`docs/decisions/0001-use-mineflayer-as-initial-backend.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0001-use-mineflayer-as-initial-backend.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0002-event-driven-companion-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0002-event-driven-companion-runtime.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0003-separate-mind-and-action-runtime.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0003-separate-mind-and-action-runtime.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0004-no-arbitrary-model-code.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0004-no-arbitrary-model-code.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |
| [`docs/decisions/0005-limit-mineflayer-to-protocol-driver.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/0005-limit-mineflayer-to-protocol-driver.md) | 当前事实看[架构页](../architecture.md)，旧理由看快照 |

### 旧文档治理与目录入口

| 旧路径（不可变原文） | 当前入口 |
|---|---|
| [`docs/decisions/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/README.md) | [文档入口](../README.md)与[贡献流程](../../CONTRIBUTING.md) |
| [`docs/decisions/template.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/decisions/template.md) | 无现行模板；流程见[贡献指南](../../CONTRIBUTING.md) |
| [`docs/document-register.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/document-register.md) | [文档入口](../README.md) |
| [`docs/documentation-policy.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/documentation-policy.md) | [文档入口](../README.md) |
| [`docs/guides/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/guides/README.md) | [文档入口](../README.md)，再分流至运行和验证指南 |

### 历史、研究、路线与提案

以下旧文件的当前入口都是本页；内容只以不可变快照存在。未决工作应回到 GitHub Issue，而不是继承旧提案状态。

| 旧路径（不可变原文） |
|---|
| [`docs/history/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/README.md) |
| [`docs/history/archive-2026-07-14-information/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/README.md) |
| [`docs/history/archive-2026-07-14-information/information-acceptance-matrix.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/information-acceptance-matrix.md) |
| [`docs/history/archive-2026-07-14-information/player-state-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/player-state-information.md) |
| [`docs/history/archive-2026-07-14-information/screen-and-overlay-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/screen-and-overlay-information.md) |
| [`docs/history/archive-2026-07-14-information/sound-and-lifecycle-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/sound-and-lifecycle-information.md) |
| [`docs/history/archive-2026-07-14-information/viewport-information.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/archive-2026-07-14-information/viewport-information.md) |
| [`docs/history/early-autonomous-agent-handoff.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/early-autonomous-agent-handoff.md) |
| [`docs/history/project-evolution.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/project-evolution.md) |
| [`docs/history/research-cognitive-perception.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/research-cognitive-perception.md) |
| [`docs/history/research-system-design.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/research-system-design.md) |
| [`docs/history/roadmap-v0.2-legal-information-interfaces.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/roadmap-v0.2-legal-information-interfaces.md) |
| [`docs/history/roadmap-v0.3-trustworthy-embodiment.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/history/roadmap-v0.3-trustworthy-embodiment.md) |
| [`docs/proposals/README.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/README.md) |
| [`docs/proposals/embodiment-architecture-reflection.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-architecture-reflection.md) |
| [`docs/proposals/embodiment-decision-register.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-decision-register.md) |
| [`docs/proposals/embodiment-interface-inventory.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/embodiment-interface-inventory.md) |
| [`docs/proposals/information-interfaces.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/information-interfaces.md) |
| [`docs/proposals/trustworthy-gaze.md`](https://github.com/spojchil/MineIntent/blob/46bcd4d28630421a4199f0857b973818f1569f92/docs/proposals/trustworthy-gaze.md) |

### 恢复稿到当前结构的改名

| 恢复稿路径 | 当前路径 | 恢复稿不可变原文 |
|---|---|---|
| `PRODUCT_CONSTITUTION.md` | [`产品.md`](../../产品.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/PRODUCT_CONSTITUTION.md) |
| `docs/source-index.md` | 本页 | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/source-index.md) |
| `docs/guides/companion-prototype.md` | [`docs/guides/run.md`](../guides/run.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/guides/companion-prototype.md) |
| `docs/guides/paper-integration.md` | [`docs/guides/validation.md`](../guides/validation.md) | [`fac8c654` 原文](https://github.com/spojchil/MineIntent/blob/fac8c654b223ce429659c655cf703b1eefc2953a/docs/guides/paper-integration.md) |
