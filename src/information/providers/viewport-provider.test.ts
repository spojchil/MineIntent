import assert from 'node:assert/strict'
import test from 'node:test'
import type { PerceptionBlock, PerceptionEntityCandidate, PerceptionPort, PerceptionPose } from '../source-ports/perception.js'
import { assertInformationProviderContract } from '../testing/provider-contract.js'
import { ViewportInformationProvider } from './viewport-provider.js'

const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }

class FakePort implements PerceptionPort {
  constructor(
    public pose: PerceptionPose,
    readonly blocks = new Map<string, PerceptionBlock>(),
    readonly entities: PerceptionEntityCandidate[] = [],
    readonly fallback?: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded',
  ) {}
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? this.fallback?.(position) ?? AIR
  }
  nearbyEntities() { return this.entities }
}
const stone = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
/**
 * Shaped like what the library actually hands over, not like the field names suggest: `type` is a
 * broad category (`mob`, `object`, `player`) and `name` is the registry species. A fixture that put
 * `sheep` in `type` would pass even if the provider read the two fields the wrong way round.
 */
const sheep = (position: { x: number; y: number; z: number }): PerceptionEntityCandidate =>
  ({ type: 'mob', name: 'sheep', position, width: 0.9, height: 1.3 })
const player = (username: string, position: { x: number; y: number; z: number }): PerceptionEntityCandidate =>
  ({ type: 'player', name: 'player', username, position, width: 0.6, height: 1.8 })
const context = () => ({
  now: new Date().toISOString(),
  scope: { processSessionId: 's', connectionState: 'play' as const, connectionEpoch: 1, uiRevision: 0, capturedAt: new Date().toISOString() },
  caller: { audience: 'participant' as const, purpose: 'participant_context' as const },
  refs: { issue: () => { throw new Error('model viewport does not issue refs') } },
})

test('viewport provider satisfies its five-field contract', async () => {
  const provider = new ViewportInformationProvider(new FakePort({ position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }))
  await assertInformationProviderContract(provider, { context: context(), request: { fields: ['frame', 'standingOnBlock', 'lookedAtBlock', 'visibleEntities', 'visibleBlocks'], page: { limit: 1 } } })
})

test('positions are world-absolute so the same block keeps one key from any stance', async () => {
  // Absolute coordinates are what make an incremental read possible at all: a body-relative tuple
  // renames every block the moment the participant moves, so nothing can be diffed against it.
  const blocks = new Map([['3,65,0', stone('stone')]])
  const entities = [sheep({ x: 5, y: 64, z: 1 })]
  const first = await new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: -Math.PI / 2, pitch: 0 }, blocks, entities,
  )).read(context(), { fields: ['frame', 'visibleEntities', 'visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)

  assert.equal(first.values.frame?.coordinates, 'minecraft_world_absolute')
  assert.deepEqual(first.values.frame?.self.position, [0, 64, 0])
  assert.deepEqual(first.values.visibleEntities?.items[0], { type: 'sheep', position: [5, 64, 1] })
  assert.deepEqual(first.values.visibleBlocks?.blocks[0], ['stone', 3, 65, 0])
  // The legend rides with the data, so a schema change cannot outdate a prompt written elsewhere.
  assert.match(first.values.frame?.legend.visibleBlocks ?? '', /x, y, z/u)

  // Same world, different stance: the block keeps its key even though the bearing to it changed.
  const second = await new ViewportInformationProvider(new FakePort(
    { position: { x: 1, y: 64, z: 1 }, yaw: -Math.PI / 2, pitch: 0 }, blocks, entities,
  )).read(context(), { fields: ['visibleBlocks'], page: { limit: 1 } }, new AbortController().signal)
  assert.deepEqual(second.values.visibleBlocks?.blocks[0], ['stone', 3, 65, 0])
})

test('viewport provider selects the exposed-face predicate for level flat ground', async () => {
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 },
    new Map(),
    [],
    position => position.y <= 63 ? stone('grass_block') : AIR,
  ))
  const result = await provider.read(
    context(),
    { fields: ['visibleBlocks'], page: { limit: 1 } },
    new AbortController().signal,
  )

  assert.equal(result.values.visibleBlocks?.truncated, true)
  assert.equal(result.values.visibleBlocks?.blocks.length, 256)
  assert.equal(result.values.visibleBlocks?.blocks.every(
    ([name, , y]) => name === 'grass_block' && y === 63,
  ), true)
})

test('a player named after a mob stays distinguishable from that mob', async () => {
  // The collision that motivated splitting the label: one of these is someone to talk to. Under a
  // single `username ?? name ?? type` string both read as `sheep`, and no amount of context tells
  // the model which it was looking at.
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 },
    new Map(),
    [player('sheep', { x: 0, y: 64, z: -3 }), sheep({ x: 1, y: 64, z: -5 })],
  ))

  const result = await provider.read(context(), { fields: ['visibleEntities'], page: { limit: 1 } }, new AbortController().signal)

  assert.deepEqual(result.values.visibleEntities, {
    items: [
      { type: 'player', player: 'sheep', position: [0, 64, -3] },
      { type: 'sheep', position: [1, 64, -5] },
    ],
    truncated: false,
  })
  // `player` is absent rather than empty on a mob, so its presence alone answers "is this a person".
  assert.equal('player' in result.values.visibleEntities!.items[1]!, false)
})

test('a read that fills the entity cap says so', async () => {
  // Eight entries with no flag cannot be told from "there were only eight". The provider owns this
  // limit, so the flag has to survive the provider's own mapping, not just the scan's.
  const crowd = Array.from({ length: 9 }, (_unused, index) => sheep({ x: 0, y: 64, z: -2 - index }))
  const provider = new ViewportInformationProvider(new FakePort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }, new Map(), crowd,
  ))

  const result = await provider.read(context(), { fields: ['visibleEntities'], page: { limit: 1 } }, new AbortController().signal)

  assert.equal(result.values.visibleEntities?.items.length, 8)
  assert.equal(result.values.visibleEntities?.truncated, true)
})

test('a projection never reuses a revision while its content changes underneath', async () => {
  // The defect this replaces: the revision was signed from the pose plus the backend's
  // `snapshotRevision`, and that counter has no subscription to blocks or to entity movement. So a
  // participant standing still watched the world change while its revision sat frozen — not as a race
  // window but on every read. Both fields are exercised because both were affected.
  // Same stance and same coordinates as the world-absolute test above, which establishes that this
  // block and this sheep are both inside the frustum and unoccluded.
  const blocks = new Map<string, PerceptionBlock>()
  const entities: PerceptionEntityCandidate[] = [sheep({ x: 5, y: 64, z: 1 })]
  const port = new FakePort({ position: { x: 0, y: 64, z: 0 }, yaw: -Math.PI / 2, pitch: 0 }, blocks, entities)
  const provider = new ViewportInformationProvider(port)
  const read = async () => provider.read(
    context(), { fields: ['visibleEntities', 'visibleBlocks'], page: { limit: 1 } }, new AbortController().signal,
  )

  const before = await read()
  assert.equal(before.values.visibleEntities?.items.length, 1)

  // The pose is deliberately untouched: it is the one input the old signature did cover.
  entities.push(sheep({ x: 5, y: 64, z: 2 }))
  const afterEntityAppears = await read()
  assert.equal(afterEntityAppears.values.visibleEntities?.items.length, 2)
  assert.notEqual(afterEntityAppears.informationRevision, before.informationRevision)

  blocks.set('3,65,0', stone('stone'))
  const afterBlockPlaced = await read()
  assert.deepEqual(afterBlockPlaced.values.visibleBlocks?.blocks, [['stone', 3, 65, 0]])
  assert.notEqual(afterBlockPlaced.informationRevision, afterEntityAppears.informationRevision)

  // `availability` is the runtime's staleness probe, so asking twice must not itself change the
  // answer — the fix must not turn a read counter into a side effect of being asked.
  assert.equal(provider.availability().informationRevision, provider.availability().informationRevision)
  assert.equal(provider.availability().informationRevision, afterBlockPlaced.informationRevision)
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
