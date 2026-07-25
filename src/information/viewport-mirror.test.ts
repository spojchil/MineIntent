import assert from 'node:assert/strict'
import test from 'node:test'
import { VIEWPORT_SCAN } from './providers/viewport-provider.js'
import type { PerceptionBlock, PerceptionPort, PerceptionPose } from './source-ports/perception.js'
import { ViewportMirror, type MirrorObservation } from './viewport-mirror.js'

const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const STONE: PerceptionBlock = { name: 'stone', visible: true, occludes: true }

class FakePort implements PerceptionPort {
  pose: PerceptionPose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  blocks = new Map<string, PerceptionBlock | 'unloaded'>()
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    return this.blocks.get(`${position.x},${position.y},${position.z}`) ?? AIR
  }
  nearbyEntities() { return [] }
}

/** A scan that saw exactly these blocks and looked as far as the scan radius allows. */
function sawBlocks(...positions: Array<[string, number, number, number]>): MirrorObservation {
  return {
    blocks: positions.map(([name, x, y, z]) => ({ name, position: { x, y, z } })),
    verifiedDistance: VIEWPORT_SCAN.maxDistance,
  }
}

test('the first look reports everything as new and the same look again reports nothing', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)

  const first = mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))
  assert.deepEqual(first, { added: [['stone', 0, 64, -5]], removed: [], unverified: 0 })
  assert.equal(mirror.size, 1)

  // Nothing changed, so nothing is said. This is the whole reason a diff is cheaper than a re-send.
  const second = mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))
  assert.deepEqual(second, { added: [], removed: [], unverified: 0 })
})

test('a block that is really gone is reported removed', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  // Mined: still in view, still reachable, and now empty. The one case that is a real removal.
  port.blocks.set('0,64,-5', AIR)
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks())

  assert.deepEqual(result, { added: [], removed: [['stone', 0, 64, -5]], unverified: 0 })
  assert.equal(mirror.size, 0)
})

test('turning away does not remove anything, and the block stays remembered', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  // The failure this guards against: a head turn empties the scan, and reporting that as removals
  // would tell the model a wall vanished every time it looked elsewhere.
  port.pose = { position: { x: 0, y: 64, z: 0 }, yaw: Math.PI, pitch: 0 }
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks())

  assert.deepEqual(result, { added: [], removed: [], unverified: 1 })
  assert.equal(mirror.size, 1)

  // Looking back reports nothing new either: the model was never told it was gone.
  port.pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  assert.deepEqual(mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5])), { added: [], removed: [], unverified: 0 })
})

test('a block hidden behind something new is unverified rather than removed', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  for (let z = -4; z <= -1; z++) for (let y = 60; y <= 70; y++) for (let x = -2; x <= 2; x++) {
    port.blocks.set(`${x},${y},${z}`, STONE)
  }
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -1]))

  assert.deepEqual(result.removed, [])
  assert.equal(result.unverified, 1)
  assert.deepEqual(result.added, [['stone', 0, 64, -1]])
})

test('an unloaded chunk verifies nothing, so the block survives the reload gap', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  port.blocks.set('0,64,-5', 'unloaded')
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks())

  assert.deepEqual(result, { added: [], removed: [], unverified: 1 })
  assert.equal(mirror.size, 1)
})

test('a truncated scan cannot remove blocks beyond how far it actually looked', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-20', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -20]))

  // The output cap keeps the nearest blocks, so a crowded view stops short. Without honouring that
  // radius, every step would churn the cap's edge with removals that never happened.
  port.blocks.set('0,64,-20', AIR)
  const result = mirror.diff(port, VIEWPORT_SCAN, { blocks: [], verifiedDistance: 6 })

  assert.deepEqual(result, { added: [], removed: [], unverified: 1 })
})

test('a replaced block is one removal plus one addition', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  port.blocks.set('0,64,-5', { name: 'oak_planks', visible: true, occludes: true })
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['oak_planks', 0, 64, -5]))

  assert.deepEqual(result.removed, [['stone', 0, 64, -5]])
  assert.deepEqual(result.added, [['oak_planks', 0, 64, -5]])
  // No stale entry left behind under the same key.
  assert.equal(mirror.size, 1)
})

test('exhausting the re-check budget degrades to unverified, never to false removals', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  const seen: Array<[string, number, number, number]> = []
  for (let x = -10; x <= 10; x++) for (let y = 60; y <= 68; y++) seen.push(['stone', x, y, -5])
  for (const [, x, y, z] of seen) port.blocks.set(`${x},${y},${z}`, STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(...seen))
  assert.equal(mirror.size, seen.length)

  // Every one of them is now genuinely gone, but there are more than the ray budget allows. The cap
  // must cost reports, not accuracy: what it cannot check stays remembered.
  for (const [, x, y, z] of seen) port.blocks.set(`${x},${y},${z}`, AIR)
  const result = mirror.diff(port, VIEWPORT_SCAN, sawBlocks())

  assert.equal(result.removed.length + result.unverified, seen.length)
  assert.equal(result.unverified > 0, true)
  assert.equal(mirror.size, result.unverified)
})

test('clearing forgets everything so a fresh conversation starts from nothing', () => {
  const port = new FakePort()
  const mirror = new ViewportMirror()
  port.blocks.set('0,64,-5', STONE)
  mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5]))

  mirror.clear()

  assert.equal(mirror.size, 0)
  // Everything is new again, which is correct: the model has not been told anything yet.
  assert.deepEqual(mirror.diff(port, VIEWPORT_SCAN, sawBlocks(['stone', 0, 64, -5])).added, [['stone', 0, 64, -5]])
})
