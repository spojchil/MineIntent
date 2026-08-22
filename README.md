# MineIntent

MineIntent 是一个把 AI 接入 Minecraft Java Edition 世界的实验项目。准确的产品定义与候选架构只见
[《产品》](./docs/产品.md)；本页只负责导航和最短启动。

## 按任务阅读

| 你要做什么 | 从这里开始 |
|---|---|
| 判断项目为什么存在、应当成为什么 | [产品](./docs/产品.md) |
| 构建与运行 | [workspace 指南](./docs/guides/rust-workspace.md) |
| 运行检查或 Paper 集成验证 | [验证指南](./docs/guides/validation.md) |
| 理解当前代码如何组成 | [当前实现结构](./docs/architecture.md) |
| 贡献代码或文档 | [贡献流程](./CONTRIBUTING.md) |
| 查看完整文档地图和权威等级 | [文档入口](./docs/README.md) |

## 最短启动

需要 Rust nightly 工具链（`rust-toolchain.toml` 已钉，azalea 的 bevy 依赖要求）、
可连接的 **Paper 26.1.2**（协议 775）服务器，以及一个 OpenAI 兼容的模型接口。

```sh
export MINEINTENT_MODEL_API_KEY_FILE=/path/to/api-key   # 密钥只走文件路径
cargo run -p companion
```

默认连 `127.0.0.1:25565`，用户名 `companion`。`Ctrl+C` 停机。
完整配置、模型接入与已知越界见 [workspace 指南](./docs/guides/rust-workspace.md)。

## 实现线

本仓库只有一条实现线：**Rust 单进程**——`crates/`，`companion` 是唯一可执行，
目标 Paper 26.1.2（协议 775）。

历史上的 TypeScript + Python 原型（目标 MC 1.21.1）从未与本线合并，已整体退役：
代码快照见 tag `typescript-prototype`（`e9b18c4`），也可检出 `archive/typescript-prototype`
分支查看。

## 许可证

本项目以 [MIT 许可证](./LICENSE)发布。
