# MineIntent

MineIntent 是一个面向 Minecraft Java Edition 的 AI 同伴项目。

它不是只接受命令并交付结果的任务机器人，而是希望成为能够进入同一个世界，与玩家自然交流、共同生活、合作、探索、建造和冒险的游戏伙伴。

长期高层目标之一，是让它在经服务器管理方许可的外部原版生存服务器中，仅凭普通玩家能够获得的信息和合法游戏行为长期参与；其他玩家仅从游戏内观察，难以可靠区分它与一名较安静、反应较慢的普通玩家。这个目标强调行为可信和无作弊，不以虚假身份声明、规避反作弊或绕过服务器规则为手段。

## 当前状态

项目仍处于开发实验期，尚不是可供长期日常游玩的成品。

当前 D40 实验只建立一个很小的动作—观察闭环：玩家通过聊天与同伴交互，模型可以依据当前第一人称视野调用相对转头或短时按键移动，然后从动作后的新观察继续判断。模型不会获得世界坐标、实体 ID 或目标 ref。

它还没有导航、跳跃、挖掘、战斗、GUI 或长期自主能力。当前自动测试验证的是协议、取消、资源释放和动作后观察；真人 + Paper + 真实模型的 D40 场景仍需重复运行。更精确的边界见[当前项目状态](./docs/current-status.md)。

## 核心原则

1. AI 是可协商的队友，不是绝对服从的工具，也不与玩家争夺游戏主导权。
2. 自主性来自同伴档案、关系、共同经历和当前情境，而不是一组机械性格参数。
3. 语言、意图、行动、真实结果和记忆必须保持一致。
4. 游戏动作只能通过可取消、带超时、可验证的能力执行。
5. 记忆保留来源和证据，并允许纠正。
6. 协议客户端获得的信息不自动等于同伴角色应该知道的信息。
7. 同伴的知识、身体过程和游戏内表现应处于普通原版玩家的可能范围内。

## 文档

- [文档总入口与真相优先级](./docs/README.md)
- [当前项目状态](./docs/current-status.md)
- [产品设计](./docs/product-design.md)
- [旧可信注视实验实况](./docs/architecture/current-system.md)
- [架构决策记录](./docs/decisions/README.md)
- [开放提案与具身决策登记册](./docs/proposals/README.md)
- [项目演进史与早期档案](./docs/history/project-evolution.md)
- [贡献与文档治理](./CONTRIBUTING.md)

## 技术基线

- Node.js 22 或更高版本。
- TypeScript、ESM、pnpm。
- Python 3，用于当前模型服务。
- Mineflayer 作为第一版 Minecraft Backend。
- Minecraft Java Edition 1.21.1 测试服务器。

技术基线会通过原型验证继续评估，不代表所有未来实现已经锁定。

## 本地验证

安装依赖：

```powershell
pnpm install
```

运行类型检查与文档检查：

```powershell
pnpm check
pnpm check:docs
```

运行自动单元和契约测试：

```powershell
pnpm test
```

## 运行 D40 原型

复制配置样例，填写主要玩家名、模型服务地址、模型名和密钥，并为 Node 与 Python 之间的本地调用生成一个独立的 `MINEINTENT_AGENT_SERVICE_TOKEN`：

```powershell
Copy-Item .env.example .env
```

确保目标 Minecraft Java Edition 1.21.1 服务器已经运行，再启动同伴决策服务和 Node 进程（两个独立进程）：

```powershell
python agent-service/server.py
```

```powershell
pnpm start
```

主要玩家发给同伴的聊天按顺序进入模型；runtime 不识别“停下”等控制口令，也不生成固定台词。是否照做、拒绝或先完成更安全的动作，由模型结合当前观察决定。只有断线、换世界/维度、应用停止或请求超时等客观失效条件会取消模型轮次和身体输入。默认只读调试状态位于 `http://127.0.0.1:3211/v1/state`。

模型密钥只从本地 `.env` 读取；`.env` 和运行数据目录 `.mineintent/` 已被 Git 忽略。Agent Service 的接口和取消语义见 [agent-service/README.md](./agent-service/README.md)，完整运行说明见 [同伴原型](./docs/guides/companion-prototype.md)。

在 `localhost:25565` 已运行受管理的 Paper 1.21.1 离线验证服务器时，可执行 Backend 生命周期集成验收：

```powershell
pnpm test:paper
```

该测试会杀死并传送测试 Bot，还会安全停止并重启本地 Paper。默认检测到其他玩家在线时拒绝执行。明确作为观察者上线时，可通过环境变量放行用户名：

```powershell
$env:MC_HOST = "localhost"
$env:MC_PORT = "25565"
$env:MC_USERNAME = "MineIntentBot"
$env:MC_OBSERVER_USERNAMES = "spojchil"
pnpm test:paper
```

## 开发流程

项目采用 GitHub Flow：非琐碎工作通过 Issue 说明目标，在短期分支中实现，由 Pull Request 关联 Issue 并经过自动检查后合并。

硬规则只有一条：`main` 只通过合并 PR 变更。架构或产品方向的结论必须以一个合并的 PR 落地，`main` 上留下的 `(#NN)` 就是它的引用地址。详细规则见 [CONTRIBUTING.md](./CONTRIBUTING.md) 与[文档治理规则](./docs/documentation-policy.md)。

## 许可证

项目当前未授予开源许可证。在正式确定分发方式前，代码保留所有权利。
