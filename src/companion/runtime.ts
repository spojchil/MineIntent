import { randomUUID } from 'node:crypto'
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
import type {
  BackendEventEnvelope, MinecraftBackendApi, MotorMoveDirection, ProtocolChatEvent,
} from '../minecraft/contracts.js'
import {
  agentToolDefinitions,
  lookArgumentsSchema,
  moveArgumentsSchema,
  rememberArgumentsSchema,
  sayArgumentsSchema,
  type AgentDecisionContext,
  type ModelProvider,
  type ToolInvocation,
} from '../models/index.js'
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

const TOOL_RESULT_PROTOCOL = 'mineintent.tool-result.v1'

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

interface ActiveRun extends RunScope {
  runId: string
  controller: AbortController
  /** Journal id of the player chat that started this run; evidence for tool-written memories. */
  chatEventId: string
  sayCount: number
  /** Movement inputs already used in the current round, so opposing pairs can be refused. */
  roundMoves: { roundId: number; directions: Set<MotorMoveDirection> }
}

/** Which resource a tool needs. Locks are per resource, not per tool: `say` and `remember` touch
 * neither the body nor each other, so a body input in flight must not block them. */
const TOOL_RESOURCE: Record<string, 'body' | 'chat' | 'memory'> = {
  look_relative: 'body', move_input: 'body', say: 'chat', remember: 'memory',
}

const OPPOSING_DIRECTION: Record<MotorMoveDirection, MotorMoveDirection> = {
  forward: 'back', back: 'forward', left: 'right', right: 'left',
}

const MOVE_EFFECT_EPSILON = 0.01
const LOOK_EFFECT_EPSILON_DEGREES = 0.01

/**
 * The runtime intentionally has one model route: an addressed player chat. Chat text has no
 * privileged control phrases, and the model-visible surface is the small tool set in
 * `models/agent-tools.ts` — two short body inputs, `say`, and `remember`.
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
  #chatTail = Promise.resolve()
  #decisionTail = Promise.resolve()
  #runGeneration = 0
  readonly #leases = new Map<'body' | 'chat' | 'memory', string>()
  #started = false

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
    this.#unsubscribe = this.#backend.subscribe(event => {
      const handle = () => this.#handleBackendEvent(event)
      if (event.kind === 'chat') {
        this.#chatTail = this.#chatTail.then(handle, handle)
          .catch(error => this.#recordFailure('runtime', 'chat_handler_failed', error))
      } else {
        void handle().catch(error => this.#recordFailure('runtime', 'backend_event_failed', error))
      }
    })
    await this.#backend.start(this.#abort.signal)
    await this.#waitForSelfChunk()
    this.#refreshDebug()
    const event = await this.#journal.append('companion.started', { summary: '同伴加入世界' })
    this.#pushRecent(event.id, event.type, '同伴加入世界')
  }

  async stop(reason = 'runtime_stopped'): Promise<void> {
    if (!this.#started) return
    this.#started = false
    this.#invalidateRuns(reason)
    await this.#chatTail
    await this.#decisionTail
    this.#unsubscribe?.()
    this.#soundHistory.dispose()
    await this.#backend.stop(reason)
    await this.#journal.append('companion.stopped', { reason })
    await this.#journal.flush()
    this.#debug.update({ connection: this.#backend.state(), currentBodyTool: undefined })
  }

  async idle(): Promise<void> {
    await this.#chatTail
    await this.#decisionTail
  }

  /** Called only by the authenticated loopback bridge while the matching player-chat run lives. */
  async executeTool(invocation: ToolInvocation): Promise<unknown> {
    const active = this.#activeRun
    if (!active || active.runId !== invocation.runId) throw new Error('tool_run_is_not_active')
    this.#assertRunCurrent(active)
    const resource = TOOL_RESOURCE[invocation.name]
    if (resource === undefined) {
      // An honest unknown-name failure instead of a transport error: the model can recover.
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: `unknown_tool:${invocation.name.slice(0, 64)}` }
    }
    // A held resource is a fact about the world, so it comes back as a failed result. Throwing here
    // would surface as a bridge 500 and kill the whole run over a transient conflict.
    if (this.#leases.has(resource)) {
      return {
        protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
        summary: `resource_busy:${resource} is held by ${this.#leases.get(resource)}`,
      }
    }
    const actionId = randomUUID()
    const startedAt = new Date().toISOString()
    this.#leases.set(resource, invocation.name)
    try {
      if (resource === 'body') {
        this.#debug.update({ currentBodyTool: { id: actionId, tool: invocation.name, purpose: 'agent tool', startedAt } })
      }
      this.#assertRunCurrent(active)
      switch (invocation.name) {
        case 'look_relative':
        case 'move_input':
          return await this.#executeBodyTool(active, invocation, actionId, startedAt)
        case 'say':
          return this.#executeSay(active, invocation, actionId)
        default:
          return await this.#executeRemember(active, invocation, actionId)
      }
    } finally {
      this.#leases.delete(resource)
      if (resource === 'body') {
        try { this.#debug.update({ currentBodyTool: undefined }) } catch { /* cleanup must not wedge the gate */ }
      }
    }
  }

  /**
   * Refuses movement inputs that cancel each other within one model round, and allows the ones a
   * player really does combine. Same-round is the meaningful unit: the model asked for these
   * together, so `forward` plus `back` is incoherent while `forward` plus `left` is how anyone
   * walks diagonally. A separate round is a new decision and starts clean.
   */
  #conflictingMove(active: ActiveRun, roundId: number, direction: MotorMoveDirection): string | undefined {
    if (active.roundMoves.roundId !== roundId) {
      active.roundMoves = { roundId, directions: new Set() }
    }
    const opposing = OPPOSING_DIRECTION[direction]
    if (active.roundMoves.directions.has(opposing)) {
      return `opposing_move:${direction} cancels ${opposing} already held this round`
    }
    active.roundMoves.directions.add(direction)
    return undefined
  }

  async #executeBodyTool(
    active: ActiveRun,
    invocation: ToolInvocation,
    actionId: string,
    startedAt: string,
  ): Promise<unknown> {
    const name = invocation.name as 'look_relative' | 'move_input'
    if (name === 'move_input') {
      const parsed = moveArgumentsSchema.safeParse(invocation.arguments)
      const conflict = parsed.success ? this.#conflictingMove(active, invocation.roundId, parsed.data.direction) : undefined
      if (conflict !== undefined) {
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: conflict }
      }
    }
    let motor: ReturnType<MinecraftBackendApi['motor']> | undefined
    try {
      motor = this.#backend.motor()
      const before = this.#backend.observationSource().selfPose()
      if (name === 'look_relative') {
        const args = lookArgumentsSchema.parse(invocation.arguments)
        await motor.lookRelative(args.yaw_degrees, args.pitch_degrees, active.controller.signal)
      } else {
        const args = moveArgumentsSchema.parse(invocation.arguments)
        await motor.move(args.direction, args.duration_ms, args.sprint, active.controller.signal)
      }
      this.#assertRunCurrent(active)
      const after = this.#backend.observationSource().selfPose()
      const viewport = await this.#readViewport(active.runId, active.controller.signal)
      this.#assertRunCurrent(active)
      const effect = name === 'look_relative' ? measuredLookEffect(before, after) : measuredMoveEffect(before, after)
      await this.#journal.append('body_tool.completed', {
        actionId, runId: active.runId, toolCallId: invocation.toolCallId, roundId: invocation.roundId,
        tool: name, startedAt,
        // Internal diagnostics may retain poses; they never cross the model result boundary.
        internal: { before, after },
      })
      this.#assertRunCurrent(active)
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'completed', effect, viewport }
    } catch (error) {
      if (active.controller.signal.aborted || !this.#scopeMatches(active)) throw error
      this.#assertRunCurrent(active)
      const viewport = await this.#readViewport(active.runId, active.controller.signal).catch(() => undefined)
      this.#assertRunCurrent(active)
      await this.#journal.append('body_tool.failed', {
        actionId, runId: active.runId, toolCallId: invocation.toolCallId, roundId: invocation.roundId,
        tool: name,
        summary: error instanceof Error ? error.message : String(error),
      })
      this.#assertRunCurrent(active)
      return {
        protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
        summary: error instanceof Error ? error.message.slice(0, 300) : 'tool_failed',
        ...(viewport ? { viewport } : {}),
      }
    } finally {
      try { if (motor) motor.releaseAll(); else this.#releaseBodyInputs() } catch { /* best effort */ }
    }
  }

  /** The only speech channel. Silence is simply this tool never being called during a run. */
  #executeSay(active: ActiveRun, invocation: ToolInvocation, actionId: string): unknown {
    const parsed = sayArgumentsSchema.safeParse(invocation.arguments)
    const text = parsed.success ? parsed.data.text.trim() : ''
    if (!text) {
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: 'say requires a non-empty text' }
    }
    let segments: number
    try {
      // actionId doubles as the speech request id so scheduler events correlate with this call.
      segments = this.#speech.schedule({ id: actionId, text })
    } catch (error) {
      return {
        protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
        summary: error instanceof Error ? error.message.slice(0, 300) : 'say_failed',
      }
    }
    active.sayCount += 1
    void this.#journal.append('say.queued', {
      actionId, runId: active.runId, toolCallId: invocation.toolCallId, roundId: invocation.roundId,
      segments, characters: text.length,
    })
    // `queued`, not `completed`: the scheduler segments and rate-limits, so the player has not seen
    // this yet and a later scope change can still cancel it. Reporting completion here would be the
    // same fabricated success the tool results exist to prevent.
    return { protocol: TOOL_RESULT_PROTOCOL, status: 'queued', segments }
  }

  async #executeRemember(
    active: ActiveRun,
    invocation: ToolInvocation,
    actionId: string,
  ): Promise<unknown> {
    const parsed = rememberArgumentsSchema.safeParse(invocation.arguments)
    const summary = parsed.success ? parsed.data.summary.trim() : ''
    if (!summary) {
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: 'remember requires a non-empty summary' }
    }
    try {
      await this.#memory.remember({
        worldId: active.worldId,
        kind: 'episode',
        summary,
        evidence: [{ kind: 'event', id: active.chatEventId }],
      })
      this.#assertRunCurrent(active)
      // The summary itself stays out of the journal, matching the redaction stance on speech.
      await this.#journal.append('memory.remembered', {
        actionId, runId: active.runId, toolCallId: invocation.toolCallId, roundId: invocation.roundId,
      })
      this.#assertRunCurrent(active)
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'completed' }
    } catch (error) {
      if (active.controller.signal.aborted || !this.#scopeMatches(active)) throw error
      return {
        protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
        summary: error instanceof Error ? error.message.slice(0, 300) : 'remember_failed',
      }
    }
  }

  async #handleBackendEvent(event: BackendEventEnvelope): Promise<void> {
    this.#interruptOnScopeChange(event)
    this.#refreshDebug()
    if (event.kind === 'chat') await this.#handleChat(event as BackendEventEnvelope<ProtocolChatEvent>)
  }

  async #handleChat(event: BackendEventEnvelope<ProtocolChatEvent>): Promise<void> {
    let snapshot
    try { snapshot = this.#backend.snapshot() } catch { return }
    if (snapshot.processSessionId !== event.processSessionId ||
      snapshot.connectionEpoch !== event.connectionEpoch || snapshot.world.worldId !== event.worldId ||
      (event.dimension !== undefined && snapshot.world.dimension !== event.dimension)) return
    const message = interpretPlayerChat(event, {
      companionUsername: snapshot.self.username,
      primaryPlayerUsernames: [this.#primaryPlayer],
      onlinePlayerUsernames: snapshot.trackedPlayers.filter(player => player.listed).map(player => player.username),
      conversationActiveWith: this.#primaryPlayer,
    })
    if (!message?.addressing.addressedToCompanion || !message.sender.isPrimaryPlayer) return
    const generation = this.#runGeneration
    const journalEvent = await this.#journal.append('player.chat.received', {
      sourceEventId: message.sourceEventId, sender: message.sender.username, text: message.text,
    })
    if (!this.#started || generation !== this.#runGeneration) return
    this.#enqueuePlayerDecision(message.sender.username, message.text, journalEvent.id, generation)
  }

  #enqueuePlayerDecision(username: string, text: string, eventId: string, generation: number): void {
    const run = async () => {
      if (!this.#started || generation !== this.#runGeneration) return
      this.#pushRecent(eventId, 'player.chat.received', `${username}: ${text}`)
      const controller = new AbortController()
      this.#modelAbort = controller
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
      dimension: snapshot.world.dimension, chatEventId: eventId, sayCount: 0,
      roundMoves: { roundId: -1, directions: new Set() },
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
      const context: AgentDecisionContext = {
        protocol: 'mineintent.agent-context.v1',
        player: { username, text },
        profile: { content: this.#profile.content },
        world: { dimension: snapshot.world.dimension, ...(snapshot.world.timeOfDay === undefined ? {} : { timeOfDay: snapshot.world.timeOfDay }) },
        observations,
        recentEvents: this.#recentEvents.map(({ type, summary }) => ({ type, summary })),
        memories: memories.map(({ kind, summary, createdAt }) => ({ kind, summary, createdAt })),
      }
      const started = Date.now()
      // Speech happens through the say tool while the run lives; the result only reports completion.
      const result = await this.#model.run({ runId, context, tools: agentToolDefinitions() }, controller.signal)
      this.#assertRunCurrent(active)
      await this.#journal.append('model.decision.completed', {
        runId, model: result.model, durationMs: Date.now() - started, usage: result.usage,
        effects: { sayCalls: active.sayCount },
      })
      this.#assertRunCurrent(active)
      this.#debug.update({ decision: {
        status: 'idle', model: result.model, contextSources: sources, retrievedMemoryIds: memoryIds,
      } })
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

  #interruptOnScopeChange(event: BackendEventEnvelope): void {
    if (event.kind !== 'lifecycle' && event.kind !== 'world') return
    const lifecycleType = event.kind === 'lifecycle' ? (event.payload as { type?: string }).type : undefined
    const lifecycleInvalidates = lifecycleType !== undefined && [
      'connection_requested', 'died', 'respawn_transition_started', 'respawned',
      'dimension_changed', 'reconnect_scheduled', 'connection_closed', 'faulted', 'stopped',
    ].includes(lifecycleType)
    const active = this.#activeRun
    if (!active) {
      if (lifecycleInvalidates) this.#invalidateRuns('world_scope_changed')
      return
    }
    const envelopeChanged = event.processSessionId !== active.processSessionId ||
      event.connectionEpoch !== active.connectionEpoch || event.worldId !== active.worldId ||
      (event.dimension !== undefined && event.dimension !== active.dimension)
    if (lifecycleInvalidates || envelopeChanged || !this.#scopeMatches(active)) this.#invalidateRuns('world_scope_changed')
  }

  #assertRunCurrent(active: ActiveRun): void {
    if (active.controller.signal.aborted || this.#activeRun !== active || this.#modelAbort !== active.controller) {
      throw new DOMException('Model run is no longer current', 'AbortError')
    }
    if (!this.#scopeMatches(active)) {
      this.#invalidateRuns('world_scope_changed')
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

  #invalidateRuns(reason: string): number {
    const generation = ++this.#runGeneration
    this.#activeRun?.controller.abort(reason)
    this.#modelAbort?.abort(reason)
    this.#speech.stop(reason)
    this.#releaseBodyInputs()
    return generation
  }

  #releaseBodyInputs(): void {
    try { this.#backend.motor().releaseAll() } catch { /* connection loss or driver cleanup failure */ }
  }

  async #readViewport(runId: string, signal: AbortSignal): Promise<ViewportValues> {
    const response = await this.#informationRuntime.query(this.#caller(runId), {
      interfaceId: 'viewport_information', operation: 'read', schemaRevision: 'viewport-information:7',
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
