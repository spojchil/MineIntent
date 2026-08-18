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

## GUI 上手实盘（2026-08-17，三跑）

口径一致，可比：空记忆起步、32 块木板、脚边一张工作台、假人只说一句
「能做几根木棍吗」，**不教任何步骤**。诊断轨迹用 `MINEINTENT_TRACE_FILE`。

| | 调用 | 错误 | move | 「格 45 变空」噪声 |
|---|---:|---:|---:|---:|
| 01 陈旧回执 | 33 | 4 | 13 | 28 |
| 02 回执改「已完成」+ 噪声修好 | 69 | 18 | 43 | 0 |
| 03 容器工具加「变化会主动通知你」 | **32** | 6 | **12** | 0 |

三跑都做成了木棍。要点：

- **模型能自己摸出 GUI**。没人教，它自己想到调 `describe` 取用法，并从
  「1-9 摆料，行优先」推出竖排格号。`describe` 改成按需取没有挡住上手。
- **跑 02 的膨胀不是「失去确认手段」**——跑 01 那个回执是在说谎（报上一 tick
  的格位），两跑都在被错位的时间线扰乱，跑 01 同样有近一半 move 是重做。
  一句「等通知」把膨胀全收回，说明起作用的是时间线而非信息密度。
- 跑 01 的产物里有「意外做出的橡木按钮」，跑 03 没有意外品。

### 待优化（未定位，需要问模型自己）

1. **「界面开着，无法移动或与世界交互」被撞到**。是提示词不到位，还是模型
   不知道自己开着屏？两者要的改法不同。
2. **空格搬运**（对着空格发 move）。要确认是不是把方向搞反了。

两条都只在小样本里出现过几次，不足以定性；反复出现才算我们的问题。定位手段是
**问模型自己**——实机测试的第三种信息来源（另两种：服务端查询=权威，
客户端日志=详细）。

## 一次增量要多少毫秒（2026-08-18）

问题是「测量各种情况下，一次增量的毫秒数」。答案不是一个数，是一条分布。

### 三个口径，别混

| 口径 | 含义 | 怎么量 |
| --- | --- | --- |
| `work` | 投影本身：视锥 + 遮挡 + 聚合（+ 记忆 diff） | `spawn_blocking` **闭包内**计时 |
| `round` | 派发 + 在阻塞池排队 + 执行 + join | 闭包**外**计时 |
| 每 tick 采集 | 主循环里从 ECS 装配快照 | `MINEINTENT_CAPTURE_TIMING=1` |

自适应节律按 `round` 退让（系统忙时排队是真实代价），分布报 `work`（那才是
算法成本）。**此前只量 round 却标成「本次投影」**，把排队记在了投影头上；订
正后两者可以直接相减看调度开销。

### 实测

```
# 静止基线（需要 --features azalea，需服务端）
cargo run --release -p world --features azalea --example scan_bench_probe -- 127.0.0.1 25565 bench 30
```

| 场景 | 构建 | 最快 | 中位/均值 | 最慢 |
| --- | --- | --- | --- | --- |
| 静止基线，纯 `scan` | release | — | 8.8ms | — |
| 静止基线，`scan_changes` | release | — | 13.3ms | — |
| 实盘（边走边挖），1000 次采样 | release | 7ms | 26ms | 122ms |
| 实盘 | debug | — | 244ms | — |

- **记忆那层 +3.7ms（35%）**：`scan_changes` 减 `scan`。
- **排队开销 ≈0ms**：同一 1000 次采样里 `work` 与 `round` 均值同为 26ms。
- **每 tick 采集 0.13ms**：20 次/秒即 0.26% CPU，不是主循环的负担。

### 结论

1. 慢的两个真因是 **debug 构建（9 倍）** 和 **场景（17 倍浮动）**。
2. 三项旧优化（section AABB 剔除、`ExposedFace` 判据、零分配探针查表）都还在，
   「旧线压到过 10ms」与今天的 8.8ms 静止基线是一致的——旧记忆没错，错的是拿
   静止基线去对实盘。
3. 曾经怀疑过的**排队**、**ECS 写锁 / 每 tick 采集**、**记忆增长**，逐一实测
   排除。`MINEINTENT_CAPTURE_TIMING` 留在树里，就是给下次同类怀疑当场证伪用的。
4. 因此**单点数字没有意义**。要报就报分布，且必须写明构建档与场景。

## 当前尚缺的验证

1. ~~纵向验收脚本尚未实跑于新栈~~ **已跑通**（2026-08-17，Paper 26.1.2 +
   DeepSeek，`fdd3792`）：首跑 9 通过 1 失败，失败的是判据自身（要求「躺够
   10 拍」与「随后回满」，是同一信号的两个方向，模型够快就必然不过）；订正后
   复跑**全部通过**。报告在 `server-run/gate-b/report.md`，判据订正前那份留作
   `report-run01-fail.md`。
2. **轮末帧（增量视口）无自动覆盖**。收集→`Passive` 投递这条链在组合根装配，
   单测覆盖不到；脚本里只作交叉印证（空 diff 不投帧是设计，故「没有帧」不判失败）。
3. **屏与容器只有机器级探针**。`crafting_probe` / `open_probe` / `swap_probe`
   要手工准备世界与物品，不在脚本里，也不在 CI。

未来的纵向验证至少应保存：每次模型请求的消息列表、工具调用、调用后采样的
观察、失败与清理结果、使用的提交与模型标识。

保存或分享证据前必须脱敏密钥、私人聊天、模型 reasoning 和世界数据。
