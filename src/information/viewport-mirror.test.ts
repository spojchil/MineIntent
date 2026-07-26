import assert from 'node:assert/strict'
import test from 'node:test'
import { visibleBlocks, type PerceptionBlock, type PerceptionPort, type PerceptionPose } from './source-ports/perception.js'
import { ViewportMirror, type ViewportDiff } from './viewport-mirror.js'

const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const STONE: PerceptionBlock = { name: 'stone', visible: true, occludes: true }
const EYE_HEIGHT = 1.62

class FakePort implements PerceptionPort {
  pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  blocks = new Map<string, PerceptionBlock | 'unloaded'>()
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? AIR
  }
  nearbyEntities() { return [] }
  set(x: number, y: number, z: number, block: PerceptionBlock | 'unloaded') {
    this.blocks.set(`${x},${y},${z}`, block)
  }
}

/** Deliberately small: these tests are about the comparison, not about scan volume. */
const SCAN = {
  horizontalRadius: 10, verticalRadius: 6, maxDistance: 12,
  frustum: {
    verticalHalfAngle: (35 * Math.PI) / 180,
    horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)),
  },
  limit: 1_000,
}

/**
 * One look, end to end: the real scan consults the mirror while it enumerates, then the mirror folds
 * the result. Going through `visibleBlocks` rather than a hand-built observation is the point — the
 * comparison now happens inside the enumeration, so a synthetic input would test nothing real.
 */
async function look(port: FakePort, mirror: ViewportMirror, limit = SCAN.limit): Promise<ViewportDiff> {
  const result = await visibleBlocks(port, { ...SCAN, limit }, undefined, mirror)
  const self = port.pose.position
  return mirror.fold({
    blocks: result.blocks,
    vanished: result.vanished,
    eye: { x: self.x, y: self.y + EYE_HEIGHT, z: self.z },
    reach: SCAN.maxDistance,
  })
}

test('the first look reports everything as new and the same look again reports nothing', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)

  assert.deepEqual(await look(port, mirror), { added: [['stone', 0, 64, -5]], removed: [], unverified: 0 })
  assert.equal(mirror.size, 1)

  // Nothing changed, so nothing is said. This is the whole reason a diff beats re-sending.
  assert.deepEqual(await look(port, mirror), { added: [], removed: [], unverified: 0 })
})

test('a block that is really gone is reported removed', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  // Mined: the enumeration reaches the voxel, finds it empty, and the read that proves that is the
  // same read it was going to make anyway. This is the only case that becomes a removal.
  port.set(0, 64, -5, AIR)

  assert.deepEqual(await look(port, mirror), { added: [], removed: [['stone', 0, 64, -5]], unverified: 0 })
  assert.equal(mirror.size, 0)
})

test('turning away removes nothing, and the block stays remembered', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  // The failure this guards: a head turn empties the hit list, and calling that a removal would tell
  // the model a wall vanished every time the companion looked elsewhere. Out-of-frustum voxels are
  // never examined, so they cannot produce a `vanished` entry at all.
  port.pose = { position: { x: 0, y: 64, z: 0 }, yaw: Math.PI, pitch: 0 }
  assert.deepEqual(await look(port, mirror), { added: [], removed: [], unverified: 1 })
  assert.equal(mirror.size, 1)

  // Looking back says nothing new either: the model was never told it was gone.
  port.pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  assert.deepEqual(await look(port, mirror), { added: [], removed: [], unverified: 0 })
})

test('a block hidden behind something new is unconfirmed rather than removed', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  // Front face at z = -1, not z = 0: a wall exactly coplanar with the eye is edge-on and has zero
  // projected area, so it is correctly invisible and would test nothing.
  for (let z = -4; z <= -2; z++) for (let y = 60; y <= 70; y++) for (let x = -2; x <= 2; x++) port.set(x, y, z, STONE)
  const result = await look(port, mirror)

  assert.deepEqual(result.removed, [])
  assert.equal(result.unverified, 1)
  assert.equal(result.added.length > 0, true, 'the new wall itself is new')
})

test('a block walled in by the player is still there, so it is not reported removed', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  // Every neighbour becomes solid, so the block has no exposed face left. It has not gone anywhere —
  // the player built around it. Treating "cannot be seen" as "is gone" is the same lie as reporting a
  // removal after a head turn.
  for (const [dx, dy, dz] of [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]]) {
    port.set(dx!, 64 + dy!, -5 + dz!, STONE)
  }
  const result = await look(port, mirror)

  assert.deepEqual(result.removed, [])
  assert.equal(mirror.size >= 1, true, 'the walled-in block is still remembered')
})

test('an unloaded chunk verifies nothing, so the block survives the reload gap', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  port.set(0, 64, -5, 'unloaded')

  assert.deepEqual(await look(port, mirror), { added: [], removed: [], unverified: 1 })
  assert.equal(mirror.size, 1)
})

test('a removal no longer depends on the output budget', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  for (let x = 0; x < 6; x++) port.set(x, 64, -5, STONE)
  await look(port, mirror)
  assert.equal(mirror.size, 6)

  // One block is mined while the cap allows only two blocks through. Under the old two-pass design a
  // voxel beyond the cut could not be judged at all; the enumeration examines every voxel regardless,
  // so the removal is exact and the blocks merely crowded out land in `unverified`.
  port.set(3, 64, -5, AIR)
  const result = await look(port, mirror, 2)

  assert.deepEqual(result.removed, [['stone', 3, 64, -5]])
  assert.equal(result.unverified, 3, 'seen but over budget counts as unconfirmed, not as absent')
})

test('a replaced block is one removal plus one addition', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  port.set(0, 64, -5, { name: 'oak_planks', visible: true, occludes: true })
  const result = await look(port, mirror)

  assert.deepEqual(result.removed, [['stone', 0, 64, -5]])
  assert.deepEqual(result.added, [['oak_planks', 0, 64, -5]])
  assert.equal(mirror.size, 1, 'no stale entry left under the same key')
})

test('clearing forgets everything so a fresh conversation starts from nothing', async () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.set(0, 64, -5, STONE)
  await look(port, mirror)

  mirror.clear()

  assert.equal(mirror.size, 0)
  assert.deepEqual((await look(port, mirror)).added, [['stone', 0, 64, -5]])
})
