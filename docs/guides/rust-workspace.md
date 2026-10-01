# workspace 指南（MC 26.1）

> 适用 `feat/mcp-compatible` 当前工作树，基线提交 `8ae31a3`。

MineIntent 的全 Rust 实现；身体由 `companion` 进程持有。目标服务端 **Paper 26.1.2 / 协议号 775**。
结构见[架构说明](../architecture.md)。

| crate | 职责 |
|---|---|
| [`crates/world`](../../crates/world) | 接入 azalea、tick 快照、成像用的方块与光照拷贝 |
| [`crates/render`](../../crates/render) | 快照 → 模型可读文字（全纯函数） |
| [`crates/perception`](../../crates/perception) | 按需看画面：`view` |
| [`crates/vision`](../../crates/vision) | 方块状态 + 本地客户端资源 → PNG（原型） |
| [`crates/screens`](../../crates/screens) | 界面互斥域：`chat_box` / `inventory` / `container` |
| [`crates/input`](../../crates/input) / [`hand`](../../crates/hand) | 键鼠（按住若干秒后松开，左右键作用于准星） / 瞬时键（快捷栏、丢弃、换手） |
| [`crates/memory`](../../crates/memory) | 单文件长期记忆：`remember` |
| [`crates/presence`](../../crates/presence) | 生死去留：`presence`（当前只有 `respawn`） |
| [`crates/context`](../../crates/context) | 提示装配与压缩策略 |
| [`crates/dispatch`](../../crates/dispatch) | 工具编排与互斥域账本 |
| [`crates/bridge`](../../crates/bridge) | stdio MCP 转接器 `mineintent-mcp` 与身体的本机接入口 |
| [`crates/companion`](../../crates/companion) | 身体组合根，选择内置模型或外接代理 |

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
| `MINEINTENT_ENTRY` | `model` | `model` 使用内置模型会话；`mcp` 开放身体工具，不读取模型密钥、人设和记忆配置 |
| `MINEINTENT_BODY_ADDR` | `127.0.0.1:25580` | 外接入口的本机 TCP 地址；身体与转接器需使用相同值 |
| `MINEINTENT_HOST` / `MINEINTENT_PORT` | `127.0.0.1` / `25565` | 目标服务器 |
| `MINEINTENT_USERNAME` | `companion` | 离线身份（本版本只支持 offline） |
| `MINEINTENT_MEMORY_FILE` | `companion-memory.md` | 长期记忆文件 |
| `MINEINTENT_PERSONA_FILE` | 内置占位 | 人设全文；Q01 未裁前是占位文本 |
| `MINEINTENT_MODEL_API_KEY_FILE` | 无 | **推荐**：密钥文件路径 |
| `MODEL_API_KEY` | 无 | 退路：密钥本身（会进 shell 历史，不推荐） |
| `MODEL_PROTOCOL` | `chat` | `chat` / `responses` / `anthropic`；图片工具要求后两者 |
| `MODEL_ENDPOINT` | 对应协议的 DeepSeek endpoint | 完整 endpoint，含协议路径；Responses 默认 `https://api.deepseek.com/responses` |
| `MODEL_NAME` | Chat 为 `deepseek-chat`，其余为 `deepseek-flash` | 模型必须支持所选协议；启用图片还需视觉能力 |
| `MINEINTENT_CLIENT_JAR` | 未设置 | 本地 26.1.2 客户端 JAR；设置后加载资源并注册 `view` 图片工具，JAR 只读 |
| `MINEINTENT_VIEW_SIZE` | `1920x1080` | `view` 的屏幕像素尺寸 `宽x高`（每边 1–2048）；`view` 的 `region` 只从这块屏幕上裁，不会更清楚 |

模型、密钥、人设与记忆变量只用于内置 `model` 入口。外接 `mcp` 入口的模型与会话
配置由客户端管理。

```bash
MINEINTENT_MODEL_API_KEY_FILE=/path/to/key cargo run -p companion
```

内置入口 `Ctrl+C` 停机：先收尾会话（30 秒上限），再停世界。

## 外接代理（MCP）

此入口面向支持 legacy `2025-11-25`、stdio 和工具调用的 MCP 客户端。
`mineintent-mcp` 使用官方 Rust SDK `rmcp` 处理协议；身体常驻于独立的 `companion`
进程。两者之间的 TCP 端口是私有协议，不能作为客户端的 MCP HTTP URL。

先构建两个程序，再让身体连接已经准备好的测试服务器：

```bash
cargo build -p companion -p bridge --bins --locked
MINEINTENT_ENTRY=mcp MINEINTENT_HOST=127.0.0.1 MINEINTENT_PORT=25565 \
  ./target/debug/companion
```

该命令不请求模型，但会以默认用户名 `companion` 进入实际 Minecraft 世界。需要改名时
设置 `MINEINTENT_USERNAME`。需要图片时在身体进程设置 `MINEINTENT_CLIENT_JAR`，
指向本地 26.1.2 客户端 JAR；图片尺寸仍由 `MINEINTENT_VIEW_SIZE` 决定。未设置 JAR
时不提供 `view`，文字处境和其他工具仍可用。客户端和它选择的模型是否支持图片，
需要分别验证。

等身体报告就绪，再在客户端添加 stdio 服务。支持 `mcpServers` 配置格式的客户端可用：

```json
{
  "mcpServers": {
    "mineintent": {
      "command": "/abs/path/to/MineIntent/target/debug/mineintent-mcp"
    }
  }
}
```

其他客户端填写同一可执行文件的绝对路径，传输选择 stdio。若更改身体地址，也要在
客户端启动转接器时设置相同的 `MINEINTENT_BODY_ADDR`。这条入口仅用于本机回环连接。
同一时刻只接受一个控制者；调试工具也会占用该连接。
身体迟启动或重启后，转接器会尝试重新连接并通知工具表变化；失败的动作不会自动
重发。客户端若没有重新发现工具，需要主动刷新或重新接入。

工具回执末尾的「——期间——」提供事件和处境变化。身体保留最近 100 条事件，按
`seq` 标识；回执包含本次新事件及上一回执的新事件，重连接手时重送有限历史，
并报告已被逐出的条数。重送会显式标明可能重复，客户端可按 `seq` 识别；这不保证
宿主已收到或读过，也不保证超过容量的恢复。历史只在当前身体进程内保留，身体
重启后清空且序号重置，不能跨重启按序号去重。处境、拾取与进展也会重送上一回执
新附带的行，并标明是当时的记录；重新接手会重新生成完整处境。

`wait` 只因新事件或等待到期返回，历史重送不会使它反复立即返回；等待期间其他
工具仍可执行。等待时长应小于客户端的请求超时。没有在途调用时，事件留待下一次
工具调用；本入口不保证在宿主空闲时自动启动模型，也不要求 channel、resources
或 Tasks 扩展。
外部代理自行管理会话，本入口不提供 `remember`；它的长期记忆如何符合
[产品](../产品.md) M01、M03、M04a、M11，仍需另行实现和验证。

关闭客户端会结束其 stdio 转接器，身体进程继续在线。停止身体使用其终端的 `Ctrl+C`。
取消调用或断开连接会收束请求，但不能撤回已经提交到世界的动作。已提交的 `input`
仍由世界线程按原规则松开，最长 200 个游戏 tick；恢复后应重新观察身体状态。
已经开始的图片渲染也不会因取消请求而立即停止。
协议与生命周期的验证步骤见[验证指南](./validation.md#外接代理mcp)。

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
