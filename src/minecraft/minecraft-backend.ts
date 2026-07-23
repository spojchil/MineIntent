import { createHash, randomUUID } from 'node:crypto'
import type {
  BackendClose,
  BackendEventEnvelope,
  BackendEventKind,
  BackendFailure,
  BackendLifecyclePayload,
  BackendReady,
  BackendState,
  BlockPosition,
  MinecraftBackendApi,
  MinecraftBackendConfig,
  MinecraftMotorDriverApi,
  MinecraftSnapshotV1,
  ProtocolBlockEvent,
  ProtocolChatEvent,
  ProtocolEntityEvent,
  ProtocolObservationSource,
  ProtocolSoundPayload,
  Unsubscribe,
} from './contracts.js'
import { parseMinecraftBackendConfig } from './config.js'
import { blockDto, entityDto, finiteNumber, readBlock, vectorDto } from './dto.js'
import type {
  BotLike,
  CancelableTimer,
  Clock,
  EntityLike,
  MineflayerBotFactory,
  RandomSource,
  Scheduler,
} from './internal.js'
import { systemClock, systemRandom, systemScheduler } from './internal.js'
import { DefaultMineflayerBotFactory } from './mineflayer-bot-factory.js'
import { MineflayerMotorDriver } from './motor-driver.js'
import type { Bot } from 'mineflayer'
import { buildSnapshot, isReady } from './snapshot.js'

interface BackendDependencies {
  botFactory: MineflayerBotFactory
  clock: Clock
  scheduler: Scheduler
  random: RandomSource
  id: () => string
}

interface CloseEvidence {
  kick?: { text: string; duringLogin: boolean }
  error?: { name: string; message: string; code?: string }
  timeoutCode?: BackendFailure['code']
}

interface ActiveConnection {
  epoch: number
  attemptId: string
  bot: BotLike
  disposers: Array<() => void>
  phaseTimer?: CancelableTimer
  readinessTimer?: CancelableTimer
  stableTimer?: CancelableTimer
  closeEvidence: CloseEvidence
  closeSealed: boolean
  previousDimension?: string
  respawnTransition: boolean
  wasDead: boolean
  lastSoundFingerprints: Map<string, number>
}

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej })
  return { promise, resolve, reject }
}

export class BackendNotReadyError extends Error {
  constructor() { super('Minecraft Backend is not ready') }
}

export class StaleBackendEpochError extends Error {
  constructor() { super('Minecraft Backend observation source belongs to a stale connection epoch') }
}

export class MinecraftBackend implements MinecraftBackendApi {
  readonly config: MinecraftBackendConfig
  readonly processSessionId: string

  #deps: BackendDependencies
  #state: BackendState = { status: 'idle' }
  #active?: ActiveConnection
  #epoch = 0
  #attempt = 0
  #lifecycleRevision = 0
  #snapshotRevision = 0
  #subscribers = new Set<(event: BackendEventEnvelope) => void>()
  #observationSubscribers = new Set<{
    epoch: number
    listener: (event: BackendEventEnvelope<ProtocolEntityEvent | ProtocolBlockEvent | ProtocolSoundPayload>) => void
  }>()
  #startDeferred?: Deferred<BackendReady>
  #stopDeferred?: Deferred<void>
  #startedReady = false
  #stopRequested = false
  #stopReason = 'stopped'
  #reconnectTimer?: CancelableTimer
  #stopTimer?: CancelableTimer
  #abortSignal?: AbortSignal
  #abortHandler?: () => void

  constructor(config: unknown, dependencies: Partial<BackendDependencies> = {}) {
    this.config = parseMinecraftBackendConfig(config)
    this.#deps = {
      botFactory: dependencies.botFactory ?? new DefaultMineflayerBotFactory(),
      clock: dependencies.clock ?? systemClock,
      scheduler: dependencies.scheduler ?? systemScheduler,
      random: dependencies.random ?? systemRandom,
      id: dependencies.id ?? randomUUID,
    }
    this.processSessionId = this.#deps.id()
  }

  state(): Readonly<BackendState> { return structuredClone(this.#state) }

  subscribe(listener: (event: BackendEventEnvelope) => void): Unsubscribe {
    this.#subscribers.add(listener)
    let active = true
    return () => { if (active) { active = false; this.#subscribers.delete(listener) } }
  }

  async start(signal: AbortSignal): Promise<BackendReady> {
    if (this.#startDeferred && !['stopped', 'faulted'].includes(this.#state.status)) return this.#startDeferred.promise
    if (signal.aborted) throw new DOMException('Backend start aborted', 'AbortError')

    this.#stopRequested = false
    this.#startedReady = false
    this.#attempt = 0
    this.#startDeferred = deferred<BackendReady>()
    this.#abortSignal = signal
    this.#abortHandler = () => { void this.stop('abort_signal') }
    signal.addEventListener('abort', this.#abortHandler, { once: true })
    this.#beginAttempt()
    return this.#startDeferred.promise
  }

  async stop(reason: string): Promise<void> {
    if (this.#stopDeferred) return this.#stopDeferred.promise
    if (this.#state.status === 'stopped' || this.#state.status === 'idle') {
      this.#transition({ status: 'stopped', reason })
      return
    }

    this.#stopDeferred = deferred<void>()
    const stopPromise = this.#stopDeferred.promise
    this.#stopRequested = true
    this.#stopReason = reason
    this.#reconnectTimer?.cancel()
    this.#reconnectTimer = undefined
    const epoch = this.#active?.epoch
    this.#transition({ status: 'stopping', ...(epoch === undefined ? {} : { epoch }), reason })
    this.#active?.phaseTimer?.cancel()
    this.#active?.readinessTimer?.cancel()
    this.#active?.stableTimer?.cancel()

    if (!this.#active) {
      this.#finishStopped()
      return stopPromise
    }

    try {
      this.#active.bot.clearControlStates?.()
      this.#active.bot.quit(reason)
    } catch {
      try { this.#active.bot.end(reason) } catch { /* already closed */ }
    }

    this.#stopTimer = this.#deps.scheduler.timeout(this.config.timeouts.stopMs, () => {
      try { this.#active?.bot.end(reason) } catch { /* already closed */ }
      this.#disposeActive()
      this.#finishStopped()
    })
    return stopPromise
  }

  snapshot(): Readonly<MinecraftSnapshotV1> {
    const active = this.#active
    if (!active || !['ready', 'dead'].includes(this.#state.status)) throw new BackendNotReadyError()
    return buildSnapshot(active.bot, this.#snapshotContext(active), this.#state.status === 'dead')
  }

  sendChat(message: string): void {
    const active = this.#active
    if (!active || this.#state.status !== 'ready') throw new BackendNotReadyError()
    if (typeof message !== 'string' || message.length === 0 || /[\r\n\0]/.test(message)) throw new TypeError('Chat message must be non-empty single-line text')
    active.bot.chat(message)
  }

  motor(): MinecraftMotorDriverApi {
    const active = this.#active
    if (!active || this.#state.status !== 'ready') throw new BackendNotReadyError()
    return new MineflayerMotorDriver(active.bot as unknown as Bot)
  }

  observationSource(): ProtocolObservationSource {
    const active = this.#active
    if (!active || !['ready', 'dead'].includes(this.#state.status)) throw new BackendNotReadyError()
    const epoch = active.epoch
    const assertCurrent = () => {
      if (!this.#active || this.#active.epoch !== epoch) throw new StaleBackendEpochError()
      return this.#active.bot
    }
    return {
      epoch: () => epoch,
      selfPose: () => {
        const bot = assertCurrent()
        if (!bot.entity) throw new BackendNotReadyError()
        return structuredClone({
          position: vectorDto(bot.entity.position),
          velocity: vectorDto(bot.entity.velocity ?? { x: 0, y: 0, z: 0 }),
          yaw: finiteNumber(bot.entity.yaw ?? 0, 'yaw'),
          pitch: finiteNumber(bot.entity.pitch ?? 0, 'pitch'),
        })
      },
      listTrackedEntities: () => Object.values(assertCurrent().entities).map(entity => entityDto(entity, epoch)),
      readBlock: position => structuredClone(readBlock(assertCurrent(), position)),
      subscribe: listener => {
        assertCurrent()
        const guarded = (event: BackendEventEnvelope<ProtocolEntityEvent | ProtocolBlockEvent | ProtocolSoundPayload>) => {
          if (event.connectionEpoch === epoch) listener(event)
        }
        const subscription = { epoch, listener: guarded }
        this.#observationSubscribers.add(subscription)
        let subscribed = true
        return () => { if (subscribed) { subscribed = false; this.#observationSubscribers.delete(subscription) } }
      },
    }
  }

  #beginAttempt(): void {
    if (this.#stopRequested) return
    const epoch = ++this.#epoch
    const attemptId = this.#deps.id()
    const attempt = ++this.#attempt
    this.#transition({ status: 'connecting', epoch, attemptId, attempt })

    let bot: BotLike
    try {
      bot = this.#deps.botFactory.create({
        host: this.config.server.host,
        port: this.config.server.port,
        username: this.config.identity.username,
        auth: this.config.identity.auth,
        version: this.config.server.version,
        ...(this.config.identity.profilesFolder ? { profilesFolder: this.config.identity.profilesFolder } : {}),
        logErrors: false,
      })
    } catch (error) {
      this.#fault({ code: 'protocol_error', message: this.#errorMessage(error), retryable: false })
      return
    }

    const active: ActiveConnection = {
      epoch,
      attemptId,
      bot,
      disposers: [],
      closeEvidence: {},
      closeSealed: false,
      respawnTransition: false,
      wasDead: false,
      lastSoundFingerprints: new Map(),
    }
    this.#active = active
    this.#installListeners(active)
    this.#emitLifecycle({ type: 'connection_requested', attempt })
    this.#setPhaseDeadline(active, 'connection_timeout', this.config.timeouts.connectMs)
  }

  #installListeners(active: ActiveConnection): void {
    const on = (name: string, listener: (...args: unknown[]) => void) => {
      active.bot.on(name, listener)
      active.disposers.push(() => active.bot.off(name, listener))
    }
    const current = () => this.#active === active && !active.closeSealed

    on('connect', () => {
      if (!current()) return
      active.phaseTimer?.cancel()
      this.#transition({ status: 'logging_in', epoch: active.epoch, attemptId: active.attemptId, attempt: this.#attempt })
      this.#emitLifecycle({ type: 'transport_connected' })
      this.#setPhaseDeadline(active, 'login_timeout', this.config.timeouts.loginMs)
    })

    on('login', () => {
      if (!current()) return
      if (active.bot.version !== this.config.server.version) {
        this.#finishClose(active, 'unsupported_version', `Expected ${this.config.server.version}, got ${String(active.bot.version)}`)
        return
      }
      active.phaseTimer?.cancel()
      active.previousDimension = active.bot.game?.dimension
      this.#transition({ status: 'spawning', epoch: active.epoch, attemptId: active.attemptId, attempt: this.#attempt })
      this.#emitLifecycle({
        type: 'logged_in',
        version: active.bot.version ?? 'unknown',
        dimension: active.bot.game?.dimension ?? 'unknown',
      })
      this.#setPhaseDeadline(active, 'spawn_timeout', this.config.timeouts.spawnMs)
    })

    on('spawn', () => {
      if (!current()) return
      this.#tryReady(active)
    })

    on('death', () => {
      if (!current() || this.#state.status === 'dead') return
      active.wasDead = true
      this.#snapshotRevision++
      this.#transition({ status: 'dead', epoch: active.epoch, attemptId: active.attemptId, diedAt: this.#now() })
      this.#emitLifecycle({ type: 'died' })
    })

    on('respawn', () => {
      if (!current()) return
      active.respawnTransition = true
      this.#emitLifecycle({ type: 'respawn_transition_started', fromDimension: active.previousDimension ?? 'unknown' })
    })

    on('game', () => {
      if (!current()) return
      const next = active.bot.game?.dimension
      const previous = active.previousDimension
      if (next && previous && next !== previous) {
        this.#snapshotRevision++
        this.#emitLifecycle({ type: 'dimension_changed', from: previous, to: next })
      }
      if (next) active.previousDimension = next
      this.#emit('world', { type: 'game_changed', dimension: next, gameMode: active.bot.game?.gameMode })
    })

    for (const eventName of ['health', 'food', 'experience', 'effects', 'heldItemChanged', 'windowUpdate']) {
      on(eventName, () => { if (current()) this.#markSnapshotChanged('self') })
    }
    for (const eventName of ['playerJoined', 'playerLeft']) {
      on(eventName, () => { if (current()) this.#markSnapshotChanged('players') })
    }

    on('entitySpawn', (...args) => this.#publishEntity(active, 'spawned', args[0]))
    on('entityMoved', (...args) => this.#publishEntity(active, 'moved', args[0]))
    on('entityUpdate', (...args) => this.#publishEntity(active, 'updated', args[0]))
    on('entityGone', (...args) => this.#publishEntity(active, 'removed', args[0]))
    on('entityHurt', (...args) => this.#publishEntityHurt(active, args[0], args[1]))

    on('blockUpdate', (...args) => {
      if (!current()) return
      const oldBlock = this.#isBlockLike(args[0]) ? blockDto(args[0]) : null
      const newBlock = this.#isBlockLike(args[1]) ? blockDto(args[1]) : null
      this.#emitObservation('block', { type: 'updated', oldBlock, newBlock } satisfies ProtocolBlockEvent)
    })
    on('chunkColumnLoad', (...args) => {
      if (!current() || !this.#isVectorLike(args[0])) return
      this.#emitObservation('block', { type: 'chunk_loaded', chunkX: Math.floor(args[0].x / 16), chunkZ: Math.floor(args[0].z / 16) })
    })
    on('chunkColumnUnload', (...args) => {
      if (!current() || !this.#isVectorLike(args[0])) return
      this.#emitObservation('block', { type: 'chunk_unloaded', chunkX: Math.floor(args[0].x / 16), chunkZ: Math.floor(args[0].z / 16) })
    })

    on('soundEffectHeard', (...args) => this.#publishSound(active, 'named_sound_effect', args))
    on('hardcodedSoundEffectHeard', (...args) => this.#publishSound(active, 'sound_effect', args))

    on('chat', (...args) => {
      if (!current()) return
      const [username, message, , , , verified] = args
      if (typeof username !== 'string' || typeof message !== 'string') return
      this.#emit('chat', {
        senderUsername: username,
        plainText: message,
        position: 'chat',
        ...(typeof verified === 'boolean' ? { verified } : {}),
      } satisfies ProtocolChatEvent)
    })

    on('kicked', (...args) => {
      if (!current()) return
      active.closeEvidence.kick = { text: this.#sanitizeText(args[0]), duringLogin: Boolean(args[1]) }
    })
    on('error', (...args) => {
      if (!current()) return
      active.closeEvidence.error = this.#sanitizeError(args[0])
      try { active.bot.end('backend_protocol_error') } catch { this.#finishClose(active, 'protocol_error', 'Mineflayer error') }
    })
    on('end', (...args) => {
      if (!current()) return
      this.#finishClose(active, undefined, this.#sanitizeText(args[0]))
    })
  }

  #tryReady(active: ActiveConnection): void {
    if (this.#active !== active || active.closeSealed) return
    if (!isReady(active.bot)) {
      active.readinessTimer?.cancel()
      active.readinessTimer = this.#deps.scheduler.timeout(10, () => this.#tryReady(active))
      return
    }

    active.phaseTimer?.cancel()
    active.readinessTimer?.cancel()
    this.#snapshotRevision++
    const wasDead = active.wasDead
    const wasTransition = active.respawnTransition
    active.wasDead = false
    active.respawnTransition = false
    this.#transition({ status: 'ready', epoch: active.epoch, attemptId: active.attemptId, readyAt: this.#now() })
    const snapshot = this.snapshot()
    if (wasDead) this.#emitLifecycle({ type: 'respawned', dimension: snapshot.world.dimension })
    this.#emitLifecycle({ type: 'ready', snapshotRevision: snapshot.snapshotRevision })
    if (wasTransition && !wasDead) this.#markSnapshotChanged('world_transition')

    if (!this.#startedReady) {
      this.#startedReady = true
      this.#startDeferred?.resolve({
        processSessionId: this.processSessionId,
        connectionEpoch: active.epoch,
        connectionAttemptId: active.attemptId,
        snapshot,
      })
    }
    active.stableTimer?.cancel()
    active.stableTimer = this.#deps.scheduler.timeout(this.config.reconnect.stableResetMs, () => {
      if (this.#active === active && this.#state.status === 'ready') this.#attempt = 0
    })
  }

  #publishEntity(active: ActiveConnection, type: 'spawned' | 'moved' | 'updated' | 'removed', raw: unknown): void {
    if (this.#active !== active || !this.#isEntityLike(raw)) return
    try {
      const dto = entityDto(raw, active.epoch)
      const event: ProtocolEntityEvent = type === 'removed'
        ? { type, entityKey: dto.entityKey, last: dto, reason: 'protocol_removed' }
        : type === 'updated'
          ? { type, entity: dto, changed: [] }
          : { type, entity: dto }
      this.#emitObservation('entity', event)
    } catch { /* invalid protocol DTO is ignored and should be counted by telemetry */ }
  }

  #publishEntityHurt(active: ActiveConnection, raw: unknown, source: unknown): void {
    if (this.#active !== active || !this.#isEntityLike(raw)) return
    const event: ProtocolEntityEvent = {
      type: 'hurt',
      entityKey: `${active.epoch}:${raw.id}`,
      ...(this.#isEntityLike(source) ? { possibleSourceEntityKey: `${active.epoch}:${source.id}` } : {}),
    }
    this.#emitObservation('entity', event)
  }

  #publishSound(active: ActiveConnection, protocolSource: ProtocolSoundPayload['protocolSource'], args: unknown[]): void {
    if (this.#active !== active) return
    let rawSoundName: unknown
    let rawSoundId: unknown
    let rawCategory: unknown
    let position: unknown
    let volume: unknown
    let pitch: unknown
    if (protocolSource === 'named_sound_effect') {
      ;[rawSoundName, position, volume, pitch] = args
    } else {
      ;[rawSoundId, rawCategory, position, volume, pitch] = args
    }
    if (!this.#isVectorLike(position) || typeof volume !== 'number' || !Number.isFinite(volume) || volume < 0 || typeof pitch !== 'number' || !Number.isFinite(pitch)) return
    const soundName = typeof rawSoundName === 'string' ? rawSoundName : undefined
    const soundId = typeof rawSoundId === 'number' && Number.isFinite(rawSoundId) ? rawSoundId : undefined
    const category = typeof rawCategory === 'string' ? rawCategory : undefined
    const sourcePosition = vectorDto(position)
    // Mineflayer emits a compatibility hardcoded callback for named sounds. The
    // compatibility event has a dummy ID, so dedupe on the shared packet facts.
    const key = `${Math.round(sourcePosition.x * 8)}:${Math.round(sourcePosition.y * 8)}:${Math.round(sourcePosition.z * 8)}:${volume}:${pitch}`
    const now = this.#deps.clock.monotonicMs()
    const last = active.lastSoundFingerprints.get(key)
    if (last !== undefined && now - last < 25) return
    active.lastSoundFingerprints.set(key, now)
    for (const [fingerprint, at] of active.lastSoundFingerprints) if (now - at > 1_000) active.lastSoundFingerprints.delete(fingerprint)
    this.#emitObservation('sound', {
      type: 'heard',
      soundKey: this.#deps.id(),
      ...(typeof soundName === 'string' ? { soundName } : {}),
      ...(typeof soundId === 'number' ? { soundId } : {}),
      ...(typeof category === 'string' ? { category } : {}),
      sourcePosition,
      volume,
      pitch,
      protocolSource,
    } satisfies ProtocolSoundPayload)
  }

  #setPhaseDeadline(active: ActiveConnection, code: BackendFailure['code'], ms: number): void {
    active.phaseTimer?.cancel()
    active.phaseTimer = this.#deps.scheduler.timeout(ms, () => {
      if (this.#active !== active || active.closeSealed) return
      active.closeEvidence.timeoutCode = code
      try { active.bot.end(code) } catch { /* handled below */ }
      this.#finishClose(active, code, code)
    })
  }

  #finishClose(active: ActiveConnection, forcedCode?: string, endReason?: string): void {
    if (this.#active !== active || active.closeSealed) return
    active.closeSealed = true
    const deliberate = this.#stopRequested
    const classification = this.#classifyClose(active.closeEvidence, forcedCode, deliberate)
    const close: BackendClose = {
      epoch: active.epoch,
      at: this.#now(),
      code: classification.code,
      retryable: classification.retryable,
      deliberate,
      ...(active.closeEvidence.kick ? { kick: active.closeEvidence.kick } : {}),
      ...(active.closeEvidence.error ? { error: active.closeEvidence.error } : {}),
      ...(endReason ? { endReason } : {}),
    }
    this.#emitLifecycle({ type: 'connection_closed', close })
    this.#disposeActive()

    if (deliberate) {
      this.#finishStopped()
      return
    }
    if (!classification.retryable || !this.config.reconnect.enabled) {
      this.#fault({
        code: classification.retryable ? 'reconnect_disabled' : classification.failureCode,
        message: classification.message,
        retryable: classification.retryable,
      })
      return
    }
    this.#scheduleReconnect(close)
  }

  #classifyClose(evidence: CloseEvidence, forcedCode: string | undefined, deliberate: boolean): {
    code: string
    retryable: boolean
    failureCode: BackendFailure['code']
    message: string
  } {
    if (deliberate) return { code: 'deliberate_stop', retryable: false, failureCode: 'protocol_error', message: this.#stopReason }
    if (evidence.timeoutCode) return { code: evidence.timeoutCode, retryable: true, failureCode: evidence.timeoutCode, message: evidence.timeoutCode }
    if (forcedCode === 'unsupported_version') return { code: forcedCode, retryable: false, failureCode: 'unsupported_version', message: 'Unsupported Minecraft version' }
    const kick = evidence.kick?.text.toLowerCase() ?? ''
    if (/server_shutdown|server closed|server restarting/.test(kick)) {
      return { code: 'server_shutdown', retryable: true, failureCode: 'protocol_error', message: evidence.kick?.text ?? 'Server shutdown' }
    }
    if (/banned|whitelist|invalid session|authentication|not authenticated/.test(kick)) {
      return { code: 'permission_denied', retryable: false, failureCode: kick.includes('auth') || kick.includes('session') ? 'authentication_failed' : 'permission_denied', message: evidence.kick?.text ?? 'Permission denied' }
    }
    if (evidence.kick) return { code: 'unclassified_kick', retryable: false, failureCode: 'permission_denied', message: evidence.kick.text }
    const errorCode = evidence.error?.code?.toUpperCase()
    if (['ECONNREFUSED', 'ECONNRESET', 'ETIMEDOUT', 'EPIPE'].includes(errorCode ?? '')) {
      return { code: errorCode!.toLowerCase(), retryable: true, failureCode: 'protocol_error', message: evidence.error?.message ?? errorCode! }
    }
    return { code: forcedCode ?? 'connection_ended', retryable: true, failureCode: 'protocol_error', message: evidence.error?.message ?? 'Connection ended' }
  }

  #scheduleReconnect(close: BackendClose): void {
    const exponent = Math.max(0, this.#attempt - 1)
    const base = Math.min(this.config.reconnect.maxDelayMs, this.config.reconnect.initialDelayMs * this.config.reconnect.multiplier ** exponent)
    const jitter = (this.#deps.random.next() * 2 - 1) * this.config.reconnect.jitterRatio
    const delay = Math.max(0, Math.round(base * (1 + jitter)))
    const retryAt = new Date(this.#deps.clock.now().getTime() + delay).toISOString()
    this.#transition({ status: 'reconnecting', attempt: this.#attempt + 1, retryAt, lastClose: close })
    this.#emitLifecycle({ type: 'reconnect_scheduled', attempt: this.#attempt + 1, retryAt, closeCode: close.code })
    this.#reconnectTimer = this.#deps.scheduler.timeout(delay, () => {
      this.#reconnectTimer = undefined
      if (!this.#stopRequested) this.#beginAttempt()
    })
  }

  #disposeActive(): void {
    const active = this.#active
    if (!active) return
    active.phaseTimer?.cancel()
    active.readinessTimer?.cancel()
    active.stableTimer?.cancel()
    for (const dispose of active.disposers.splice(0).reverse()) {
      try { dispose() } catch { /* cleanup is best effort and idempotent */ }
    }
    for (const subscription of this.#observationSubscribers) {
      if (subscription.epoch === active.epoch) this.#observationSubscribers.delete(subscription)
    }
    this.#active = undefined
  }

  #finishStopped(): void {
    this.#stopTimer?.cancel()
    this.#stopTimer = undefined
    this.#disposeActive()
    this.#detachAbort()
    this.#transition({ status: 'stopped', reason: this.#stopReason })
    this.#emitLifecycle({ type: 'stopped', reason: this.#stopReason })
    if (!this.#startedReady) this.#startDeferred?.reject(new DOMException('Backend stopped before ready', 'AbortError'))
    this.#stopDeferred?.resolve()
    this.#stopDeferred = undefined
  }

  #fault(failure: BackendFailure): void {
    this.#disposeActive()
    this.#detachAbort()
    this.#transition({ status: 'faulted', failure })
    this.#emitLifecycle({ type: 'faulted', failure })
    if (!this.#startedReady) this.#startDeferred?.reject(new Error(`${failure.code}: ${failure.message}`))
  }

  #detachAbort(): void {
    if (this.#abortSignal && this.#abortHandler) this.#abortSignal.removeEventListener('abort', this.#abortHandler)
    this.#abortSignal = undefined
    this.#abortHandler = undefined
  }

  #transition(next: BackendState): void {
    this.#state = next
    this.#lifecycleRevision++
  }

  #markSnapshotChanged(group: string): void {
    this.#snapshotRevision++
    this.#emit('snapshot_changed', { group, snapshotRevision: this.#snapshotRevision })
  }

  #emitLifecycle(payload: BackendLifecyclePayload): void { this.#emit('lifecycle', payload) }

  #emitObservation(kind: 'entity' | 'block' | 'sound', payload: ProtocolEntityEvent | ProtocolBlockEvent | ProtocolSoundPayload): void {
    const event = this.#makeEvent(kind, payload)
    for (const subscription of [...this.#observationSubscribers]) {
      try { subscription.listener(event) } catch { /* subscriber isolation */ }
    }
    this.#deliver(event)
  }

  #emit(kind: BackendEventKind, payload: unknown): void { this.#deliver(this.#makeEvent(kind, payload)) }

  #makeEvent<T>(kind: BackendEventKind, payload: T): BackendEventEnvelope<T> {
    const active = this.#active
    return {
      protocol: 'mineintent.minecraft.backend-event.v1',
      id: this.#deps.id(),
      kind,
      occurredAt: this.#now(),
      processSessionId: this.processSessionId,
      connectionEpoch: active?.epoch ?? this.#epoch,
      connectionAttemptId: active?.attemptId ?? 'none',
      worldId: this.config.worldId,
      ...(active?.bot.game?.dimension ? { dimension: active.bot.game.dimension } : {}),
      payload,
    }
  }

  #deliver(event: BackendEventEnvelope): void {
    for (const listener of [...this.#subscribers]) {
      try { listener(structuredClone(event)) } catch { /* subscriber isolation */ }
    }
  }

  #snapshotContext(active: ActiveConnection) {
    return {
      worldId: this.config.worldId,
      processSessionId: this.processSessionId,
      connectionEpoch: active.epoch,
      connectionAttemptId: active.attemptId,
      lifecycleRevision: this.#lifecycleRevision,
      snapshotRevision: this.#snapshotRevision,
      capturedAt: this.#now(),
    }
  }

  #now(): string { return this.#deps.clock.now().toISOString() }
  #sanitizeText(value: unknown): string {
    const text = this.#flattenText(value) || 'unknown'
    return text.replace(/[\r\n\t]+/g, ' ').slice(0, 500)
  }
  #flattenText(value: unknown): string {
    if (typeof value === 'string') return value
    if (typeof value === 'number' || typeof value === 'boolean') return String(value)
    if (Array.isArray(value)) return value.map(item => this.#flattenText(item)).filter(Boolean).join(' ')
    if (!value || typeof value !== 'object') return ''
    const record = value as Record<string, unknown>
    const parts = [record.text, record.translate, record.extra, record.with]
      .map(item => this.#flattenText(item)).filter(Boolean)
    if (parts.length > 0) return parts.join(' ')
    const customToString = record.toString
    if (typeof customToString === 'function' && customToString !== Object.prototype.toString) {
      const rendered = String(value)
      if (rendered !== '[object Object]') return rendered
    }
    try { return JSON.stringify(value) } catch { return '' }
  }
  #sanitizeError(value: unknown): { name: string; message: string; code?: string } {
    if (value instanceof Error) {
      const code = 'code' in value && typeof value.code === 'string' ? value.code : undefined
      return { name: value.name, message: value.message.slice(0, 500), ...(code ? { code } : {}) }
    }
    return { name: 'Error', message: this.#sanitizeText(value) }
  }
  #errorMessage(value: unknown): string { return value instanceof Error ? value.message : this.#sanitizeText(value) }
  #isVectorLike(value: unknown): value is { x: number; y: number; z: number } {
    return Boolean(value && typeof value === 'object' && 'x' in value && 'y' in value && 'z' in value)
  }
  #isEntityLike(value: unknown): value is EntityLike {
    return Boolean(value && typeof value === 'object' && 'id' in value && 'position' in value && this.#isVectorLike(value.position))
  }
  #isBlockLike(value: unknown): value is Parameters<typeof blockDto>[0] {
    return Boolean(value && typeof value === 'object' && 'position' in value && this.#isVectorLike(value.position))
  }
}

export function endpointHash(host: string, port: number): string {
  return createHash('sha256').update(`${host.toLowerCase()}:${port}`).digest('hex').slice(0, 16)
}
