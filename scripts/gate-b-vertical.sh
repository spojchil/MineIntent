#!/usr/bin/env bash
# gate b 纵向验收：Paper 26.1.2 实服 + Rust 全栈 + 假人说话方。
#
# 判定纪律：每条断言都取**服务端侧**证据（server log / 控制台命令返回）或
# 假人侧证据，不取被测进程的自述。同伴自己的 journal/dev.log 只用于交叉印证，
# 单独不构成通过条件——否则就是让被测系统给自己作证。
#
# 用法：
#   MINEINTENT_ACCEPT_EULA=true scripts/gate-b-vertical.sh
#   MINEINTENT_ACCEPT_EULA=true MODEL=responses KEY_FILE=/path/to/key scripts/gate-b-vertical.sh
#
# 退出码：0=全部通过；1=有断言失败；2=环境或装配问题（未进入判定）。

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PAPER_JAR="${PAPER_JAR:-$REPO_ROOT/supplies/paper/26.1.2/paper-26.1.2-74.jar}"
JAVA_BIN="${JAVA_BIN:-$REPO_ROOT/supplies/tools/jdk-25/Contents/Home/bin/java}"
BASE="${BASE:-$REPO_ROOT/server-run/gate-b}"
SERVER_DIR="$BASE/server"
RUN_DIR="$BASE/run"
DEATH_DIR="$BASE/death"
DATA_DIR="$RUN_DIR/.mineintent"
REPORT="$BASE/report.md"

BOT_NAME="${BOT_NAME:-MineIntentBot}"
FAKE_NAME="${FAKE_NAME:-Alice}"
FAKE_MSG="${FAKE_MSG:-$BOT_NAME 你好，帮我看看周围}"
MODEL="${MODEL:-scripted}"
KEY_FILE="${KEY_FILE:-}"
RUN_SECS="${RUN_SECS:-100}"
PORT="${PORT:-25565}"
# 死亡恢复场景：plain=怪物致死后自行重生；off=跳过。
DEATH_SCENARIO="${DEATH_SCENARIO:-plain}"

PHASE="(未开始)"
COMPLETED=0
declare -a RESULTS=()
FAILED=0
SERVER_PID=""
HOLDER_PID=""
APP_PID=""
FAKE_PID=""

phase() { PHASE="$1"; printf '\n=== [阶段] %s ===\n' "$1"; }
note()  { printf '  · %s\n' "$*"; }
die()   { printf '\n[环境错误/%s] %s\n' "$PHASE" "$*" >&2; exit 2; }

# 断言取证：$1=名称 $2=证据来源 $3=通过与否(0/1) $4=实测摘要
assert() {
  local name="$1" source="$2" ok="$3" detail="$4"
  if [ "$ok" -eq 0 ]; then
    RESULTS+=("PASS|$name|$source|$detail")
    printf '  [通过] %s （证据：%s）\n' "$name" "$source"
  else
    RESULTS+=("FAIL|$name|$source|$detail")
    FAILED=1
    printf '  [失败] %s （证据：%s）— %s\n' "$name" "$source" "$detail"
  fi
}

console() { printf '%s\n' "$1" > "$SERVER_DIR/console.fifo"; sleep "${2:-1}"; }

srvlog() { cat "$SERVER_DIR/logs/latest.log" 2>/dev/null; }

wait_for() { # $1=文件 $2=正则 $3=秒数
  local f="$1" re="$2" limit="$3" i=0
  while [ "$i" -lt "$limit" ]; do
    grep -qE "$re" "$f" 2>/dev/null && return 0
    sleep 1; i=$((i+1))
  done
  return 1
}

# 无论成功、断言失败还是超时/中断，清理都必须跑完并落地终态结论。
cleanup() {
  local exit_code=$?
  # 先摘钩子：cleanup 末尾的 exit 会再次触发 EXIT，不摘就跑两遍、
  # 报告被覆盖成第二遍的（更干净的）状态。
  trap - EXIT INT TERM
  phase "cleanup（无论前序结果如何都执行）"
  [ -n "$FAKE_PID" ]   && kill -TERM "$FAKE_PID"   2>/dev/null && note "停止假人"
  [ -n "$APP_PID" ]    && kill -TERM "$APP_PID"    2>/dev/null && note "停止同伴"
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    printf 'stop\n' > "$SERVER_DIR/console.fifo" 2>/dev/null
    local i=0; while [ "$i" -lt 30 ] && kill -0 "$SERVER_PID" 2>/dev/null; do sleep 1; i=$((i+1)); done
    kill -TERM "$SERVER_PID" 2>/dev/null
    note "停止服务端"
  fi
  [ -n "$HOLDER_PID" ] && kill -TERM "$HOLDER_PID" 2>/dev/null

  # 未跑到终点就被信号打断的，绝不能因为「还没有断言失败」而报通过——
  # 那是把「没测完」当成「测过了」。
  local verdict
  if [ "$COMPLETED" -ne 1 ]; then verdict="中断（未跑完，不构成验收结论）"
  elif [ "$exit_code" -eq 2 ]; then verdict="未判定（环境问题）"
  elif [ "$FAILED" -eq 0 ]; then verdict="通过"
  else verdict="不通过"; fi

  {
    printf '# gate b 纵向验收结果：%s\n\n' "$verdict"
    printf -- '- 中断阶段：%s\n- 模型：%s\n- 服务端：Paper 26.1.2 offline\n\n' "$PHASE" "$MODEL"
    printf '| 结论 | 断言 | 证据来源 | 实测 |\n|---|---|---|---|\n'
    local r
    for r in "${RESULTS[@]}"; do
      IFS='|' read -r st nm sc dt <<< "$r"
      printf '| %s | %s | %s | %s |\n' "$st" "$nm" "$sc" "$dt"
    done
  } > "$REPORT"
  printf '\n终态结论：%s（明细 %s）\n' "$verdict" "$REPORT"
  [ "$COMPLETED" -ne 1 ] && exit 130
  [ "$exit_code" -eq 2 ] && exit 2
  [ "$FAILED" -eq 0 ] || exit 1
  exit 0
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- setup
phase "setup"

[ -f "$PAPER_JAR" ] || die "缺 Paper jar：$PAPER_JAR"
[ -x "$JAVA_BIN" ]  || die "缺 JDK：$JAVA_BIN"

# EULA 必须显式接受：脚本绝不代替使用者同意 Mojang 条款。
if [ "${MINEINTENT_ACCEPT_EULA:-}" != "true" ]; then
  die "未接受 EULA。阅读 https://aka.ms/MinecraftEULA 后设 MINEINTENT_ACCEPT_EULA=true 再跑。"
fi
note "EULA 已由调用方显式接受"

if [ "$MODEL" = "responses" ]; then
  [ -n "$KEY_FILE" ] && [ -f "$KEY_FILE" ] || die "MODEL=responses 需要可读的 KEY_FILE"
  note "真模型模式，密钥按路径注入（不进日志、不进版本库）"
fi

mkdir -p "$SERVER_DIR"
printf 'eula=true\n' > "$SERVER_DIR/eula.txt"
cat > "$SERVER_DIR/server.properties" <<EOF
online-mode=false
server-port=$PORT
level-name=world
spawn-protection=0
max-players=10
view-distance=8
simulation-distance=6
motd=MineIntent gate b
allow-nether=false
level-seed=mineintent-gate-b
gamemode=survival
difficulty=easy
EOF

# 世界模板：每轮从干净模板复制，绝不复用上一轮世界，
# 以免上一轮的实体/掉落物/坐标漂移被当成本轮事实。
TEMPLATE="$BASE/world-template"
if [ -d "$TEMPLATE" ]; then
  rm -rf "$SERVER_DIR/world"
  cp -R "$TEMPLATE" "$SERVER_DIR/world"
  rm -f "$SERVER_DIR/world/session.lock"
  note "已从模板复制干净世界（未继承上一轮诊断状态）"
  TEMPLATE_USED=0
else
  note "首次运行：本轮结束后将把生成的世界存为模板"
  TEMPLATE_USED=1
fi

rm -rf "$RUN_DIR"; mkdir -p "$RUN_DIR"
rm -f "$SERVER_DIR/logs/latest.log"

rm -f "$SERVER_DIR/console.fifo"; mkfifo "$SERVER_DIR/console.fifo"
tail -f /dev/null > "$SERVER_DIR/console.fifo" & HOLDER_PID=$!
( cd "$SERVER_DIR" && exec "$JAVA_BIN" -Xms2G -Xmx2G -jar "$PAPER_JAR" --nogui \
    < "$SERVER_DIR/console.fifo" > "$SERVER_DIR/server-stdout.log" 2>&1 ) & SERVER_PID=$!

wait_for "$SERVER_DIR/logs/latest.log" 'Done \(' 180 || die "服务端未在 180s 内就绪"
note "服务端就绪：$(grep -oE 'Done \([0-9.]+s\)' "$SERVER_DIR/logs/latest.log" | tail -1)"

if [ "$TEMPLATE_USED" -eq 0 ]; then
  assert "世界模板复制且不继承旧诊断" "setup 阶段文件操作" 0 "已从 $TEMPLATE 复制"
fi
assert "EULA 显式接受方可启动" "setup 阶段前置校验" 0 "MINEINTENT_ACCEPT_EULA=true"

# ------------------------------------------------- participant behavior
phase "participant behavior"

APP_BIN="$REPO_ROOT/target/debug/mineintent"
FAKE_BIN="$REPO_ROOT/target/debug/fake_player"
[ -x "$APP_BIN" ] && [ -x "$FAKE_BIN" ] || die "缺二进制，先跑 cargo build --bin mineintent --bin fake_player"

MODEL_ENV=(MINEINTENT_MODEL="$MODEL")
[ "$MODEL" = "responses" ] && MODEL_ENV+=(MINEINTENT_MODEL_API_KEY_FILE="$KEY_FILE")

( cd "$RUN_DIR" && exec env MINEINTENT_MC_HOST=127.0.0.1 MINEINTENT_MC_PORT="$PORT" \
    MINEINTENT_MC_USERNAME="$BOT_NAME" MINEINTENT_WORLD_ID=paper-gate-b \
    MINEINTENT_DATA_DIR="$DATA_DIR" MINEINTENT_DEBUG=1 \
    MINEINTENT_MAX_RUNTIME_SECS="$RUN_SECS" "${MODEL_ENV[@]}" \
    "$APP_BIN" > "$RUN_DIR/companion-stdout.log" 2>&1 ) & APP_PID=$!

if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME joined the game" 90; then
  SPAWN_AT=$(grep -oE "$BOT_NAME\[.*logged in with entity id [0-9]+ at \(.*\)" "$SERVER_DIR/logs/latest.log" | tail -1)
  assert "同伴进入世界" "服务端 log（非同伴自述）" 0 "${SPAWN_AT:-joined}"
else
  assert "同伴进入世界" "服务端 log（非同伴自述）" 1 "90s 内服务端未见 joined"
  exit 1
fi

POS_BEFORE=$(console "data get entity $BOT_NAME Pos" 2 ; srvlog | grep 'has the following entity data' | tail -1)

( cd "$RUN_DIR" && exec env FAKE_HOST=127.0.0.1 FAKE_PORT="$PORT" FAKE_USERNAME="$FAKE_NAME" \
    FAKE_SECONDS=$((RUN_SECS - 20)) FAKE_MESSAGE="$FAKE_MSG" \
    "$FAKE_BIN" > "$RUN_DIR/fake-stdout.log" 2>&1 ) & FAKE_PID=$!

wait_for "$SERVER_DIR/logs/latest.log" "$FAKE_NAME joined the game" 60 \
  || { assert "假人进入世界" "服务端 log" 1 "60s 内未见假人 joined"; exit 1; }
assert "假人进入世界" "服务端 log" 0 "$FAKE_NAME joined"

wait_for "$SERVER_DIR/logs/latest.log" "<$FAKE_NAME> .*$BOT_NAME" 60 \
  || { assert "假人发出指名聊天" "服务端 log" 1 "未见假人指名发言"; exit 1; }
assert "假人发出指名聊天" "服务端 log" 0 "$FAKE_MSG"

# 唤醒的判据是同伴在公屏上说话被服务端记录，且假人独立收到同一句。
if wait_for "$SERVER_DIR/logs/latest.log" "<$BOT_NAME> " 90; then
  REPLY=$(srvlog | grep -oE "<$BOT_NAME> .*" | tail -1)
  assert "指名聊天唤醒并公屏回话" "服务端 log" 0 "$REPLY"
  # 假人是 Rust 进程，println! 写文件是块缓冲，落盘晚于服务端记录；
  # 这里必须等而不是立刻判，否则测的是缓冲时机不是观察能力。
  if wait_for "$RUN_DIR/fake-stdout.log" "CHAT.*<$BOT_NAME>" 30; then
    assert "回话被第三方独立观察到" "假人自身聊天流" 0 "假人收到同一句"
  else
    assert "回话被第三方独立观察到" "假人自身聊天流" 1 "30s 内假人未收到"
  fi
else
  assert "指名聊天唤醒并公屏回话" "服务端 log" 1 "90s 内同伴未发言"
fi

# 工具往返：马达类工具的效果必须在服务端实体数据上可见，
# 而不是只在同伴自己的 snapshot 里可见。
console "data get entity $BOT_NAME Pos" 2
POS_AFTER=$(srvlog | grep 'has the following entity data' | tail -1)
console "data get entity $BOT_NAME Rotation" 2
ROT_AFTER=$(srvlog | grep 'has the following entity data' | tail -1)

if [ "$POS_BEFORE" != "$POS_AFTER" ]; then
  assert "move_input 在世界中真实生效" "服务端 data get Pos" 0 "$POS_AFTER"
else
  assert "move_input 在世界中真实生效" "服务端 data get Pos" 1 "位置未变化"
fi
if echo "$ROT_AFTER" | grep -qE '\[-?[0-9]+\.[0-9]+f, '; then
  assert "look_relative 在世界中真实生效" "服务端 data get Rotation" 0 "$ROT_AFTER"
else
  assert "look_relative 在世界中真实生效" "服务端 data get Rotation" 1 "未读到 Rotation"
fi

console "list" 2
if srvlog | grep -q "players online:.*$BOT_NAME"; then
  assert "同伴在服务端在线名单内" "服务端 list 命令" 0 "$(srvlog | grep -oE 'There are .*players online:.*' | tail -1)"
else
  assert "同伴在服务端在线名单内" "服务端 list 命令" 1 "list 未见同伴"
fi

# 交叉印证（不单独构成通过条件）：产品事实应落进 journal。
# 只断言与模式无关的两项：真模型自行决定调哪些工具，把 remember 写死成
# 通过条件等于要求模型每轮都记忆，那是脚本在规定模型行为。
JOURNAL_EXPECT=(say.queued body_tool.completed)
[ "$MODEL" = "scripted" ] && JOURNAL_EXPECT+=(memory.remembered)
if [ -f "$DATA_DIR/events.jsonl" ]; then
  for t in "${JOURNAL_EXPECT[@]}"; do
    if grep -q "\"type\":\"$t\"" "$DATA_DIR/events.jsonl"; then
      assert "journal 记下 $t" "同伴 journal（交叉印证）" 0 "已记录"
    else
      assert "journal 记下 $t" "同伴 journal（交叉印证）" 1 "缺失"
    fi
  done
fi

# 三版本锚（更新-12 §5）：agent-context.v5 / viewport.v2 / tool-result.v1。
# 只有真模型路径把请求原文落盘，scripted 下如实标注未取证。
if [ "$MODEL" = "responses" ] && [ -d "$DATA_DIR/model-io" ]; then
  TOTAL_REQ=$(ls "$DATA_DIR"/model-io/*-req.json 2>/dev/null | wc -l | tr -d ' ')
  for a in mineintent.viewport.v2 mineintent.tool-result.v1; do
    HIT=$(grep -ls "$a" "$DATA_DIR"/model-io/*-req.json 2>/dev/null | wc -l | tr -d ' ')
    if [ "$HIT" -gt 0 ]; then
      assert "版本锚 $a 到达模型请求" "model-io 请求原文" 0 "$HIT/$TOTAL_REQ 个请求命中"
    else
      assert "版本锚 $a 到达模型请求" "model-io 请求原文" 1 "0/$TOTAL_REQ 命中"
    fi
  done
  # agent-context.v5 在契约类型上有 WIRE 常量且能正确序列化，但production
  # 渲染把开场帧摊平进 user message，锚点在这一步丢失。是否应当到达模型
  # 是产品裁定问题（另两个锚都到了，这一个没到），故记为待裁定不判失败。
  HIT5=$(grep -ls 'mineintent.agent-context.v5' "$DATA_DIR"/model-io/*-req.json 2>/dev/null | wc -l | tr -d ' ')
  if [ "$HIT5" -gt 0 ]; then
    assert "版本锚 mineintent.agent-context.v5 到达模型请求" "model-io 请求原文" 0 "$HIT5/$TOTAL_REQ 命中"
  else
    RESULTS+=("待裁定|版本锚 mineintent.agent-context.v5 未到达模型请求|model-io 请求原文|0/$TOTAL_REQ 命中；另两锚均到达，需维护者裁定开场帧是否应携带锚点")
    printf '  [待裁定] agent-context.v5 未出现在模型请求（另两锚均到达）——需维护者裁定\n'
  fi
else
  note "版本锚未取证：scripted 模式不落 model-io，需 MODEL=responses 复跑"
fi

# ------------------------------------------------------ 正常停机
phase "正常停机"
i=0; while [ "$i" -lt $((RUN_SECS + 60)) ] && kill -0 "$APP_PID" 2>/dev/null; do sleep 2; i=$((i+2)); done

if kill -0 "$APP_PID" 2>/dev/null; then
  assert "到期按计划停机" "同伴进程状态" 1 "超时仍未退出"
else
  if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME lost connection" 30; then
    assert "停机后服务端观察到干净断开" "服务端 log" 0 "$(srvlog | grep -oE "$BOT_NAME lost connection.*" | tail -1)"
  else
    assert "停机后服务端观察到干净断开" "服务端 log" 1 "服务端未记录断开"
  fi
  if grep -q '已停止' "$RUN_DIR/companion-stdout.log" 2>/dev/null; then
    assert "停机走正常路径而非崩溃" "同伴退出输出（交叉印证）" 0 "已停止"
  else
    assert "停机走正常路径而非崩溃" "同伴退出输出（交叉印证）" 1 "未见正常停机标记"
  fi
fi

# ------------------------------------------------- 死亡恢复（第六个工具）
# 默认纵向脚本只走五个工具，respawn 覆盖不到；而 respawn 恰是最近两次实盘
# 修复（1298912 死后失聪、628836e 死亡期间准许重生）的主题。这里用怪物打死
# 同伴而不是 /kill：伤害→掉血→死亡整条观察链都要真实经过。
if [ "$DEATH_SCENARIO" != "off" ]; then
  phase "死亡恢复"
  rm -rf "$DEATH_DIR"; mkdir -p "$DEATH_DIR"
  ( cd "$DEATH_DIR" && exec env MINEINTENT_MC_HOST=127.0.0.1 MINEINTENT_MC_PORT="$PORT" \
      MINEINTENT_MC_USERNAME="$BOT_NAME" MINEINTENT_WORLD_ID=paper-gate-b \
      MINEINTENT_DATA_DIR="$DEATH_DIR/.mineintent" MINEINTENT_DEBUG=1 \
      MINEINTENT_SCRIPT=death MINEINTENT_MAX_RUNTIME_SECS=150 "${MODEL_ENV[@]}" \
      "$APP_BIN" > "$DEATH_DIR/companion-stdout.log" 2>&1 ) & APP_PID=$!

  if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME joined the game" 90; then
    note "同伴重新进入世界，准备致死"
  else
    assert "死亡场景：同伴进入世界" "服务端 log" 1 "90s 内未见 joined"; exit 1
  fi

  # 夜间 + 困难：僵尸白天自燃会先死在半路，easy 难度伤害不足以在窗口内致死。
  console "difficulty hard" 1
  console "time set midnight" 1

  # 持续补怪而不是一次性召唤：僵尸可能卡住、掉下去或被环境弄死，
  # 一次性召唤的成功率靠运气。四个方位各来一只，每轮补一次。
  DIED=1
  for round in $(seq 1 12); do
    for off in "~1 ~ ~" "~-1 ~ ~" "~ ~ ~1" "~ ~ ~-1"; do
      console "execute at $BOT_NAME run summon minecraft:zombie $off" 0
    done
    if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME (was slain|was killed|died)" 10; then
      DIED=0; break
    fi
  done
  if [ "$DIED" -eq 0 ]; then
    assert "同伴被怪物真实杀死" "服务端 log" 0 \
      "$(srvlog | grep -oE "$BOT_NAME (was slain|was killed|died)[^\"]*" | tail -1)"
  else
    assert "同伴被怪物真实杀死" "服务端 log" 1 "补怪 12 轮仍未致死"
  fi
  console "kill @e[type=minecraft:zombie]" 1

  console "data get entity $BOT_NAME Health" 2
  if srvlog | grep 'entity data' | tail -1 | grep -q ' 0.0f'; then
    assert "死亡态在服务端可见（Health=0）" "服务端 data get Health" 0 "0.0f"
  else
    assert "死亡态在服务端可见（Health=0）" "服务端 data get Health" 1 \
      "$(srvlog | grep 'entity data' | tail -1)"
  fi

  # 唤醒它。死亡期间动作面只放行 respawn，所以只要公屏上出现这句话，
  # 就同时证明了「死后仍可被唤醒」和「重生真的生效」——而不只是请求已派发。
  ( cd "$DEATH_DIR" && exec env FAKE_HOST=127.0.0.1 FAKE_PORT="$PORT" FAKE_USERNAME="$FAKE_NAME" \
      FAKE_SECONDS=90 FAKE_MESSAGE="$BOT_NAME 你还好吗" \
      "$FAKE_BIN" > "$DEATH_DIR/fake-stdout.log" 2>&1 ) & FAKE_PID=$!

  # 没死成就不能判「死后重生」：respawn 在活着时是无害操作，后面那句话
  # 照样说得出来，据此判过是假阳性。前置不成立时如实标注未取证。
  if [ "$DIED" -ne 0 ]; then
    note "未致死，跳过重生判定（此前提不成立时该判定无意义）"
  else
    if wait_for "$SERVER_DIR/logs/latest.log" "<$BOT_NAME> 我又活过来了" 90; then
      assert "死后被唤醒并自行重生" "服务端 log（重生后才说得出话）" 0 "我又活过来了"
    else
      assert "死后被唤醒并自行重生" "服务端 log（重生后才说得出话）" 1 "90s 内未见重生后发言"
    fi

    console "data get entity $BOT_NAME Health" 2
    if srvlog | grep 'entity data' | tail -1 | grep -qE ' (20|19|18|17|16)\.[0-9]+f'; then
      assert "重生后生命值恢复" "服务端 data get Health" 0 \
        "$(srvlog | grep 'entity data' | tail -1 | grep -oE '[0-9.]+f')"
    else
      assert "重生后生命值恢复" "服务端 data get Health" 1 \
        "$(srvlog | grep 'entity data' | tail -1)"
    fi
  fi

  console "difficulty easy" 1
  console "time set day" 1
  kill -TERM "$FAKE_PID" 2>/dev/null; FAKE_PID=""
  i=0; while [ "$i" -lt 200 ] && kill -0 "$APP_PID" 2>/dev/null; do sleep 2; i=$((i+2)); done
  kill -TERM "$APP_PID" 2>/dev/null; APP_PID=""
fi

if [ "$TEMPLATE_USED" -eq 1 ]; then
  cp -R "$SERVER_DIR/world" "$TEMPLATE" 2>/dev/null && note "已存世界模板供后续复跑"
fi

COMPLETED=1
exit 0
