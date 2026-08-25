# workspace 指南（MC 26.1）

MineIntent 的全 Rust 单进程实现。目标服务端 **Paper 26.1.2 / 协议号 775**。
结构见[架构说明](../architecture.md)。

| crate | 职责 |
|---|---|
| [`crates/world`](../../crates/world) | 接入 azalea、tick 快照、视口内核、方块读取 |
| [`crates/render`](../../crates/render) | 快照 → 模型可读文字（全纯函数） |
| [`crates/perception`](../../crates/perception) | 主动看：`scan` |
| [`crates/screens`](../../crates/screens) | 界面互斥域：`chat_box` / `inventory` / `container` |
| [`crates/motion`](../../crates/motion) / [`hand`](../../crates/hand) | 位移朝向 / 攻挖用 |
| [`crates/memory`](../../crates/memory) | 单文件长期记忆：`remember` |
| [`crates/presence`](../../crates/presence) | 生死去留：`presence`（当前只有 `respawn`） |
| [`crates/context`](../../crates/context) | 提示装配与压缩策略 |
| [`crates/dispatch`](../../crates/dispatch) | 工具编排与互斥域账本 |
| [`crates/companion`](../../crates/companion) | 组合根，唯一可执行 |

模型—工具循环内核**不在本仓**：它是独立仓库
[midturn](https://github.com/spojchil/midturn) 的 git 依赖，依赖键仍是 `agent`
（源码里写 `agent::`）。rev 钉在根 `Cargo.toml`，见[架构说明](../architecture.md) §5。

## 构建与运行

```bash
cargo build --workspace
cargo test --workspace --all-targets
cargo run -p companion
```

**工具链**：`rust-toolchain.toml` 钉 nightly，理由是 azalea 0.16 及其 bevy
依赖需要 nightly 特性。除 `world` 之外的 crate 本身不需要——内核在 stable
上就能构建（midturn 仓库的 CI 跑的就是 stable）。

只有走 rustup 的 `cargo` 才认 `rust-toolchain.toml`。若系统上另装了
Homebrew 的 stable Rust 且它排在 `PATH` 前面，构建会报「Azalea currently
requires nightly Rust」，此时显式指到 nightly：

```bash
export PATH="$HOME/.rustup/toolchains/nightly-x86_64-apple-darwin/bin:$PATH"
```

## 配置

全部走环境变量。**密钥只从文件路径读**，不进命令行、不进日志。

| 变量 | 默认 | 说明 |
|---|---|---|
| `MINEINTENT_HOST` / `MINEINTENT_PORT` | `127.0.0.1` / `25565` | 目标服务器 |
| `MINEINTENT_USERNAME` | `companion` | 离线身份（本版本只支持 offline） |
| `MINEINTENT_MEMORY_FILE` | `companion-memory.md` | 长期记忆文件 |
| `MINEINTENT_PERSONA_FILE` | 内置占位 | 人设全文；Q01 未裁前是占位文本 |
| `MINEINTENT_MODEL_API_KEY_FILE` | 无 | **推荐**：密钥文件路径 |
| `MODEL_API_KEY` | 无 | 退路：密钥本身（会进 shell 历史，不推荐） |
| `MODEL_ENDPOINT` | DeepSeek chat/completions | 完整 endpoint，含协议路径 |
| `MODEL_NAME` | `deepseek-chat` | 模型名 |

```bash
MINEINTENT_MODEL_API_KEY_FILE=/path/to/key cargo run -p companion
```

`Ctrl+C` 停机：先收尾会话（30 秒上限），再停世界。

## 模型接入

按**协议形状**分层，不按供应商。`agent` 的 adapters 提供三套：

| feature | 协议 |
|---|---|
| `openai` | OpenAI Chat Completions + Responses |
| `anthropic` | Anthropic Messages |
| `all-adapters` | 以上全部 |

组合根当前用 `Protocol::openai_chat()`，DeepSeek 只是当前用这个形状的一家。
远端默认要求 HTTPS（明文只自动放行 loopback），wire 日志默认关闭，
开启后只记 body、从不记 header。

三套协议的一致性由内核自己的 `protocol-smoke` 示例打真实端点验证。内核已是
git 依赖、不再是工作区成员，那个示例**跑不到本仓**（`cargo run -p agent` 会报
「did not match any packages」）——要跑得去 [midturn](https://github.com/spojchil/midturn)
仓库里跑。

## 唤醒

当前是**脚手架**，不是判据：组合根只做「别人对我说话就醒」，外加受伤与
移动任务终局。正式判据未裁，待决项见 issue #135。

游标用单调 `seq` 而非 tick——同一游戏刻内可能有多条事实，要「恰好一次」
消费必须用 seq。启动前的历史存量不消费。

## 事实来源边界

`FactSource` 三分：

- `Commanded`：我们下令产生的；
- `ClientPredicted`：客户端本地推导/配音，**不当作服务端事实**；
- `ServerObserved`：服务端明示。

伤害窗由 `ClientboundSetHealth` 包驱动，不做每 tick 采样——自动重生会把
死亡瞬间的 `14→0→20` 压进一个 tick，采样会整个错过 0（实测发生）。

## 死亡

**自动重生已关**（2026-08-17）：死亡是持续状态，起不起来由模型自己用
`presence` 工具决定。附带效果是 `SelfState.alive` 终于采得到了——自动重生
在时那个 false 几乎必然被上面那个单 tick 压缩吞掉。

死亡期间的动作面按原版收窄（`dispatch` 的 `LifeGate`，26.1.2 客户端字节码
考证）：

| | 死亡期间 | 依据 |
|---|---|---|
| 看世界、听声音 | **可以**（`Free` 类照常） | 死亡屏不暂停：`DeathScreen.isPauseScreen()` 恒 false，且多人下 `Minecraft.pause` 的第一道闸 `hasSingleplayerServer()` 本就为 false |
| 收到别人说话 | **可以**（唤醒与帧照常走） | 聊天 HUD 归 `Gui` 渲染，不经 screen |
| 说话、移动、动手、开屏 | **不可以**（`Body` 类全拦） | `handleKeybinds()` 只在 `screen == null` 时调用，死亡屏是非空 screen |
| 复活 | **可以**（`Vital` 类不受闸门压制） | 归进 `Body` 就没有出路了 |

自动重连与资源包接受保持关闭。下线/上线尚未实现——接入模块还是一次性的
（`Module::start` 起线程、`stop` 合流即终，`EPOCH` 是硬常量），且离线期间
没有 tick，唤醒循环整个停摆。

## 视口

`scan` 不带参数是环视当前朝向的整个视野（视锥 + 遮挡）；带 `at` 是定向确认
指定坐标，最多 16 个唯一坐标，逐坐标返回可见或闭合的不可见原因。

未加载方块让相关可见性射线**保守失败**，不会被当作空气。

投影是纯 CPU 重活，组合根放 `spawn_blocking`，不占异步线程。

## 已知越界

寻路器在**整个已加载世界模型**上做 A\*，隐含泄露同伴未看见地形的可通行性，
越过视口纪律。收敛方案随高级移动打磨裁定。
