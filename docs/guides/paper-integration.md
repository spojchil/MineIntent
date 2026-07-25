---
status: reference
authority: informative
implementation: partial
last_verified: 2026-07-23
applies_to: codex/trustworthy-passive-context@57d438e
---

# Paper 1.21.1 集成测试

普通 PR CI 运行单元和契约测试，不启动 Minecraft。真实 Paper 测试通过手动 GitHub Actions 工作流在仓库专用的 self-hosted runner 上执行。

## 覆盖范围

下面描述**场景代码会检查什么**，不等于当前分支已经在线跑通验收——最新实验分支仍没有 Actions 或 Paper run：

- Minecraft Backend 连接、死亡、重生和服务端重启后的自动重连
- 测试客户端移动，以及清除控制状态后的停止
- 装备镐、挖掘方块、确认方块消失和掉落进入背包
- 同伴启动、共同收集木材、暂停、恢复、危险处理和返回活动起点
- 共同经历写入记忆，并在同伴重启后被检索
- 失败、超时和正常结束时清理 Bot 与 Paper 进程

这些场景使用确定性模型替身，不需要模型 API 密钥，也不能替代真人与真实模型体验测试。

## Self-hosted runner

工作流需要带以下标签的 runner：

```text
self-hosted, Linux, ARM64, mineintent, paper-ci
```

仓库 Actions Variables：

| 名称 | 用途 |
|---|---|
| `PAPER_CI_NODE_BIN` | Node 22 与 pnpm 所在目录 |
| `PAPER_CI_NPM_REGISTRY` | runner 使用的 npm 镜像 |
| `PAPER_CI_JAVA` | Java 21 可执行文件 |
| `PAPER_CI_JAR` | 锁定的 Paper 1.21.1 JAR |
| `PAPER_CI_TEMPLATE` | 基准世界目录 |

Paper 只监听 `127.0.0.1:25566`。工作流保持单并发，避免端口和内存冲突。

## 世界隔离

首次运行生成基准世界。以后每次运行都会：

1. 复制基准世界到本轮 artifact 目录。
2. 清除复制来的旧日志与崩溃报告。
3. 写入仅用于本地测试的端口和 offline-mode 配置。
4. 在副本中执行场景。
5. 停止进程并保存 JSONL、摘要和服务端日志。
6. 删除本轮世界副本。

不要用硬链接复制 region 文件，否则 Paper 的写入可能污染基准世界。

## 运行

在 GitHub 上手动启动：

```powershell
gh workflow run "Paper Integration"
gh run watch
```

本地 Windows Backend 生命周期测试：

```powershell
$env:MC_OBSERVER_USERNAMES='spojchil'
pnpm test:paper
```

测试会控制 Bot、杀死实体、切换维度并重启服务端。不要在有未授权玩家或重要世界数据的服务器上运行。
