---
status: reference
authority: informative
implementation: partial
last_verified: 2026-07-24
applies_to: agent/main-reset-d40@c12ea9b
---

# Paper 1.21.1 集成测试

MineIntent 将普通 CI 和真实 Paper 测试分开。普通 PR CI 不启动 Minecraft；Paper Integration 通过 GitHub 手动触发，并在仓库专用的 self-hosted runner 上运行。

PR #73 保留的 Paper 场景是通用协议与测试环境裁判，不是真实模型驱动的 D40 验收。当前候选分支尚无 Paper run，因此下列内容只说明场景代码会检查什么。

## 覆盖范围

真实场景代码当前覆盖：

- Minecraft Backend 连接、死亡、重生和服务端重启后自动重连；
- 测试客户端的基础移动和清除控制状态后的取消，用于验证服务器与协议环境；
- 测试客户端装备镐、挖掘方块、确认方块消失和掉落进入背包；该场景不宣称是同伴当前生产能力；
- 所有失败、超时和正常结束路径均安全停止 Bot 与 Paper；
- setup、companion、assertion、cleanup 分阶段记录，不把控制台布置命令算作同伴能力。

场景不向生产模型传入 Paper 命令、raw 协议状态或 fixture 真值。测试客户端能挖掘或移动，也不表示 D40 生产同伴已获得该高层能力。

## shumei4 Runner

仓库 Runner 名为 `shumei4-paper-ci`，标签为：

```text
self-hosted, Linux, ARM64, mineintent, paper-ci
```

Paper 只监听 `127.0.0.1:25566`，不需要开放 Minecraft 公网端口。工作流 concurrency 固定为单并发，防止两个场景共享端口或竞争内存。

仓库 Actions Variables：

| 名称 | 用途 |
|---|---|
| `PAPER_CI_NODE_BIN` | Node 22 与 pnpm 所在目录 |
| `PAPER_CI_NPM_REGISTRY` | shumei4 可稳定访问的 npm 镜像 |
| `PAPER_CI_JAVA` | ARM64 Java 可执行文件 |
| `PAPER_CI_JAR` | 锁定的 Paper 1.21.1 build 133 JAR |
| `PAPER_CI_TEMPLATE` | 零玩家、零场景修改的基准世界目录 |

这些值是路径与镜像地址，不是密钥。Runner 注册令牌只在首次注册时使用，不保存在仓库变量中。

## 世界隔离

第一次运行会生成一次基准世界，在 Paper 报告 ready 后立即安全停止，并写入模板标记。以后每次运行：

1. 普通复制基准模板到本次 artifact 目录；
2. 删除复制来的旧日志和崩溃报告；
3. 覆盖 localhost、端口和 offline-mode 测试配置；
4. 在副本中执行场景；
5. 安全停止进程，将控制台日志移出世界目录；
6. 删除本轮世界副本，只上传 JSONL、摘要和服务端日志。

当前 Paper 工作流不需要、也不保存模型 API 密钥。真实 D40 体验需要单独配置本地模型，并手工记录模型可见信息、工具调用、延迟、token 和最终聊天。

不能用硬链接复制 region 文件，因为 Paper 的原地写入可能反向污染模板。

## 运行

合并到默认分支后：

```powershell
gh workflow run "Paper Integration"
gh run watch
```

本地 Windows 完整后端生命周期测试仍使用：

```powershell
$env:MC_OBSERVER_USERNAMES='spojchil'
npm run test:paper
```

本地测试会控制 Bot、死亡、维度和服务端重启，不应在有未授权玩家在线时运行。
