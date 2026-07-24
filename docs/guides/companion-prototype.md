---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-24
---

# 运行 D40 同伴原型

D40 只验证一个最小场景：同伴能否从当前视野出发，通过相对转头和短时按键移动获得新观察，并根据实际结果继续判断或回复。它不是完整 Minecraft Agent。

## 前提

- Node.js 22 或更高版本、pnpm 11.3.0。
- Python 3.10 或更高版本，无额外 Python 依赖。
- Minecraft Java Edition / Paper 1.21.1 服务器。
- 支持 Chat Completions、tool calls 和 `response_format: json_object` 的 OpenAI-compatible 模型接口。

从 `.env.example` 复制 `.env`，至少填写：

```dotenv
MINEINTENT_MC_HOST=localhost
MINEINTENT_MC_PORT=25565
MINEINTENT_MC_USERNAME=MineIntentBot
MINEINTENT_PRIMARY_PLAYER=你的游戏名
MINEINTENT_AGENT_SERVICE_TOKEN=独立生成的本地令牌，至少32字符
MINEINTENT_MODEL_BASE_URL=https://服务商地址/v1
MINEINTENT_MODEL_API_KEY=只放在本地的模型密钥
MINEINTENT_MODEL=模型名
```

不要提交 `.env`、密钥、认证目录、玩家私人聊天或世界存档。

## 启动

先启动模型服务：

```powershell
python agent-service/server.py
```

再在另一个终端启动 MineIntent：

```powershell
pnpm start
```

主要玩家发给同伴的聊天会按顺序进入模型。可以从简单的观察与短移动开始：

```text
Bot，看看那只羊
Bot，往前走一点
Bot，停下
```

“停下”只是普通文本：runtime 不识别控制关键词、不直接释放移动键，也不发送固定确认。它会在当前轮次之后原样交给模型；模型可以照做、拒绝、延后，或先处理它从当前观察判断出的危险。断线、死亡/重生、换世界/维度、应用停止和请求超时仍会硬取消已经失效的执行。

`move_input` 只是 50–1500 ms 的短原子输入，当前聊天队列会等待本轮动作和动作后观察完成。十秒移动之类的长动作不属于这个接口；未来应启动独立的后台 action job，让后续模型轮次读取 job 状态并决定是否终止。

## 数据与调试

默认运行数据写入 `.mineintent/`：

- `events.jsonl`：运行事件与失败摘要；
- `memories.json`：之前已存在的原型记忆。D40 会检索它，但不新写模型声称的动作经历。

本地只读调试接口：

```text
GET http://127.0.0.1:3211/health
GET http://127.0.0.1:3211/v1/state
```

当前事件日志不是完整 D40 trace：它不保存每轮模型可见视野和工具结果。真人实验需另外记录场景、工具调用、耗时、动作后观察、最终聊天、token 和取消结果。

## 当前限制

没有后台 action job、自动导航、跳跃、挖掘、战斗、GUI 或长期自主。工具循环的轮数和总时间有上限，但真实延迟、token 成本、打断观感和语言真实性仍需重复的 Paper + 真实模型实验。
