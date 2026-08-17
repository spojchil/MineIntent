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
cargo test -p world --all-targets                    # 不开 azalea feature：纯类型层
cargo test -p render -p context -p dispatch          # 纯函数与策略层
```

`world` 的 `azalea` feature 关掉时不拖 bevy 编译；`Inner` 的状态转换测试
（`machine/state.rs`）与移动判定表（`machine/movement.rs`）都能脱离 azalea 跑。
这不是可选项——它是「纯状态转换可单测」这条设计的验收方式。

> 内核（依赖键 `agent`，本体是 [midturn](https://github.com/spojchil/midturn)）
> 自 2026-08-16 起是 git 依赖，**不再是工作区成员**：`cargo test -p agent` 与
> `cargo test -p midturn` 都跑不了（后者会报「requires dev-dependencies and is
> not a member of the workspace」）。内核的测试与协议冒烟随上游仓跑；本仓验收
> 的是「钉住的那个 rev 能把下游编过、测过」。

## 纵向验收

`scripts/gate-b-vertical.sh` 跑 Paper 实服 + `companion` 全栈 + 假人作说话方。

```sh
# 二进制必须在同一次 cargo 调用里构建（分两次跑会因 feature 并集不同
# 反复重编整条 azalea 链）：
cargo build -p companion -p world --features world/azalea \
  --bin companion --example fake_player

MINEINTENT_ACCEPT_EULA=true KEY_FILE=/path/to/key ./scripts/gate-b-vertical.sh
```

⚠ **脚本绝不代替使用者同意 Mojang 条款**——`MINEINTENT_ACCEPT_EULA` 必须由
运行者自己设置，这是有意的。

**`KEY_FILE` 是必填**：组合根启动即读密钥，新栈没有脚本化模型模式，纵向验收
必然打真模型。密钥按路径注入，不进命令行、不进日志、不进版本库。

判据一律取服务端侧证据（server log / 控制台命令返回）或假人侧证据。同伴自己的
stdout 只进「交叉印证」栏，不单独构成通过条件。当前判的是：进入世界、指名聊天
唤醒并公屏回话、回话被第三方独立观察、在线名单、位移与朝向的**前后差值**在服务端
可见、怪物致死后自动重生、死后仍能被唤醒说话、SIGINT 后干净断开。

> 2026-08-17 按新栈重写。旧脚本的三处判据在新栈没有对应物，是删掉而不是改写的：
> journal（`events.jsonl`）、版本锚（`agent-context.v5` 等 model-io 请求原文）、
> `respawn` 工具（新栈保留 azalea 自动重生，死亡恢复不经模型）。理由写在脚本
> 文件头。

## 当前尚缺的验证

1. **纵向验收脚本尚未实跑于新栈**。脚本已按 `companion` 重写并验过环境校验、
   报告落盘与解析逻辑，但完整一趟需要 Paper jar、JDK、真模型密钥与运行者
   自己接受 EULA——**尚无新栈下的实跑记录**。
2. **轮末帧（增量视口）无自动覆盖**。收集→`Passive` 投递这条链在组合根装配，
   单测覆盖不到；脚本里只作交叉印证（空 diff 不投帧是设计，故「没有帧」不判失败）。
3. **屏与容器只有机器级探针**。`crafting_probe` / `open_probe` / `swap_probe`
   要手工准备世界与物品，不在脚本里，也不在 CI。

未来的纵向验证至少应保存：每次模型请求的消息列表、工具调用、调用后采样的
观察、失败与清理结果、使用的提交与模型标识。

未来的纵向验证至少应保存：每次模型请求的消息列表、工具调用、调用后采样的
观察、失败与清理结果、使用的提交与模型标识。

保存或分享证据前必须脱敏密钥、私人聊天、模型 reasoning 和世界数据。
