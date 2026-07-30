# 验证当前实现

> 本页说明每项检查能提供什么证据，以及它不能证明什么。验证结果不产生产品权威。

## 快速检查

```sh
corepack pnpm check
corepack pnpm check:docs
corepack pnpm test
corepack pnpm test:agent
```

如果 `test:agent` 使用的 `python` 命令不可用，可以直接运行：

```sh
python3 -m unittest discover -s agent-service -p 'test_*.py'
```

| 命令 | 覆盖范围 | 不证明什么 |
|---|---|---|
| `corepack pnpm check` | TypeScript 类型检查 | 运行行为正确 |
| `corepack pnpm check:docs` | Markdown 本地链接、越界链接和旧仓库名检查 | 文档内容准确、完整或仍然新鲜 |
| `corepack pnpm test` | Node/TypeScript 单元与契约测试 | 真实 Minecraft 或模型体验 |
| `corepack pnpm test:agent` | Python Agent Service 单元测试 | 真实供应商兼容性或游戏闭环 |

这些检查都不会启动 Minecraft。

## 隔离的 Paper 集成测试

`test:paper:ci` 会复制专用模板，在临时世界副本中运行，然后删除副本。它仍具有破坏性前置动作；模板必须是
专用于测试、允许被完整删除的目录，不得有未授权玩家在线。

### 覆盖范围

当前场景验证：

- Minecraft Backend 连接、死亡、重生和服务端重启后的重连。
- 独立 Mineflayer 测试 Bot 的移动，以及清除控制状态后的停止。
- 测试 Bot 挖掘、方块消失和物品进入背包。
- 超时、清理、隔离副本和诊断 artifact。

它不启动 Python Agent Service，不调用真实模型，也不验证聊天、关系、记忆或当前模型工具循环。

### 本地运行

POSIX shell：

```sh
MC_JAVA=/path/to/java \
MC_SERVER_JAR=/path/to/paper.jar \
MC_SERVER_TEMPLATE=/path/to/disposable-template \
MC_EULA=true \
corepack pnpm test:paper:ci
```

PowerShell：

```powershell
$env:MC_JAVA = 'C:\path\to\java.exe'
$env:MC_SERVER_JAR = 'C:\path\to\paper.jar'
$env:MC_SERVER_TEMPLATE = 'C:\path\to\disposable-template'
$env:MC_EULA = 'true'
corepack pnpm test:paper:ci
```

可选变量包括 `MC_PORT`、`MC_USERNAME` 和 `MC_ARTIFACTS_DIR`。运行副本默认位于
`.artifacts/paper/<runId>/server`。

如果 `MC_SERVER_TEMPLATE` 中没有 `world/level.dat`，初始化流程会递归删除该模板目录后重新生成基准世界。
绝不能把它指向重要世界、仓库或通用服务器目录，也不要使用硬链接复制 region 文件。

## 旧 Windows 直连测试

```powershell
corepack pnpm test:paper
```

这条路径直接调用 `mcserver/mc.ps1`，只支持 Windows，并直接修改 `mcserver/` 管理的当前世界：它会清除测试 Bot、
传送维度并重启服务端。只能在一次性或已备份的世界中使用，最好无人在线。

可用 `MC_HOST`、`MC_PORT`、`MC_USERNAME` 和 `MC_OBSERVER_USERNAMES` 调整连接。检测到不在
`MC_OBSERVER_USERNAMES` 中的玩家时，测试会拒绝继续。无论测试前服务端是否运行，清理完成后服务端都会保持运行。

## GitHub Actions

`Paper Integration` 是手动触发的工作流，互斥运行且单次上限 15 分钟：

```sh
gh workflow run "Paper Integration"
gh run watch
```

它要求带以下标签的 self-hosted runner：

```text
self-hosted, Linux, ARM64, mineintent, paper-ci
```

仓库 Actions Variables：

| 名称 | 用途 |
|---|---|
| `PAPER_CI_NODE_BIN` | Node 与 pnpm 所在目录 |
| `PAPER_CI_NPM_REGISTRY` | runner 使用的 npm registry |
| `PAPER_CI_JAVA` | Java 21 可执行文件 |
| `PAPER_CI_JAR` | Paper 1.21.1 JAR |
| `PAPER_CI_TEMPLATE` | 专用、可删除的基准世界目录 |

运行摘要、JSONL 和服务端日志作为 artifact 保留 14 天；世界运行副本在清理阶段删除。

## 当前尚缺的验证

当前没有可重复的“当前 Agent + 真实模型 + Paper”纵向验收。绿色 Paper workflow 不能被描述为产品闭环已经通过。

未来的纵向验证至少应保存：

- 每轮模型输入；
- 工具调用；
- 动作后的观察；
- 失败与清理结果；
- 使用的代码提交和模型标识。

保存或分享证据前必须脱敏密钥、私人聊天、模型 reasoning 和世界数据。
