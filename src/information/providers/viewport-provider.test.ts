import assert from 'node:assert/strict'
import test from 'node:test'
import type { PerceptionBlock, PerceptionEntityCandidate, PerceptionPort, PerceptionPose } from '../source-ports/perception.js'
import { assertInformationProviderContract } from '../testing/provider-contract.js'
import { ViewportInformationProvider } from './viewport-provider.js'

class FakePort implements PerceptionPort {
  constructor(public pose: PerceptionPose, readonly blocks = new Map<string, PerceptionBlock>(), readonly entities: PerceptionEntityCandidate[] = []) {}
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) { return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? { name: 'air', visible: false, occludes: false } }
  nearbyEntities() { return this.entities }
}
const stone = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
const context = () => ({
  now: new Date().toISOString(),
  scope: { processSessionId: 's', connectionState: 'play' as const, connectionEpoch: 1, uiRevision: 0, capturedAt: new Date().toISOString() },
  caller: { audience: 'companion' as const, purpose: 'companion_context' as const },
  refs: { issue: () => { throw new Error('model viewport does not issue refs') } },
})

test('viewport provider satisfies its five-field contract', async () => {
  const provider = new ViewportInformationProvider(new FakePort({ position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }))
  await assertInformationProviderContract(provider, { context: context(), request: { fields: ['frame', 'standingOnBlock', 'lookedAtBlock', 'visibleEntities', 'visibleBlocks'], page: { limit: 1 } } })
})

test('positions are world-absolute so the same block keeps one key from any stance', async () => {
  // Absolute coordinates are what make an incremental read possible at all: a body-relative tuple
  // renames every block the moment the companion moves, so nothing can be diffed against it.
  const blocks = new Map([['3,65,0', stone('stone')]])
  const entities = [{ type: 'sheep', position: { x: 5, y: 64, z: 1 }, height: 1.3 }]
  const first = await new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: -Math.PI / 2, pitch: 0 }, blocks, entities,
  )).read(context(), { fields: ['frame', 'visibleEntities', 'visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)

  assert.equal(first.values.frame?.coordinates, 'minecraft_world_absolute')
  assert.deepEqual(first.values.frame?.self.position, [0, 64, 0])
  assert.deepEqual(first.values.visibleEntities?.[0], ['sheep', 5, 64, 1])
  assert.deepEqual(first.values.visibleBlocks?.blocks[0], ['stone', 3, 65, 0])
  // The legend rides with the data, so a schema change cannot outdate a prompt written elsewhere.
  assert.match(first.values.frame?.legend.visibleBlocks ?? '', /x, y, z/u)

  // Same world, different stance: the block keeps its key even though the bearing to it changed.
  const second = await new ViewportInformationProvider(new FakePort(
    { position: { x: 1, y: 64, z: 1 }, yaw: -Math.PI / 2, pitch: 0 }, blocks, entities,
  )).read(context(), { fields: ['visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)
  assert.deepEqual(second.values.visibleBlocks?.blocks[0], ['stone', 3, 65, 0])
})

test('player tuples use the username as their compact entity label', async () => {
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 },
    new Map(),
    [{ type: 'player', username: 'Alex', position: { x: 0, y: 64, z: -3 }, width: 0.6, height: 1.8 }],
  ))

  const result = await provider.read(context(), { fields: ['visibleEntities'], page: { limit: 1 } }, new AbortController().signal)

  assert.deepEqual(result.values.visibleEntities, [['Alex', 0, 64, -3]])
})

test('an entities-only read still honors the deadline signal', async () => {
  // The declared timeoutMs cannot be enforced by the runtime's Promise.race alone: a synchronous
  // scan blocks the event loop, so the deadline timer has no chance to fire until it returns.
  // Every field that scans the world has to observe the signal itself, not just visibleBlocks.
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 },
    new Map(),
    [{ type: 'sheep', position: { x: 0, y: 64, z: -3 }, width: 0.9, height: 1.3 }],
  ))
  const controller = new AbortController()
  controller.abort()

  await assert.rejects(provider.read(context(), { fields: ['visibleEntities'], page: { limit: 1 } }, controller.signal))
})
