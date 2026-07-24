import assert from 'node:assert/strict'
import path from 'node:path'
import { copyFileSync, existsSync, rmSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import mineflayer, { type Bot } from 'mineflayer'
import type { BackendEventEnvelope, BackendLifecyclePayload } from '../minecraft/contracts.js'
import { defaultMinecraftBackendConfig } from '../minecraft/config.js'
import { MinecraftBackend } from '../minecraft/minecraft-backend.js'
import { JsonlIntegrationRecorder } from './recorder.js'
import { PaperProcessServer } from './paper-process-server.js'
import { PaperScenarioRunner } from './scenario-runner.js'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..')
const runId = process.env.GITHUB_RUN_ID ?? `local-${Date.now()}`
const artifactsRoot = path.resolve(process.env.MC_ARTIFACTS_DIR ?? path.join(root, '.artifacts', 'paper'))
const runtimeDirectory = path.join(artifactsRoot, runId, 'server')
const templateDirectory = required('MC_SERVER_TEMPLATE')
const port = Number(process.env.MC_PORT ?? 25566)
const username = process.env.MC_USERNAME ?? 'MineIntentBotCI'
const recorder = new JsonlIntegrationRecorder(artifactsRoot, runId)
const server = new PaperProcessServer({
  java: required('MC_JAVA'), jar: required('MC_SERVER_JAR'), directory: runtimeDirectory, port,
  eulaAccepted: process.env.MC_EULA === 'true', templateDirectory, startupTimeoutMs: 180_000, stopTimeoutMs: 90_000,
})

async function main(): Promise<void> {
  const results = []
  let backend: MinecraftBackend | undefined
  try {
    await ensureTemplate()
    server.prepareFresh()
    recorder.record('suite', 'setup', 'paper_runtime_prepared', { port, runtimeDirectory, templateDirectory })
    await server.start()
    recorder.record('suite', 'setup', 'paper_started', { port })

    const lifecycle = await lifecycleScenario()
    results.push(lifecycle.result)
    backend = lifecycle.backend
    await backend.stop('lifecycle_scenario_complete')
    backend = undefined

    const bot = await connectBot(username)
    try { results.push(...await behaviorScenarios(bot)) }
    finally { bot.quit('integration_complete') }

    assert.equal(results.every(result => result.status === 'passed'), true, JSON.stringify(results))
  } finally {
    if (backend && backend.state().status !== 'stopped') await backend.stop('suite_cleanup')
    await server.stop()
    recorder.record('suite', 'cleanup', 'paper_stopped', {})
    recorder.writeSummary(results)
    const consoleLog = path.join(runtimeDirectory, 'console.log')
    if (existsSync(consoleLog)) copyFileSync(consoleLog, path.join(recorder.directory, 'paper-console.log'))
    rmSync(runtimeDirectory, { recursive: true, force: true })
    recorder.record('suite', 'cleanup', 'paper_runtime_removed', { runtimeDirectory })
  }
}

// D40 behavior needs a separate live experiment; this suite keeps reusable Paper/protocol checks.
async function ensureTemplate(): Promise<void> {
  if (existsSync(path.join(templateDirectory, 'world', 'level.dat'))) {
    recorder.record('template', 'setup', 'template_reused', { templateDirectory })
    return
  }
  const builder = new PaperProcessServer({
    java: required('MC_JAVA'), jar: required('MC_SERVER_JAR'), directory: templateDirectory, port,
    eulaAccepted: process.env.MC_EULA === 'true', startupTimeoutMs: 180_000, stopTimeoutMs: 90_000,
  })
  recorder.record('template', 'setup', 'template_generation_started', { templateDirectory })
  builder.prepareFresh()
  try { await builder.start() } finally { await builder.stop() }
  writeFileSync(path.join(templateDirectory, 'mineintent-template.json'), `${JSON.stringify({ minecraft: '1.21.1', paperBuild: 133, createdAt: new Date().toISOString() }, null, 2)}\n`, 'utf8')
  recorder.record('template', 'cleanup', 'template_generation_completed', { templateDirectory })
}

async function lifecycleScenario() {
  const events: BackendEventEnvelope[] = []
  const backend = new MinecraftBackend({
    worldId: 'paper-ci-1.21.1', server: { host: '127.0.0.1', port, version: '1.21.1' },
    identity: { username, auth: 'offline' }, ...defaultMinecraftBackendConfig,
    timeouts: { connectMs: 10_000, loginMs: 20_000, spawnMs: 60_000, stopMs: 5_000 },
    reconnect: { ...defaultMinecraftBackendConfig.reconnect, initialDelayMs: 250, maxDelayMs: 1_000, stableResetMs: 2_000 },
  })
  backend.subscribe(event => { events.push(event); recorder.record('lifecycle-death-reconnect', 'companion', 'backend_event', event) })
  const runner = new PaperScenarioRunner(recorder)
  const result = await runner.run({
    name: 'lifecycle-death-reconnect', timeoutMs: 180_000,
    setup: async ctx => { ctx.record('setup', 'known_state', { freshWorld: true, port }) },
    run: async ctx => {
      const ready = await backend.start(ctx.signal)
      assert.equal(ready.snapshot.world.minecraftVersion, '1.21.1')
      const firstEpoch = ready.connectionEpoch
      server.send(`kill ${username}`)
      await waitFor(events, event => lifecycleType(event) === 'died', 'death')
      await waitFor(events, event => lifecycleType(event) === 'respawned', 'respawn')
      await server.restart()
      await waitFor(events, event => lifecycleType(event) === 'ready' && event.connectionEpoch > firstEpoch, 'reconnected ready', 90_000)
      assert.equal(backend.snapshot().connectionEpoch > firstEpoch, true)
      ctx.record('assertion', 'lifecycle_verified', { firstEpoch, finalEpoch: backend.snapshot().connectionEpoch })
    },
    cleanup: async ctx => ctx.record('cleanup', 'backend_cleanup_deferred', {}),
  })
  return { result, backend }
}

async function behaviorScenarios(bot: Bot) {
  const runner = new PaperScenarioRunner(recorder)
  const results = []
  results.push(await runner.run({
    name: 'movement', timeoutMs: 30_000,
    setup: async ctx => { await fixture(bot); ctx.record('setup', 'fixture_ready', {}) },
    run: async ctx => {
      const start = bot.entity.position.clone()
      await bot.lookAt(start.offset(6, 0, 0), true)
      bot.setControlState('forward', true)
      try { await waitUntil(() => bot.entity.position.distanceTo(start) >= 2, 10_000, 'two-block movement') }
      finally { bot.setControlState('forward', false) }
      ctx.record('assertion', 'movement_verified', { distance: bot.entity.position.distanceTo(start) })
    }, cleanup: async () => bot.clearControlStates(),
  }))
  results.push(await runner.run({
    name: 'movement-cancellation', timeoutMs: 30_000,
    setup: async () => fixture(bot),
    run: async ctx => {
      await bot.lookAt(bot.entity.position.offset(6, 0, 0), true)
      bot.setControlState('forward', true); await delay(250); bot.clearControlStates()
      const cancelledAt = bot.entity.position.clone(); await delay(750)
      const drift = bot.entity.position.distanceTo(cancelledAt)
      assert.equal(drift < 0.35, true, `Bot drifted ${drift} blocks after cancellation`)
      ctx.record('assertion', 'cancellation_verified', { drift })
    }, cleanup: async () => bot.clearControlStates(),
  }))
  results.push(await runner.run({
    name: 'dig-and-inventory', timeoutMs: 45_000,
    setup: async ctx => {
      await fixture(bot); server.send(`clear ${username}`); server.send(`give ${username} minecraft:iron_pickaxe 1`)
      server.send(`execute at ${username} run setblock ~1 ~ ~ minecraft:stone`)
      await waitUntil(() => bot.inventory.items().some(item => item.name === 'iron_pickaxe'), 10_000, 'iron pickaxe inventory')
      await waitUntil(() => bot.blockAt(bot.entity.position.offset(1, 0, 0).floored())?.name === 'stone', 10_000, 'dig target block')
      ctx.record('setup', 'dig_fixture_ready', { relativeBlock: [1, 0, 0] })
    },
    run: async ctx => {
      const pickaxe = bot.inventory.items().find(item => item.name === 'iron_pickaxe'); assert.ok(pickaxe)
      await bot.equip(pickaxe, 'hand')
      const block = bot.blockAt(bot.entity.position.offset(1, 0, 0).floored()); assert.equal(block?.name, 'stone')
      await bot.dig(block)
      await waitUntil(() => bot.blockAt(block.position)?.name === 'air', 10_000, 'stone removal')
      const pickupPosition = block.position.offset(0.5, 0, 0.5)
      await bot.lookAt(pickupPosition, true)
      bot.setControlState('forward', true)
      try { await waitUntil(() => bot.entity.position.distanceTo(pickupPosition) < 0.45, 5_000, 'drop pickup position') }
      finally { bot.setControlState('forward', false) }
      await waitUntil(() => bot.inventory.items().some(item => item.name === 'cobblestone'), 10_000, 'cobblestone inventory')
      ctx.record('assertion', 'dig_inventory_verified', { inventory: bot.inventory.items().map(item => ({ name: item.name, count: item.count })) })
    }, cleanup: async () => { bot.stopDigging(); server.send(`clear ${username}`) },
  }))
  return results
}

async function fixture(bot: Bot): Promise<void> {
  bot.clearControlStates()
  server.send(`execute at ${username} run fill ~-4 ~-1 ~-4 ~8 ~-1 ~4 minecraft:stone`)
  server.send(`execute at ${username} run fill ~-4 ~ ~-4 ~8 ~3 ~4 minecraft:air`)
  await waitUntil(() => bot.blockAt(bot.entity.position.offset(0, -1, 0).floored())?.name === 'stone', 10_000, 'fixture platform')
  await delay(100)
}

async function connectBot(name: string): Promise<Bot> {
  const bot = mineflayer.createBot({ host: '127.0.0.1', port, username: name, auth: 'offline', version: '1.21.1', logErrors: false })
  await new Promise<void>((resolve, reject) => { bot.once('spawn', resolve); bot.once('error', reject); bot.once('kicked', reason => reject(new Error(String(reason)))) })
  recorder.record('behavior-suite', 'companion', 'bot_connected', { username: name })
  return bot
}

function lifecycleType(event: BackendEventEnvelope): string | undefined { return event.kind === 'lifecycle' ? (event.payload as BackendLifecyclePayload).type : undefined }
async function waitFor(events: BackendEventEnvelope[], predicate: (event: BackendEventEnvelope) => boolean, description: string, timeout = 30_000) { return waitUntil(() => events.find(predicate), timeout, description) }
async function waitUntil<T>(predicate: () => T | undefined | false, timeoutMs: number, description: string): Promise<T> { const start = Date.now(); while (Date.now() - start < timeoutMs) { const value = predicate(); if (value) return value; await delay(25) } throw new Error(`Timed out waiting for ${description}`) }
function delay(ms: number): Promise<void> { return new Promise(resolve => setTimeout(resolve, ms)) }
function required(name: string): string { const value = process.env[name]; if (!value) throw new Error(`${name} is required`); return path.resolve(value) }

await main()
