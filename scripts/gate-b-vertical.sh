#!/usr/bin/env bash
# gate b 纵向验收：Paper 26.1.2 实服 + Rust 全栈（companion）+ 假人说话方。
#
# 判定纪律：每条断言都取**服务端侧**证据（server log / 控制台命令返回）或
# 假人侧证据，不取被测进程的自述。同伴自己的 stdout 只用于交叉印证，
# 单独不构成通过条件——否则就是让被测系统给自己作证。
#
# 用法：
#   MINEINTENT_ACCEPT_EULA=true KEY_FILE=/path/to/key scripts/gate-b-vertical.sh
#
# 退出码：0=全部通过；1=有断言失败；2=环境或装配问题（未进入判定）。
#
# ── 2026-08-17 按新栈重写 ────────────────────────────────────────────
# 旧脚本接的是已删除的 `mineintent-app`。除了换二进制与环境变量，有四处
# 判据在新栈没有对应物，是**删掉**而不是改写的，理由各自写在原地：
#
#   1. `MODEL=scripted` 没了——组合根启动即读密钥（main.rs 的 read_api_key），
#      没有脚本化模型这条路。纵向验收从此必然打真模型，KEY_FILE 是必填。
#   2. journal（`events.jsonl`）没了——新栈不落事件日志，那三条交叉印证
#      （say.queued / body_tool.completed / memory.remembered）无从取证。
#      能落盘的产品事实只剩记忆文件，改判它。
#   3. 版本锚（agent-context.v5 / viewport.v2 / tool-result.v1）没了——
#      新栈既不落 model-io 请求原文，源码里也没有锚常量，没有可断言的对象。
#   4. 旧的 respawn 工具没了。死亡场景改判「死亡是持续状态」+「模型自己
#      决定复活并生效」+「复活后仍能被唤醒说话」，最后一条是历史故障
#      （死后失聪）的回归目标。见下面死亡段的原地说明。
#
# 另有两处旧断言在此重写而非照搬：位移与朝向此前只判「读得到 Rotation」，
# 那对任何活着的实体都成立，是句空话；现在两者都取前后差值。
#
# ── 2026-08-17 第二次改动：死亡成为模型自己的事 ──────────────────────
# 自动重生（azalea 的 AutoRespawnPlugin）已在 machine/connect.rs 关掉，
# 死亡期间的动作面按原版收窄（dispatch 的生命闸门）。死亡段的判据随之从
# 「自动重生生效」翻成「躺得住 + 自己起得来」，理由写在该段原地。

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PAPER_JAR="${PAPER_JAR:-$REPO_ROOT/supplies/paper/26.1.2/paper-26.1.2-74.jar}"
JAVA_BIN="${JAVA_BIN:-$REPO_ROOT/supplies/tools/jdk-25/Contents/Home/bin/java}"
BASE="${BASE:-$REPO_ROOT/server-run/gate-b}"
SERVER_DIR="$BASE/server"
RUN_DIR="$BASE/run"
REPORT="$BASE/report.md"

BOT_NAME="${BOT_NAME:-MineIntentBot}"
FAKE_NAME="${FAKE_NAME:-Alice}"
# 指名 + 明确的位移请求：位移断言判的是「听懂并执行位移指令」这条纵切，
# 拿一句寒暄去要求模型走路，那是脚本在规定模型行为。
FAKE_MSG="${FAKE_MSG:-$BOT_NAME 你好，请向前走几步，然后说说你看到了什么}"
FAKE_MSG_AFTER_DEATH="${FAKE_MSG_AFTER_DEATH:-$BOT_NAME 你还好吗}"
KEY_FILE="${KEY_FILE:-}"
MODEL_ENDPOINT="${MODEL_ENDPOINT:-}"
MODEL_NAME="${MODEL_NAME:-}"
# 一轮对话的观察窗：真模型往返 + 工具执行 + 服务端落日志。
OBSERVE_SECS="${OBSERVE_SECS:-120}"
PORT="${PORT:-25565}"
# 死亡场景：plain=怪物致死后看模型自己决定复活；off=跳过。
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

# 前提不成立时如实标注「未取证」，不混进通过也不混进失败——
# 把没测的记成测过了，比测失败更坏。
unproven() {
  local name="$1" why="$2"
  RESULTS+=("未取证|$name|—|$why")
  printf '  [未取证] %s — %s\n' "$name" "$why"
}

console() { printf '%s\n' "$1" > "$SERVER_DIR/console.fifo"; sleep "${2:-1}"; }

srvlog() { cat "$SERVER_DIR/logs/latest.log" 2>/dev/null; }

entity_data() { srvlog | grep 'has the following entity data' | tail -1; }

# 从 `data get entity <name> Pos` 的回显取横向坐标（形如 [1.5d, 71.0d, -3.2d]）。
# 只取 x/z：重力下落会改 y，那不是工具生效的证据。
pos_xz() {
  console "data get entity $BOT_NAME Pos" 2
  entity_data | grep -oE '\[-?[0-9.]+d, *-?[0-9.]+d, *-?[0-9.]+d\]' \
    | tr -d '[]d' | awk -F',[[:space:]]*' '{print $1, $3}'
}

# 同上取朝向（形如 [-1.5f, 0.0f]）。
rot_yp() {
  console "data get entity $BOT_NAME Rotation" 2
  entity_data | grep -oE '\[-?[0-9.]+f, *-?[0-9.]+f\]' \
    | tr -d '[]f' | awk -F',[[:space:]]*' '{print $1, $2}'
}

# 横向位移距离。打印距离，退出码 0=超过阈值，1=没超过，2=读数缺失。
moved_xz() { # $1=前 $2=后 $3=阈值（格）
  awk -v a="$1" -v b="$2" -v t="$3" 'BEGIN{
    split(a, p, " "); split(b, q, " ");
    if (p[1] == "" || q[1] == "") { print "读数缺失"; exit 2 }
    d = sqrt((q[1]-p[1])^2 + (q[2]-p[2])^2);
    printf "%.2f 格", d;
    exit (d > t ? 0 : 1)
  }'
}

# 朝向变化量（yaw 按 ±180 环绕折算）。退出码同上。
turned() { # $1=前 $2=后 $3=阈值（度）
  awk -v a="$1" -v b="$2" -v t="$3" 'BEGIN{
    split(a, p, " "); split(b, q, " ");
    if (p[1] == "" || q[1] == "") { print "读数缺失"; exit 2 }
    dy = q[1] - p[1];
    while (dy > 180) dy -= 360;
    while (dy < -180) dy += 360;
    dp = q[2] - p[2];
    m = (dy < 0 ? -dy : dy); n = (dp < 0 ? -dp : dp);
    if (n > m) m = n;
    printf "yaw %+.1f° pitch %+.1f°", dy, dp;
    exit (m > t ? 0 : 1)
  }'
}

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
  # 先记住停在哪儿：下面 phase "cleanup" 会覆盖 PHASE，不先存起来的话
  # 报告里那一栏永远写着「中断阶段：cleanup」，等于没记。
  local stopped_at="$PHASE"
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
  #
  # 判定顺序在 2026-08-17 修正过两处（旧顺序是 COMPLETED → exit_code → FAILED）：
  #   - die() 必然发生在 COMPLETED=1 之前，所以旧顺序里「未判定（环境问题）」
  #     那一支永远进不去，环境错误一律报成 130，文件头写的「2=环境问题」
  #     从来没有兑现过。
  #   - 前置断言失败后脚本会 `exit 1` 早停，旧顺序把它归成「中断」。
  #     已经抓到实打实的失败，测没测完都改不了这个结论——那是不通过。
  # 「没测完」只在一种情况下才是结论：一条失败都没有的半途而废。
  local verdict
  if [ "$exit_code" -eq 2 ]; then verdict="未判定（环境问题）"
  elif [ "$FAILED" -ne 0 ]; then verdict="不通过"
  elif [ "$COMPLETED" -ne 1 ]; then verdict="中断（未跑完，不构成验收结论）"
  else verdict="通过"; fi

  # 报告目录可能还不存在：setup 的前置校验（缺 jar / 缺 JDK / 未接受 EULA）
  # 在 mkdir 之前就 die，此时写报告会失败，而末尾照样打印一条指向不存在
  # 文件的路径——「报告说了什么」和「报告在不在」不一致，比没有报告更坏。
  mkdir -p "$(dirname "$REPORT")" 2>/dev/null
  {
    printf '# gate b 纵向验收结果：%s\n\n' "$verdict"
    printf -- '- 停在阶段：%s\n' "$stopped_at"
    printf -- '- 模型：%s @ %s\n' "${MODEL_NAME:-组合根默认}" "${MODEL_ENDPOINT:-组合根默认}"
    printf -- '- 提交：%s\n' "$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo 未知)"
    printf -- '- 服务端：Paper 26.1.2 offline\n\n'
    printf '| 结论 | 断言 | 证据来源 | 实测 |\n|---|---|---|---|\n'
    local r
    for r in "${RESULTS[@]}"; do
      IFS='|' read -r st nm sc dt <<< "$r"
      printf '| %s | %s | %s | %s |\n' "$st" "$nm" "$sc" "$dt"
    done
  } > "$REPORT"
  printf '\n终态结论：%s（明细 %s）\n' "$verdict" "$REPORT"
  [ "$exit_code" -eq 2 ] && exit 2
  [ "$FAILED" -ne 0 ] && exit 1
  [ "$COMPLETED" -ne 1 ] && exit 130
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

# 新栈没有脚本化模型：组合根启动即 read_api_key，缺密钥直接退出。
# 所以密钥是环境前置，不是可选项。
[ -n "$KEY_FILE" ] && [ -f "$KEY_FILE" ] \
  || die "缺 KEY_FILE。新栈的组合根启动即要密钥（没有 scripted 模式），纵向验收必然打真模型。"
note "密钥按路径注入（不进命令行、不进日志、不进版本库）"

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

APP_BIN="$REPO_ROOT/target/debug/companion"
FAKE_BIN="$REPO_ROOT/target/debug/examples/fake_player"
# 两个目标必须在**同一次** cargo 调用里构建。分两次跑的话，`-p world` 那次
# 的依赖图里没有 midturn，tokio 等共享依赖的 feature 并集随之变小，azalea
# 整条链的指纹跟着变——来回切就整棵重编（实测每次约 1 分钟）。
#
# azalea 跟随 bevy 要 nightly；本仓有 rust-toolchain.toml，但只有走 rustup
# 的 cargo 才认它。系统上装了 Homebrew 的 stable cargo 时须显式指到 nightly。
[ -x "$APP_BIN" ] && [ -x "$FAKE_BIN" ] || die "缺二进制，先跑：
  cargo build -p companion -p world --features world/azalea --bin companion --example fake_player
（若报「Azalea currently requires nightly Rust」，说明用到了非 rustup 的 cargo：
  export PATH=\"\$HOME/.rustup/toolchains/nightly-x86_64-apple-darwin/bin:\$PATH\"）"

MODEL_ENV=(MINEINTENT_MODEL_API_KEY_FILE="$KEY_FILE")
[ -n "$MODEL_ENDPOINT" ] && MODEL_ENV+=(MODEL_ENDPOINT="$MODEL_ENDPOINT")
[ -n "$MODEL_NAME" ] && MODEL_ENV+=(MODEL_NAME="$MODEL_NAME")

APP_LOG="$RUN_DIR/companion-stdout.log"
MEMORY_FILE="$RUN_DIR/companion-memory.md"

# 组合根没有运行时限（只听 Ctrl+C），所以由本脚本决定何时收工：
# 观察窗跑完后发 SIGINT，在「正常停机」阶段判它走的是不是正常路径。
( cd "$RUN_DIR" && exec env MINEINTENT_HOST=127.0.0.1 MINEINTENT_PORT="$PORT" \
    MINEINTENT_USERNAME="$BOT_NAME" MINEINTENT_MEMORY_FILE="$MEMORY_FILE" \
    "${MODEL_ENV[@]}" \
    "$APP_BIN" > "$APP_LOG" 2>&1 ) & APP_PID=$!

if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME joined the game" 90; then
  SPAWN_AT=$(grep -oE "$BOT_NAME\[.*logged in with entity id [0-9]+ at \(.*\)" "$SERVER_DIR/logs/latest.log" | tail -1)
  assert "同伴进入世界" "服务端 log（非同伴自述）" 0 "${SPAWN_AT:-joined}"
else
  assert "同伴进入世界" "服务端 log（非同伴自述）" 1 "90s 内服务端未见 joined"
  exit 1
fi

# 落地静置：出生点下落期间的坐标变化不是工具生效，取基线前先等它站稳。
sleep 5
POS_BEFORE=$(pos_xz)
ROT_BEFORE=$(rot_yp)
note "基线：横向 (${POS_BEFORE:-读数缺失})、朝向 (${ROT_BEFORE:-读数缺失})"

( cd "$RUN_DIR" && exec "$FAKE_BIN" 127.0.0.1 "$PORT" "$FAKE_NAME" "$FAKE_MSG" \
    "$OBSERVE_SECS" > "$RUN_DIR/fake-stdout.log" 2>&1 ) & FAKE_PID=$!

wait_for "$SERVER_DIR/logs/latest.log" "$FAKE_NAME joined the game" 60 \
  || { assert "假人进入世界" "服务端 log" 1 "60s 内未见假人 joined"; exit 1; }
assert "假人进入世界" "服务端 log" 0 "$FAKE_NAME joined"

wait_for "$SERVER_DIR/logs/latest.log" "<$FAKE_NAME> .*$BOT_NAME" 60 \
  || { assert "假人发出指名聊天" "服务端 log" 1 "未见假人指名发言"; exit 1; }
assert "假人发出指名聊天" "服务端 log" 0 "$FAKE_MSG"

# 唤醒的判据是同伴在公屏上说话被服务端记录，且假人独立收到同一句。
# 这一条同时证明了整条纵切：聊天入窗 → 唤醒判据 → 内核轮 → 真模型 →
# chat_box 工具 → 世界。中间任何一环断了它都不成立。
if wait_for "$SERVER_DIR/logs/latest.log" "<$BOT_NAME> " "$OBSERVE_SECS"; then
  REPLY=$(srvlog | grep -oE "<$BOT_NAME> .*" | tail -1)
  assert "指名聊天唤醒并公屏回话" "服务端 log" 0 "$REPLY"
  # 假人是 Rust 进程，println! 写文件是块缓冲，落盘晚于服务端记录；
  # 这里必须等而不是立刻判，否则测的是缓冲时机不是观察能力。
  if wait_for "$RUN_DIR/fake-stdout.log" "\[聊天\] $BOT_NAME:" 30; then
    assert "回话被第三方独立观察到" "假人自身聊天流" 0 "假人收到同一句"
  else
    assert "回话被第三方独立观察到" "假人自身聊天流" 1 "30s 内假人未收到"
  fi
else
  assert "指名聊天唤醒并公屏回话" "服务端 log" 1 "${OBSERVE_SECS}s 内同伴未发言"
fi

# 马达类工具：效果必须在服务端实体数据上可见，而不是只在同伴自己的
# snapshot 里可见。位移与朝向都取前后差值——「读得到 Rotation」对任何
# 活着的实体都成立，那是句空话。
#
# 轮询而不是睡一个拍脑袋的固定值：forward 走的是寻路，到达要时间，
# 睡短了测的是寻路速度不是工具有没有生效。每轮约 4s（两条控制台命令
# 各等 2s），上限 15 轮。
MOVED=2; TURNED=2; DIST=""; TURN=""
for _ in $(seq 1 15); do
  [ "$MOVED" -ne 0 ] && {
    POS_AFTER=$(pos_xz); DIST=$(moved_xz "$POS_BEFORE" "$POS_AFTER" 0.5); MOVED=$?
  }
  [ "$TURNED" -ne 0 ] && {
    ROT_AFTER=$(rot_yp); TURN=$(turned "$ROT_BEFORE" "$ROT_AFTER" 1.0); TURNED=$?
  }
  [ "$MOVED" -eq 0 ] && [ "$TURNED" -eq 0 ] && break
done

case "$MOVED" in
  0) assert "位移指令在世界中真实生效" "服务端 data get Pos（前后差值）" 0 "横向移动 $DIST" ;;
  1) assert "位移指令在世界中真实生效" "服务端 data get Pos（前后差值）" 1 "横向仅移动 $DIST（阈值 0.5 格）" ;;
  *) assert "位移指令在世界中真实生效" "服务端 data get Pos（前后差值）" 1 "$DIST" ;;
esac

case "$TURNED" in
  0) assert "朝向变化在世界中真实生效" "服务端 data get Rotation（前后差值）" 0 "$TURN" ;;
  1) assert "朝向变化在世界中真实生效" "服务端 data get Rotation（前后差值）" 1 "$TURN（阈值 1°）" ;;
  *) assert "朝向变化在世界中真实生效" "服务端 data get Rotation（前后差值）" 1 "$TURN" ;;
esac

console "list" 2
if srvlog | grep -q "players online:.*$BOT_NAME"; then
  assert "同伴在服务端在线名单内" "服务端 list 命令" 0 "$(srvlog | grep -oE 'There are .*players online:.*' | tail -1)"
else
  assert "同伴在服务端在线名单内" "服务端 list 命令" 1 "list 未见同伴"
fi

# ------------------------------------------------------ 交叉印证
# 以下都取自同伴自己的输出，**不单独构成通过条件**；它们只回答
# 「服务端侧看到的现象，内部是不是按预期的路径产生的」。
phase "交叉印证（同伴自述，不构成通过条件）"

if TOOLS=$(grep -oE '\[组合根\] 工具表：.*' "$APP_LOG" | tail -1) && [ -n "$TOOLS" ]; then
  MISSING=""
  for t in chat_box inventory container remember motion look hand scan presence; do
    printf '%s' "$TOOLS" | grep -q "\"$t\"" || MISSING="$MISSING $t"
  done
  if [ -z "$MISSING" ]; then
    note "工具表九件齐全：chat_box inventory container remember motion look hand scan presence"
    RESULTS+=("交叉印证|工具表九件齐全|同伴 stdout|$TOOLS")
  else
    note "工具表缺:$MISSING"
    RESULTS+=("交叉印证|工具表缺齐全性|同伴 stdout|缺:$MISSING")
  fi
else
  note "未读到工具表输出"
fi

# 轮末帧（增量视口）：提交 26f20ca 自陈「组合根装配，无单测覆盖面」，
# 这里是它唯一的实盘观察点。空 diff 不投帧是设计（不制造废轮），
# 所以「没有帧」不判失败，只如实记。
if grep -q '轮末帧：' "$APP_LOG" 2>/dev/null; then
  FRAMES=$(grep -c '轮末帧：' "$APP_LOG")
  note "轮末帧投递 $FRAMES 次"
  RESULTS+=("交叉印证|轮末帧投递发生过|同伴 stdout|$FRAMES 次；$(grep -oE '轮末帧：.*' "$APP_LOG" | tail -1)")
else
  note "本轮无轮末帧（视野内无差异时不投帧是设计）"
  RESULTS+=("交叉印证|轮末帧未发生|同伴 stdout|空 diff 不投帧，非失败")
fi

# 记忆落盘是新栈唯一的持久产品事实。但把它写成通过条件等于要求模型
# 每轮都调 remember，那是脚本在规定模型行为——只作印证。
if [ -s "$MEMORY_FILE" ]; then
  note "记忆文件已写入（$(wc -c < "$MEMORY_FILE" | tr -d ' ') 字节）"
  RESULTS+=("交叉印证|remember 落盘|记忆文件|$(wc -c < "$MEMORY_FILE" | tr -d ' ') 字节")
else
  note "本轮模型未调用 remember（不构成失败）"
  RESULTS+=("交叉印证|remember 未调用|记忆文件|文件为空或不存在")
fi

# --------------------------------------- 死亡（持续状态 + 模型自己决定复活）
# 用怪物打死而不是 /kill：伤害→掉血→死亡整条观察链都要真实经过。
#
# 2026-08-17 起自动重生已关（machine/connect.rs），死亡是**持续状态**：
# 不调 presence 工具就一直躺着。这把判据从「自动重生生效」翻成了两条更强的：
#
#   一、躺得住：致死后一段时间内服务端 Health 保持 0。这是自动重生真的关掉了
#       的唯一服务端侧证据。它同时让状态呈现变得可观察——自动重生在时，
#       14→0→20 压在一个 tick 里，每 tick 采样的 alive 几乎必然错过 false
#       （state.rs 的 track_health 为此把伤害改成包驱动）。
#   二、自己起得来：随后 Health 由 0 回满。自动重生既然关了，复活包的唯一
#       发出者就是模型调的 presence——服务端看到回满即证明那条链走通了。
#       比旧脚本的「自动重生生效」强得多：那条连模型都不经过。
#
# 死亡期间的动作面按原版收窄（26.1.2 客户端字节码考证）：看得见世界、听得见
# 声音、收得到别人说话（死亡屏不暂停），但开不了口（handleKeybinds 只在
# screen == null 时调用）。所以下面「说话」这条只可能发生在复活之后——
# 它同时是历史故障「死后失聪」的回归。
if [ "$DEATH_SCENARIO" != "off" ]; then
  phase "死亡恢复"
  [ -n "$FAKE_PID" ] && kill -TERM "$FAKE_PID" 2>/dev/null; FAKE_PID=""

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

  if [ "$DIED" -ne 0 ]; then
    # 前提不成立时这三条判定无意义：活着的实体本来就满血、本来就能说话，
    # 据此判过是假阳性。
    unproven "死亡是持续状态（不会自己恢复）" "未致死，前提不成立"
    unproven "模型自己决定复活并生效" "未致死，前提不成立"
    unproven "复活后仍能被唤醒说话" "未致死，前提不成立"
  else
    # 一、躺得住。自动重生若还开着，Health 会在死亡同一个 tick 内就回满，
    # 这一轮轮询必然当场看到满值。所以「一段时间内保持 0」就是它确实关了。
    STAYED_DEAD=0
    for _ in $(seq 1 10); do
      console "data get entity $BOT_NAME Health" 1
      if entity_data | grep -qE ' (20|19|18|17|16)\.[0-9]+f'; then
        STAYED_DEAD=1; break
      fi
    done
    if [ "$STAYED_DEAD" -eq 0 ]; then
      assert "死亡是持续状态（不会自己恢复）" "服务端 data get Health（10 次轮询）" 0 \
        "十余秒内 Health 未自行回满"
    else
      assert "死亡是持续状态（不会自己恢复）" "服务端 data get Health（10 次轮询）" 1 \
        "生命值自行回满了，自动重生疑似仍开着：$(entity_data)"
    fi

    # 二、自己起得来。自动重生已关，复活包的唯一发出者就是模型调的 presence；
    # 服务端看到 Health 由 0 回满，即证明「死亡入模型 → 模型决定 → 工具 →
    # 世界」整条链走通了。不用「公屏上出现某句话」间接证明：实测重生会把
    # 作用域推到下一代，重生前入队的发言作为旧作用域遗留被正确拦掉，
    # 拿一个必然失败的现象当判据是错的。
    RESPAWNED=1
    for _ in $(seq 1 "$OBSERVE_SECS"); do
      console "data get entity $BOT_NAME Health" 1
      if entity_data | grep -qE ' (20|19|18|17|16)\.[0-9]+f'; then
        RESPAWNED=0; break
      fi
    done
    if [ "$RESPAWNED" -eq 0 ]; then
      assert "模型自己决定复活并生效" "服务端 data get Health（自动重生已关）" 0 \
        "$(entity_data | grep -oE '[0-9.]+f' | tail -1)"
    else
      assert "模型自己决定复活并生效" "服务端 data get Health（自动重生已关）" 1 \
        "${OBSERVE_SECS} 次轮询仍未回满，模型未复活：$(entity_data)"
    fi

    # 交叉印证：死亡作为**事件**有没有到模型面前。伤害窗是包驱动的，
    # 按 state.rs 的说法它不该漏；漏了就说明那条「不采样所以不漏」的
    # 理由在实盘上不成立，是要查的事。不判失败，如实记。
    #
    # 状态那条（alive → 「你已经死亡。」）本脚本仍看不到——它只进上下文不进
    # stdout。但自动重生关掉之后它至少变得**采得到**了，要验它得先有
    # model-io 落盘。
    if grep -q '你死了' "$APP_LOG" 2>/dev/null; then
      note "死亡事件已投给模型：$(grep -oE '→ .*你死了。*' "$APP_LOG" | tail -1)"
      RESULTS+=("交叉印证|死亡事件到达模型|同伴 stdout 的投递原文|$(grep -oE '→ .*你死了[^⏎]*' "$APP_LOG" | tail -1)")
    else
      note "同伴投递行里没有死亡事件——伤害窗声称包驱动不漏，这里没到，值得查"
      RESULTS+=("交叉印证|死亡事件未到达模型|同伴 stdout 的投递原文|伤害窗声称包驱动不漏，实盘未见，待查")
    fi

    # 回归目标：历史上死过一次之后同伴就再也不响应了（死后失聪）。
    # 判据仍是服务端看到它公屏发言，且必须是死亡消息之后的新发言。
    #
    # 注意这条现在依赖上一条：死亡期间 chat_box 被生命闸门拦着（对齐原版，
    # 死人开不了口），所以只有复活了才可能说话。上一条不过，这条必然不过——
    # 那不是重复判定，是「聋」与「哑」两件事在新口径下的先后关系。
    console "difficulty easy" 1
    console "time set day" 1
    REPLIES_BEFORE=$(srvlog | grep -cE "<$BOT_NAME> ")
    ( cd "$RUN_DIR" && exec "$FAKE_BIN" 127.0.0.1 "$PORT" "$FAKE_NAME" \
        "$FAKE_MSG_AFTER_DEATH" 90 > "$RUN_DIR/fake-after-death.log" 2>&1 ) & FAKE_PID=$!
    wait_for "$SERVER_DIR/logs/latest.log" "<$FAKE_NAME> .*$BOT_NAME" 60 \
      || note "假人重进后未见指名发言（下面的判定仍按发言数比较）"

    SPOKE=1
    for _ in $(seq 1 "$OBSERVE_SECS"); do
      [ "$(srvlog | grep -cE "<$BOT_NAME> ")" -gt "$REPLIES_BEFORE" ] && { SPOKE=0; break; }
      sleep 1
    done
    if [ "$SPOKE" -eq 0 ]; then
      assert "死后仍能被唤醒说话（回归：死后失聪）" "服务端 log（重生后的新发言）" 0 \
        "$(srvlog | grep -oE "<$BOT_NAME> .*" | tail -1)"
    else
      assert "死后仍能被唤醒说话（回归：死后失聪）" "服务端 log（重生后的新发言）" 1 \
        "${OBSERVE_SECS}s 内未见重生后的新发言"
    fi
    kill -TERM "$FAKE_PID" 2>/dev/null; FAKE_PID=""
  fi
fi

# ------------------------------------------------------ 正常停机
# 组合根只听 Ctrl+C（SIGINT）：cleanup 用的 SIGTERM 走的是默认终止，
# 判「正常路径」必须发 SIGINT。
phase "正常停机"
kill -INT "$APP_PID" 2>/dev/null || note "同伴已先行退出"
i=0; while [ "$i" -lt 90 ] && kill -0 "$APP_PID" 2>/dev/null; do sleep 2; i=$((i+2)); done

if kill -0 "$APP_PID" 2>/dev/null; then
  assert "收到 SIGINT 后按计划停机" "同伴进程状态" 1 "90s 内未退出"
else
  APP_PID=""
  if wait_for "$SERVER_DIR/logs/latest.log" "$BOT_NAME lost connection" 30; then
    assert "停机后服务端观察到干净断开" "服务端 log" 0 "$(srvlog | grep -oE "$BOT_NAME lost connection.*" | tail -1)"
  else
    assert "停机后服务端观察到干净断开" "服务端 log" 1 "服务端未记录断开"
  fi
  if grep -q '已停机' "$APP_LOG" 2>/dev/null; then
    assert "停机走正常路径而非崩溃" "同伴退出输出（交叉印证）" 0 "已停机"
  else
    assert "停机走正常路径而非崩溃" "同伴退出输出（交叉印证）" 1 "未见正常停机标记"
  fi
fi

if [ "$TEMPLATE_USED" -eq 1 ]; then
  cp -R "$SERVER_DIR/world" "$TEMPLATE" 2>/dev/null && note "已存世界模板供后续复跑"
fi

COMPLETED=1
exit 0
