import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { JsonlEventJournal } from '../events/index.js'
import { FileMemoryStore } from '../memory/index.js'
import type {
  BackendEventEnvelope, BackendReady, BackendState, MinecraftBackendApi, MinecraftMotorDriverApi,
  MinecraftSnapshotV1, MotorMoveDirection, MotorMoveDirections, ProtocolObservationSource, Unsubscribe,
} from '../minecraft/contracts.js'
import type { AgentDecisionContext, ModelProvider, ModelRunResult, WireToolDefinition } from '../models/index.js'
import { DebugStateStore } from '../telemetry/index.js'
import { CompanionRuntime } from './runtime.js'

type ModelInput = { runId: string; context: AgentDecisionContext; tools: readonly WireToolDefinition[] }

let callSequence = 0
/** Mirrors the real executor contract: declare once, then echo the host's opaque identity. */
class TestToolRound {
  #roundId?: string
  #lastToolCallId?: string
  constructor(private readonly runtime: CompanionRuntime, private readonly runId: string) {}

  async execute(name: string, args: Record<string, unknown>): Promise<unknown> {
    callSequence += 1
    const round = this.#roundId === undefined ? { new: true as const } : { id: this.#roundId }
    const toolCallId = `call-${callSequence}`
    const execution = await this.runtime.executeTool({
      runId: this.runId, toolCallId, round, name, arguments: args,
    })
    this.#lastToolCallId = toolCallId
    this.#roundId = execution.roundId
    return execution.result
  }

  next(): void { this.#roundId = undefined }
  get roundId(): string | undefined { return this.#roundId }
  get lastToolCallId(): string | undefined { return this.#lastToolCallId }
}

class FakeModel implements ModelProvider {
  calls: ModelInput[] = []
  unexpectedHandlerFailures: unknown[] = []
  handler: (input: ModelInput, signal: AbortSignal) => Promise<ModelRunResult> = async () => ({ model: 'fake' })
  async run(input: ModelInput, signal: AbortSignal) {
    this.calls.push(structuredClone(input))
    try {
      return await this.handler(input, signal)
    } catch (error) {
      // Runtime deliberately absorbs model failures after recording them. Capture at the fake-model
      // boundary so an assertion thrown inside a handler cannot make its own test silently pass;
      // cancellation-driven throws are expected control flow and are excluded by the same signal
      // the runtime handed to the model. Do not exclude every post-cancel error: an AssertionError
      // thrown after cancellation is still a broken test.
      const expectedCancellation = signal.aborted && (
        (error instanceof DOMException && error.name === 'AbortError') || error === signal.reason
      )
      if (!expectedCancellation) this.unexpectedHandlerFailures.push(error)
      throw error
    }
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
  health = 20
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
      self: { entityKey: 'self', username: 'Bot', position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: this.yaw, pitch: this.pitch, onGround: true, alive: true, health: this.health, food: 20, foodSaturation: 5, effects: [] },
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
  /** Mineflayer's `health` event only says "self changed", so the drop has to be found by comparing. */
  emitSelfChanged(health: number) {
    this.health = health
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `self-${this.revision++}`, kind: 'snapshot_changed', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a', worldId: this.worldId, dimension: this.dimension,
      payload: { reason: 'self' },
    } satisfies BackendEventEnvelope
    for (const listener of this.subscribers) listener(event)
  }

  emitChat(text: string) {
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `chat-${this.revision++}`, kind: 'chat', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a', worldId: this.worldId, dimension: this.dimension,
      payload: { senderUsername: 'Alice', plainText: text, position: 'chat' },
    } satisfies BackendEventEnvelope
    for (const listener of this.subscribers) listener(event)
  }
  /** The envelope carries the world the sound happened in, which is what the history has to keep. */
  emitSound(payload: { soundName: string; sourcePosition: { x: number; y: number; z: number } }) {
    const event = {
      protocol: 'mineintent.minecraft.backend-event.v1', id: `sound-${this.revision++}`, kind: 'sound', occurredAt: new Date().toISOString(),
      processSessionId: this.processSessionId, connectionEpoch: this.connectionEpoch, connectionAttemptId: 'a', worldId: this.worldId, dimension: this.dimension,
      payload: { ...payload, volume: 1, pitch: 1 },
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
  moveCalls: MotorMoveDirection[][] = []
  nextMoveDelta?: { x: number; y: number; z: number }
  constructor(private backend: FakeBackend) {}
  async lookRelative(yawDegrees: number, pitchDegrees: number, signal: AbortSignal) {
    signal.throwIfAborted()
    this.backend.yaw -= yawDegrees * Math.PI / 180
    this.backend.pitch -= pitchDegrees * Math.PI / 180
    this.backend.revision++
  }
  async move(directions: MotorMoveDirections, durationMs: number, _sprint: boolean | undefined, signal: AbortSignal) {
    this.moving = true
    this.moveCalls.push([...directions])
    try {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(resolve, durationMs)
        const abort = () => { clearTimeout(timer); reject(new DOMException('aborted', 'AbortError')) }
        signal.addEventListener('abort', abort, { once: true })
        if (signal.aborted) abort()
      })
      const forwardAmount = Number(directions.includes('forward')) - Number(directions.includes('back'))
      const rightAmount = Number(directions.includes('right')) - Number(directions.includes('left'))
      const forward = { x: -Math.sin(this.backend.yaw), z: -Math.cos(this.backend.yaw) }
      const right = { x: -forward.z, z: forward.x }
      const delta = this.nextMoveDelta ?? {
        x: forward.x * forwardAmount + right.x * rightAmount,
        y: 0,
        z: forward.z * forwardAmount + right.z * rightAmount,
      }
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

async function fixture(t: test.TestContext, options: {
  gateJournal?: boolean
  speechIntervalMs?: number
  expectedHandlerFailures?: number
} = {}) {
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
  t.after(async () => {
    try {
      await runtime.stop('test')
      const summaries = model.unexpectedHandlerFailures.map(error => error instanceof Error
        ? `${error.name}: ${error.message}`
        : String(error))
      assert.equal(
        model.unexpectedHandlerFailures.length,
        options.expectedHandlerFailures ?? 0,
        `unexpected model handler failures: ${summaries.join(' | ')}`,
      )
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })
  const journalFile = path.join(directory, 'events.jsonl')
  const readJournal = async (): Promise<Array<{ type: string; payload: Record<string, unknown> }>> => {
    await journal.flush()
    const raw = await readFile(journalFile, 'utf8').catch(() => '')
    return raw.split('\n').filter(Boolean).map(line => JSON.parse(line))
  }
  return { backend, model, runtime, memory, debug, journal, readJournal }
}

test('say reports queued rather than completed, and journals the whole correlation chain', async t => {
  const { backend, model, runtime, readJournal } = await fixture(t, { speechIntervalMs: 0 })
  let result: unknown
  let hostedRoundId: string | undefined
  let sentToolCallId: string | undefined
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    result = await round.execute('say', { text: '好'.repeat(300) })
    hostedRoundId = round.roundId
    sentToolCallId = round.lastToolCallId
    return { model: 'fake' }
  }
  backend.emitChat('Bot，说一段长话')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  // schedule() 只入队：分段与最小间隔都在之后发生，所以此刻玩家还没看到任何东西。
  assert.deepEqual(result, { protocol: 'mineintent.tool-result.v1', status: 'queued', segments: 2 })
  const events = await readJournal()
  const queued = events.find(event => event.type === 'say.queued')
  assert.ok(queued, 'say must leave a journal record')
  const payload = queued!.payload as { toolCallId: string; roundId: string; segments: number; actionId: string }
  assert.equal(payload.toolCallId, sentToolCallId)
  assert.match(hostedRoundId!, /^[0-9a-f-]{36}$/u)
  assert.equal(payload.roundId, hostedRoundId)
  assert.equal(payload.segments, 2)
  // actionId 同时是 speech 请求 id，调度器事件才能和这次调用对上。
  const scheduled = events.find(event => event.type === 'speech.scheduled')
  assert.equal((scheduled!.payload as { requestId: string }).requestId, payload.actionId)
})

test('the model sees exactly the contracts registered for dispatch', async t => {
  const { backend, model, runtime } = await fixture(t)
  backend.emitChat('Bot，你能做什么？')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  const tools = model.calls[0]!.tools
  assert.deepEqual(tools.map(tool => tool.function.name), [
    'look_relative', 'move_input', 'say', 'remember',
  ])
  assert.match(tools.find(tool => tool.function.name === 'move_input')!.function.description, /前后键或左右键同时按会互相抵消/u)
})

test('a key set moves diagonally, while opposing keys execute and report no effect', async t => {
  const { backend, model, runtime } = await fixture(t)
  const results: unknown[] = []
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    results.push(await round.execute('move_input', {
      directions: ['forward', 'left'], duration_ms: 50,
    }))
    results.push(await round.execute('move_input', {
      directions: ['forward', 'back'], duration_ms: 50,
    }))
    return { model: 'fake' }
  }
  backend.emitChat('Bot，走走看')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  assert.deepEqual(backend.motorInstance.moveCalls, [
    ['forward', 'left'],
    ['forward', 'back'],
  ])
  const diagonal = results[0] as {
    status: string
    effect: { relativeDisplacement: number[]; movement: string }
  }
  assert.equal(diagonal.status, 'completed')
  assert.deepEqual(diagonal.effect.relativeDisplacement, [-1, 0, 1])
  assert.equal(diagonal.effect.movement, 'changed')
  const opposing = results[1] as {
    status: string
    effect: { relativeDisplacement: number[]; distance: number; movement: string }
  }
  assert.equal(opposing.status, 'completed')
  assert.deepEqual(opposing.effect.relativeDisplacement, [0, 0, 0])
  assert.equal(opposing.effect.distance, 0)
  assert.equal(opposing.effect.movement, 'no_effect')
})

test('round ids are minted by the run host and a replaced round cannot be resumed', async t => {
  const { backend, model, runtime } = await fixture(t)
  let firstRoundId: string | undefined
  let secondRoundId: string | undefined
  let staleRejection: unknown
  let continuedRoundId: string | undefined
  model.handler = async input => {
    const first = await runtime.executeTool({
      runId: input.runId, toolCallId: 'call-first-round', round: { new: true },
      name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 },
    })
    const second = await runtime.executeTool({
      runId: input.runId, toolCallId: 'call-second-round', round: { new: true },
      name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 },
    })
    firstRoundId = first.roundId
    secondRoundId = second.roundId
    try {
      await runtime.executeTool({
        runId: input.runId, toolCallId: 'call-stale-round', round: { id: first.roundId },
        name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 },
      })
    } catch (error) {
      staleRejection = error
    }
    const continued = await runtime.executeTool({
      runId: input.runId, toolCallId: 'call-live-round', round: { id: second.roundId },
      name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 },
    })
    continuedRoundId = continued.roundId
    return { model: 'fake' }
  }

  backend.emitChat('Bot，看看')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  assert.match(firstRoundId!, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u)
  assert.notEqual(secondRoundId, firstRoundId)
  assert.equal(staleRejection instanceof Error ? staleRejection.message : undefined, 'tool_round_is_not_active')
  assert.equal(continuedRoundId, secondRoundId)
})

test('a held resource fails the call instead of killing the run, and only blocks its own resource', async t => {
  const { backend, model, runtime } = await fixture(t)
  const results: unknown[] = []
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    // Open the host before overlapping calls; the real agent loop learns this id from the first
    // sequential response and then echoes it for the rest of the assistant response.
    await round.execute('look_relative', { yaw_degrees: 0, pitch_degrees: 0 })
    // 身体占用期间发起第二个身体调用：应得到真实失败，而不是异常。
    const moving = round.execute('move_input', { directions: ['forward'], duration_ms: 300 })
    await waitFor(() => backend.motorInstance.moving)
    results.push(await round.execute('look_relative', { yaw_degrees: 5, pitch_degrees: 0 }))
    // say 用的是聊天资源，不该被身体挡住——否则「行动前先说一句」就无法实现。
    results.push(await round.execute('say', { text: '我先动一下。' }))
    results.push(await moving)
    return { model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  assert.equal((results[0] as { status: string }).status, 'failed')
  assert.match((results[0] as { summary: string }).summary, /resource_busy:body is held by move_input/u)
  assert.equal((results[1] as { status: string }).status, 'queued')
  assert.equal((results[2] as { status: string }).status, 'completed')
  assert.equal(backend.messages.includes('我先动一下。'), true)
})

test('startup is local; player chat runs the two-tool closed loop with measured effects and no memory write', async t => {
  const { backend, model, runtime, memory } = await fixture(t)
  assert.equal(model.calls.length, 0)
  const results: unknown[] = []
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    results.push(await round.execute('look_relative', { yaw_degrees: 90, pitch_degrees: 0 }))
    results.push(await round.execute('move_input', { directions: ['forward'], duration_ms: 50 }))
    results.push(await round.execute('say', { text: '我看到羊了，走近了一点。' }))
    return { model: 'fake' }
  }
  backend.emitChat('Bot，看看那只羊，再走过去一点')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(model.calls.length, 1)
  // The pushed frame carries the pose but never the scan: `visibleBlocks` was the largest item in
  // the prompt by an order of magnitude, and re-sending it every request is what the tool replaces.
  const opening = model.calls[0]!.context.frame
  assert.deepEqual(opening.self, { position: [0, 64, 0], yawDegrees: 0, pitchDegrees: 0 })
  assert.doesNotMatch(JSON.stringify(opening), /visibleBlocks|visibleEntities/u)
  assert.equal(model.calls[0]!.context.stable.profile.content.length > 0, true)
  const first = results[0] as {
    viewport: { visibleEntities: { items: Array<Record<string, unknown>>; truncated: boolean } }
  }
  // World-absolute: the entry names where the sheep is, not where it is relative to the body, so
  // the same sheep keeps one identity across the turn and the step that follow.
  assert.deepEqual(first.viewport.visibleEntities.items[0], { type: 'sheep', position: [5, 64, 0] })
  assert.equal(first.viewport.visibleEntities.truncated, false)
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

test('a new player chat waits behind an in-flight turn without taking control from the model', async t => {
  const { backend, model, runtime } = await fixture(t)
  let first = true
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    if (first) {
      first = false
      await round.execute('move_input', { directions: ['forward'], duration_ms: 80 })
      await round.execute('say', { text: '我先走完这一步。' })
      return { model: 'fake' }
    }
    await round.execute('say', { text: '我听见了，再判断是否停下。' })
    return { model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  while (!backend.motorInstance.moving) await new Promise(resolve => setTimeout(resolve, 1))
  const releasesBeforeChat = backend.motorInstance.releases
  backend.emitChat('Bot，停下')
  await new Promise(resolve => setTimeout(resolve, 15))
  assert.equal(backend.motorInstance.moving, true)
  assert.equal(backend.motorInstance.releases, releasesBeforeChat)
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
  assert.deepEqual(model.calls.map(call => call.context.frame.player?.text), ['Bot，往前走', 'Bot，停下'])
  // The message rides in the frame's `player`, so it is not also replayed as an event: each fact is
  // stated once, which is what lets a frame be appended instead of a window being re-sent.
  assert.equal(model.calls[1]!.context.frame.events.some(event => event.summary.includes('停下')), false)
})

test('damage taken mid-run reaches the model as a frame beside the next tool result', async t => {
  const { backend, model, runtime } = await fixture(t)
  const frames: Array<Awaited<ReturnType<typeof runtime.takePendingFrame>>> = []
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    // Hurt while the model is mid-loop. Nothing asked about health, so a pull-only design would
    // never surface it: the frame channel is the only way this can reach the model at all.
    backend.emitSelfChanged(13.5)
    await new Promise(resolve => setTimeout(resolve, 5))
    await round.execute('say', { text: '有东西打我。' })
    frames.push(await runtime.takePendingFrame(input.runId))
    // Drained, not a window: a second ask has nothing left to report.
    frames.push(await runtime.takePendingFrame(input.runId))
    return { model: 'fake' }
  }

  backend.emitChat('Bot，跟着我')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  const frame = frames[0]
  assert.equal(frame?.events.length, 1)
  assert.equal(frame.events[0]!.type, 'self.health.dropped')
  assert.match(frame.events[0]!.summary, /20 → 13\.5（-6\.5）/u)
  assert.equal(frame.status?.health, 13.5)
  assert.equal(frame.player, undefined)
  assert.equal(frames[1], undefined)
})

test('a fact observed in one world never reaches a run in another', async t => {
  const { backend, model, runtime } = await fixture(t)

  // Hurt in the overworld, with nothing yet asking for a frame — the queue holds it.
  backend.emitSelfChanged(13.5)
  await new Promise(resolve => setTimeout(resolve, 5))

  backend.changeScope({ dimension: 'the_nether' })
  await new Promise(resolve => setTimeout(resolve, 5))

  backend.emitChat('Bot，这是哪')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()

  // "受到伤害，生命值 20 → 13.5" is a statement about somewhere the companion no longer is, and the
  // model has no way to tell a replayed injury from a fresh one.
  const frame = model.calls[0]!.context.frame
  assert.deepEqual(frame.events, [])
  assert.equal(frame.world.dimension, 'the_nether')

  // The health baseline goes with it: kept across the change, the next comparison is against a
  // number from the previous world and invents an injury that never happened.
  backend.emitSelfChanged(13.5)
  await new Promise(resolve => setTimeout(resolve, 5))
  backend.emitChat('Bot，还好吗')
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  assert.deepEqual(model.calls[1]!.context.frame.events, [])
})

test('sounds heard before a world change are not replayed with their old distance and bearing', async t => {
  const { backend, model, runtime } = await fixture(t)

  backend.emitSound({ soundName: 'entity.zombie.ambient', sourcePosition: { x: 3, y: 64, z: 0 } })
  await new Promise(resolve => setTimeout(resolve, 5))
  backend.emitChat('Bot，你听到了吗')
  await waitFor(() => model.calls.length === 1)
  await runtime.idle()
  // Established first, so the second half cannot pass by the sound never having been recorded.
  assert.equal(model.calls[0]!.context.frame.sound?.recentSounds.length, 1)

  backend.changeScope({ dimension: 'the_nether' })
  await new Promise(resolve => setTimeout(resolve, 5))
  backend.emitChat('Bot，现在呢')
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()

  // A stored sound is a distance and a bearing measured from where the companion stood. After the
  // change both describe a place that no longer exists, and the entry itself does not say so.
  assert.deepEqual(model.calls[1]!.context.frame.sound?.recentSounds, [])
})

test('ordinary player chats preserve arrival order while the first journal write waits', async t => {
  const { backend, model, runtime, journal } = await fixture(t, { gateJournal: true })
  const gated = journal as GateJournal
  gated.blockNext('player.chat.received')

  backend.emitChat('Bot，往前走')
  await gated.blocked()
  backend.emitChat('Bot，停下')
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(model.calls.length, 0)
  gated.release()

  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  assert.deepEqual(model.calls.map(call => call.context.frame.player?.text), ['Bot，往前走', 'Bot，停下'])
})

test('a newer chat preserves model-authored segments from the earlier turn', async t => {
  const { backend, model, runtime } = await fixture(t, { speechIntervalMs: 25 })
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    const text = input.context.frame.player?.text.includes('停下') ? '这是模型的回复。' : '旧'.repeat(300)
    await round.execute('say', { text })
    return { model: 'fake' }
  }

  backend.emitChat('Bot，说一段很长的话')
  await waitFor(() => backend.messages.length === 1)
  backend.emitChat('Bot，停下')
  await waitFor(() => backend.messages.includes('这是模型的回复。'))

  assert.deepEqual(backend.messages, ['旧'.repeat(256), '旧'.repeat(44), '这是模型的回复。'])
})

test('a connection-epoch scope change synchronously aborts the active run and releases movement', async t => {
  const { backend, model, runtime } = await fixture(t)
  let activeRunId: string | undefined
  let activeRoundId: string | undefined
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    await round.execute('look_relative', { yaw_degrees: 0, pitch_degrees: 0 })
    activeRunId = input.runId
    activeRoundId = round.roundId
    await round.execute('move_input', { directions: ['forward'], duration_ms: 1500 })
    await round.execute('say', { text: '不应发送' })
    return { model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  backend.changeScope({ connectionEpoch: 2 })
  assert.ok(backend.motorInstance.releases > releases, 'scope event releases before asynchronous handling')
  assert.ok(activeRunId && activeRoundId)
  await assert.rejects(runtime.executeTool({
    runId: activeRunId, toolCallId: 'call-after-scope-loss', round: { id: activeRoundId },
    name: 'look_relative', arguments: { yaw_degrees: 0, pitch_degrees: 0 },
  }), error => error instanceof Error && (
    error.message === 'tool_run_is_not_active' || error.message === 'Model run is no longer current'
  ))
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
  assert.equal(backend.messages.includes('不应发送'), false)
})

test('connection_closed aborts even while the last snapshot still has the old scope', async t => {
  const { backend, model, runtime } = await fixture(t)
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    await round.execute('move_input', { directions: ['forward'], duration_ms: 1500 })
    await round.execute('say', { text: '不应发送' })
    return { model: 'fake' }
  }
  backend.emitChat('Bot，往前走')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  backend.closeConnectionWithoutChangingSnapshot()
  assert.ok(backend.motorInstance.releases > releases)
  await runtime.idle()
  assert.equal(backend.motorInstance.moving, false)
})

test('a scope change drops chat that is still waiting for its journal write', async t => {
  const { backend, model, runtime, journal } = await fixture(t, { gateJournal: true })
  const gated = journal as GateJournal
  gated.blockNext('player.chat.received')

  backend.emitChat('Bot，旧世界里的消息')
  await gated.blocked()
  backend.changeScope({ connectionEpoch: 2 })
  gated.release()

  await runtime.idle()
  assert.equal(model.calls.length, 0)
})

test('connection_closed cancels speech segments even when no model run remains active', async t => {
  const { backend, model, runtime } = await fixture(t, { speechIntervalMs: 25 })
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    await round.execute('say', { text: '旧'.repeat(300) })
    return { model: 'fake' }
  }

  backend.emitChat('Bot，说一段很长的话')
  await waitFor(() => backend.messages.length === 1)
  backend.closeConnectionWithoutChangingSnapshot()
  await new Promise(resolve => setTimeout(resolve, 40))

  assert.deepEqual(backend.messages, ['旧'.repeat(256)])
})

test('release failure cannot wedge the tool gate and sub-epsilon motion is reported without quantization', async t => {
  const { backend, model, runtime, debug } = await fixture(t)
  const results: unknown[] = []
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    backend.motorInstance.releaseFailures = 1
    results.push(await round.execute('look_relative', { yaw_degrees: 0, pitch_degrees: 0 }))
    backend.motorInstance.nextMoveDelta = { x: 0.0005, y: 0, z: 0 }
    results.push(await round.execute('move_input', { directions: ['forward'], duration_ms: 50 }))
    return { model: 'fake' }
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
    const round = new TestToolRound(runtime, input.runId)
    await round.execute('move_input', { directions: ['forward'], duration_ms: 1500 })
    return { model: 'fake' }
  }
  backend.emitChat('Bot，持续往前')
  await waitFor(() => backend.motorInstance.moving)
  const releases = backend.motorInstance.releases
  const stopping = runtime.stop('explicit_test_stop')
  assert.ok(backend.motorInstance.releases > releases)
  await stopping
  assert.equal(backend.motorInstance.moving, false)
})

test('queued player turns preserve both model replies after a delayed completion journal', async t => {
  const { backend, model, runtime, journal } = await fixture(t, { gateJournal: true })
  const gated = journal as GateJournal
  gated.blockNext('model.decision.completed')
  model.handler = async input => {
    const round = new TestToolRound(runtime, input.runId)
    const text = model.calls.length === 1 ? '旧回复' : '新回复'
    await round.execute('say', { text })
    return { model: 'fake' }
  }
  backend.emitChat('Bot，第一句话')
  await gated.blocked()
  backend.emitChat('Bot，第二句话')
  gated.release()
  await waitFor(() => model.calls.length === 2)
  await runtime.idle()
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(backend.messages.includes('旧回复'), true)
  assert.equal(backend.messages.includes('新回复'), true)
})

/**
 * World coordinates are no longer forbidden: a vanilla player reads their own position and the
 * targeted block off the F3 screen, so absolute positions are not privileged information, and an
 * integer voxel is the only key stable enough to diff a viewport against. What the boundary still
 * forbids is *handles* — internal identities the model could otherwise fabricate and hand back as
 * if it had been given them. Those stay out.
 */
function assertNoForbiddenSpatialKeys(value: unknown): void {
  if (Array.isArray(value)) return value.forEach(assertNoForbiddenSpatialKeys)
  if (!value || typeof value !== 'object') return
  for (const [key, child] of Object.entries(value)) {
    assert.equal(['ref', 'entityKey', 'entityId', 'worldId'].includes(key), false, `forbidden model key: ${key}`)
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
