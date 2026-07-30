import assert from 'node:assert/strict'
import path from 'node:path'
import { copyFileSync, existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import mineflayer, { type Bot } from 'mineflayer'
import { randomUUID } from 'node:crypto'
import { BackendPerceptionPort } from '../companion/information-adapters.js'
import { CompanionRuntime } from '../companion/runtime.js'
import { JsonlEventJournal } from '../events/journal.js'
import { raycastLookedAtBlock } from '../information/source-ports/perception.js'
import { FileMemoryStore } from '../memory/memory-store.js'
import type { BackendEventEnvelope, BackendLifecyclePayload } from '../minecraft/contracts.js'
import { defaultMinecraftBackendConfig } from '../minecraft/config.js'
import { MinecraftBackend } from '../minecraft/minecraft-backend.js'
import { DebugStateStore } from '../telemetry/debug-state.js'
import { JsonlIntegrationRecorder } from './recorder.js'
import { PaperProcessServer } from './paper-process-server.js'
import { PaperScenarioRunner } from './scenario-runner.js'
import { PrototypeScenarioModel } from './prototype-scenario-model.js'

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

    results.push(await companionPrototypeScenario())

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

async function companionPrototypeScenario() {
  const runner = new PaperScenarioRunner(recorder)
  const companionName = 'IntentBotCI', playerName = 'IntentPlayerCI'
  const dataDirectory = path.join(recorder.directory, 'prototype-data')
  const memoryFile = path.join(dataDirectory, 'memories.json')
  let player: Bot | undefined
  let runtime: CompanionRuntime | undefined
  let backend: MinecraftBackend | undefined
  return runner.run({
    name: 'companion-v0.3-grounded-gaze', timeoutMs: 240_000,
    setup: async ctx => {
      player = await connectBot(playerName)
      server.send(`execute at ${playerName} run fill ~-8 ~-1 ~-8 ~12 ~-1 ~8 minecraft:stone`)
      server.send(`execute at ${playerName} run fill ~-8 ~ ~-8 ~12 ~5 ~8 minecraft:air`)
      for (let index = 3; index <= 9; index++) server.send(`execute at ${playerName} run setblock ~${index} ~ ~ minecraft:oak_log`)
      await waitUntil(() => player!.blockAt(player!.entity.position.offset(0, -1, 0).floored())?.name === 'stone', 10_000, 'prototype platform')
      ctx.record('setup', 'prototype_fixture_ready', { playerName, companionName, logCount: 7 })
    },
    run: async ctx => {
      const messages: string[] = []
      player!.on('chat', (sender, message) => { if (sender === companionName) messages.push(message) })
      const firstModel = new PrototypeScenarioModel()
      const first = createPrototypeRuntime(companionName, playerName, firstModel, memoryFile, dataDirectory, 'first')
      runtime = first.runtime; backend = first.backend
      await runtime.start()
      server.send(`tp ${companionName} ${player!.entity.position.x} ${player!.entity.position.y} ${player!.entity.position.z + 1}`)
      await waitUntil(() => distance(backend!.snapshot().self.position, player!.entity.position) < 3, 10_000, 'companion fixture teleport')
      await waitUntil(() => messages.some(message => message.includes('我来了')), 10_000, 'natural greeting')

      server.send(`tp ${companionName} ${player!.entity.position.x} ${player!.entity.position.y} ${player!.entity.position.z + 4} 0 0`)
      await waitUntil(() => distance(backend!.snapshot().self.position, player!.entity.position) > 3.5, 10_000, 'gaze fixture position')
      const gazeMessageStart = messages.length
      player!.chat('看向我')
      await waitUntil(() => messages.slice(gazeMessageStart).some(message => message.includes('看到你了')), 20_000, 'verified visual attention')
      const self = backend.snapshot().self
      const expectedYaw = Math.atan2(
        -(player!.entity.position.x - self.position.x),
        -(player!.entity.position.z - self.position.z),
      )
      assert.equal(yawDistance(self.yaw, expectedYaw) < 0.15, true, `gaze yaw was ${self.yaw}, expected ${expectedYaw}`)
      const firstJournal = path.join(dataDirectory, 'events-first.jsonl')
      const gazeEvents = readFileSync(firstJournal, 'utf8').trim().split(/\r?\n/u).map(line => JSON.parse(line) as { type: string; payload: any })
      assert.equal(gazeEvents.some(event => event.type === 'embodiment.controller.evidence' && event.payload.stage === 'outcome_verified'), true)
      assert.equal(gazeEvents.some(event => event.type === 'embodiment.controller.terminal' && event.payload.status === 'completed'), true)
      ctx.record('assertion', 'visual_attention_verified', { yawError: yawDistance(self.yaw, expectedYaw) })

      const blockTarget = blockAtRelativeYaw(self, 0.5, 3.5)
      const yawBeforeBlockAttention = self.yaw
      server.send(`setblock ${blockTarget.x} ${blockTarget.y} ${blockTarget.z} minecraft:gold_block`)
      await waitUntil(() => {
        const block = backend!.observationSource().readBlock(blockTarget)
        return block.status === 'loaded' && block.block.name === 'gold_block'
      }, 10_000, 'relative-coordinate block fixture')
      const blockMessageStart = messages.length
      player!.chat('看向金块')
      await waitUntil(() => messages.slice(blockMessageStart).some(message => message.includes('看见金块了')), 20_000, 'grounded block attention')
      const selection = firstModel.viewportSelections.at(-1)
      assert.ok(selection)
      assert.equal(selection.name, 'gold_block')
      assert.equal(selection.relativePosition.every(value => Number.isInteger(value * 2)), true)
      const hit = raycastLookedAtBlock(new BackendPerceptionPort(backend), 4.5)
      assert.equal(hit?.name, 'gold_block')
      assert.deepEqual(hit?.position, blockTarget)
      assert.equal(yawDistance(yawBeforeBlockAttention, backend.snapshot().self.yaw) > 0.2, true)
      const blockEvents = readFileSync(firstJournal, 'utf8').trim().split(/\r?\n/u).map(line => JSON.parse(line) as { type: string; payload: any })
      const blockGrounding = blockEvents.find(event =>
        event.type === 'embodiment.grounding.finished' && event.payload.effectId === 'embodied_gaze_block')
      assert.equal(blockGrounding?.payload.status, 'grounded')
      assert.equal(blockGrounding?.payload.evidenceIds.includes(selection.readId), true)
      assert.equal(blockEvents.some(event =>
        event.type === 'embodiment.controller.terminal' && event.payload.effectId === 'embodied_gaze_block' &&
        event.payload.status === 'completed' && event.payload.reasonCode === 'visual_attention_verified'), true)
      ctx.record('assertion', 'relative_coordinate_ref_verified', {
        relativePosition: selection.relativePosition,
        readId: selection.readId,
        targetName: hit?.name,
      })

      const woodBeforeProposal = woodCount(backend.snapshot())
      player!.chat('一起收集些木头吧')
      await waitUntil(() => messages.some(message => message.includes('先观察')), 15_000, 'V2 observation response')
      assert.equal(runtime.activity()?.status, 'proposed')
      assert.equal(first.debug.snapshot().currentBehavior, undefined)
      assert.equal(woodCount(backend.snapshot()), woodBeforeProposal)
      ctx.record('assertion', 'legacy_skill_fallback_absent', { wood: woodBeforeProposal })

      player!.chat('等一下')
      await waitUntil(() => messages.some(message => message.includes('停下')), 5_000, 'pause acknowledgement')
      ctx.record('assertion', 'stop_verified', { wood: woodCount(backend.snapshot()) })

      server.send(`damage ${companionName} 13`)
      await waitUntil(() => messages.some(message => message.includes('受伤了')), 15_000, 'danger warning')
      server.send(`effect give ${companionName} minecraft:instant_health 1 5 true`)
      await waitUntil(() => backend!.snapshot().self.health > 8, 10_000, 'companion healed')
      ctx.record('assertion', 'danger_response_verified', { health: backend.snapshot().self.health })

      player!.chat('记住我们今天一起开始收集木材')
      await waitUntil(async () => (await new FileMemoryStore(memoryFile).list('paper-ci-v0.3')).length === 1, 15_000, 'memory candidate accepted')
      const memories = await new FileMemoryStore(memoryFile).list('paper-ci-v0.3')
      assert.equal(memories.length, 1)
      assert.equal(memories[0]!.evidence.some(evidence => evidence.kind === 'event'), true)
      ctx.record('assertion', 'first_session_verified', { memoryId: memories[0]!.id })

      await runtime.stop('prototype_restart')
      runtime = undefined; backend = undefined
      await delay(500)

      const secondModel = new PrototypeScenarioModel()
      const second = createPrototypeRuntime(companionName, playerName, secondModel, memoryFile, dataDirectory, 'second')
      runtime = second.runtime; backend = second.backend
      await runtime.start()
      server.send(`tp ${companionName} ${player!.entity.position.x} ${player!.entity.position.y} ${player!.entity.position.z + 1}`)
      await waitUntil(() => secondModel.contexts[0]?.fragments.filter(fragment => fragment.section === 'retrieved_memories').length === 1, 15_000, 'startup memory retrieval')
      const messageStart = messages.length
      player!.chat('上次我们做了什么？')
      await waitUntil(() => messages.slice(messageStart).some(message => message.includes('上次我们一起开始收集木材')), 20_000, 'cross-session memory answer')
      assert.equal(second.debug.snapshot().decision?.retrievedMemoryIds.includes(memories[0]!.id), true)
      ctx.record('assertion', 'restart_memory_verified', { memoryId: memories[0]!.id, debug: second.debug.snapshot() })
    },
    cleanup: async ctx => {
      if (runtime) await runtime.stop('prototype_cleanup')
      runtime = undefined; backend = undefined
      player?.quit('prototype_complete'); player = undefined
      server.send('kill @e[type=minecraft:item]')
      ctx.record('cleanup', 'prototype_clients_stopped', {})
    },
  })
}

function createPrototypeRuntime(
  companionName: string,
  playerName: string,
  model: PrototypeScenarioModel,
  memoryFile: string,
  dataDirectory: string,
  session: string,
) {
  const backend = new MinecraftBackend({
    worldId: 'paper-ci-v0.3', server: { host: '127.0.0.1', port, version: '1.21.1' },
    identity: { username: companionName, auth: 'offline' }, ...defaultMinecraftBackendConfig,
    timeouts: { connectMs: 10_000, loginMs: 20_000, spawnMs: 60_000, stopMs: 5_000 },
    reconnect: { ...defaultMinecraftBackendConfig.reconnect, initialDelayMs: 250, maxDelayMs: 1_000 },
  })
  const debug = new DebugStateStore()
  const runtime = new CompanionRuntime({
    backend, model, memory: new FileMemoryStore(memoryFile),
    journal: new JsonlEventJournal(path.join(dataDirectory, `events-${session}.jsonl`), 'paper-ci-v0.3', randomUUID()),
    profile: { profileId: 'ci-companion', versionId: 'ci-v1', content: '你是可靠、自然且诚实的 Minecraft 伙伴。', sourcePath: 'ci-profile' },
    debug, primaryPlayer: playerName, speechIntervalMs: 100,
  })
  return { runtime, backend, debug }
}

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
function woodCount(snapshot: ReturnType<MinecraftBackend['snapshot']>): number { return snapshot.inventory.slots.filter(slot => slot.itemName.endsWith('_log') || slot.itemName.endsWith('_stem')).reduce((sum, slot) => sum + slot.count, 0) }
function distance(left: { x: number; y: number; z: number }, right: { x: number; y: number; z: number }): number { return Math.hypot(left.x - right.x, left.y - right.y, left.z - right.z) }
function yawDistance(left: number, right: number): number { const delta = Math.abs(left - right) % (Math.PI * 2); return Math.min(delta, Math.PI * 2 - delta) }
function blockAtRelativeYaw(
  pose: { position: { x: number; y: number; z: number }; yaw: number },
  yawOffset: number,
  distanceFromFeet: number,
): { x: number; y: number; z: number } {
  const yaw = pose.yaw + yawOffset
  return {
    x: Math.floor(pose.position.x - Math.sin(yaw) * distanceFromFeet),
    y: Math.floor(pose.position.y) + 1,
    z: Math.floor(pose.position.z - Math.cos(yaw) * distanceFromFeet),
  }
}
async function waitFor(events: BackendEventEnvelope[], predicate: (event: BackendEventEnvelope) => boolean, description: string, timeout = 30_000) { return waitUntil(() => events.find(predicate), timeout, description) }
async function waitUntil<T>(predicate: () => T | undefined | false | Promise<T | undefined | false>, timeoutMs: number, description: string): Promise<T> { const start = Date.now(); while (Date.now() - start < timeoutMs) { const value = await predicate(); if (value) return value; await delay(25) } throw new Error(`Timed out waiting for ${description}`) }
function delay(ms: number): Promise<void> { return new Promise(resolve => setTimeout(resolve, ms)) }
function required(name: string): string { const value = process.env[name]; if (!value) throw new Error(`${name} is required`); return path.resolve(value) }

await main()
