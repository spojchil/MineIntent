# 验证当前实现

> 本页说明每项检查能提供什么证据，以及**它不能证明什么**。验证结果不产生产品权威。
> MCP 部分适用 `feat/mcp-compatible` 当前工作树，基线提交 `8ae31a3`。
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

## 外接代理（MCP）

MCP 检查分三层；下列命令是检查入口，不是通过记录。实现以
[`bridge`](../../crates/bridge/src) 与
[`companion::mcp_entry`](../../crates/companion/src/mcp_entry.rs) 为准。

### 无 Minecraft、无真实模型的检查

```sh
cargo test -p bridge --all-targets --locked
cargo test -p companion --bin companion --test mcp_startup --locked
cargo clippy -p bridge -p companion --all-targets --locked -- -D warnings
```

桥接测试使用本机 TCP 和测试身体；组合根测试使用构造快照、信箱和测试工具。
`mcp_startup` 子进程测试核对非法入口与端口占用在进入世界之前报错，以及 MCP
入口不读取无效的内置模型配置。
它们不连接 Minecraft 或模型服务。Cargo 初次取得依赖和工具链仍可能需要联网，
`companion` 的构建仍包含 Azalea，不能把“不接实服”理解为轻量编译。

核对结果时分别记录标准握手与工具载荷、取消与 EOF 清理、等待期间其他调用的响应、
连接占用与重新接手、事件留存与重送、超过 100 条后的丢失提示；仅测试信箱不能
证明真实 stdio 子进程的退出路径。
图片回执的 JSON 形状通过，也不能代替有效 PNG 的解码与客户端显示检查。

### 独立标准客户端互操作

先用[官方 Inspector](https://modelcontextprotocol.io/docs/2026-07-28/tools/inspector)
或官方 SDK 客户端连接 stdio 转接器；协议选择 legacy `2025-11-25`，不启用供应商
扩展。下面的 CLI 命令每次建立新会话，适合单次检查；取消、并发与连续调用需使用
Inspector Web/TUI 或保持连接的 SDK 测试客户端。

```sh
npx @modelcontextprotocol/inspector --cli /abs/path/to/mineintent-mcp \
  --method initialize --format json
npx @modelcontextprotocol/inspector --cli /abs/path/to/mineintent-mcp \
  --method tools/list --format json
npx @modelcontextprotocol/inspector --cli /abs/path/to/mineintent-mcp \
  --method tools/call --tool-name wait --tool-args-json '{"seconds":1}' --format json
```

`initialize` 不依赖世界；工具表和调用需要身体在线或专用测试身体。Inspector 自身
不需要模型，但连接真实身体并执行动作工具会改变世界，测试时使用独立测试服。

最小互操作判据：

- 协商版本、工具 schema、请求 ID 与错误形状正确；stdout 不混入日志。
- 文字、执行失败和有效 PNG 图片回执能被客户端解码。
- 取消、超时、EOF 后未完成调用得到清理；下一调用或下一控制者能继续操作。
- 取消等待不消费事件；顺序调用时每条事件只出现在一次回执里，`wait` 与动作重叠时
  两边回执都带重叠期间的事件；重接历史可按 `seq` 识别，且不使下一次 `wait` 反复
  立返。历史与序号仅属当前身体进程，此项不验证跨进程恢复、宿主确认或
  exactly-once 投递。
- 开场先发 `server/discover`（声明 `2026-07-28`）再 `initialize` 的客户端，探测得到
  `-32022`，随后不带 `_meta` 的 `tools/list` 仍成功。
- 身体不在线、迟启动和重启时，客户端获得明确失败并能主动重试；不声称客户端会
  自动重连或自动重新发现工具。
- 不加载 channel 时，调用回执仍能带回事件，已有 `wait` 能提前返回；宿主完全空闲
  时不要求模型自行开始新一轮。加载 channel 的 Claude Code 在空闲时收到敲门后自行
  调用工具取回事件。

[官方 Conformance](https://github.com/modelcontextprotocol/conformance) 的公开服务器
检查入口使用 HTTP `--url`，不能直接指向本仓私有 TCP 端口。可先采用 Inspector、
官方 SDK 与所声明版本的 schema 做互操作检查；不为运行全套 SDK 一致性场景扩展
产品接口。resources、Tasks 和现代 `2026-07-28` 分代不在本入口的兼容承诺内。

### 真实世界与模型边界

真实 MCP 身体还需在隔离测试服核对：进入世界、`input` 按键及松开、屏状态、`view`、
死亡与复活、Minecraft 断线、取消/重连后的身体状态和进程停机。用 Inspector 驱动
即可验证世界效果，不必调用模型；宿主是否正确向模型提供图片、模型是否理解回执、
空闲调度与长期记忆则需要额外宿主验证。协议单测不提供这些证据。

`scripts/gate-b-vertical.sh` 是内置模型入口的实服验收，会请求真实模型并写入测试世界，
不能用作上述无模型检查或 MCP 兼容性的证据。

## 按需图片原型

`cargo test -p vision --all-targets` 使用自制资源验证模型解析、几何、相机、透明
遮挡和资源版本拒绝；`cargo clippy -p vision -p world --features vision/live
--all-targets -- -D warnings` 检查实际连接探针。运行命令与画面限制见
[vision README](../../crates/vision/README.md)。合成场景图片只能证明资源到像素
链路；连接测试服务端运行 `capture` 才提供真实协议状态到图片的证据，两者均不
证明与官方画面一致或已经具备完整实体视觉。
启动测试服、连接采图、画面检查、计时范围和关服步骤见
[协议采图探针](../../crates/vision/README.md#协议采图探针)。
完整交互验证需启动 `companion`、配置视觉模型，让假玩家聊天触发 `view`，再问 bot
图像观察和工具反馈；步骤见 [完整交互测试](../../crates/vision/README.md#bot-与假玩家的完整交互测试)。
`capture` 成功或渲染基准通过不等于这条模型链路已验证。

`cargo test -p perception --lib` 检查图片工具参数、失败边界和二进制图片回执；
`cargo test -p companion --bin companion` 检查世界姿态转换、协议配置拒绝，以及
本地 HTTP 请求中的图片块与 call ID。HTTP 测试不调用付费模型，也不证明模型理解图片。

光栅化正确性另外用测试专用射线实现交叉检查：相机变换、近面/侧面裁剪、透视
贴图、径向远距离、透明深度顺序与共享边。`cargo run --release -p vision
--example benchmark -- <client.jar>` 分别输出 JAR 打开、资源冷/热缓存成像和 PNG
编码耗时；两组均为固定合成场景，不能拿这个数声称实服端到端或官方帧率。

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

**`KEY_FILE` 是必填**：组合根启动即读密钥，没有脚本化模型模式，纵向验收
必然打真模型。密钥按路径注入，不进命令行、不进日志、不进版本库。

判据一律取服务端侧证据（server log / 控制台命令返回）或假人侧证据。同伴自己的
stdout 只进「交叉印证」栏，不单独构成通过条件。当前判的是：进入世界、指名聊天
唤醒并公屏回话、回话被第三方独立观察、在线名单、位移与朝向的**前后差值**在服务端
可见、怪物致死后自动重生、死后仍能被唤醒说话、SIGINT 后干净断开。

## 当前尚缺的验证

1. **纵向验收脚本没有在键鼠架构上跑过**。它的判据（如「怪物致死后自动重生」）
   写于移动还靠坐标工具、复活不经模型的时候；现在复活要模型调 `presence`，
   移动只剩 `input`。
2. **帧的投递没有端到端覆盖**。判据在 `companion::frame`，带单测（发不发车、
   拾取搭车、无车不吞处境差异、积压封顶、压缩后重投）；组合根只剩取快照与投递，
   走 `NextModelRequest`，脚本里只作交叉印证。
3. **屏、容器与键鼠只有机器级探针**。`world/examples` 下的 `*_probe`
   要手工准备世界与物品，不在脚本里，也不在 CI。

未来的纵向验证至少应保存：每次模型请求的消息列表、工具调用、调用后采样的
观察、失败与清理结果、使用的提交与模型标识。

保存或分享证据前必须脱敏密钥、私人聊天、模型 reasoning 和世界数据。
