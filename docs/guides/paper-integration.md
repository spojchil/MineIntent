# Paper 1.21.1 集成测试

Paper 测试都具有破坏性，但两条路径的隔离方式不同：`test:paper:ci` 使用专用模板和临时副本，`test:paper`
直接修改 `mcserver/` 管理的现有世界。只能使用一次性或已备份的世界，且不得有未授权玩家在线。

## 当前覆盖

[`paper-ci-integration.ts`](../../src/integration/paper-ci-integration.ts) 当前只验证可复用的 Paper/协议边界：

- Minecraft Backend 连接、死亡、重生和服务端重启后的重连。
- 独立 Mineflayer 测试 Bot 的移动与清除控制状态后的停止。
- 独立测试 Bot 的挖掘、方块消失和背包拾取。
- 超时、清理、隔离世界副本和诊断 artifact。

它不启动当前 Python Agent，不调用真实模型，也不验证同伴聊天、关注、记忆或工具循环。源码明确把 Agent 行为留给
单独的 live experiment；绿色 Paper 工作流不能被描述为当前产品闭环已经通过。

## GitHub Actions

`Paper Integration` 是手动工作流，需要带以下标签的 self-hosted runner：

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

触发并观察：

```sh
gh workflow run "Paper Integration"
gh run watch
```

每轮会复制基准世界，在副本中运行，保存摘要、JSONL 和服务端日志，然后删除世界副本。不要用硬链接复制 region 文件。

## 本地路径

跨平台场景可以在设置以下环境变量后运行：

```sh
MC_JAVA=/path/to/java \
MC_SERVER_JAR=/path/to/paper.jar \
MC_SERVER_TEMPLATE=/path/to/template \
MC_EULA=true \
corepack pnpm test:paper:ci
```

这会创建和删除 `.artifacts/paper/` 下的运行副本。`MC_SERVER_TEMPLATE` 必须指向专用于测试、允许被完整
删除的目录；如果其中没有 `world/level.dat`，初始化流程会先递归删除该目录，再生成新的基准世界。绝不能
把它指向重要世界、仓库或通用服务器目录。

`corepack pnpm test:paper` 是另一条旧的本地管理路径，源码调用 `powershell.exe` 和 `mcserver/mc.ps1`，
因此当前只支持 Windows。它直接操作现有服务器世界，会杀死 Bot、传送维度并重启服务端；发现未列入
`MC_OBSERVER_USERNAMES` 的玩家时会拒绝运行。无论运行前服务器是否在线，清理结束时都会使服务器保持运行。

## 尚缺的验收

当前主线仍需要一条可重复的“当前 Agent + 真实模型 + Paper”纵向场景。建立该场景时必须保存逐轮输入、工具调用、
动作后观察和清理结果，同时脱敏密钥、私人聊天和世界数据。
