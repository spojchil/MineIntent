import assert from 'node:assert/strict'
import { test } from 'node:test'
import { YIELD_EVERY_WORK_UNITS, raycastLookedAtBlock, standingOnBlock, visibleBlocks } from './perception.js'
import type { PerceptionBlock, PerceptionPort, PerceptionPose, VisibleBlocksOptions } from './perception.js'

const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const opaque = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
const transparent = (name: string): PerceptionBlock => ({ name, visible: true, occludes: false })

class FakePerceptionPort implements PerceptionPort {
  constructor(
    public pose: PerceptionPose,
    private readonly blocks: Map<string, PerceptionBlock | 'unloaded'> = new Map(),
    private readonly fallback?: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded',
  ) {}
  selfPose(): PerceptionPose { return this.pose }
  blockAt(position: PerceptionPose['position']): PerceptionBlock | 'unloaded' {
    const key = `${position.x},${position.y},${position.z}`
    return this.blocks.get(key) ?? this.fallback?.(position) ?? AIR
  }
  nearbyEntities() { return [] }
}

const TEST_FRUSTUM = {
  verticalHalfAngle: (35 * Math.PI) / 180,
  horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)),
}
const DEFAULT_OPTIONS: VisibleBlocksOptions = {
  horizontalRadius: 8,
  verticalRadius: 4,
  maxDistance: 10,
  frustum: TEST_FRUSTUM,
  limit: 24,
  predicate: 'exposed_face',
}

test('visibleBlocks includes an exposed, unoccluded block directly ahead', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map([['0,65,-3', opaque('stone')]])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.truncated, false)
  assert.equal(result.blocks.length, 1)
  assert.deepEqual(result.blocks[0]!.position, { x: 0, y: 65, z: -3 })
  assert.equal(result.blocks[0]!.name, 'stone')
})

test('visibleBlocks excludes a block fully enclosed by occluding neighbors', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map([
    ['1,65,-6', opaque('target')],
    ['2,65,-6', opaque('wall')], ['0,65,-6', opaque('wall')],
    ['1,66,-6', opaque('wall')], ['1,64,-6', opaque('wall')],
    ['1,65,-5', opaque('wall')], ['1,65,-7', opaque('wall')],
  ])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.blocks.some(block => block.name === 'target'), false)
})

test('visibleBlocks excludes an exposed block behind the camera', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map([['0,65,6', opaque('stone')]])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.blocks.length, 0)
})

test('visibleBlocks excludes a target behind an occluding wall', async () => {
  const pose: PerceptionPose = { position: { x: 0.5, y: 64, z: 0.5 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock>()
  for (let x = -2; x <= 2; x++) {
    for (let y = 63; y <= 68; y++) blocks.set(`${x},${y},-3`, opaque('wall'))
  }
  blocks.set('0,65,-8', opaque('hidden'))
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.blocks.some(block => block.name === 'hidden'), false)
  assert.equal(result.blocks.some(block => block.name === 'wall'), true)
})

test('visibleBlocks sorts by distance and marks the list truncated past the limit', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map([
    ['0,65,-2', opaque('nearest')],
    ['-3,65,-6', opaque('farther')],
  ])
  const port = new FakePerceptionPort(pose, blocks)
  const full = await visibleBlocks(port, DEFAULT_OPTIONS)
  assert.equal(full.truncated, false)
  assert.equal(full.blocks.length, 2)
  assert.equal(full.blocks[0]!.name, 'nearest')

  const limited = await visibleBlocks(port, { ...DEFAULT_OPTIONS, limit: 1 })
  assert.equal(limited.truncated, true)
  assert.equal(limited.blocks.length, 1)
  assert.equal(limited.blocks[0]!.name, 'nearest')
})

test('visibleBlocks treats unloaded candidates conservatively and honors cancellation', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>([['0,65,-3', 'unloaded']])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.blocks.length, 0)

  const controller = new AbortController()
  controller.abort()
  await assert.rejects(visibleBlocks(new FakePerceptionPort(pose), DEFAULT_OPTIONS, controller.signal))
})

test('a dense scan yields to the event loop and stops at the yield where it is aborted', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const leaves: PerceptionBlock = { name: 'oak_leaves', visible: true, occludes: false }
  class LeafPort implements PerceptionPort {
    calls = 0
    constructor(private readonly onCall?: (calls: number) => void) {}
    selfPose() { return pose }
    blockAt() {
      this.calls++
      this.onCall?.(this.calls)
      return leaves
    }
    nearbyEntities() { return [] }
  }
  const options: VisibleBlocksOptions = {
    horizontalRadius: 16,
    verticalRadius: 8,
    maxDistance: 16,
    frustum: TEST_FRUSTUM,
    limit: 64,
    predicate: 'exposed_face',
  }

  let timerRan = false
  setTimeout(() => { timerRan = true }, 0)
  const complete = new LeafPort()
  await visibleBlocks(complete, options)
  assert.equal(timerRan, true, 'a pending timer must get a turn while the scan is in flight')
  assert.ok(complete.calls > YIELD_EVERY_WORK_UNITS * 8, 'the fixture must cross several work quanta')

  const controller = new AbortController()
  const interrupted = new LeafPort(calls => {
    if (calls === 100) setImmediate(() => controller.abort())
  })
  await assert.rejects(visibleBlocks(interrupted, options, controller.signal))
  assert.ok(
    interrupted.calls < YIELD_EVERY_WORK_UNITS,
    `aborted scan did ${interrupted.calls} lookups; it should stop within one work quantum`,
  )
})

test('a visible non-occluding block does not hide an opaque block behind it', async () => {
  const pose: PerceptionPose = { position: { x: 0.5, y: 64, z: 0.5 }, yaw: 0, pitch: 0 }
  const blocks = new Map([
    ['0,65,-2', transparent('glass')],
    ['0,65,-3', opaque('stone')],
  ])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), DEFAULT_OPTIONS)
  assert.equal(result.blocks.some(block => block.name === 'glass'), true)
  assert.equal(result.blocks.some(block => block.name === 'stone'), true)
})

test('the view frustum is rectangular rather than an isotropic cone', async () => {
  const pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map([
    // About 50.4° sideways: outside a 35° cone, inside the 16:9 horizontal half-angle (~51.2°).
    ['11,65,-10', opaque('sideways')],
    // About 39.7° upward: outside the 35° vertical half-angle.
    ['0,73,-10', opaque('raised')],
  ])
  const result = await visibleBlocks(new FakePerceptionPort(pose, blocks), {
    ...DEFAULT_OPTIONS, horizontalRadius: 12, verticalRadius: 10, maxDistance: 20,
  })
  assert.equal(result.blocks.some(block => block.name === 'sideways'), true)
  assert.equal(result.blocks.some(block => block.name === 'raised'), false)
})

test('raycastLookedAtBlock sees transparent blocks while standingOnBlock rejects air', () => {
  const pose: PerceptionPose = { position: { x: 0.5, y: 64, z: 0.5 }, yaw: 0, pitch: 0 }
  const port = new FakePerceptionPort(pose, new Map([
    ['0,65,-2', transparent('glass')],
    ['0,65,-3', opaque('stone')],
    ['0,63,0', opaque('grass_block')],
  ]))
  assert.equal(raycastLookedAtBlock(port, 4.5)?.name, 'glass')
  assert.deepEqual(standingOnBlock(port), { name: 'grass_block' })
  assert.equal(standingOnBlock(new FakePerceptionPort(pose)), null)
})

/** Solid opaque half-space with its sole exposed layer at y=63. */
function flatGround(pitch: number): FakePerceptionPort {
  return new FakePerceptionPort(
    { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch },
    new Map(),
    position => position.y <= 63 ? opaque('grass_block') : AIR,
  )
}

const FLAT_GROUND_SCAN: VisibleBlocksOptions = {
  horizontalRadius: 32,
  verticalRadius: 20,
  maxDistance: 32,
  frustum: TEST_FRUSTUM,
  limit: 100_000,
}

function flatGroundOracle(): Set<string> {
  const eye = { x: 0, y: 64 + 1.62, z: 0 }
  const expected = new Set<string>()
  for (let x = -FLAT_GROUND_SCAN.horizontalRadius; x <= FLAT_GROUND_SCAN.horizontalRadius; x++) {
    for (let z = -FLAT_GROUND_SCAN.horizontalRadius; z <= FLAT_GROUND_SCAN.horizontalRadius; z++) {
      // Enumeration culls by voxel centre before testing an exposed face, so the independent oracle
      // uses that same declared candidate reference point and derives the answer geometrically.
      const delta = { x: x + 0.5 - eye.x, y: 63.5 - eye.y, z: z + 0.5 - eye.z }
      const depth = -delta.z
      if (depth <= 0) continue
      if (Math.hypot(delta.x, delta.y, delta.z) > FLAT_GROUND_SCAN.maxDistance) continue
      if (Math.abs(delta.x) > depth * Math.tan(TEST_FRUSTUM.horizontalHalfAngle)) continue
      if (Math.abs(delta.y) > depth * Math.tan(TEST_FRUSTUM.verticalHalfAngle)) continue
      expected.add(`${x},63,${z}`)
    }
  }
  return expected
}

test('the legacy block-centre predicate reports no level flat ground', async () => {
  const level = await visibleBlocks(flatGround(0), { ...FLAT_GROUND_SCAN, predicate: 'block_centre' })
  assert.equal(level.blocks.length, 0)

  const downward = await visibleBlocks(flatGround(-1.2), { ...FLAT_GROUND_SCAN, predicate: 'block_centre' })
  assert.equal(downward.blocks.length > 0, true)
})

test('the exposed-face predicate matches the flat-ground oracle while section culling reduces work', async () => {
  const metrics = { sectionsTested: 0, sectionsSkipped: 0, voxelsExamined: 0 }
  const result = await visibleBlocks(flatGround(0), {
    ...FLAT_GROUND_SCAN,
    predicate: 'exposed_face',
    metrics,
  })
  const expected = flatGroundOracle()

  assert.equal(expected.size, 900)
  assert.deepEqual(new Set(result.blocks.map(block => `${block.position.x},${block.position.y},${block.position.z}`)), expected)
  const candidateBoxVolume = (2 * FLAT_GROUND_SCAN.horizontalRadius + 1) ** 2
    * (2 * FLAT_GROUND_SCAN.verticalRadius + 1)
  assert.equal(metrics.voxelsExamined < candidateBoxVolume / 2, true, `${metrics.voxelsExamined} of ${candidateBoxVolume}`)
  assert.equal(metrics.sectionsSkipped > 0, true)
})
