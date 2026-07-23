import assert from 'node:assert/strict'
import test from 'node:test'
import { raycastLookedAtBlock, standingOnBlock, viewRelativePosition, visibleBlocks, visibleEntities } from './perception.js'
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

test('visible blocks are FOV/occlusion filtered and cancellable', async () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const port = new FakePort(pose, new Map([['0,65,-3', opaque('stone')], ['0,65,-8', opaque('hidden')]]))
  const result = await visibleBlocks(port, { horizontalRadius: 8, verticalRadius: 3, maxDistance: 10, halfAngle: Math.PI / 4, limit: 8 })
  assert.deepEqual(result.blocks.map(block => block.name), ['stone'])
  const controller = new AbortController()
  controller.abort()
  await assert.rejects(visibleBlocks(port, { horizontalRadius: 32, verticalRadius: 4, maxDistance: 32, halfAngle: Math.PI / 4, limit: 8 }, controller.signal))
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
    horizontalRadius: 4, verticalRadius: 2, maxDistance: 6, halfAngle: Math.PI / 4, limit: 16,
  })
  assert.equal(result.blocks.some(block => block.name === 'stone'), true)
})

test('visible entities exclude behind and occluded candidates', () => {
  const pose = { position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch: 0 }
  const entities = [
    { type: 'sheep', position: { x: 2, y: 64, z: -5 }, height: 1.3 },
    { type: 'cow', position: { x: 0, y: 64, z: 5 }, height: 1.4 },
  ]
  const result = visibleEntities(new FakePort(pose, new Map(), entities), 32, Math.PI / 4, 8)
  assert.equal(result.length, 1)
  assert.equal(result[0]!.type, 'sheep')
  assert.equal(result[0]!.direction, 'ahead')
})
