import assert from 'node:assert/strict'
import test from 'node:test'
import { YIELD_EVERY_WORK_UNITS, raycastLookedAtBlock, standingOnBlock, viewRelativePosition, visibleBlocks, visibleEntities } from './perception.js'
import type { PerceptionBlock, PerceptionEntityCandidate, PerceptionPort, PerceptionPose } from './perception.js'

class FakePort implements PerceptionPort {
  constructor(
    public pose: PerceptionPose,
    readonly blocks = new Map<string, PerceptionBlock | 'unloaded'>(),
    readonly entities: PerceptionEntityCandidate[] = [],
  ) {}
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? { name: 'air', visible: false, occludes: false }
  }
  nearbyEntities() { return this.entities }
}
const opaque = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
const transparent = (name: string): PerceptionBlock => ({ name, visible: true, occludes: false })

test('view-relative position uses [right, up, forward]', () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  assert.deepEqual(viewRelativePosition(pose, { x: 5, y: 66, z: -3 }), [5, 2, 3])
})

test('look and underfoot observations retain only internal positions for projection', () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const port = new FakePort(pose, new Map([['0,65,-3', opaque('stone')], ['0,63,0', opaque('grass_block')]]))
  assert.deepEqual(raycastLookedAtBlock(port, 4.5), { name: 'stone', position: { x: 0, y: 65, z: -3 } })
  assert.deepEqual(standingOnBlock(port), { name: 'grass_block', position: { x: 0, y: 63, z: 0 } })
})

// 与生产一致：垂直取 vanilla 默认 FOV 70° 的一半，水平由 16:9 推出（≈51.22°）。
const TEST_FRUSTUM = {
  verticalHalfAngle: (35 * Math.PI) / 180,
  horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)),
}

test('visible blocks are FOV/occlusion filtered and cancellable', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const port = new FakePort(pose, new Map([['0,65,-3', opaque('stone')], ['0,65,-8', opaque('hidden')]]))
  const result = await visibleBlocks(port, { horizontalRadius: 8, verticalRadius: 3, maxDistance: 10, frustum: TEST_FRUSTUM, limit: 8 })
  assert.deepEqual(result.blocks.map(block => block.name), ['stone'])
  const controller = new AbortController()
  controller.abort()
  await assert.rejects(visibleBlocks(port, { horizontalRadius: 32, verticalRadius: 4, maxDistance: 32, frustum: TEST_FRUSTUM, limit: 8 }, controller.signal))
})

test('a non-occluding visible neighbor exposes the block behind transparent material', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const port = new FakePort(pose, new Map([
    ['0,65,-3', opaque('stone')],
    ['0,65,-2', transparent('glass')],
    ['1,65,-3', opaque('wall')],
    ['-1,65,-3', opaque('wall')],
    ['0,66,-3', opaque('wall')],
    ['0,64,-3', opaque('wall')],
    ['0,65,-4', opaque('wall')],
  ]))
  const result = await visibleBlocks(port, {
    horizontalRadius: 4, verticalRadius: 2, maxDistance: 6, frustum: TEST_FRUSTUM, limit: 16,
  })
  assert.equal(result.blocks.some(block => block.name === 'stone'), true)
})

test('the view frustum is rectangular, not an isotropic cone', async () => {
  // Both candidates sit 5 blocks ahead at eye height, one displaced sideways by 48° and one
  // raised by 40°. An isotropic cone of any single half-angle either admits both or rejects
  // both; vanilla's 16:9 frustum (51.22° wide, 35° tall) admits only the sideways one. That
  // asymmetry is the entire point of the shape, so assert it directly.
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const eyeY = 64 + 1.62
  const height = 1.3
  const forward = 5
  const sideways = forward * Math.tan((48 * Math.PI) / 180)
  const raised = forward * Math.tan((40 * Math.PI) / 180)
  const result = await visibleEntities(new FakePort(pose, new Map(), [
    { type: 'sheep', position: { x: sideways, y: eyeY - height / 2, z: -forward }, height },
    { type: 'cow', position: { x: 0, y: eyeY + raised - height / 2, z: -forward }, height },
  ]), 32, TEST_FRUSTUM, 8)
  assert.deepEqual(result.map(entity => entity.type), ['sheep'])
})

test('visible entities exclude behind and occluded candidates', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const entities = [
    { type: 'sheep', position: { x: 2, y: 64, z: -5 }, height: 1.3 },
    { type: 'cow', position: { x: 0, y: 64, z: 5 }, height: 1.4 },
  ]
  const result = await visibleEntities(new FakePort(pose, new Map(), entities), 32, TEST_FRUSTUM, 8)
  assert.equal(result.length, 1)
  assert.equal(result[0]!.type, 'sheep')
})

test('an entity remains visible when its hitbox intersects the frustum but its center does not', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const sheep = { type: 'sheep', position: { x: 0, y: 64, z: -1 }, width: 0.9, height: 1.3 }

  // At one block away the sheep's center is 44.1° below the crosshair, outside the 35°
  // vertical half-FOV. Its upper hitbox is still on screen, exactly like the live D40 case.
  const result = await visibleEntities(new FakePort(pose, new Map(), [sheep]), 32, TEST_FRUSTUM, 8)

  assert.deepEqual(result.map(entity => entity.type), ['sheep'])
})

test('entity scanning yields to the event loop and honors cancellation', async () => {
  // Cost scales with tracked entities, not with `limit`: the cap applies after filtering, and an
  // occluded candidate is the expensive case because it pays for every hitbox sample. A solid
  // wall with a herd behind it is enough to cross the yield budget several times over.
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>()
  for (let x = -2; x <= 2; x++) {
    for (let y = 63; y <= 67; y++) blocks.set(`${x},${y},-3`, opaque('wall'))
  }
  const entities: PerceptionEntityCandidate[] = []
  for (let index = 0; index < 24; index++) {
    entities.push({ type: 'sheep', position: { x: 0, y: 64, z: -5 - index }, width: 0.9, height: 1.3 })
  }
  const port = new FakePort(pose, blocks, entities)

  let timerRan = false
  setTimeout(() => { timerRan = true }, 0)
  const result = await visibleEntities(port, 32, TEST_FRUSTUM, 8)
  assert.deepEqual(result, [], 'the wall hides every candidate')
  assert.equal(timerRan, true, 'a pending timer must get a turn while the scan is in flight')

  const controller = new AbortController()
  controller.abort()
  await assert.rejects(visibleEntities(port, 32, TEST_FRUSTUM, 8, controller.signal))
})

test('a scan aborted during a yield stops at that yield, not the next one', async () => {
  // The production case is a deadline, scope invalidation, disconnect, or shutdown aborting the
  // owning run while the scan is parked in `await`. Ordinary player chat is FIFO and does not
  // preempt it. Aborting before the scan starts only exercises the entry check, while aborting
  // during synchronous work is caught before the next yield either way. To land inside the yield
  // window the abort has to be queued from within the scan itself.
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  // Leaves are visible but non-occluding, so no occlusion ray terminates early and every candidate
  // pays for the full distance. This is the most expensive real terrain.
  const leaves: PerceptionBlock = { name: 'oak_leaves', visible: true, occludes: false }
  class LeafPort implements PerceptionPort {
    calls = 0
    constructor(readonly onCall?: (calls: number) => void) {}
    selfPose() { return pose }
    revision() { return 1 }
    // Counted before the hook, and not inside the optional call: `onCall?.(++calls)` skips the
    // argument entirely when there is no hook, so the uninstrumented port would never count.
    blockAt() { this.calls++; this.onCall?.(this.calls); return leaves }
    nearbyEntities() { return [] }
  }
  const options = { horizontalRadius: 16, verticalRadius: 8, maxDistance: 16, frustum: TEST_FRUSTUM, limit: 64 }

  const complete = new LeafPort()
  await visibleBlocks(complete, options)
  assert.ok(complete.calls > YIELD_EVERY_WORK_UNITS * 8, 'the scene must be big enough to yield repeatedly')

  const controller = new AbortController()
  // Queued well inside the first quantum, so the abort callback is already waiting in the event
  // loop when the scan parks, and runs during that first yield rather than before or after it.
  const interrupted = new LeafPort(calls => {
    if (calls === 100) setImmediate(() => controller.abort())
  })
  await assert.rejects(visibleBlocks(interrupted, options, controller.signal))
  // A frustum-rejected voxel costs a work unit but no lookup, so one quantum of work is well under
  // YIELD_EVERY_WORK_UNITS lookups here — roughly 1.3k. Re-checking after the yield is what buys
  // that: with only the pre-yield check the scan resumes for a second quantum and lands past 3k.
  assert.ok(
    interrupted.calls < YIELD_EVERY_WORK_UNITS,
    `aborted scan did ${interrupted.calls} of ${complete.calls} lookups; it should stop at the yield`
      + ` the abort landed in, within one ${YIELD_EVERY_WORK_UNITS}-unit quantum`,
  )
})

/**
 * The 开阔平地 fixture from the scan experiment: `y <= 63` is a solid opaque half-space and the
 * player stands at `y = 64`. Only the `y = 63` layer has an exposed face (upward), which makes the
 * correct answer computable in closed form rather than borrowed from another implementation.
 */
function flatGround(pitch: number): FakePort {
  const blocks = new Map<string, PerceptionBlock | 'unloaded'>()
  for (let x = -40; x <= 40; x++) for (let z = -40; z <= 40; z++) for (let y = 44; y <= 63; y++) {
    blocks.set(`${x},${y},${z}`, opaque('grass_block'))
  }
  return new FakePort({ position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch }, blocks)
}

const SCAN = {
  horizontalRadius: 32, verticalRadius: 20, maxDistance: 32,
  // 70° vertical FOV at 16:9, matching the viewport provider.
  frustum: { verticalHalfAngle: (35 * Math.PI) / 180, horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)) },
  limit: 100_000,
}

test('the block-centre predicate reports no ground at all when looking level across flat ground', async () => {
  // The defect stated plainly: a ray aimed at a distant ground block's *centre* enters nearer ground
  // first, because the centre sits half a block below the surface a player actually sees.
  const level = await visibleBlocks(flatGround(0), SCAN)
  assert.equal(level.blocks.length, 0)

  // It is not that nothing is visible — the same world with the same predicate finds ground once the
  // gaze tilts down far enough that the rays stop grazing. So the predicate is pose-dependent in a
  // way the world is not, which is exactly what makes an incremental read hard to trust.
  const down = await visibleBlocks(flatGround(-1.2), SCAN)
  assert.equal(down.blocks.length > 0, true)
})

test('the exposed-face predicate finds the ground, and only ground that is really in view', async () => {
  const port = flatGround(0)
  const result = await visibleBlocks(port, { ...SCAN, predicate: 'exposed_face' })

  assert.equal(result.blocks.length > 200, true, `expected hundreds of surfaces, got ${result.blocks.length}`)

  // Oracle written independently of the implementation: the eye sits 1.62 above the top-face plane,
  // every top face is reachable because a ray descending to y=64 never dips below it, so a face is
  // visible exactly when it is inside the frustum and within range.
  //
  // Bounds are checked against the face with a block of slack on purpose. Culling runs against the
  // voxel *centre*, half a block lower, so the two disagree slightly at the frustum edge — see
  // scripts/visibility-predicate-comparison.ts, where matching the reference points takes recall
  // from an apparent 98.7% to an exact 100%.
  const eye = { x: 0, y: 64 + 1.62, z: 0 }
  for (const block of result.blocks) {
    assert.equal(block.position.y, 63, 'only the surface layer has an exposed face')
    const face = { x: block.position.x + 0.5, y: 64, z: block.position.z + 0.5 }
    const delta = { x: face.x - eye.x, y: face.y - eye.y, z: face.z - eye.z }
    const depth = -delta.z // yaw 0 looks along -Z, pitch 0 keeps the axis level
    assert.equal(depth > 0, true, 'nothing behind the camera')
    assert.equal(Math.hypot(delta.x, delta.y, delta.z) <= SCAN.maxDistance + 1, true, 'within range')
    assert.equal(Math.abs(delta.x) <= depth * Math.tan(SCAN.frustum.horizontalHalfAngle) + 1e-9, true, 'inside horizontal FOV')
    assert.equal(Math.abs(delta.y) <= depth * Math.tan(SCAN.frustum.verticalHalfAngle) + 1e-9, true, 'inside vertical FOV')
  }
})
