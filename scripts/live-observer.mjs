/**
 * 现场观察者：以测试玩家身份进服、发一句话、记录同伴的回复与真实动作轨迹。
 *
 * 它替代的是「人坐在客户端里打字并盯着看」这一环，产出三路证据里的第三路：
 *   1. 服务端控制台  —— 唯一的独立真值（实体坐标、方块查询），由调用方自行采集
 *   2. .mineintent/events.jsonl —— 运行时实际做了什么
 *   3. 本脚本 —— 玩家能观察到的：可见动作与聊天
 *
 * 判断「像不像人」仍然需要真人；本脚本只负责让「动没动、走到哪、说了什么」
 * 可复现、可比较。
 *
 * 用法：node scripts/live-observer.mjs ["要说的话"] [秒数]
 * 前置：Paper 已运行、agent-service 已运行、同伴已连接；通过 MINEINTENT_TEST_PLAYER
 *       使用下面的专用测试身份。
 */
import { readFileSync } from 'node:fs'
import mineflayer from 'mineflayer'

const env = Object.fromEntries(
  readFileSync(new URL('../.env', import.meta.url), 'utf8')
    .split(/\r?\n/)
    .filter(line => line && !line.startsWith('#') && line.includes('='))
    .map(line => { const i = line.indexOf('='); return [line.slice(0, i).trim(), line.slice(i + 1).trim()] }),
)

// 场景参数。改这里而不是改正文；坐标要和 setup 时用服务端命令摆放的一致。
const SCENE = {
  /** 观察者的旁观机位：能同时看到同伴和目标，且不挡路。 */
  observerTeleport: '1162 90 966 -90 20',
  /** 目标实体的 CustomName；用它精确定位，避免抓到世界里的同类实体。 */
  targetName: 'TestSheep',
  /** 姿态采样间隔，毫秒。 */
  sampleIntervalMs: 250,
}

const PROMPT = process.argv[2] ?? '看看那只羊，然后走过去'
const RUN_SECONDS = Number(process.argv[3] ?? 90)
const BOT = env.MINEINTENT_MC_USERNAME ?? 'MineIntentBot'
// 操作者应提供专用测试身份，不借用真人玩家名：真人在线时会被顶号，同伴的长期记忆
// 和关系状态也不该把自动化流量记成真人交互。脚本无法从用户名本身判断它是否属于真人。
const TESTER = process.env.MINEINTENT_TEST_PLAYER ?? env.MINEINTENT_TEST_PLAYER ?? 'LiveTester'

const t0 = Date.now()
const stamp = () => `+${((Date.now() - t0) / 1000).toFixed(2)}s`
const log = (tag, detail) => console.log(`${stamp().padStart(9)} ${tag.padEnd(12)} ${detail}`)

const observer = mineflayer.createBot({
  host: env.MINEINTENT_MC_HOST ?? '127.0.0.1',
  port: Number(env.MINEINTENT_MC_PORT ?? 25565),
  username: TESTER,
  auth: 'offline',
  version: '1.21.1',
})

const degrees = radians => Number((radians * 180 / Math.PI).toFixed(1))
/** 最短弧长：yaw 从 179° 走到 -179° 是转了 2°，不是 358°。 */
const angleDelta = (a, b) => Math.abs(((a - b + 540) % 360) - 180)
const findTarget = () => Object.values(observer.entities).find(entity =>
  entity.name === 'sheep'
  && entity.metadata?.some?.(field => JSON.stringify(field ?? '').includes(SCENE.targetName)))

let previous = null

function sampleBot() {
  const entity = observer.players[BOT]?.entity
  if (!entity) return
  const now = {
    x: Number(entity.position.x.toFixed(2)),
    y: Number(entity.position.y.toFixed(2)),
    z: Number(entity.position.z.toFixed(2)),
    yaw: degrees(entity.yaw),
    pitch: degrees(entity.pitch),
  }
  if (previous) {
    const horizontal = Math.hypot(now.x - previous.x, now.z - previous.z)
    const vertical = now.y - previous.y
    // 三维位移：只算水平会漏掉坠落，而坠落会改变目标的仰角并把它挤出视锥。
    const moved = Math.hypot(horizontal, vertical)
    const turned = angleDelta(now.yaw, previous.yaw) + angleDelta(now.pitch, previous.pitch)
    if (moved < 0.05 && turned < 1) return
    const climb = vertical < -0.05 ? ` ↓${(-vertical).toFixed(2)}` : vertical > 0.05 ? ` ↑${vertical.toFixed(2)}` : ''
    log('BOT_POSE', `pos=(${now.x}, ${now.y}, ${now.z}) yaw=${now.yaw}° pitch=${now.pitch}°`
      + ` | Δ位移=${moved.toFixed(2)}格(水平${horizontal.toFixed(2)}${climb}) Δ转向=${turned.toFixed(1)}°`)
  } else {
    log('BOT_POSE', `初始 pos=(${now.x}, ${now.y}, ${now.z}) yaw=${now.yaw}° pitch=${now.pitch}°`)
  }
  previous = now
}

observer.once('spawn', async () => {
  log('OBSERVER', `已进入世界，用户名 ${observer.username}`)
  observer.chat('/gamemode spectator')
  await new Promise(resolve => setTimeout(resolve, 800))
  observer.chat(`/tp @s ${SCENE.observerTeleport}`)
  await new Promise(resolve => setTimeout(resolve, 1500))

  const target = findTarget()
  log('SCENE', target
    ? `${SCENE.targetName} 在 (${target.position.x.toFixed(1)}, ${target.position.y.toFixed(1)}, ${target.position.z.toFixed(1)})`
    : `视野内没有找到 ${SCENE.targetName}`)
  sampleBot()

  const timer = setInterval(sampleBot, SCENE.sampleIntervalMs)
  await new Promise(resolve => setTimeout(resolve, 500))

  log('PLAYER_SAY', PROMPT)
  observer.chat(PROMPT)

  setTimeout(() => {
    clearInterval(timer)
    const entity = observer.players[BOT]?.entity
    const target = entity && findTarget()
    if (target) log('RESULT', `结束时同伴与 ${SCENE.targetName} 相距 ${entity.position.distanceTo(target.position).toFixed(2)} 格`)
    log('OBSERVER', '结束')
    observer.quit()
    setTimeout(() => process.exit(0), 500)
  }, RUN_SECONDS * 1000)
})

observer.on('chat', (username, message) => {
  if (username === observer.username) return
  log(username === BOT ? 'BOT_REPLY' : 'CHAT', `<${username}> ${message}`)
})

observer.on('kicked', reason => log('KICKED', String(reason)))
observer.on('error', error => log('ERROR', error.message))
