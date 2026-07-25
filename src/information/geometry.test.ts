import assert from 'node:assert/strict'
import { test } from 'node:test'
import { distanceBetween, lookDirection, relativeBearing } from './geometry.js'

test('distanceBetween computes 3D euclidean distance', () => {
  assert.equal(distanceBetween({ x: 0, y: 0, z: 0 }, { x: 3, y: 0, z: 4 }), 5)
  assert.equal(distanceBetween({ x: 1, y: 1, z: 1 }, { x: 1, y: 1, z: 1 }), 0)
})

// Pinned against mineflayer's own yaw/pitch-to-direction formula (lib/plugins/ray_trace.js
// getViewDirection, and mineflayer-pathfinder's independent getViewVector/blockInteraction
// example — all three agree). If this ever needs to change, re-verify against those first;
// a silent sign flip here means every raycast/bearing looks the wrong way without erroring.
test('lookDirection matches mineflayer\'s own yaw/pitch-to-direction formula', () => {
  const cases: Array<[number, number, [number, number, number]]> = [
    [0, 0, [0, 0, -1]],
    [Math.PI / 2, 0, [-1, 0, 0]],
    [0, Math.PI / 2, [0, 1, 0]],
  ]
  for (const [yaw, pitch, [x, y, z]] of cases) {
    const direction = lookDirection(yaw, pitch)
    assert.ok(Math.abs(direction.x - x) < 1e-9, `x for yaw=${yaw} pitch=${pitch}`)
    assert.ok(Math.abs(direction.y - y) < 1e-9, `y for yaw=${yaw} pitch=${pitch}`)
    assert.ok(Math.abs(direction.z - z) < 1e-9, `z for yaw=${yaw} pitch=${pitch}`)
  }
})

test('relativeBearing classifies target position relative to self facing', () => {
  const self = { x: 0, y: 64, z: 0 }
  // yaw 0 faces north (-Z). Facing north, east (+X) is on the player's right and west (-X)
  // on the left — the orientation a player reads off F3 or a compass.
  assert.equal(relativeBearing(0, self, { x: 0, y: 64, z: -5 }), 'ahead')
  assert.equal(relativeBearing(0, self, { x: 0, y: 64, z: 5 }), 'behind')
  assert.equal(relativeBearing(0, self, { x: -5, y: 64, z: 0 }), 'left')
  assert.equal(relativeBearing(0, self, { x: 5, y: 64, z: 0 }), 'right')
})

test('relativeBearing agrees with the rightward axis of lookDirection at every yaw', () => {
  const self = { x: 0, y: 64, z: 0 }
  for (const yaw of [0, Math.PI / 2, Math.PI, -Math.PI / 2, 0.7]) {
    const look = lookDirection(yaw, 0)
    // Rightward axis: the look vector rotated a quarter turn clockwise in the XZ plane.
    const rightward = { x: -look.z, z: look.x }
    const toRight = { x: rightward.x * 5, y: 64, z: rightward.z * 5 }
    const toLeft = { x: -rightward.x * 5, y: 64, z: -rightward.z * 5 }
    assert.equal(relativeBearing(yaw, self, toRight), 'right', `yaw ${yaw} rightward`)
    assert.equal(relativeBearing(yaw, self, toLeft), 'left', `yaw ${yaw} leftward`)
  }
})

test('relativeBearing rotates with self yaw', () => {
  const self = { x: 0, y: 64, z: 0 }
  const facingPositiveX = -Math.PI / 2 // lookDirection(-PI/2, 0) -> (1, 0, 0)
  assert.equal(relativeBearing(facingPositiveX, self, { x: 5, y: 64, z: 0 }), 'ahead')
})

test('relativeBearing defaults to ahead when target is exactly at self position', () => {
  const self = { x: 0, y: 64, z: 0 }
  assert.equal(relativeBearing(0, self, { x: 0, y: 64, z: 0 }), 'ahead')
})
