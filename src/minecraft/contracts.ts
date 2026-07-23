export interface Vec3Value {
  x: number
  y: number
  z: number
}

export interface BlockPosition extends Vec3Value {}

export type AabbValue = [number, number, number, number, number, number]

export interface MinecraftBackendConfig {
  worldId: string
  server: { host: string; port: number; version: '1.21.1' }
  identity: { username: string; auth: 'offline' | 'microsoft'; profilesFolder?: string }
  timeouts: { connectMs: number; loginMs: number; spawnMs: number; stopMs: number }
  reconnect: {
    enabled: boolean
    initialDelayMs: number
    multiplier: number
    maxDelayMs: number
    jitterRatio: number
    stableResetMs: number
  }
}

export type BackendState =
  | { status: 'idle' }
  | { status: 'connecting'; epoch: number; attemptId: string; attempt: number }
  | { status: 'logging_in'; epoch: number; attemptId: string; attempt: number }
  | { status: 'spawning'; epoch: number; attemptId: string; attempt: number }
  | { status: 'ready'; epoch: number; attemptId: string; readyAt: string }
  | { status: 'dead'; epoch: number; attemptId: string; diedAt: string }
  | { status: 'reconnecting'; attempt: number; retryAt: string; lastClose: BackendClose }
  | { status: 'stopping'; epoch?: number; reason: string }
  | { status: 'stopped'; reason?: string }
  | { status: 'faulted'; failure: BackendFailure }

export interface BackendClose {
  epoch: number
  at: string
  code: string
  retryable: boolean
  deliberate: boolean
  kick?: { text: string; duringLogin: boolean }
  error?: { name: string; message: string; code?: string }
  endReason?: string
}

export interface BackendFailure {
  code:
    | 'invalid_config'
    | 'unsupported_version'
    | 'authentication_failed'
    | 'permission_denied'
    | 'connection_timeout'
    | 'login_timeout'
    | 'spawn_timeout'
    | 'protocol_error'
    | 'reconnect_disabled'
  message: string
  retryable: boolean
}

export interface WorldSnapshot {
  worldId: string
  dimension: string
  minecraftVersion: '1.21.1'
  protocolVersion: number
  gameMode: 'survival' | 'creative' | 'adventure' | 'spectator'
  difficulty?: 'peaceful' | 'easy' | 'normal' | 'hard'
  minY: number
  height: number
  serverViewDistance?: number
  timeOfDay?: number
  isRaining?: boolean
}

export interface SelfSnapshot {
  entityKey: string
  username: string
  position: Vec3Value
  velocity: Vec3Value
  yaw: number
  pitch: number
  onGround: boolean
  alive: boolean
  health: number
  food: number
  foodSaturation: number
  oxygen?: number
  experience?: { level: number; progress: number; total: number }
  effects: Array<{ name: string; amplifier: number; durationTicks?: number }>
}

export interface InventorySnapshot {
  selectedHotbarSlot: number
  slots: Array<{
    slot: number
    itemName: string
    count: number
    metadata?: number
    durabilityUsed?: number
  }>
}

export interface TrackedPlayerSnapshot {
  playerKey: string
  uuid?: string
  username: string
  listed: boolean
  entityTracked: boolean
  position?: Vec3Value
  yaw?: number
  pitch?: number
  heldItemName?: string
}

export interface MinecraftSnapshotV1 {
  protocol: 'mineintent.minecraft.snapshot.v1'
  snapshotRevision: number
  lifecycleRevision: number
  capturedAt: string
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  world: WorldSnapshot
  self: SelfSnapshot
  inventory: InventorySnapshot
  trackedPlayers: TrackedPlayerSnapshot[]
}

export interface ProtocolEntitySnapshot {
  entityKey: string
  protocolEntityId: number
  type: string
  name?: string
  username?: string
  uuid?: string
  position: Vec3Value
  velocity: Vec3Value
  yaw: number
  pitch: number
  headYaw?: number
  width: number
  height: number
  onGround: boolean
  pose?: string
  heldItemName?: string
  equipment: Array<{ slot: number; itemName: string; count: number }>
  valid: boolean
}

export type ProtocolEntityEvent =
  | { type: 'spawned'; entity: ProtocolEntitySnapshot }
  | { type: 'moved'; entity: ProtocolEntitySnapshot }
  | { type: 'updated'; entity: ProtocolEntitySnapshot; changed: string[] }
  | { type: 'animation'; entityKey: string; animation: string }
  | { type: 'hurt'; entityKey: string; possibleSourceEntityKey?: string }
  | { type: 'removed'; entityKey: string; last: ProtocolEntitySnapshot; reason: 'protocol_removed' }

export interface ProtocolBlockSnapshot {
  position: BlockPosition
  name: string
  stateId: number
  properties: Record<string, string | number | boolean>
  collisionShapes: AabbValue[]
  transparentHint: boolean
  boundingBox: 'block' | 'empty'
}

export type ProtocolBlockEvent =
  | { type: 'updated'; oldBlock: ProtocolBlockSnapshot | null; newBlock: ProtocolBlockSnapshot | null }
  | { type: 'chunk_loaded'; chunkX: number; chunkZ: number }
  | { type: 'chunk_unloaded'; chunkX: number; chunkZ: number }

export type BlockReadResult =
  | { status: 'loaded'; block: ProtocolBlockSnapshot }
  | { status: 'unloaded' }
  | { status: 'out_of_world' }

export interface ProtocolSoundPayload {
  type: 'heard'
  soundKey: string
  soundName?: string
  soundId?: number
  category?: string
  sourcePosition: Vec3Value
  volume: number
  pitch: number
  protocolSource: 'named_sound_effect' | 'sound_effect'
}

export interface ProtocolChatEvent {
  senderUsername?: string
  plainText: string
  position?: 'chat' | 'system' | 'game_info'
  verified?: boolean
}

export type BackendLifecyclePayload =
  | { type: 'connection_requested'; attempt: number }
  | { type: 'transport_connected' }
  | { type: 'logged_in'; version: string; dimension: string }
  | { type: 'ready'; snapshotRevision: number }
  | { type: 'died' }
  | { type: 'respawn_transition_started'; fromDimension: string }
  | { type: 'respawned'; dimension: string }
  | { type: 'dimension_changed'; from: string; to: string }
  | { type: 'reconnect_scheduled'; attempt: number; retryAt: string; closeCode: string }
  | { type: 'connection_closed'; close: BackendClose }
  | { type: 'faulted'; failure: BackendFailure }
  | { type: 'stopped'; reason: string }

export type BackendEventKind =
  | 'lifecycle'
  | 'self'
  | 'world'
  | 'entity'
  | 'block'
  | 'sound'
  | 'chat'
  | 'player_list'
  | 'snapshot_changed'

export interface BackendEventEnvelope<T = unknown> {
  protocol: 'mineintent.minecraft.backend-event.v1'
  id: string
  kind: BackendEventKind
  occurredAt: string
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  worldId: string
  dimension?: string
  payload: T
}

export type Unsubscribe = () => void

export interface ProtocolObservationSource {
  epoch(): number
  selfPose(): Readonly<Pick<SelfSnapshot, 'position' | 'velocity' | 'yaw' | 'pitch'>>
  listTrackedEntities(): readonly Readonly<ProtocolEntitySnapshot>[]
  readBlock(position: BlockPosition): Readonly<BlockReadResult>
  subscribe(
    listener: (event: BackendEventEnvelope<ProtocolEntityEvent | ProtocolBlockEvent | ProtocolSoundPayload>) => void,
  ): Unsubscribe
}

export interface BackendReady {
  processSessionId: string
  connectionEpoch: number
  connectionAttemptId: string
  snapshot: Readonly<MinecraftSnapshotV1>
}

export type MotorMoveDirection = 'forward' | 'back' | 'left' | 'right'

/**
 * The deliberately small physical boundary used by the D40 experiment. These methods are
 * player inputs, not navigation or target-specific skills. A future observed-space navigation
 * tool may be built above this boundary, but it must remain a distinct model-visible tool.
 */
export interface MinecraftMotorDriverApi {
  lookRelative(yawDegrees: number, pitchDegrees: number, signal: AbortSignal): Promise<void>
  move(direction: MotorMoveDirection, durationMs: number, sprint: boolean | undefined, signal: AbortSignal): Promise<void>
  releaseAll(): void
}

export interface MinecraftBackendApi {
  start(signal: AbortSignal): Promise<BackendReady>
  stop(reason: string): Promise<void>
  state(): Readonly<BackendState>
  snapshot(): Readonly<MinecraftSnapshotV1>
  subscribe(listener: (event: BackendEventEnvelope) => void): Unsubscribe
  observationSource(): ProtocolObservationSource
  motor(): MinecraftMotorDriverApi
  sendChat(message: string): void
}
