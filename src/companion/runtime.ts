import { randomUUID } from 'node:crypto'
import { z } from 'zod'
import type { JsonlEventJournal } from '../events/index.js'
import {
  composePassiveObservations,
  CurrentStatusProvider,
  InformationRegistry,
  InformationRuntime,
  InMemoryInformationAccessPolicy,
  InventoryProvider,
  SoundInformationProvider,
  ViewportInformationProvider,
  lookDirection,
  type PassiveObservations,
  type TrustedInformationCaller,
  type ViewportValues,
} from '../information/index.js'
import type { FileMemoryStore } from '../memory/index.js'
import type { BackendEventEnvelope, MinecraftBackendApi, ProtocolChatEvent } from '../minecraft/contracts.js'
import type { D40DecisionContext, D40ToolInvocation, ModelProvider } from '../models/index.js'
import { interpretPlayerChat, SpeechScheduler } from '../speech/index.js'
import type { DebugContextSource } from '../telemetry/contracts.js'
import type { DebugStateStore } from '../telemetry/debug-state.js'
import {
  BackendInformationScopeSource,
  BackendInventoryPort,
  BackendPerceptionPort,
  BackendSelfVitalsPort,
  SoundHistory,
} from './information-adapters.js'
import type { CompanionProfile } from './profile.js'

const INFORMATION_GRANT_ID = 'grant-context-composer'
const INFORMATION_PRINCIPAL_ID = 'context-composer'

const lookArgumentsSchema = z.strictObject({
  yaw_degrees: z.number().finite().min(-90).max(90),
  pitch_degrees: z.number().finite().min(-90).max(90),
})
const moveArgumentsSchema = z.strictObject({
  direction: z.enum(['forward', 'back', 'left', 'right']),
  duration_ms: z.number().int().min(50).max(1_500),
  sprint: z.boolean().optional(),
})

export interface CompanionRuntimeOptions {
  backend: MinecraftBackendApi
  model: ModelProvider
  memory: FileMemoryStore
  journal: JsonlEventJournal
  profile: CompanionProfile
  debug: DebugStateStore
  primaryPlayer: string
  speechIntervalMs?: number
}

interface RunScope {
  processSessionId: string
  connectionEpoch: number
  worldId: string
  dimension: string
}

interface ActiveRun extends RunScope { runId: string; controller: AbortController }

const MOVE_EFFECT_EPSILON = 0.01
const LOOK_EFFECT_EPSILON_DEGREES = 0.01

/**
 * The D40 runtime intentionally has one model route: an addressed player chat. Startup and
 * danger remain local runtime events, and the only model-visible body surface is two short
 * relative inputs. This is an experiment loop, not a replacement planning architecture.
 */
export class CompanionRuntime {
  readonly #backend: MinecraftBackendApi
  readonly #model: ModelProvider
  readonly #memory: FileMemoryStore
  readonly #journal: JsonlEventJournal
  readonly #profile: CompanionProfile
  readonly #debug: DebugStateStore
  readonly #primaryPlayer: string
  readonly #speech: SpeechScheduler
  readonly #soundHistory: SoundHistory
  readonly #informationRuntime: InformationRuntime
  readonly #abort = new AbortController()
  readonly #recentEvents: Array<{ id: string; type: string; summary: string }> = []
  #unsubscribe?: () => void
  #modelAbort?: AbortController
  #activeRun?: ActiveRun
  #decisionTail = Promise.resolve()
  #toolBusy = false
  #started = false
  #lastDangerAt = 0

  constructor(options: CompanionRuntimeOptions) {
    this.#backend = options.backend
    this.#model = options.model
    this.#memory = options.memory
    this.#journal = options.journal
    this.#profile = options.profile
    this.#debug = options.debug
    this.#primaryPlayer = options.primaryPlayer
    this.#speech = new SpeechScheduler({ send: message => this.#backend.sendChat(message) }, {
      minimumIntervalMs: options.speechIntervalMs ?? 1_000,
      onEvent: event => { void this.#journal.append(`speech.${event.type}`, withoutPrivateSpeech(event)) },
    })
    this.#soundHistory = new SoundHistory(this.#backend)
    this.#informationRuntime = buildInformationRuntime(this.#backend, this.#soundHistory)
  }

  async start(): Promise<void> {
    if (this.#started) return
    this.#started = true
    await this.#memory.load()
    this.#unsubscribe = this.#backend.subscribe(event => { void this.#handleBackendEvent(event) })
    await this.#backend.start(this.#abort.signal)
    await this.#waitForSelfChunk()
    this.#refreshDebug()
    const event = await this.#journal.append('companion.started', { summary: '同伴加入世界' })
    this.#pushRecent(event.id, event.type, '同伴加入世界')
  }

  async stop(reason = 'runtime_stopped'): Promise<void> {
    if (!this.#started) return
    this.#started = false
    this.#abortRunsAndRelease(reason)
    await this.#decisionTail
    this.#speech.stop(reason)
    this.#unsubscribe?.()
    this.#soundHistory.dispose()
    await this.#backend.stop(reason)
    await this.#journal.append('companion.stopped', { reason })
    await this.#journal.flush()
    this.#debug.update({ connection: this.#backend.state(), currentBodyTool: undefined })
  }

  async idle(): Promise<void> { await this.#decisionTail }

  /** Called only by the authenticated loopback bridge while the matching player-chat run lives. */
  async executeBodyTool(invocation: D40ToolInvocation): Promise<unknown> {
    const active = this.#activeRun
    if (!active || active.runId !== invocation.runId) throw new Error('tool_run_is_not_active')
    this.#assertRunCurrent(active)
    if (this.#toolBusy) throw new Error('body_tool_already_running')
    this.#toolBusy = true
    const actionId = randomUUID()
    const startedAt = new Date().toISOString()
    let motor: ReturnType<MinecraftBackendApi['motor']> | undefined
    try {
      this.#debug.update({ currentBodyTool: { id: actionId, tool: invocation.name, purpose: 'D40 short input', startedAt } })
      this.#assertRunCurrent(active)
      motor = this.#backend.motor()
      const before = this.#backend.observationSource().selfPose()
      if (invocation.name === 'look_relative') {
        const args = lookArgumentsSchema.parse(invocation.arguments)
        await motor.lookRelative(args.yaw_degrees, args.pitch_degrees, active.controller.signal)
      } else {
        const args = moveArgumentsSchema.parse(invocation.arguments)
        await motor.move(args.direction, args.duration_ms, args.sprint, active.controller.signal)
      }
      this.#assertRunCurrent(active)
      const after = this.#backend.observationSource().selfPose()
      const viewport = await this.#readViewport(invocation.runId, active.controller.signal)
      this.#assertRunCurrent(active)
      const effect = invocation.name === 'look_relative'
        ? measuredLookEffect(before, after)
        : measuredMoveEffect(before, after)
      await this.#journal.append('body_tool.completed', {
        actionId, runId: invocation.runId, tool: invocation.name, startedAt,
        // Internal diagnostics may retain poses; they never cross the model result boundary.
        internal: { before, after },
      })
      this.#assertRunCurrent(active)
      return {
        protocol: 'mineintent.d40-tool-result.v1',
        status: 'completed',
        effect,
        viewport,
      }
    } catch (error) {
      if (active.controller.signal.aborted || !this.#scopeMatches(active)) throw error
      this.#assertRunCurrent(active)
      const viewport = await this.#readViewport(invocation.runId, active.controller.signal).catch(() => undefined)
      this.#assertRunCurrent(active)
      await this.#journal.append('body_tool.failed', {
        actionId, runId: invocation.runId, tool: invocation.name,
        summary: error instanceof Error ? error.message : String(error),
      })
      this.#assertRunCurrent(active)
      return {
        protocol: 'mineintent.d40-tool-result.v1', status: 'failed',
        summary: error instanceof Error ? error.message.slice(0, 300) : 'tool_failed',
        ...(viewport ? { viewport } : {}),
      }
    } finally {
      try { if (motor) motor.releaseAll(); else this.#releaseBodyInputs() } catch { /* best effort */ }
      finally {
        this.#toolBusy = false
        try { this.#debug.update({ currentBodyTool: undefined }) } catch { /* cleanup must not wedge the body-tool gate */ }
      }
    }
  }

  async #handleBackendEvent(event: BackendEventEnvelope): Promise<void> {
    this.#interruptOnScopeChange(event)
    this.#refreshDebug()
    if (event.kind === 'chat') await this.#handleChat(event as BackendEventEnvelope<ProtocolChatEvent>)
    if (event.kind === 'self' || event.kind === 'snapshot_changed') this.#considerDanger()
  }

  async #handleChat(event: BackendEventEnvelope<ProtocolChatEvent>): Promise<void> {
    let snapshot
    try { snapshot = this.#backend.snapshot() } catch { return }
    const message = interpretPlayerChat(event, {
      companionUsername: snapshot.self.username,
      primaryPlayerUsernames: [this.#primaryPlayer],
      onlinePlayerUsernames: snapshot.trackedPlayers.filter(player => player.listed).map(player => player.username),
      conversationActiveWith: this.#primaryPlayer,
    })
    if (!message?.addressing.addressedToCompanion || !message.sender.isPrimaryPlayer) return
    this.#abortRunsAndRelease(message.controlIntent === 'safety_stop' ? 'player_safety_stop' : 'new_player_chat')
    const journalEvent = await this.#journal.append('player.chat.received', {
      sourceEventId: message.sourceEventId, sender: message.sender.username, text: message.text,
      controlIntent: message.controlIntent,
    })
    this.#pushRecent(journalEvent.id, journalEvent.type, `${message.sender.username}: ${message.text}`)

    if (message.controlIntent === 'safety_stop') {
      this.#speech.schedule({ id: randomUUID(), text: '好，我停下。' })
      await this.#journal.append('companion.safety_stop.applied', { sourceEventId: journalEvent.id })
      return
    }
    this.#enqueuePlayerDecision(message.sender.username, message.text, journalEvent.id)
  }

  #enqueuePlayerDecision(username: string, text: string, eventId: string): void {
    const controller = new AbortController()
    this.#modelAbort = controller
    const run = async () => {
      if (!this.#started || controller.signal.aborted) return
      await this.#runPlayerDecision(username, text, eventId, controller)
    }
    this.#decisionTail = this.#decisionTail.then(run, run).catch(error => this.#recordFailure('model', 'decision_failed', error))
  }

  async #runPlayerDecision(username: string, text: string, eventId: string, controller: AbortController): Promise<void> {
    const runId = randomUUID()
    const snapshot = this.#backend.snapshot()
    const active: ActiveRun = {
      runId, controller, processSessionId: snapshot.processSessionId,
      connectionEpoch: snapshot.connectionEpoch, worldId: snapshot.world.worldId,
      dimension: snapshot.world.dimension,
    }
    this.#activeRun = active
    let sources: DebugContextSource[] = []
    let memoryIds: string[] = []
    try {
      this.#assertRunCurrent(active)
      const memories = (await this.#memory.search(snapshot.world.worldId, text, 5)).map(result => result.record)
      memoryIds = memories.map(memory => memory.id)
      this.#assertRunCurrent(active)
      const observations = await this.#composePassiveObservations(runId, controller.signal)
      this.#assertRunCurrent(active)
      sources = [
        { id: this.#profile.versionId, kind: 'profile', size: this.#profile.content.length },
        { id: eventId, kind: 'player', size: text.length },
        ...memories.map(memory => ({ id: memory.id, kind: 'memory' as const, size: memory.summary.length })),
      ]
      this.#debug.update({ observations, decision: {
        status: 'running', runId, startedAt: new Date().toISOString(), contextSources: sources,
        retrievedMemoryIds: memoryIds,
      } })
      const context: D40DecisionContext = {
        protocol: 'mineintent.d40-context.v1',
        player: { username, text },
        profile: { content: this.#profile.content },
        world: { dimension: snapshot.world.dimension, ...(snapshot.world.timeOfDay === undefined ? {} : { timeOfDay: snapshot.world.timeOfDay }) },
        observations,
        recentEvents: this.#recentEvents.map(({ type, summary }) => ({ type, summary })),
        memories: memories.map(({ kind, summary, createdAt }) => ({ kind, summary, createdAt })),
      }
      const started = Date.now()
      const result = await this.#model.run({ runId, context }, controller.signal)
      this.#assertRunCurrent(active)
      await this.#journal.append('model.decision.completed', {
        runId, model: result.model, durationMs: Date.now() - started, usage: result.usage,
        effects: { speech: Boolean(result.decision.speech) },
      })
      this.#assertRunCurrent(active)
      this.#debug.update({ decision: {
        status: 'idle', model: result.model, contextSources: sources, retrievedMemoryIds: memoryIds,
      } })
      this.#assertRunCurrent(active)
      if (result.decision.speech) {
        this.#speech.schedule({ id: randomUUID(), text: result.decision.speech })
      }
    } catch (error) {
      if (controller.signal.aborted) return
      this.#debug.update({ decision: { status: 'failed', runId, contextSources: sources, retrievedMemoryIds: memoryIds } })
      this.#recordFailure('model', 'decision_failed', error)
    } finally {
      controller.abort('model_run_finished')
      this.#releaseBodyInputs()
      if (this.#activeRun?.controller === controller) this.#activeRun = undefined
      if (this.#modelAbort === controller) this.#modelAbort = undefined
    }
  }

  #considerDanger(): void {
    if (!this.#started || Date.now() - this.#lastDangerAt < 10_000) return
    let health
    try { health = this.#backend.snapshot().self.health } catch { return }
    if (health > 8) return
    this.#lastDangerAt = Date.now()
    this.#abortRunsAndRelease('danger_reflex')
    this.#speech.schedule({ id: randomUUID(), text: '我受伤了，先停一下。' })
  }

  #interruptOnScopeChange(event: BackendEventEnvelope): void {
    const active = this.#activeRun
    if (!active || (event.kind !== 'lifecycle' && event.kind !== 'world')) return
    const envelopeChanged = event.processSessionId !== active.processSessionId ||
      event.connectionEpoch !== active.connectionEpoch || event.worldId !== active.worldId ||
      (event.dimension !== undefined && event.dimension !== active.dimension)
    if (envelopeChanged || !this.#scopeMatches(active)) this.#abortRunsAndRelease('world_scope_changed')
  }

  #assertRunCurrent(active: ActiveRun): void {
    if (active.controller.signal.aborted || this.#activeRun !== active || this.#modelAbort !== active.controller) {
      throw new DOMException('Model run is no longer current', 'AbortError')
    }
    if (!this.#scopeMatches(active)) {
      this.#abortRunsAndRelease('world_scope_changed')
      throw new DOMException('Minecraft world scope changed', 'AbortError')
    }
  }

  #scopeMatches(scope: RunScope): boolean {
    try {
      if (this.#backend.state().status !== 'ready') return false
      const snapshot = this.#backend.snapshot()
      return snapshot.processSessionId === scope.processSessionId &&
        snapshot.connectionEpoch === scope.connectionEpoch &&
        snapshot.world.worldId === scope.worldId && snapshot.world.dimension === scope.dimension
    } catch { return false }
  }

  #abortRunsAndRelease(reason: string): void {
    this.#activeRun?.controller.abort(reason)
    this.#modelAbort?.abort(reason)
    this.#releaseBodyInputs()
  }

  #releaseBodyInputs(): void {
    try { this.#backend.motor().releaseAll() } catch { /* connection loss or driver cleanup failure */ }
  }

  async #readViewport(runId: string, signal: AbortSignal): Promise<ViewportValues> {
    const response = await this.#informationRuntime.query(this.#caller(runId), {
      interfaceId: 'viewport_information', operation: 'read', schemaRevision: 'viewport-information:5',
      fields: ['frame', 'standingOnBlock', 'lookedAtBlock', 'visibleEntities', 'visibleBlocks'],
    }, signal)
    if (response.protocol !== 'mineintent.information-read.v1') throw new Error(`viewport_read_failed:${response.protocol}`)
    return response.values as unknown as ViewportValues
  }

  async #composePassiveObservations(runId: string, signal: AbortSignal): Promise<PassiveObservations> {
    try { return await composePassiveObservations(this.#informationRuntime, this.#caller(runId), signal) }
    catch (error) {
      this.#recordFailure('runtime', 'passive_observations_failed', error)
      return { omissions: [] }
    }
  }

  #caller(runId: string): TrustedInformationCaller {
    return {
      principalId: INFORMATION_PRINCIPAL_ID, grantId: INFORMATION_GRANT_ID, purpose: 'companion_context',
      correlationId: runId, decisionRunId: runId,
    }
  }

  #refreshDebug(): void {
    let body
    try {
      const snapshot = this.#backend.snapshot()
      const inventory = new Map<string, number>()
      for (const slot of snapshot.inventory.slots) inventory.set(slot.itemName, (inventory.get(slot.itemName) ?? 0) + slot.count)
      body = {
        position: snapshot.self.position, health: snapshot.self.health, food: snapshot.self.food,
        inventory: [...inventory].map(([itemName, count]) => ({ itemName, count })),
      }
    } catch { /* connection is not ready */ }
    this.#debug.update({ connection: this.#backend.state(), body })
  }

  async #waitForSelfChunk(attempts = 20, intervalMs = 100): Promise<void> {
    for (let attempt = 0; attempt < attempts; attempt++) {
      try {
        const position = this.#backend.snapshot().self.position
        const result = this.#backend.observationSource().readBlock({
          x: Math.floor(position.x), y: Math.floor(position.y) - 1, z: Math.floor(position.z),
        })
        if (result.status !== 'unloaded') return
      } catch { /* backend not ready */ }
      await new Promise(resolve => setTimeout(resolve, intervalMs))
    }
  }

  #pushRecent(id: string, type: string, summary: string): void {
    this.#recentEvents.push({ id, type, summary })
    if (this.#recentEvents.length > 20) this.#recentEvents.shift()
  }

  #recordFailure(source: 'backend' | 'model' | 'body_tool' | 'memory' | 'runtime', code: string, error: unknown): void {
    const summary = error instanceof Error ? error.message : String(error)
    this.#debug.failure({ at: new Date().toISOString(), source, code, summary })
    void this.#journal.append(`${source}.failed`, { code, summary })
  }
}

interface PoseSample {
  position: { x: number; y: number; z: number }
  yaw: number
  pitch: number
}

function measuredLookEffect(before: PoseSample, after: PoseSample) {
  const yawDegrees = radiansToDegrees(normalizeRadians(before.yaw - after.yaw))
  const pitchDegrees = radiansToDegrees(before.pitch - after.pitch)
  return {
    relativeTurnDegrees: { yaw: withoutNegativeZero(yawDegrees), pitch: withoutNegativeZero(pitchDegrees) },
    turned: Math.hypot(yawDegrees, pitchDegrees) > LOOK_EFFECT_EPSILON_DEGREES,
  }
}

function measuredMoveEffect(before: PoseSample, after: PoseSample) {
  const delta = {
    x: after.position.x - before.position.x,
    y: after.position.y - before.position.y,
    z: after.position.z - before.position.z,
  }
  const forward = lookDirection(before.yaw, 0)
  const right = { x: -forward.z, z: forward.x }
  const relativeDisplacement: [number, number, number] = [
    withoutNegativeZero(delta.x * right.x + delta.z * right.z),
    withoutNegativeZero(delta.y),
    withoutNegativeZero(delta.x * forward.x + delta.z * forward.z),
  ]
  const distance = Math.hypot(delta.x, delta.y, delta.z)
  return {
    relativeDisplacement,
    distance: withoutNegativeZero(distance),
    movement: distance > MOVE_EFFECT_EPSILON ? 'changed' as const : 'no_effect' as const,
  }
}

function normalizeRadians(value: number): number {
  let normalized = value % (Math.PI * 2)
  if (normalized > Math.PI) normalized -= Math.PI * 2
  if (normalized < -Math.PI) normalized += Math.PI * 2
  return normalized
}
function radiansToDegrees(value: number): number { return value * 180 / Math.PI }
function withoutNegativeZero(value: number): number { return Object.is(value, -0) ? 0 : value }

function withoutPrivateSpeech(event: unknown): unknown {
  if (!event || typeof event !== 'object') return event
  const copy = { ...(event as Record<string, unknown>) }
  if ('text' in copy) copy.text = '[REDACTED]'
  return copy
}

function buildInformationRuntime(backend: MinecraftBackendApi, soundHistory: SoundHistory): InformationRuntime {
  const registry = new InformationRegistry()
  registry.register(new CurrentStatusProvider(new BackendSelfVitalsPort(backend)))
  registry.register(new InventoryProvider(new BackendInventoryPort(backend)))
  registry.register(new SoundInformationProvider(soundHistory))
  registry.register(new ViewportInformationProvider(new BackendPerceptionPort(backend)))
  registry.seal('1.21.1')
  const accessPolicy = new InMemoryInformationAccessPolicy()
  accessPolicy.put({
    id: INFORMATION_GRANT_ID, principalId: INFORMATION_PRINCIPAL_ID, audience: 'companion',
    allowedInterfaces: ['current_status', 'inventory_information', 'sound_information', 'viewport_information'],
    purpose: 'companion_context',
  })
  return new InformationRuntime({
    registry, accessPolicy, scopeSource: new BackendInformationScopeSource(backend, randomUUID()),
  })
}
