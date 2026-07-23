# MineIntent

MineIntent 是一个面向 Minecraft Java Edition 的 AI 同伴项目。

它的目标不是成为只接受命令并交付结果的任务机器人，而是进入同一个世界，与玩家自然交流、共同生活、合作、探索、建造和冒险，缓解独自游玩时缺少朋友陪伴的问题。

长期目标之一，是让同伴在服务器管理方许可的原版生存服务器中，只使用普通玩家可获得的信息和合法游戏行为参与游戏；其他玩家仅从游戏内长期观察，难以可靠区分它与一名较安静、反应较慢、技术普通的人类玩家。这个目标不授权规避反作弊、隐瞒部署身份或违反服务器规则。

## 当前状态

项目仍处于开发实验期，不是可供长期日常游玩的成品。

当前工作集中在 [D40 look + move 端到端实验](./docs/d40-experiment.md)：让模型根据当前视野相对转头，再通过真实按键语义移动，并从动作后的新观察继续判断。它用于暴露延迟、费用、打断、碰撞和幻觉纠正等真实问题，不代表最终架构已经确定。

已确认的能力、实验边界和首轮问题见[当前项目状态](./docs/current-status.md)。仓库不再用大量未来设计文档替尚不存在的实现背书；当前行为以代码、测试和真实运行结果为准。

## 核心原则

1. AI 是可协商的队友，不是绝对服从的工具，也不与玩家争夺游戏主导权。
2. 语言、行动、真实结果和记忆应保持一致；模型声称不能替代世界事实。
3. 同伴只能依据普通玩家可合法获得的信息行动。
4. 游戏行为必须经过其他玩家可观察的正常身体过程，并能被暂停或停止。
5. 先用小型纵向实验认识问题，再决定值得长期保留的机制。

## 文档

- [文档入口](./docs/README.md)
- [产品设计](./docs/product-design.md)
- [当前项目状态](./docs/current-status.md)
- [D40 实验](./docs/d40-experiment.md)
- [架构决策记录](./docs/decisions/README.md)
- [Paper 集成测试](./docs/guides/paper-integration.md)
- [贡献规范](./CONTRIBUTING.md)

## 技术基线

- Node.js 22 或更高版本
- TypeScript、ESM、pnpm
- Python 3，用于当前模型服务
- Mineflayer 作为第一版 Minecraft Backend
- Minecraft Java Edition / Paper 1.21.1 测试环境

这些是当前实现选择，不是对所有未来后端和运行形态的承诺。

## 本地运行

安装依赖并复制配置：

~~~powershell
pnpm install
Copy-Item .env.example .env
~~~

在 .env 中填写玩家名、模型服务地址、模型名和密钥，并为 Node 与本地 Agent Service 配置同一个独立随机令牌 `MINEINTENT_AGENT_SERVICE_TOKEN`（至少 32 个字符，不要复用模型密钥）。不要提交 .env、认证目录、私人聊天或世界存档。

先启动模型服务：

~~~powershell
python agent-service/server.py
~~~

再在另一个终端启动 MineIntent：

~~~powershell
pnpm start
~~~

同伴通过游戏聊天与玩家交流。当前模型服务与工具循环直接由 [agent-service/server.py](./agent-service/server.py) 及相邻测试定义。

## 验证

~~~powershell
pnpm check
pnpm test
~~~

在仓库创建的临时 Paper 1.21.1 世界中运行通用生命周期与协议测试：

~~~powershell
$env:MC_JAVA = "C:\path\to\java.exe"
$env:MC_SERVER_JAR = "C:\path\to\paper-1.21.1.jar"
$env:MC_SERVER_TEMPLATE = "C:\path\to\paper-template"
$env:MC_EULA = "true"
pnpm test:paper:ci
~~~

该测试会控制测试 Bot，并可能停止或重启 Paper。不要在存在未授权玩家或重要世界存档时运行。更多说明见 [Paper 集成测试](./docs/guides/paper-integration.md)。

## 开发流程

项目使用 GitHub Flow。非琐碎改动在短期分支中完成，经 Pull Request 和自动检查后进入 main。设计讨论可以从实验开始；合并说明必须如实记录实际验证结果和仍未解决的问题。

## 许可证

项目当前未授予开源许可证。在正式确定分发方式前，代码保留所有权利。
