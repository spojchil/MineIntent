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
import { ExecutionArbiter, type ExecutionResource } from '../execution/index.js'
import type {
  BackendEventEnvelope, MinecraftBackendApi, ProtocolChatEvent,
} from '../minecraft/contracts.js'
import {
  agentToolDefinitions,
  lookArgumentsSchema,
  moveArgumentsSchema,
  rememberArgumentsSchema,
  sayArgumentsSchema,
  type AgentDecisionContext,
  type AgentFrame,
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
  round?: ActiveRound
}

/**
 * One model response that contains tool calls. It is born on the response's first call because a
 * response without calls never reaches this process. Its identity, scope and movement ledger all
 * die with the owning run; keeping any of them elsewhere would let middle-layer knowledge outlive
 * the context it mirrors.
 */
interface ActiveRound extends RunScope {
  roundId: string
  runId: string
  directions: Set<string>
}

type HostedToolInvocation = Omit<ToolInvocation, 'round'> & { roundId: string }

/** Which resource a tool needs. Locks are per resource, not per tool: `say` and `remember` touch
 * neither the body nor each other, so a body input in flight must not block them. */
const TOOL_RESOURCE: Record<string, ExecutionResource> = {
  look_relative: 'body', move_input: 'body', say: 'chat', remember: 'memory',
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
  /**
   * Events waiting for the next frame to carry them. Drained rather than kept as a rolling window:
   * a window has to be re-sent every request to stay current, which is what made the old
   * `recentEvents` both redundant and hostile to the prompt cache. Drained, each event is stated
   * once and then lives in the conversation as history.
   */
  readonly #pendingEvents: Array<{ type: string; summary: string; scope?: RunScope }> = []
  #omittedEvents = 0
  #lastHealth?: number
  /** Kept for the local debug view, which shows what was read rather than what was sent. */
  #lastObservations?: PassiveObservations
  #unsubscribe?: () => void
  #modelAbort?: AbortController
  #activeRun?: ActiveRun
  #chatTail = Promise.resolve()
  #decisionTail = Promise.resolve()
  #runGeneration = 0
  readonly #execution = new ExecutionArbiter()
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
    await this.#journal.append('companion.started', { summary: '同伴加入世界' })
    this.#pushPending('companion.started', '同伴加入世界')
    this.#noticeSelfChanges()
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
  async executeTool(invocation: ToolInvocation): Promise<{ roundId: string; result: unknown }> {
    const active = this.#activeRun
    if (!active || active.runId !== invocation.runId) throw new Error('tool_run_is_not_active')
    this.#assertRunCurrent(active)
    const round = this.#roundFor(active, invocation.round)
    const { round: _declaration, ...call } = invocation
    const hosted: HostedToolInvocation = { ...call, roundId: round.roundId }
    const resource = TOOL_RESOURCE[invocation.name]
    if (resource === undefined) {
      // An honest unknown-name failure instead of a transport error: the model can recover.
      return {
        roundId: round.roundId,
        result: { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: `unknown_tool:${invocation.name.slice(0, 64)}` },
      }
    }
    // A held resource is a fact about the world, so it comes back as a failed result. Throwing here
    // would surface as a bridge 500 and kill the whole run over a transient conflict.
    const acquired = this.#execution.acquire({ resource, runId: active.runId, toolName: invocation.name })
    if ('code' in acquired) {
      return {
        roundId: round.roundId,
        result: { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: acquired.summary },
      }
    }
    const { actionId, acquiredAt: startedAt } = acquired.lease
    try {
      if (resource === 'body') {
        this.#debug.update({ currentBodyTool: { id: actionId, tool: invocation.name, purpose: 'agent tool', startedAt } })
      }
      this.#assertRunCurrent(active)
      let result: unknown
      switch (invocation.name) {
        case 'look_relative':
        case 'move_input':
          result = await this.#executeBodyTool(active, round, hosted, actionId, startedAt)
          break
        case 'say':
          result = this.#executeSay(active, hosted, actionId)
          break
        default:
          result = await this.#executeRemember(active, hosted, actionId)
      }
      return { roundId: round.roundId, result }
    } finally {
      acquired.release()
      if (resource === 'body') {
        try { this.#debug.update({ currentBodyTool: undefined }) } catch { /* cleanup must not wedge the gate */ }
      }
    }
  }

  async #executeBodyTool(
    active: ActiveRun,
    round: ActiveRound,
    invocation: HostedToolInvocation,
    actionId: string,
    startedAt: string,
  ): Promise<unknown> {
    const name = invocation.name as 'look_relative' | 'move_input'
    if (name === 'move_input') {
      const parsed = moveArgumentsSchema.safeParse(invocation.arguments)
      const conflict = parsed.success
        ? this.#execution.admitMove(round, parsed.data.direction)
        : undefined
      if (conflict !== undefined) {
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: conflict.summary }
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
  #executeSay(active: ActiveRun, invocation: HostedToolInvocation, actionId: string): unknown {
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
    invocation: HostedToolInvocation,
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
    if (event.kind === 'self' || event.kind === 'snapshot_changed') this.#noticeSelfChanges()
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
      // The message itself rides in the opening frame's `player`, so it is deliberately not also
      // pushed as an event: each fact is stated once.
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
    }
    this.#activeRun = active
    let sources: DebugContextSource[] = []
    let memoryIds: string[] = []
    try {
      this.#assertRunCurrent(active)
      const memories = (await this.#memory.search(snapshot.world.worldId, text, 5)).map(result => result.record)
      memoryIds = memories.map(memory => memory.id)
      this.#assertRunCurrent(active)
      const frame = await this.#composeFrame(active, controller.signal, { username, text })
      this.#assertRunCurrent(active)
      sources = [
        { id: this.#profile.versionId, kind: 'profile', size: this.#profile.content.length },
        { id: eventId, kind: 'player', size: text.length },
        ...memories.map(memory => ({ id: memory.id, kind: 'memory' as const, size: memory.summary.length })),
      ]
      this.#debug.update({ observations: this.#lastObservations, decision: {
        status: 'running', runId, startedAt: new Date().toISOString(), contextSources: sources,
        retrievedMemoryIds: memoryIds,
      } })
      const context: AgentDecisionContext = {
        protocol: 'mineintent.agent-context.v2',
        stable: {
          profile: { content: this.#profile.content },
          memories: memories.map(({ kind, summary, createdAt }) => ({ kind, summary, createdAt })),
        },
        frame,
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
      active.round = undefined
      this.#execution.pruneSettledJobs()
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

  /**
   * Opens a round only when the agent loop's first tool call declares one. The UUID rides back on
   * that call's existing response, so later calls can echo it without another network turn; a
   * response with no tools creates no host at all.
   */
  #roundFor(active: ActiveRun, declaration: ToolInvocation['round']): ActiveRound {
    if ('new' in declaration) {
      const round: ActiveRound = {
        roundId: randomUUID(), runId: active.runId, directions: new Set<string>(),
        processSessionId: active.processSessionId, connectionEpoch: active.connectionEpoch,
        worldId: active.worldId, dimension: active.dimension,
      }
      active.round = round
      return round
    }
    const round = active.round
    if (round === undefined || round.roundId !== declaration.id || round.runId !== active.runId) {
      throw new Error('tool_round_is_not_active')
    }
    if (!sameScope(round, active)) throw new Error('tool_round_scope_is_not_active')
    return round
  }

  #scopeMatches(scope: RunScope): boolean {
    try { if (this.#backend.state().status !== 'ready') return false } catch { return false }
    const current = this.#currentScope()
    return current !== undefined && sameScope(current, scope)
  }

  #invalidateRuns(reason: string): number {
    const generation = ++this.#runGeneration
    // Scope loss voids every lease and running job, not just the awaited call.
    this.#forgetPendingFacts()
    this.#execution.invalidate(reason)
    if (this.#activeRun) {
      this.#activeRun.round = undefined
      this.#activeRun.controller.abort(reason)
    }
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
      interfaceId: 'viewport_information', operation: 'read', schemaRevision: 'viewport-information:10',
      fields: ['frame', 'standingOnBlock', 'lookedAtBlock', 'visibleEntities', 'visibleBlocks'],
    }, signal)
    if (response.protocol !== 'mineintent.information-read.v1') throw new Error(`viewport_read_failed:${response.protocol}`)
    return response.values as unknown as ViewportValues
  }

  async #composePassiveObservations(runId: string, signal: AbortSignal): Promise<PassiveObservations> {
    try {
      this.#lastObservations = await composePassiveObservations(this.#informationRuntime, this.#caller(runId), signal)
      return this.#lastObservations
    } catch (error) {
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

  /**
   * Stamps the scope the fact was observed in, at the moment it is observed.
   *
   * Not cleared on invalidation instead, which was the other candidate: invalidation is a reaction,
   * so a fact pushed between the world actually changing and the runtime noticing would survive the
   * sweep. Stamping cannot have that gap — an event either carries the scope it was seen in or it is
   * unattributable, and both are decided here rather than later.
   *
   * A missing scope means the snapshot was unreadable, which is itself a reason not to replay the
   * fact into whatever world comes next.
   */
  #pushPending(type: string, summary: string): void {
    this.#pendingEvents.push({ type, summary, scope: this.#currentScope() })
    // Bounded because nothing guarantees a frame is coming: an idle companion could otherwise
    // accumulate events forever. Dropping the oldest is counted, never hidden.
    while (this.#pendingEvents.length > 20) {
      this.#pendingEvents.shift()
      this.#omittedEvents += 1
    }
  }

  #currentScope(): RunScope | undefined {
    try {
      const snapshot = this.#backend.snapshot()
      return {
        processSessionId: snapshot.processSessionId,
        connectionEpoch: snapshot.connectionEpoch,
        worldId: snapshot.world.worldId,
        dimension: snapshot.world.dimension,
      }
    } catch { return undefined }
  }

  /**
   * Drains only what belongs to this run's world. "受到伤害" from the world before a dimension change
   * is not news in the world after it — it is a statement about somewhere the companion no longer is,
   * and the model has no way to tell that from a fresh injury.
   *
   * Facts from another scope are dropped rather than held: there is no run that could ever be the
   * right audience for them. They are counted into `omittedEvents` for the same reason the size cap
   * is — the model is told that something was left out, never quietly given less.
   */
  #drainEvents(scope: RunScope): { events: Array<{ type: string; summary: string }>; omittedEvents?: number } {
    const pending = this.#pendingEvents.splice(0, this.#pendingEvents.length)
    const events: Array<{ type: string; summary: string }> = []
    for (const entry of pending) {
      if (entry.scope !== undefined && sameScope(entry.scope, scope)) {
        events.push({ type: entry.type, summary: entry.summary })
      } else {
        this.#omittedEvents += 1
      }
    }
    const omittedEvents = this.#omittedEvents
    this.#omittedEvents = 0
    return { events, ...(omittedEvents > 0 ? { omittedEvents } : {}) }
  }

  /**
   * Forgets facts and baselines belonging to a world the companion has left. The health baseline
   * matters as much as the queue: kept across a respawn it compares 20 against the 0 from dying, and
   * kept across a dimension change it turns any ordinary difference into a phantom injury.
   */
  #forgetPendingFacts(): void {
    this.#pendingEvents.length = 0
    this.#omittedEvents = 0
    this.#lastHealth = undefined
  }

  /**
   * Notices the things the model would never ask about. `snapshot_changed` only says "self changed",
   * so a drop has to be found by comparing: without this the frame channel exists but has no
   * interrupt to carry, and damage stays invisible until something happens to look at status.
   */
  #noticeSelfChanges(): void {
    let health: number | undefined
    try { health = this.#backend.snapshot().self.health } catch { return }
    const previous = this.#lastHealth
    this.#lastHealth = health
    if (previous === undefined || health >= previous) return
    const lost = Math.round((previous - health) * 10) / 10
    this.#pushPending('self.health.dropped', `受到伤害，生命值 ${previous} → ${health}（-${lost}）`)
  }

  /** Composes a frame for the tool bridge, but only when there is news that warrants one. */
  async takePendingFrame(runId: string): Promise<AgentFrame | undefined> {
    const active = this.#activeRun
    if (!active || active.runId !== runId || this.#pendingEvents.length === 0) return undefined
    try { return await this.#composeFrame(active, active.controller.signal) }
    catch (error) {
      // A frame is an extra: failing to build one must never fail the tool result it rides with.
      this.#recordFailure('runtime', 'frame_compose_failed', error)
      return undefined
    }
  }

  async #composeFrame(
    active: ActiveRun,
    signal: AbortSignal,
    player?: { username: string; text: string },
  ): Promise<AgentFrame> {
    const observations = await this.#composePassiveObservations(active.runId, signal)
    const self = observations.viewport?.frame.self
    let world: AgentFrame['world'] = { dimension: active.dimension }
    try {
      const snapshot = this.#backend.snapshot()
      world = { dimension: snapshot.world.dimension, ...(snapshot.world.timeOfDay === undefined ? {} : { timeOfDay: snapshot.world.timeOfDay }) }
    } catch { /* connection is not ready; the run scope still names the dimension */ }
    return {
      at: new Date().toISOString(),
      ...(player === undefined ? {} : { player }),
      world,
      ...(self === undefined ? {} : { self: { position: self.position, yawDegrees: self.yawDegrees, pitchDegrees: self.pitchDegrees } }),
      ...(observations.currentStatus === undefined ? {} : { status: observations.currentStatus }),
      ...(observations.inventory === undefined ? {} : { inventory: observations.inventory }),
      ...(observations.sound === undefined ? {} : { sound: observations.sound }),
      ...this.#drainEvents(active),
      omissions: observations.omissions,
    }
  }

  #recordFailure(source: 'backend' | 'model' | 'body_tool' | 'memory' | 'runtime', code: string, error: unknown): void {
    const summary = error instanceof Error ? error.message : String(error)
    this.#debug.failure({ at: new Date().toISOString(), source, code, summary })
    void this.#journal.append(`${source}.failed`, { code, summary })
  }
}

/**
 * One definition of "the same world", used by every check that needs one. Two places that must
 * agree about scope identity is the same defect shape as two places that must agree about where a
 * player's eyes are.
 */
function sameScope(left: RunScope, right: RunScope): boolean {
  return left.processSessionId === right.processSessionId &&
    left.connectionEpoch === right.connectionEpoch &&
    left.worldId === right.worldId && left.dimension === right.dimension
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

/**
 * Says which frame it measured in, in place. The viewport declares every position it reports as
 * `minecraft_world_absolute`, and a model that carries that legend across to an action result would
 * read this triple as a world offset. It is not one: it is resolved against the pre-move facing, so
 * the same displacement means different things depending on where the companion was looking.
 */
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
    coordinates: 'body_relative_before_move' as const,
    legend: 'relativeDisplacement 是 [右, 上, 前] 三个格数，相对移动前的朝向，不是世界绝对坐标',
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
