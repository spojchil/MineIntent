import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { PerceptionBlock, PerceptionEntityCandidate, PerceptionPort, PerceptionPose } from '../source-ports/perception.js'
import { assertInformationProviderContract } from '../testing/provider-contract.js'
import { ViewportInformationProvider } from './viewport-provider.js'

const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const opaque = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })

class FakePerceptionPort implements PerceptionPort {
  constructor(
    public pose: PerceptionPose,
    private blocks: Map<string, PerceptionBlock | 'unloaded'>,
    private entities: PerceptionEntityCandidate[] = [],
    private fallback?: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded',
  ) {}
  selfPose(): PerceptionPose { return this.pose }
  blockAt(position: PerceptionPose['position']): PerceptionBlock | 'unloaded' {
    return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? this.fallback?.(position) ?? AIR
  }
  nearbyEntities(): readonly PerceptionEntityCandidate[] { return this.entities }
}

function context() {
  return {
    now: new Date().toISOString(),
    scope: { processSessionId: 's', connectionState: 'play' as const, connectionEpoch: 1, uiRevision: 0, capturedAt: new Date().toISOString() },
    caller: { audience: 'companion' as const, purpose: 'companion_context' as const },
    refs: { issue: () => { throw new Error('not used') } },
  }
}

test('viewport provider satisfies the provider contract', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, new Map()))
  await assertInformationProviderContract(provider, { context: context(), request: { fields: ['standingOnBlock', 'lookedAtBlock', 'nearbyTrackedEntities', 'visibleBlocks'], page: { limit: 1 } } })
})

test('viewport provider surfaces visibleBlocks through the read path', async () => {
  const pose: PerceptionPose = { position: { x: 100.25, y: 64, z: 100.75 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>([['102,65,97', { name: 'stone', visible: true, occludes: true }]])
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, blocks))
  const result = await provider.read(context(), { fields: ['visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.visibleBlocks?.truncated, false)
  assert.deepEqual(result.values.visibleBlocks?.blocks[0], [2, 1, -3, 'stone'])
})

test('viewport provider selects the exposed-face predicate for level flat ground', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const provider = new ViewportInformationProvider(new FakePerceptionPort(
    pose,
    new Map(),
    [],
    position => position.y <= 63 ? opaque('grass_block') : AIR,
  ))
  const result = await provider.read(
    context(),
    { fields: ['visibleBlocks'], page: { limit: 1 } },
    new AbortController().signal,
  )

  assert.equal(result.values.visibleBlocks?.truncated, true)
  assert.equal(result.values.visibleBlocks?.blocks.length, 24)
  assert.equal(result.values.visibleBlocks?.blocks.every(([, y, , name]) => y === -1 && name === 'grass_block'), true)
})

test('viewport provider reports the block directly underfoot', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>([['0,63,0', { name: 'grass_block', visible: true, occludes: true }]])
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, blocks))
  const result = await provider.read(context(), { fields: ['standingOnBlock'], page: { limit: 1 } }, new AbortController().signal)
  assert.deepEqual(result.values.standingOnBlock, { name: 'grass_block' })
})

test('viewport provider reports standingOnBlock as null when underfoot is unloaded', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, new Map([['0,63,0', 'unloaded']])))
  const result = await provider.read(context(), { fields: ['standingOnBlock'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.standingOnBlock, null)
})

test('viewport provider finds the nearest solid block along the sightline', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>([['0,65,-3', { name: 'stone', visible: true, occludes: true }]])
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, blocks))
  const result = await provider.read(context(), { fields: ['lookedAtBlock'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.lookedAtBlock?.name, 'stone')
})

test('viewport provider reports null when the sightline is all air within range', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, new Map()))
  const result = await provider.read(context(), { fields: ['lookedAtBlock'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.lookedAtBlock, null)
})

test('viewport provider stops at unloaded terrain instead of guessing past it', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>()
  for (let z = 0; z >= -6; z--) blocks.set(`0,65,${z}`, 'unloaded')
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, blocks))
  const result = await provider.read(context(), { fields: ['lookedAtBlock'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.lookedAtBlock, null)
})

test('viewport provider sorts nearby tracked entities by distance and bounds the list', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const entities: PerceptionEntityCandidate[] = [
    { type: 'zombie', position: { x: 0, y: 64, z: -10 } },
    { type: 'player', username: 'Alex', position: { x: 0, y: 64, z: -2 } },
    { type: 'cow', position: { x: 0, y: 64, z: -30 } },
  ]
  const provider = new ViewportInformationProvider(new FakePerceptionPort(pose, new Map(), entities))
  const result = await provider.read(context(), { fields: ['nearbyTrackedEntities'], page: { limit: 1 } }, new AbortController().signal)
  const list = result.values.nearbyTrackedEntities!
  assert.equal(list.length, 2)
  assert.equal(list[0]!.username, 'Alex')
  assert.equal(list[0]!.direction, 'ahead')
  assert.equal(list[1]!.type, 'zombie')
})
