# MineIntent

当前方向草案：MineIntent 让一个具身 AI 作为持续参与者进入 Minecraft Java Edition 世界。项目负责建立真实、充分且不预设
玩家特权的感知、记忆、行动和关系条件；具体关注、关系、行动与冲突处理由 AI 根据经历和情境选择。
合作、陪伴、拒绝、疏远、冲突或独处都可能发生，不是系统预先保证或优化的结果。

产品根目的与硬边界正在通过[《产品宪法》草案](./PRODUCT_CONSTITUTION.md)重新确认。
早期材料、后续纠正及其当前地位见[来源索引](./docs/source-index.md)。

## 当前实现

以下只描述 `main@46bcd4d` 的代码事实，不代表最终产品决定：

- Node/TypeScript 进程连接 Minecraft 1.21.1，Python Agent Service 调用支持标准工具调用的
  OpenAI-compatible Chat Completions 模型接口。
- 当前模型工具为 `look_relative`、`move_input`、`view`、`say` 和 `remember`。
- 转头与移动是有界、可取消的身体输入；动作后会返回测得结果和新的观察。
- 当前被配置为 `MINEINTENT_PRIMARY_PLAYER` 的玩家每条公屏消息都会唤醒模型，其他玩家的消息在写入日志前
  就被丢弃。系统因此在 AI 判断前取消了其他关系可能；这是与产品宪法草案拟议原则及
  [Issue #109](https://github.com/spojchil/MineIntent/issues/109) 候选方向不一致的已知实现差异，不是产品规则。
- 当前记忆仍是结构化 JSON 记录；候选方向是由 AI 编辑、直接进入提示词的文本记忆。
- 尚无导航、跳跃、挖掘、战斗、GUI、后台主动行为或完整长期生存能力。

当前主线尚没有 Paper + 当前 Agent + 真实模型的可重复端到端验收。

## 快速开始

要求：Node.js 22+、Corepack、Python 3.9+，以及一台可连接的 Minecraft Java 1.21.1 服务器。
只有在本机运行 Paper 或 Paper 集成测试时才需要 Java 21 和 Paper JAR。

```sh
corepack pnpm install --frozen-lockfile
cp .env.example .env
```

编辑 `.env`。当前过渡实现仍要求填写 `MINEINTENT_PRIMARY_PLAYER`，并需要一个独立的本地服务令牌：

```sh
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

在两个终端分别启动：

```sh
python3 agent-service/server.py
```

```sh
corepack pnpm start
```

只读调试状态默认位于 `http://127.0.0.1:3211/v1/state`。密钥只应保存在未提交的 `.env` 中。

完整配置和故障排查见[运行指南](./docs/guides/companion-prototype.md)。

## 验证

```sh
corepack pnpm check
corepack pnpm check:docs
corepack pnpm test
python3 -m unittest discover -s agent-service -p 'test_*.py'
```

真实 Paper 测试具有破坏性前置动作，只能在隔离测试世界运行；见 [Paper 集成指南](./docs/guides/paper-integration.md)。

## 文档

- [产品宪法草案](./PRODUCT_CONSTITUTION.md)
- [运行指南](./docs/guides/companion-prototype.md)
- [Paper 集成指南](./docs/guides/paper-integration.md)
- [历史来源索引](./docs/source-index.md)
- [贡献规范](./CONTRIBUTING.md)

## 许可证

本项目以 [MIT 许可证](./LICENSE)发布。
