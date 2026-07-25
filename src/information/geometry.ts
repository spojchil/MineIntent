export interface Point3 { x: number; y: number; z: number }

export type RelativeDirection = 'ahead' | 'right' | 'behind' | 'left'

export function distanceBetween(a: Point3, b: Point3): number {
  return Math.hypot(a.x - b.x, a.y - b.y, a.z - b.z)
}

/**
 * Matches Mineflayer's own yaw/pitch-to-direction convention exactly (cross-checked against
 * mineflayer/lib/plugins/ray_trace.js and mineflayer-pathfinder's getViewVector/blockInteraction
 * example — all three independently agree on this sign convention). `entity.yaw`/`entity.pitch`
 * from the protocol are meaningless without it, so this must not drift from the real library.
 */
export function lookDirection(yaw: number, pitch: number): Point3 {
  return {
    x: -Math.sin(yaw) * Math.cos(pitch),
    y: Math.sin(pitch),
    z: -Math.cos(yaw) * Math.cos(pitch),
  }
}

export function relativeBearing(selfYaw: number, selfPosition: Point3, targetPosition: Point3): RelativeDirection {
  const dx = targetPosition.x - selfPosition.x
  const dz = targetPosition.z - selfPosition.z
  if (dx === 0 && dz === 0) return 'ahead'
  const look = lookDirection(selfYaw, 0)
  const dot = look.x * dx + look.z * dz
  const cross = look.x * dz - look.z * dx
  const angle = Math.atan2(cross, dot)
  const quarter = Math.PI / 2
  if (angle >= -quarter / 2 && angle < quarter / 2) return 'ahead'
  // A positive cross product puts the target on the rightward axis: facing north (yaw 0, -Z),
  // east (+X) is on the player's right, matching what a player reads off F3 or the compass.
  if (angle >= quarter / 2 && angle < quarter * 3 / 2) return 'right'
  if (angle >= -quarter * 3 / 2 && angle < -quarter / 2) return 'left'
  return 'behind'
}
