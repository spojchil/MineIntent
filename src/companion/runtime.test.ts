import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { JsonlEventJournal } from '../events/index.js'
import { FileMemoryStore } from '../memory/index.js'
import type {
  BackendEventEnvelope, BackendReady, BackendState, MinecraftBackendApi, MinecraftMotorDriverApi,
  MinecraftSnapshotV1, MotorMoveDirection, ProtocolObservationSource, Unsubscribe,
} from '../minecraft/contracts.js'
import type { D40DecisionContext, D40ToolInvocation, ModelProvider, ModelRunResult } from '../models/index.js'
import { DebugStateStore } from '../telemetry/index.js'
import { CompanionRuntime } from './runtime.js'

class FakeModel implements ModelProvider {
  calls: Array<{ runId: string; context: D40DecisionContext }> = []
  handler: (input: { runId: string; context: D40DecisionContext }, signal: AbortSignal) => Promise<ModelRunResult> = async () => ({
    decision: { protocol: 'mineintent.d40-decision.v1', speech: '好。', memory: null }, model: 'fake',
  })
  async run(input: { runId: string; context: D40DecisionContext }, signal: AbortSignal) {
    this.calls.push(structuredClone(input))
    return this.handler(input, signal)
  }
}

class GateJournal extends JsonlEventJournal {
  #blockedType?: string
  #startedResolve?: () => void
  #releaseResolve?: () => void
  #started = Promise.resolve()
  #release = Promise.resolve()

  blockNext(type: string): void {
    this.#blockedType = type
    this.#started = new Promise(resolve => { this.#startedResolve = resolve })
    this.#release = new Promise(resolve => { this.#releaseResolve = resolve })
  }
  async blocked(): Promise<void> { await this.#started }
  release(): void { this.#releaseResolve?.() }
  override async append<T>(type: string, payload: T) {
    if (type === this.#blockedType) {
      this.#blockedType = undefined
      this.#startedResolve?.()
      await this.#release
    }
    return super.append(type, payload)
  }
}

class FakeBackend implements MinecraftBackendApi {
  state_: BackendState = { status: 'idle' }
  processSessionId = 's'
  connectionEpoch = 1
  worldId = 'w'
  dimension = 'overworld'
  revision = 1
  position = { x: 0, y: 64, z: 0 }
  yaw = 0
  pitch = 0
  messages: string[] = []
  motorInstance = new FakeMotor(this)
  subscribers = new Set<(event: BackendEventEnvelope) => void>()

  async start(): Promise<BackendReady> {
    this.state_ = { status: 'ready', epoch: this.connectionEpoch, attemptId: 'a', readyAt: new Date().toISOString() }
    return { processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a', snapshot: this.snapshot() }
  }
  async stop(reason: string) { this.state_ = { status: 'stopped', reason } }
  state() { return this.state_ }
  snapshot(): Readonly<MinecraftSnapshotV1> {
    return {
      protocol: 'mineintent.minecraft.snapshot.v1', snapshotRevision: this.revision, lifecycleRevision: 1,
      capturedAt: new Date().toISOString(), processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a',
      world: { worldId: this.worldId, dimension: this.dimension, minecraftVersion: '1.21.1', protocolVersion: 767, gameMode: 'survival', minY: -64, height: 384, timeOfDay: 1000 },
      self: { entityKey: 'self', username: 'Bot', position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: this.yaw, pitch: this.pitch, onGround: true, alive: true, health: 20, food: 20, foodSaturation: 5, effects: [] },
      inventory: { selectedHotbarSlot: 0, slots: [] },
      trackedPlayers: [{ playerKey: 'alice', username: 'Alice', listed: true, entityTracked: true }],
    }
  }
  subscribe(listener: (event: BackendEventEnvelope) => void): Unsubscribe { this.subscribers.add(listener); return () => this.subscribers.delete(listener) }
  observationSource(): ProtocolObservationSource {
    return {
      epoch: () => this.connectionEpoch,
      selfPose: () => ({ position: { ...this.position }, velocity: { x: 0, y: 0, z: 0 }, yaw: this.yaw, pitch: this.pitch }),
      listTrackedEntities: () => [
        { entityKey: 'self', protocolEntityId: 1, type: 'player', username: 'Bot', position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: this.yaw, pitch: this.pitch, width: 0.6, height: 1.8, onGround: true, equipment: [], valid: true },
        { entityKey: 'sheep', protocolEntityId: 2, type: 'mob', name: 'sheep', position: { x: 5, y: 64, z: 0 }, velocity: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0, width: 0.9, height: 1.3, onGround: true, equipment: [], valid: true },
      ],
      readBlock: position => ({ status: 'loaded', block: {
        position, name: position.y === 63 ? 'grass_block' : 'air', stateId: 0, properties: {}, collisionShapes: [],
        transparentHint: position.y !== 63, boundingBox: position.y === 63 ? 'block' : 'empty',
      } }),
      subscribe: () => () => {},
    }
  }
  motor() { return this.motorInstance }
  sendChat(message: string) { this.messages.push(message) }
  emitChat(text: string) {
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `chat-${this.revision++}`, kind: 'chat', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a', worldId: this.worldId, dimension: this.dimension,
      payload: { senderUsername: 'Alice', plainText: text, position: 'chat' },
    } satisfies BackendEventEnvelope
    for (const listener of this.subscribers) listener(event)
  }
  changeScope(change: { connectionEpoch?: number; worldId?: string; dimension?: string }) {
    if (change.connectionEpoch !== undefined) this.connectionEpoch = change.connectionEpoch
    if (change.worldId !== undefined) this.worldId = change.worldId
    if (change.dimension !== undefined) this.dimension = change.dimension
    this.state_ = { status: 'ready', epoch: this.connectionEpoch, attemptId: 'changed', readyAt: new Date().toISOString() }
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `scope-${this.revision++}`, kind: 'lifecycle', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'changed',
      worldId: this.worldId, dimension: this.dimension, payload: { type: 'dimension_changed' },
    } satisfies BackendEventEnvelope
    for (const listener of this.subscribers) listener(event)
  }
  closeConnectionWithoutChangingSnapshot() {
    this.state_ = { status: 'stopped', reason: 'connection_closed' }
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `closed-${this.revision++}`, kind: 'lifecycle', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a',
      worldId: this.worldId, dimension: this.dimension, payload: { type: 'connection_closed' },
    } satisfies BackendEventEnvelope
    for (const listener of this.subscribers) listener(event)
  }
}

class FakeMotor implements MinecraftMotorDriverApi {
  releases = 0
  releaseFailures = 0
  moving = false
  nextMoveDelta?: { x: number; y: number; z: number }
  constructor(private backend: FakeBackend) {}
  async lookRelative(yawDegrees: number, pitchDegrees: number, signal: AbortSignal) {
    signal.throwIfAborted()
    this.backend.yaw -= yawDegrees * Math.PI / 180
    this.backend.pitch -= pitchDegrees * Math.PI / 180
    this.backend.revision++
  }
  async move(direction: MotorMoveDirection, durationMs: number, _sprint: boolean | undefined, signal: AbortSignal) {
    this.moving = true
    try {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(resolve, durationMs)
        const abort = () => { clearTimeout(timer); reject(new DOMException('aborted', 'AbortError')) }
        signal.addEventListener('abort', abort, { once: true })
        if (signal.aborted) abort()
      })
      const amount = direction === 'forward' ? 1 : direction === 'back' ? -1 : 0
      const delta = this.nextMoveDelta ?? { x: amount, y: 0, z: 0 }
      this.nextMoveDelta = undefined
      this.backend.position = {
        x: this.backend.position.x + delta.x,
        y: this.backend.position.y + delta.y,
        z: this.backend.position.z + delta.z,
      }
      this.backend.revision++
    } finally { this.moving = false; this.releaseAll() }
  }
  releaseAll() {
    this.releases++
    if (this.releaseFailures > 0) { this.releaseFailures--; throw new Error('simulated release failure') }
  }
}

async function fixture(t: test.TestContext, options: { gateJournal?: boolean; speechIntervalMs?: number } = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'mineintent-runtime-'))
  const backend = new FakeBackend()
  const model = new FakeModel()
  const memory = new FileMemoryStore(path.join(directory, 'memory.json'))
  const debug = new DebugStateStore()
  const journal = options.gateJournal
    ? new GateJournal(path.join(directory, 'events.jsonl'), 'w', 's')
    : new JsonlEventJournal(path.join(directory, 'events.jsonl'), 'w', 's')
  const runtime = new CompanionRuntime({
    backend, model, memory,
    journal,
    profile: { profileId: 'test', versionId: 'profile-1', content: '安静、诚实的朋友。', sourcePath: 'profile.md' },
    debug, primaryPlayer: 'Alice', speechIntervalMs: options.speechIntervalMs ?? 0,
  })
  await runtime.start()
  t.after(async () => { await runtime.stop('test'); await rm(directory, { recursive: true, force: true }) })
  return { backend, model, runtime, memory, debug, journal }
}

test('startup is local; player chat runs the two-tool closed loop with measured effects and no memory write', async t => {
  const { backend, model, runtime, memory } = await fixture(t)
  assert.equal(model.calls.length, 0)
  const results: unknown[] = []
  model.handler = async input => {
    results.push(await runtime.executeBodyTool({ runId: input.runId, name: 'look_relative', arguments: { yaw_degrees: 90, pitch_degrees: 0 } }))
    results.push(await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 50 } }))
    return { decision: {
      protocol: 'mineintent.d40-decision.v1', speech: '我看到羊了，走近了一点。',
      memory: { kind: 'episode', summary: 'D40 暂不写入这条记忆。' },
    }, model: 'fake' }
  }
  backend.emitChat('Bot，看看那只羊，再走过去一点')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(model.calls.length, 1)
  assert.equal(model.calls[0]!.context.observations.viewport?.visibleEntities.length, 0)
  const first = results[0] as { viewport: { visibleEntities: Array<{ name?: string }> } }
  assert.equal(first.viewport.visibleEntities[0]?.name, 'sheep')
  const lookEffect = (results[0] as { effect: { relativeTurnDegrees: { yaw: number; pitch: number }; turned: boolean } }).effect
  assert.ok(Math.abs(lookEffect.relativeTurnDegrees.yaw - 90) < 1e-9)
  assert.equal(lookEffect.relativeTurnDegrees.pitch, 0)
  assert.equal(lookEffect.turned, true)
  const moveEffect = (results[1] as { effect: { relativeDisplacement: number[]; movement: string } }).effect
  assert.ok(Math.abs(moveEffect.relativeDisplacement[0]!) < 1e-12)
  assert.equal(moveEffect.relativeDisplacement[1], 0)
  assert.equal(moveEffect.relativeDisplacement[2], 1)
  assert.equal(moveEffect.movement, 'changed')
  assert.equal(backend.messages.at(-1), '我看到羊了，走近了一点。')
  assert.deepEqual(await memory.list('w'), [])
  assertNoForbiddenSpatialKeys(model.calls[0]!.context)
  assertNoForbiddenSpatialKeys(results)
})

test('a new player chat aborts an in-flight move and releases controls', async t => {
  const { backend, model, runtime } = await fixture(t)
  let first = true
  model.handler = async input => {
    if (first) {
      first = false
      await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 1500 } })
    }
    return { decision: { protocol: 'mineintent.d40-decision.v1', speech: '停下来看你。', memory: null }, model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  while (!backend.motorInstance.moving) await new Promise(resolve => setTimeout(resolve, 1))
  const releasesBeforeChat = backend.motorInstance.releases
  backend.emitChat('Bot，看我这边')
  assert.ok(backend.motorInstance.releases > releasesBeforeChat, 'new chat releases inputs before its first await')
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
  assert.ok(backend.motorInstance.releases >= 2)
  assert.equal(model.calls.length, 2)
})

test('a safety stop invalidates an older chat that is still waiting for its journal entry', async t => {
  const { backend, model, runtime, journal } = await fixture(t, { gateJournal: true })
  const gated = journal as GateJournal
  gated.blockNext('player.chat.received')

  backend.emitChat('Bot，往前走')
  await gated.blocked()
  backend.emitChat('Bot，停下')
  await waitFor(() => backend.messages.includes('好，我停下。'))
  gated.release()

  await runtime.idle()
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(model.calls.length, 0)
  assert.equal(backend.motorInstance.moving, false)
})

test('a safety stop cancels unsent segments from an older reply', async t => {
  const { backend, model } = await fixture(t, { speechIntervalMs: 25 })
  model.handler = async () => ({
    decision: { protocol: 'mineintent.d40-decision.v1', speech: '旧'.repeat(300), memory: null },
    model: 'fake',
  })

  backend.emitChat('Bot，说一段很长的话')
  await waitFor(() => backend.messages.length === 1)
  backend.emitChat('Bot，停下')
  await waitFor(() => backend.messages.includes('好，我停下。'))
  await new Promise(resolve => setTimeout(resolve, 40))

  assert.deepEqual(backend.messages, ['旧'.repeat(256), '好，我停下。'])
})

test('a connection-epoch scope change synchronously aborts the active run and releases movement', async t => {
  const { backend, model, runtime } = await fixture(t)
  model.handler = async input => {
    await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 1500 } })
    return { decision: { protocol: 'mineintent.d40-decision.v1', speech: '不应发送', memory: null }, model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  backend.changeScope({ connectionEpoch: 2 })
  assert.ok(backend.motorInstance.releases > releases, 'scope event releases before asynchronous handling')
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
  assert.equal(backend.messages.includes('不应发送'), false)
})

test('connection_closed aborts even while the last snapshot still has the old scope', async t => {
  const { backend, model, runtime } = await fixture(t)
  model.handler = async input => {
    await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 1500 } })
    return { decision: { protocol: 'mineintent.d40-decision.v1', speech: '不应发送', memory: null }, model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  backend.closeConnectionWithoutChangingSnapshot()
  assert.ok(backend.motorInstance.releases > releases)
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
})

test('release failure cannot wedge the tool gate and sub-epsilon motion is reported without quantization', async t => {
  const { backend, model, runtime, debug } = await fixture(t)
  const results: unknown[] = []
  model.handler = async input => {
    backend.motorInstance.releaseFailures = 1
    results.push(await runtime.executeBodyTool({ runId: input.runId, name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 } }))
    backend.motorInstance.nextMoveDelta = { x: 0.0005, y: 0, z: 0 }
    results.push(await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 50 } }))
    return { decision: { protocol: 'mineintent.d40-decision.v1', speech: null, memory: null }, model: 'fake' }
  }
  backend.emitChat('Bot，试着动一点')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()
  assert.equal((results[0] as { status: string }).status, 'completed')
  const effect = (results[1] as { effect: { relativeDisplacement: number[]; distance: number; movement: string } }).effect
  assert.deepEqual(effect.relativeDisplacement, [0.0005, 0, 0])
  assert.equal(effect.distance, 0.0005)
  assert.equal(effect.movement, 'no_effect')
  assert.equal(debug.snapshot().currentBodyTool, undefined)
})

test('stop aborts and releases synchronously before awaiting the decision tail', async t => {
  const { backend, model, runtime } = await fixture(t)
  model.handler = async input => {
    await runtime.executeBodyTool({ runId: input.runId, name: 'move_input', arguments: { direction: 'forward', duration_ms: 1500 } })
    return { decision: { protocol: 'mineintent.d40-decision.v1', speech: null, memory: null }, model: 'fake' }
  }
  backend.emitChat('Bot，持续往前')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  const stopping = runtime.stop('explicit_test_stop')
  assert.ok(backend.motorInstance.releases > releases)
  await stopping
  assert.equal(backend.motorInstance.moving, false)
})

test('a superseded model result cannot speak after its completion journal await', async t => {
  const { backend, model, runtime, journal } = await fixture(t, { gateJournal: true })
  const gated = journal as GateJournal
  gated.blockNext('model.decision.completed')
  model.handler = async (_input) => ({
    decision: {
      protocol: 'mineintent.d40-decision.v1',
      speech: model.calls.length === 1 ? '旧回复' : '新回复',
      memory: null,
    },
    model: 'fake',
  })
  backend.emitChat('Bot，第一句话')
  await gated.blocked()
  backend.emitChat('Bot，第二句话')
  gated.release()
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(backend.messages.includes('旧回复'), false)
  assert.equal(backend.messages.includes('新回复'), true)
})

function assertNoForbiddenSpatialKeys(value: unknown): void {
  if (Array.isArray(value)) return value.forEach(assertNoForbiddenSpatialKeys)
  if (!value || typeof value !== 'object') return
  for (const [key, child] of Object.entries(value)) {
    assert.equal(['ref', 'x', 'y', 'z', 'position', 'entityKey', 'worldId'].includes(key), false, `forbidden model key: ${key}`)
    assertNoForbiddenSpatialKeys(child)
  }
}

async function waitFor(predicate: () => boolean, timeoutMs = 2_000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('timed out waiting for runtime')
    await new Promise(resolve => setTimeout(resolve, 2))
  }
}
