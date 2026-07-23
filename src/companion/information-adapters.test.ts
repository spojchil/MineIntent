import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { test } from 'node:test'
import type {
  BackendEventEnvelope, BackendReady, BackendState, MinecraftBackendApi, MinecraftMotorDriverApi,
  MinecraftSnapshotV1, ProtocolObservationSource, Unsubscribe,
} from '../minecraft/contracts.js'
import {
  BackendInformationScopeSource, BackendInventoryPort, BackendPerceptionPort, BackendSelfVitalsPort, SoundHistory,
} from './information-adapters.js'

class FakeBackend extends EventEmitter implements MinecraftBackendApi {
  state_: BackendState = { status: 'idle' }
  entities: MinecraftSnapshotV1['trackedPlayers'] = []
  position = { x: 0, y: 64, z: 0 }

  async start(): Promise<BackendReady> { throw new Error('not used') }
  async stop(): Promise<void> {}
  state(): Readonly<BackendState> { return this.state_ }
  snapshot(): Readonly<MinecraftSnapshotV1> {
    return {
      protocol: 'mineintent.minecraft.snapshot.v1', snapshotRevision: 1, lifecycleRevision: 1,
      capturedAt: new Date().toISOString(), processSessionId: 'session', connectionEpoch: 1, connectionAttemptId: 'attempt',
      world: { worldId: 'test-world', dimension: 'overworld', minecraftVersion: '1.21.1', protocolVersion: 767, gameMode: 'survival', minY: -64, height: 384 },
      self: { entityKey: 'self', username: 'Bot', position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0, onGround: true, alive: true, health: 15, food: 12, foodSaturation: 3, effects: [] },
      inventory: { selectedHotbarSlot: 2, slots: [{ slot: 9, itemName: 'oak_log', count: 2 }] },
      trackedPlayers: this.entities,
    }
  }
  subscribe(listener: (event: BackendEventEnvelope) => void): Unsubscribe { this.on('backend', listener); return () => this.off('backend', listener) }
  observationSource(): ProtocolObservationSource {
    return {
      epoch: () => 1,
      selfPose: () => ({ position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0 }),
      listTrackedEntities: () => [
        { entityKey: 'self', protocolEntityId: 0, type: 'player', username: 'Bot', position: this.position, velocity: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0, width: 0.6, height: 1.8, onGround: true, equipment: [], valid: true },
        { entityKey: '1:alex', protocolEntityId: 1, type: 'player', username: 'Alex', position: { x: 0, y: 64, z: 3 }, velocity: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0, width: 0.6, height: 1.8, onGround: true, equipment: [], valid: true },
      ],
      readBlock: (position) => position.y === 60 ? { status: 'loaded', block: { position, name: 'stone', stateId: 1, properties: {}, collisionShapes: [], transparentHint: false, boundingBox: 'block' } } : { status: 'unloaded' },
      subscribe: () => () => {},
    }
  }
  motor(): MinecraftMotorDriverApi { throw new Error('not used') }
  sendChat(): void {}
  emitSound(sourcePosition: { x: number; y: number; z: number }): void {
    this.emit('backend', {
      protocol: 'mineintent.minecraft.backend-event.v1', id: 'event-1', kind: 'sound', occurredAt: new Date().toISOString(),
      processSessionId: 'session', connectionEpoch: 1, connectionAttemptId: 'attempt', worldId: 'test-world', dimension: 'overworld',
      payload: { type: 'heard', soundKey: 'k', soundName: 'entity.cow.ambient', category: 'neutral', sourcePosition, volume: 1, pitch: 1, protocolSource: 'named_sound_effect' },
    } satisfies BackendEventEnvelope)
  }
}

test('BackendSelfVitalsPort and BackendInventoryPort read straight through the snapshot', () => {
  const backend = new FakeBackend()
  assert.equal(new BackendSelfVitalsPort(backend).current().health, 15)
  assert.deepEqual(new BackendInventoryPort(backend).current(), { selectedHotbarSlot: 2, slots: [{ slot: 9, itemName: 'oak_log', count: 2 }] })
})

test('BackendPerceptionPort excludes self from nearby entities and maps block loading', () => {
  const backend = new FakeBackend()
  const port = new BackendPerceptionPort(backend)
  const nearby = port.nearbyEntities()
  assert.equal(nearby.length, 1)
  assert.equal(nearby[0]!.username, 'Alex')
  assert.deepEqual(port.blockAt({ x: 0, y: 60, z: 0 }), { name: 'stone', visible: true, occludes: true })
  assert.equal(port.blockAt({ x: 0, y: 70, z: 0 }), 'unloaded')
})

test('SoundHistory records distance and direction relative to self, bounded and revisioned', () => {
  const backend = new FakeBackend()
  const history = new SoundHistory(backend)
  assert.equal(history.revision(), 0)
  backend.emitSound({ x: 0, y: 64, z: -5 })
  assert.equal(history.revision(), 1)
  const [entry] = history.recent(10)
  assert.equal(entry!.soundName, 'entity.cow.ambient')
  assert.equal(entry!.distance, 5)
  assert.equal(entry!.direction, 'ahead')
  history.dispose()
  backend.emitSound({ x: 0, y: 64, z: -1 })
  assert.equal(history.revision(), 1, 'no longer subscribed after dispose')
})

test('BackendInformationScopeSource maps backend state to connection state and includes world info once ready', () => {
  const backend = new FakeBackend()
  const source = new BackendInformationScopeSource(backend, 'session-1')
  assert.equal(source.capture().connectionState, 'disconnected')
  backend.state_ = { status: 'ready', epoch: 4, attemptId: 'a', readyAt: new Date().toISOString() }
  const captured = source.capture()
  assert.equal(captured.connectionState, 'play')
  assert.equal(captured.connectionEpoch, 4)
  assert.equal(captured.worldId, 'test-world')
})
