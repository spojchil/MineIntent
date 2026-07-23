import { distanceBetween, relativeBearing, type InformationScopeSnapshot, type InformationScopeSource, type SoundHistoryPort, type SoundObservation } from '../information/index.js'
import type { InventoryPort } from '../information/source-ports/inventory.js'
import type { PerceptionEntityCandidate, PerceptionPort, PerceptionPose } from '../information/source-ports/perception.js'
import type { SelfVitalsPort } from '../information/source-ports/self-vitals.js'
import type { BackendEventEnvelope, MinecraftBackendApi, ProtocolSoundPayload, Unsubscribe } from '../minecraft/contracts.js'

export class BackendSelfVitalsPort implements SelfVitalsPort {
  constructor(private readonly backend: MinecraftBackendApi) {}
  current(): ReturnType<SelfVitalsPort['current']> { return this.backend.snapshot().self }
}

export class BackendInventoryPort implements InventoryPort {
  constructor(private readonly backend: MinecraftBackendApi) {}
  current(): ReturnType<InventoryPort['current']> { return this.backend.snapshot().inventory }
}

export class BackendPerceptionPort implements PerceptionPort {
  constructor(private readonly backend: MinecraftBackendApi) {}

  selfPose(): PerceptionPose {
    return this.backend.observationSource().selfPose()
  }

  revision(): number { return this.backend.snapshot().snapshotRevision }

  blockAt(position: PerceptionPose['position']): ReturnType<PerceptionPort['blockAt']> {
    const result = this.backend.observationSource().readBlock(position)
    if (result.status !== 'loaded') return 'unloaded'
    const visible = !['air', 'cave_air', 'void_air'].includes(result.block.name)
    return { name: result.block.name, visible, occludes: visible && !result.block.transparentHint }
  }

  nearbyEntities(): readonly PerceptionEntityCandidate[] {
    let selfEntityKey: string | undefined
    try { selfEntityKey = this.backend.snapshot().self.entityKey } catch { selfEntityKey = undefined }
    return this.backend.observationSource().listTrackedEntities()
      .filter((entity) => entity.entityKey !== selfEntityKey)
      .map((entity) => ({
        type: entity.type,
        ...(entity.name ? { name: entity.name } : {}),
        ...(entity.username ? { username: entity.username } : {}),
        position: entity.position,
        height: entity.height,
      }))
  }
}

const SOUND_HISTORY_CAPACITY = 20

export class SoundHistory implements SoundHistoryPort {
  readonly #entries: SoundObservation[] = []
  readonly #unsubscribe: Unsubscribe
  #revision = 0

  constructor(private readonly backend: MinecraftBackendApi) {
    this.#unsubscribe = backend.subscribe((event) => { if (event.kind === 'sound') this.#record(event) })
  }

  recent(limit: number): readonly SoundObservation[] {
    return this.#entries.slice(-limit).reverse()
  }

  revision(): number { return this.#revision }

  dispose(): void { this.#unsubscribe() }

  #record(event: BackendEventEnvelope): void {
    const payload = event.payload as ProtocolSoundPayload
    let self
    try { self = this.backend.snapshot().self } catch { return }
    const observation: SoundObservation = {
      ...(payload.soundName ? { soundName: payload.soundName } : {}),
      ...(payload.category ? { category: payload.category } : {}),
      distance: distanceBetween(self.position, payload.sourcePosition),
      direction: relativeBearing(self.yaw, self.position, payload.sourcePosition),
      volume: payload.volume,
      pitch: payload.pitch,
      observedAt: event.occurredAt,
    }
    this.#entries.push(observation)
    if (this.#entries.length > SOUND_HISTORY_CAPACITY) this.#entries.shift()
    this.#revision++
  }
}

const CONNECTED_STATUSES = new Set(['connecting', 'logging_in', 'spawning', 'ready', 'dead'])

export class BackendInformationScopeSource implements InformationScopeSource {
  constructor(private readonly backend: MinecraftBackendApi, private readonly processSessionId: string) {}

  capture(): Readonly<InformationScopeSnapshot> {
    const state = this.backend.state()
    const connectionState = state.status === 'ready' || state.status === 'dead' ? 'play'
      : state.status === 'connecting' || state.status === 'logging_in' || state.status === 'spawning' ? 'connecting'
      : 'disconnected'
    const connectionEpoch = CONNECTED_STATUSES.has(state.status) ? (state as { epoch: number }).epoch : 0
    let worldId: string | undefined
    let dimension: string | undefined
    try {
      const snapshot = this.backend.snapshot()
      worldId = snapshot.world.worldId
      dimension = snapshot.world.dimension
    } catch { /* backend not ready */ }
    return {
      processSessionId: this.processSessionId,
      connectionState,
      connectionEpoch,
      ...(worldId ? { worldId } : {}),
      ...(dimension ? { dimension } : {}),
      uiRevision: 0,
      capturedAt: new Date().toISOString(),
    }
  }
}
