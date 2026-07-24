import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'
import type { BackendEventEnvelope, BackendLifecyclePayload } from './contracts.js'
import { defaultMinecraftBackendConfig } from './config.js'
import { MinecraftBackend } from './minecraft-backend.js'

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..')
const managerScript = path.join(projectRoot, 'mcserver', 'mc.ps1')
const username = process.env.MC_USERNAME ?? 'MineIntentBot'

function manager(...args: string[]): string {
  const result = spawnSync('powershell.exe', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', managerScript, ...args,
  ], { cwd: path.dirname(managerScript), encoding: 'utf8', timeout: 120_000 })
  if (result.status !== 0) throw new Error(`mc.ps1 ${args[0]} failed: ${result.stderr || result.stdout}`)
  return result.stdout.trim()
}

function lifecycleType(event: BackendEventEnvelope): string | undefined {
  return event.kind === 'lifecycle' ? (event.payload as BackendLifecyclePayload).type : undefined
}

async function waitFor(
  events: BackendEventEnvelope[],
  predicate: (event: BackendEventEnvelope) => boolean,
  description: string,
  timeoutMs = 30_000,
  since = 0,
): Promise<BackendEventEnvelope> {
  const existing = events.slice(since).find(predicate)
  if (existing) return existing
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Timed out waiting for ${description}`)), timeoutMs)
    const interval = setInterval(() => {
      const found = events.slice(since).find(predicate)
      if (!found) return
      clearTimeout(timeout)
      clearInterval(interval)
      resolve(found)
    }, 25)
  })
}

async function main(): Promise<void> {
  const status = manager('status')
  if (!status.includes('运行中')) manager('start')

  const backend = new MinecraftBackend({
    worldId: 'local-paper-1.21.1',
    server: { host: process.env.MC_HOST ?? 'localhost', port: Number(process.env.MC_PORT ?? 25565), version: '1.21.1' },
    identity: { username, auth: 'offline' },
    ...defaultMinecraftBackendConfig,
    timeouts: { connectMs: 10_000, loginMs: 20_000, spawnMs: 30_000, stopMs: 5_000 },
    reconnect: { ...defaultMinecraftBackendConfig.reconnect, initialDelayMs: 250, maxDelayMs: 1_000, stableResetMs: 5_000 },
  })
  const events: BackendEventEnvelope[] = []
  backend.subscribe(event => {
    events.push(event)
    if (event.kind === 'lifecycle') process.stdout.write(`${JSON.stringify(event)}\n`)
  })
  const controller = new AbortController()

  try {
    const ready = await backend.start(controller.signal)
    assert.equal(ready.snapshot.world.minecraftVersion, '1.21.1')
    assert.equal(ready.snapshot.world.worldId, 'local-paper-1.21.1')
    assert.equal(ready.snapshot.self.username, username)
    assert.equal(ready.snapshot.self.health > 0, true)
    assert.equal(Number.isFinite(ready.snapshot.self.position.x), true)
    const allowedObservers = new Set((process.env.MC_OBSERVER_USERNAMES ?? '').split(',').map(name => name.trim()).filter(Boolean))
    const unexpectedHumans = ready.snapshot.trackedPlayers.filter(player => player.username !== username && !allowedObservers.has(player.username))
    assert.deepEqual(
      unexpectedHumans.map(player => player.username),
      [],
      `Refusing destructive Paper integration while unexpected players are online: ${unexpectedHumans.map(player => player.username).join(', ')}`,
    )
    const firstEpoch = ready.connectionEpoch

    const deathStart = events.length
    manager('send', `kill ${username}`)
    await waitFor(events, event => lifecycleType(event) === 'died', 'death', 30_000, deathStart)
    await waitFor(events, event => lifecycleType(event) === 'respawned', 'respawn', 30_000, deathStart)
    assert.equal(backend.snapshot().self.alive, true)

    const dimensionStart = events.length
    manager('send', `execute in minecraft:the_nether run tp ${username} 0 80 0`)
    await waitFor(events, event => lifecycleType(event) === 'dimension_changed', 'dimension change', 30_000, dimensionStart)
    assert.equal(backend.snapshot().world.dimension, 'the_nether')

    const restartStart = events.length
    manager('stop', '--timeout', '120')
    await waitFor(events, event => lifecycleType(event) === 'connection_closed', 'server close', 30_000, restartStart)
    manager('start')
    await waitFor(events, event => lifecycleType(event) === 'ready' && event.connectionEpoch > firstEpoch, 'reconnected ready', 60_000)
    assert.equal(backend.snapshot().connectionEpoch > firstEpoch, true)
    assert.equal(backend.snapshot().self.alive, true)

    await backend.stop('paper_integration_complete')
    assert.equal(backend.state().status, 'stopped')
    assert.equal(events.filter(event => lifecycleType(event) === 'died').length, 1)
    process.stdout.write(JSON.stringify({ status: 'passed', events: events.length, finalEpoch: backend.state() }) + '\n')
  } finally {
    if (backend.state().status !== 'stopped') await backend.stop('paper_integration_cleanup')
    const current = manager('status')
    if (!current.includes('运行中')) manager('start')
  }
}

await main()
