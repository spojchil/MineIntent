# 验证当前实现

> 本页说明每项检查能提供什么证据，以及**它不能证明什么**。验证结果不产生产品权威。
>
> TypeScript 原型的验证方式（pnpm / Paper 集成工作流）随那条线留在 `main`，
> 说明见[历史](../history/run-typescript.md)。

## 快速检查

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
```

| 命令 | 覆盖范围 | **不证明什么** |
|---|---|---|
| `cargo fmt --all --check` | 格式一致 | 任何行为 |
| `cargo clippy … -D warnings` | 静态检查，无整体放行的 lint | 逻辑正确 |
| `cargo test --workspace` | 单元与契约测试 | 真实 Minecraft 或真实模型下的行为 |

**这些检查都不会启动 Minecraft，也不会调用真实模型。**

### 内核必须能脱离 azalea 构建

```sh
cargo test -p agent --all-targets                    # 零项目依赖的循环内核
cargo test -p world --all-targets                    # 不开 azalea feature：纯类型层
cargo test -p render -p context -p dispatch          # 纯函数与策略层
```

`world` 的 `azalea` feature 关掉时不拖 bevy 编译；`Inner` 的状态转换测试
（`machine/state.rs`）与移动判定表（`machine/movement.rs`）都能脱离 azalea 跑。
这不是可选项——它是「纯状态转换可单测」这条设计的验收方式。

## 真实模型端点冒烟

`agent` 的 `protocol-smoke` 用**同一套场景**打三个真实服务商入口，验证三套 wire
协议行为一致。需要密钥，不进 CI。

```sh
export MODEL_API_KEY="$(cat /path/to/key)"
export MODEL_NAME=... MODEL_CHAT_ENDPOINT=... MODEL_RESPONSES_ENDPOINT=... MODEL_ANTHROPIC_ENDPOINT=...
cargo run -p agent --example protocol_smoke --features all-adapters
```

它证明协议适配器能与真实端点往返；**不证明**同伴在游戏里的行为。

## 纵向验收

`scripts/gate-b-vertical.sh` 跑 Paper 实服 + 全栈 + 假人作说话方。

```sh
MINEINTENT_ACCEPT_EULA=true ./scripts/gate-b-vertical.sh
```

⚠ **脚本绝不代替使用者同意 Mojang 条款**——`MINEINTENT_ACCEPT_EULA` 必须由
运行者自己设置，这是有意的。

> 该脚本写于旧栈时期，接的是已删除的 `mineintent-app`。**当前不可用**，
> 需按 `companion` 的配置重写后才能再作为验收手段。

## 当前尚缺的验证

1. **纵向验收脚本失效**（见上）。当前只有手工实盘：进服 / 对话 / `remember`
   落盘 / 重启记忆延续 / 受伤唤醒 / 死亡复活后应答 / `go_to` 到达汇报，
   逐条记在提交说明里，没有可重复的脚本。
2. **`companion` 零测试**。380 行组合根里有唤醒重试、自激防护（按 UUID 比对）
   与聊天游标——纵切逻辑没有任何自动覆盖。
3. **观测面未经生产使用**。`agent` 的 `Observer` / `StreamObserver`
   在本仓库零处接线，只有单元测试走过。

未来的纵向验证至少应保存：每次模型请求的消息列表、工具调用、调用后采样的
观察、失败与清理结果、使用的提交与模型标识。

保存或分享证据前必须脱敏密钥、私人聊天、模型 reasoning 和世界数据。
