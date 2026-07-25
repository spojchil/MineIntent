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

test('entities and blocks share [right, up, forward] with an explicit legend', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: -Math.PI / 2, pitch: 0 }
  const provider = new ViewportInformationProvider(new FakePort(
    pose,
    new Map([['3,65,0', stone('stone')]]),
    [{ type: 'sheep', position: { x: 5, y: 64, z: 1 }, height: 1.3 }],
  ))
  const result = await provider.read(context(), { fields: ['frame', 'visibleEntities', 'visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)
  assert.deepEqual(result.values.frame?.axes, ['right', 'up', 'forward'])
  assert.deepEqual(result.values.visibleEntities?.[0], ['sheep', 1, 0, 5])
  assert.deepEqual(result.values.visibleBlocks?.blocks[0], ['stone', 0, 1, 3])
  const serialized = JSON.stringify(result.values)
  assert.doesNotMatch(serialized, /"(?:ref|x|y|z)":/u)
})

test('player tuples use the username as their compact entity label', async () => {
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 },
    new Map(),
    [{ type: 'player', username: 'Alex', position: { x: 0, y: 64, z: -3 }, width: 0.6, height: 1.8 }],
  ))

  const result = await provider.read(context(), { fields: ['visibleEntities'], page: { limit: 1 } }, new AbortController().signal)

  assert.deepEqual(result.values.visibleEntities, [['Alex', 0, 0, 3]])
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
