# MineIntent

MineIntent 是一个把 AI 接入 Minecraft Java Edition 世界的实验项目。准确的产品定义与候选架构只见
[《产品》](./docs/产品.md)；本页只负责导航和最短启动。

## 按任务阅读

| 你要做什么 | 从这里开始 |
|---|---|
| 判断项目为什么存在、应当成为什么 | [产品](./docs/产品.md) |
| 运行当前原型 | [运行指南](./docs/guides/run.md) |
| 构建或运行 Rust 移植（进行中） | [Rust workspace 指南](./docs/guides/rust-workspace.md) |
| 运行检查或 Paper 集成验证 | [验证指南](./docs/guides/validation.md) |
| 理解当前代码如何组成 | [当前实现结构](./docs/architecture.md) |
| 贡献代码或文档 | [贡献流程](./CONTRIBUTING.md) |
| 追溯历史判断和旧文档 | [历史来源与旧路径](./docs/history/index.md) |
| 查看完整文档地图和权威等级 | [文档入口](./docs/README.md) |

## 最短启动

需要 Node.js 22+、Corepack、Python 3.9+、可连接的 Minecraft Java 1.21.1 服务器，以及支持标准工具调用的
OpenAI-compatible Chat Completions 模型接口。

```sh
corepack pnpm install --frozen-lockfile
cp .env.example .env
```

编辑 `.env` 后，在两个终端分别运行：

```sh
python3 agent-service/server.py
```

```sh
corepack pnpm start
```

Windows PowerShell 可用 `Copy-Item .env.example .env` 代替 `cp`。配置、敏感数据和排障说明见
[运行指南](./docs/guides/run.md)。

> 仓库里另有一套进行中的全 Rust 单进程实现（`crates/`，目标 Paper 26.1.2）。
> 它尚未接管运行，上面的启动方式仍是当前可用的原型；
> Rust 侧的构建与运行见 [Rust workspace 指南](./docs/guides/rust-workspace.md)。

## 许可证

本项目以 [MIT 许可证](./LICENSE)发布。
