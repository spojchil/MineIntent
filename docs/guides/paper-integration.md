---
status: reference
authority: informative
implementation: partial
last_verified: 2026-07-24
---

# Paper 1.21.1 集成测试

普通 PR CI 运行类型检查和自动测试，不默认启动 Minecraft。真实 Paper 场景需要受管理的本地服务端或仓库专用的 self-hosted runner。

## 测试边界

Paper 场景可以用于：

- 验证 Mineflayer 连接、死亡、重生和服务端重启后的生命周期；
- 观察玩家实际可见的转头、移动、挖掘和交互过程；
- 比较模型可见信息、动作结果与服务端测试裁判；
- 验证取消、超时、玩家打断和清理路径；
- 保存有界日志，用于分析延迟、调用次数和失败原因。

测试中的 Paper 命令、raw 协议状态和 fixture 真值只能用于布置与断言，不能进入同伴的生产认知输入。测试能执行某个动作，也不表示该动作已经成为生产同伴能力。

当前 D40 场景仍在实验中；在真实模型与 Paper 运行记录产生以前，不把场景代码或单元测试称为端到端验收通过。

## Self-hosted runner

建议使用仓库专用、单并发的 runner，并让 Paper 只监听 loopback 测试端口。通用标签可以是：

~~~text
self-hosted, Linux, mineintent, paper-ci
~~~

当前工作流使用以下仓库 Actions Variables：

| 名称 | 用途 |
|---|---|
| PAPER_CI_NODE_BIN | Node 与 pnpm 所在目录 |
| PAPER_CI_NPM_REGISTRY | runner 可稳定访问的 npm registry |
| PAPER_CI_JAVA | Java 21 可执行文件 |
| PAPER_CI_JAR | 锁定的 Paper 1.21.1 JAR |
| PAPER_CI_TEMPLATE | 零玩家、零场景修改的基准世界目录 |

这些值不应包含密钥。Runner 注册令牌只用于注册过程，不保存在普通仓库变量中。

## 世界隔离

建议先生成并安全关闭一份基准世界。每次测试：

1. 把模板复制到本轮 artifact 目录；
2. 删除副本中的旧日志和崩溃报告；
3. 覆盖 loopback、测试端口和测试身份配置；
4. 运行 setup、companion、assertion、cleanup 阶段；
5. 安全停止 Paper，并把日志移出世界副本；
6. 删除本轮世界，只上传必要且已检查的诊断文件。

不要用硬链接复制 region 文件，Paper 的原地写入可能反向污染模板。不要上传玩家数据、认证文件、模型密钥或未经检查的聊天正文。

## 运行

手动触发仓库工作流：

~~~powershell
gh workflow run "Paper Integration"
gh run watch
~~~

在本机让测试程序复制模板、启动并管理临时 Paper 服务端：

~~~powershell
$env:MC_JAVA = "C:\path\to\java.exe"
$env:MC_SERVER_JAR = "C:\path\to\paper-1.21.1.jar"
$env:MC_SERVER_TEMPLATE = "C:\path\to\paper-template"
$env:MC_EULA = "true"
$env:MC_PORT = "25566" # 可选，默认 25566
$env:MC_USERNAME = "MineIntentBotCI" # 可选
pnpm test:paper:ci
~~~

本地测试可能控制 Bot、传送或杀死测试实体，并停止或重启服务端。不要对包含重要存档或未授权在线玩家的服务器运行。
