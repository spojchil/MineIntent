import { setImmediate as yieldToEventLoop } from 'node:timers/promises'
import { lookDirection, type Point3 } from '../geometry.js'

export interface PerceptionPose { position: Point3; yaw: number; pitch: number }
export interface PerceptionBlock {
  name: string
  visible: boolean
  occludes: boolean
}
export interface PerceptionEntityCandidate {
  type: string
  name?: string
  username?: string
  position: Point3
  width?: number
  height?: number
}
export interface PerceptionPort {
  selfPose(): PerceptionPose
  revision(): number
  blockAt(position: Point3): PerceptionBlock | 'unloaded'
  nearbyEntities(): readonly PerceptionEntityCandidate[]
}

export interface LookedAtBlock { name: string; position: Point3 }
export interface VisibleEntity {
  type: string
  name?: string
  username?: string
  position: Point3
  distance: number
}
export interface VisibleBlock { name: string; position: Point3; distance: number }

/**
 * Half-angles of vanilla's view frustum. Minecraft renders a rectangular frustum, not a cone:
 * the FOV slider is the *vertical* field of view (GameRenderer hands it to
 * Matrix4f.perspective, whose first argument is fovy), and the horizontal angle follows from
 * the window aspect ratio. A single isotropic half-angle is wrong in both directions at once.
 */
export interface ViewFrustum {
  /** Half of the vertical FOV in radians. Vanilla's default slider value of 70° gives 35°. */
  verticalHalfAngle: number
  /** Half of the horizontal FOV in radians. At 16:9 this is atan(tan(35°) · 16/9) ≈ 51.22°. */
  horizontalHalfAngle: number
}

/**
 * Which test decides that a candidate block is actually visible.
 *
 * `block_centre` casts one ray at the block's centre and accepts only if that ray reaches it. It is
 * the original test and it is wrong in a way that is easy to miss: **a block's centre is not the
 * surface a player sees.** A distant ground block's centre sits below the surface, so the ray enters
 * nearer ground first and the block is judged occluded — looking level across flat ground therefore
 * reports no ground at all.
 *
 * `exposed_face` asks the question a player's eye actually answers: is any face of this block both
 * uncovered and reachable. Only faces the eye is on the outer side of can qualify, which is at most
 * three, so it also reads fewer neighbours than the separate exposure check the centre test needs.
 */
export type VisibilityPredicate = 'block_centre' | 'exposed_face'

export interface VisibleBlocksOptions {
  horizontalRadius: number
  verticalRadius: number
  maxDistance: number
  frustum: ViewFrustum
  limit: number
  /**
   * Defaults to `exposed_face`. `block_centre` is kept only so the comparison harness can still run
   * the old test; it reports no ground at all when looking level across flat ground.
   */
  predicate?: VisibilityPredicate
}

const EYE_HEIGHT = 1.62
const STEP = 0.25
/**
 * Work units between yields. Exported because it is the unit of cancellation latency: after a
 * signal aborts, a scan may do up to one more quantum before it stops, and the tests assert
 * against that bound rather than a magic number.
 */
export const YIELD_EVERY_WORK_UNITS = 2_048

interface RayHit { voxel: Point3; name: string }
interface AxisAlignedBox { min: Point3; max: Point3 }

/**
 * Work counter shared by every scan loop and every raycast they fire. Counting only the outer
 * loop understates the real cost by orders of magnitude: each candidate voxel and each entity
 * hitbox sample casts a ray whose steps used to be invisible to the yield budget, so a dense
 * scene could run thousands of blockAt calls between two yields. Rays increment the same counter
 * and callers yield *between* rays, which bounds uninterrupted work at one raycast.
 */
interface ScanBudget { work: number }

/**
 * Yields, checking cancellation on both sides: before, so a scan that is already doomed stops
 * paying, and after, so a signal aborted while the event loop ran does not buy another full
 * quantum of work. The post-yield check matters when the owning run is aborted while the scan is
 * parked, for example by a deadline, world/connection scope invalidation, disconnect, or
 * application shutdown. Ordinary player chat is FIFO and does not preempt the active decision.
 *
 * Callers guard this with `budget.work >= YIELD_EVERY_WORK_UNITS` instead of testing inside,
 * because an `async` function allocates a promise and takes a microtask turn on *every* call,
 * and the guard is false for all but one iteration in ~2048 of the hot loop.
 */
async function spendBudget(budget: ScanBudget, signal?: AbortSignal): Promise<void> {
  budget.work = 0
  signal?.throwIfAborted()
  await yieldToEventLoop()
  signal?.throwIfAborted()
}

/** Camera axes for a roll-free first-person view: right stays level, up tilts with pitch. */
function viewAxes(yaw: number, pitch: number): { right: Point3; up: Point3; forward: Point3 } {
  const forward = lookDirection(yaw, pitch)
  const level = lookDirection(yaw, 0)
  const right = { x: -level.z, y: 0, z: level.x }
  return { right, up: cross(right, forward), forward }
}

/**
 * Rectangular frustum test in camera space, replacing the cone's single dot-product threshold.
 * Comparing against z · tan(half) keeps it to two multiplies instead of two arctangents.
 */
function insideFrustum(
  axes: { right: Point3; up: Point3; forward: Point3 },
  delta: Point3,
  frustum: ViewFrustum,
): boolean {
  const depth = dot(delta, axes.forward)
  if (depth <= 0) return false
  return Math.abs(dot(delta, axes.right)) <= depth * Math.tan(frustum.horizontalHalfAngle)
    && Math.abs(dot(delta, axes.up)) <= depth * Math.tan(frustum.verticalHalfAngle)
}

function cross(a: Point3, b: Point3): Point3 {
  return { x: a.y * b.z - a.z * b.y, y: a.z * b.x - a.x * b.z, z: a.x * b.y - a.y * b.x }
}

export function raycastLookedAtBlock(port: PerceptionPort, maxDistance: number): LookedAtBlock | null {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const hit = firstHit(port, eye, lookDirection(pose.yaw, pose.pitch), maxDistance, 'visible')
  return hit === null || hit === 'unloaded' ? null : { name: hit.name, position: hit.voxel }
}

/**
 * Why a remembered voxel did not appear in a scan. Only `absent` means the block is actually gone.
 * `occluded` covers both "something is in front of it" and "it has been walled in" — a buried block
 * is still there, so it must not be reported as a removal. `present` means it is there and visible
 * and the scan simply did not list it, which is a disagreement to stay quiet about.
 */
export type VoxelVerdict = 'absent' | 'present' | 'out_of_view' | 'occluded' | 'unloaded'

/**
 * Judges a single remembered voxel against the current stance.
 *
 * This is what keeps an incremental read honest. A block missing from a scan has at least five
 * possible explanations, and only one of them means it is gone: turning the head puts it outside
 * the frustum, walking can put a wall in front of it, distance and the output cap can push it past
 * what the scan verified, and an unloaded chunk verifies nothing at all. Reporting every absence as
 * a removal would tell the model a wall vanished every time the companion looked away.
 *
 * The frustum and distance tests are pure arithmetic, so the common case — a head turn — is settled
 * without touching the world. Only a voxel that is genuinely in view yet missing costs a ray.
 */
export function classifyVoxel(
  port: PerceptionPort,
  options: VisibleBlocksOptions & { verifiedDistance: number },
  voxel: Point3,
  budget?: ScanBudget,
): VoxelVerdict {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const axes = viewAxes(pose.yaw, pose.pitch)
  const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const delta = subtract(center, eye)
  const distance = length(delta)
  // `verifiedDistance` is how far this scan actually looked: with a truncated result that is the
  // last kept block's distance, not maxDistance. Without it, every step would churn the cap's edge.
  if (distance > Math.min(options.maxDistance, options.verifiedDistance)) return 'out_of_view'
  if (distance > 0 && !insideFrustum(axes, delta, options.frustum)) return 'out_of_view'
  const block = port.blockAt(voxel)
  if (block === 'unloaded') return 'unloaded'
  // The only removal there is: nothing occupies the voxel any more.
  if (!block.visible) return 'absent'
  // Still there, just not seen this time — walled in, or something moved in front of it. Both mean
  // "not verified empty", and both must stay quiet. Reporting a buried block as gone would tell the
  // model a wall vanished when the player merely built around it.
  if (!isVisibleCandidate(port, eye, voxel, distance, options.predicate, budget)) return 'occluded'
  return 'present'
}

export async function visibleBlocks(
  port: PerceptionPort,
  options: VisibleBlocksOptions,
  signal?: AbortSignal,
): Promise<{ blocks: VisibleBlock[]; truncated: boolean; verifiedDistance: number }> {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const axes = viewAxes(pose.yaw, pose.pitch)
  const selfVoxel = { x: Math.floor(pose.position.x), y: Math.floor(pose.position.y), z: Math.floor(pose.position.z) }
  const candidates: VisibleBlock[] = []
  const budget: ScanBudget = { work: 0 }
  signal?.throwIfAborted()

  for (let dx = -options.horizontalRadius; dx <= options.horizontalRadius; dx++) {
    for (let dz = -options.horizontalRadius; dz <= options.horizontalRadius; dz++) {
      for (let dy = -options.verticalRadius; dy <= options.verticalRadius; dy++) {
        if (budget.work >= YIELD_EVERY_WORK_UNITS) await spendBudget(budget, signal)
        budget.work++
        const voxel = { x: selfVoxel.x + dx, y: selfVoxel.y + dy, z: selfVoxel.z + dz }
        const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
        const delta = subtract(center, eye)
        const distance = length(delta)
        if (distance > options.maxDistance) continue
        if (distance > 0 && !insideFrustum(axes, delta, options.frustum)) continue
        const block = port.blockAt(voxel)
        if (block === 'unloaded' || !block.visible) continue
        if (!isVisibleCandidate(port, eye, voxel, distance, options.predicate, budget)) continue
        candidates.push({ name: block.name, position: voxel, distance })
      }
    }
  }
  signal?.throwIfAborted()
  candidates.sort((left, right) => left.distance - right.distance)
  const blocks = candidates.slice(0, options.limit)
  // How far the scan can be trusted. Nearest-first means a truncated result is "everything out to
  // this radius" rather than an arbitrary sample, which is the only reason a cap can be diffed at
  // all: a voxel beyond it was never examined, so its absence says nothing.
  const truncated = candidates.length > options.limit
  const verifiedDistance = truncated && blocks.length > 0
    ? blocks[blocks.length - 1]!.distance
    : options.maxDistance
  return { blocks, truncated, verifiedDistance }
}

/**
 * Cancellable because the cost is driven by how many entities the world is tracking, not by
 * `limit`: the cap applies after filtering, and every *occluded* candidate pays for all of its
 * hitbox samples, which is the normal case in a forest or beside a pen. A synchronous version
 * cannot be interrupted at all — not even the provider's own read deadline can fire, since the
 * timer has no chance to run until the scan returns.
 */
export async function visibleEntities(
  port: PerceptionPort,
  maxDistance: number,
  frustum: ViewFrustum,
  limit: number,
  signal?: AbortSignal,
): Promise<VisibleEntity[]> {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const axes = viewAxes(pose.yaw, pose.pitch)
  const budget: ScanBudget = { work: 0 }
  const candidates: VisibleEntity[] = []
  signal?.throwIfAborted()
  for (const entity of port.nearbyEntities()) {
    // Counted even when rejected below, so a world tracking thousands of entities still yields.
    if (budget.work >= YIELD_EVERY_WORK_UNITS) await spendBudget(budget, signal)
    budget.work++
    const width = Math.max(entity.width ?? 0.6, 0.01)
    const height = Math.max(entity.height ?? 1.8, 0.01)
    const halfWidth = width / 2
    const bounds: AxisAlignedBox = {
      min: { x: entity.position.x - halfWidth, y: entity.position.y, z: entity.position.z - halfWidth },
      max: { x: entity.position.x + halfWidth, y: entity.position.y + height, z: entity.position.z + halfWidth },
    }
    const center = { x: entity.position.x, y: entity.position.y + height / 2, z: entity.position.z }
    const distance = length(subtract(center, eye))
    if (distanceToBox(eye, bounds) > maxDistance || !boxIntersectsFrustum(axes, eye, bounds, frustum)) continue
    let visible = pointInsideBox(eye, bounds)
    for (const point of visible ? [] : boxVisibilitySamples(bounds)) {
      if (budget.work >= YIELD_EVERY_WORK_UNITS) await spendBudget(budget, signal)
      const pointDelta = subtract(point, eye)
      if (insideFrustum(axes, pointDelta, frustum) && lineIsClear(port, eye, point, budget)) {
        visible = true
        break
      }
    }
    if (!visible) continue
    candidates.push({
      type: entity.type,
      ...(entity.name ? { name: entity.name } : {}),
      ...(entity.username ? { username: entity.username } : {}),
      position: entity.position,
      distance,
    })
  }
  signal?.throwIfAborted()
  candidates.sort((left, right) => left.distance - right.distance)
  return candidates.slice(0, limit)
}

/**
 * Conservative AABB/frustum rejection. An entity is outside only when all eight hitbox corners
 * lie beyond the same frustum plane; checking whether any one corner is inside would miss a
 * frustum passing through the middle of a close or large hitbox.
 */
function boxIntersectsFrustum(
  axes: { right: Point3; up: Point3; forward: Point3 },
  eye: Point3,
  bounds: AxisAlignedBox,
  frustum: ViewFrustum,
): boolean {
  const tanHorizontal = Math.tan(frustum.horizontalHalfAngle)
  const tanVertical = Math.tan(frustum.verticalHalfAngle)
  const corners = boxCorners(bounds).map(point => {
    const delta = subtract(point, eye)
    return { depth: dot(delta, axes.forward), right: dot(delta, axes.right), up: dot(delta, axes.up) }
  })
  return ![
    (point: (typeof corners)[number]) => point.depth <= 0,
    (point: (typeof corners)[number]) => point.right < -point.depth * tanHorizontal,
    (point: (typeof corners)[number]) => point.right > point.depth * tanHorizontal,
    (point: (typeof corners)[number]) => point.up < -point.depth * tanVertical,
    (point: (typeof corners)[number]) => point.up > point.depth * tanVertical,
  ].some(outsidePlane => corners.every(outsidePlane))
}

function boxCorners(bounds: AxisAlignedBox): Point3[] {
  return [bounds.min.x, bounds.max.x].flatMap(x =>
    [bounds.min.y, bounds.max.y].flatMap(y =>
      [bounds.min.z, bounds.max.z].map(z => ({ x, y, z }))))
}

/** Inset samples avoid treating a ray grazing the ground or an adjacent block as occluded. */
function boxVisibilitySamples(bounds: AxisAlignedBox): Point3[] {
  const axisSamples = (minimum: number, maximum: number, fractions: readonly number[]) =>
    fractions.map(fraction => minimum + (maximum - minimum) * fraction)
  const xs = axisSamples(bounds.min.x, bounds.max.x, [0.05, 0.5, 0.95])
  const ys = axisSamples(bounds.min.y, bounds.max.y, [0.15, 0.5, 0.85])
  const zs = axisSamples(bounds.min.z, bounds.max.z, [0.05, 0.5, 0.95])
  return xs.flatMap(x => ys.flatMap(y => zs.map(z => ({ x, y, z }))))
}

function pointInsideBox(point: Point3, bounds: AxisAlignedBox): boolean {
  return point.x >= bounds.min.x && point.x <= bounds.max.x
    && point.y >= bounds.min.y && point.y <= bounds.max.y
    && point.z >= bounds.min.z && point.z <= bounds.max.z
}

function distanceToBox(point: Point3, bounds: AxisAlignedBox): number {
  const dx = Math.max(bounds.min.x - point.x, 0, point.x - bounds.max.x)
  const dy = Math.max(bounds.min.y - point.y, 0, point.y - bounds.max.y)
  const dz = Math.max(bounds.min.z - point.z, 0, point.z - bounds.max.z)
  return Math.hypot(dx, dy, dz)
}

export function standingOnBlock(port: PerceptionPort): { name: string; position: Point3 } | null {
  const pose = port.selfPose()
  const position = { x: Math.floor(pose.position.x), y: Math.floor(pose.position.y) - 1, z: Math.floor(pose.position.z) }
  const block = port.blockAt(position)
  return block === 'unloaded' || !block.visible ? null : { name: block.name, position }
}

/** Quantized body-local [right, up, forward]; never a world-coordinate channel. */
export function viewRelativePosition(pose: PerceptionPose, position: Point3, quantum = 0.5): [number, number, number] {
  const offset = subtract(position, pose.position)
  const forward = lookDirection(pose.yaw, 0)
  const right = { x: -forward.z, z: forward.x }
  return [
    roundTo(offset.x * right.x + offset.z * right.z, quantum),
    roundTo(offset.y, quantum),
    roundTo(offset.x * forward.x + offset.z * forward.z, quantum),
  ]
}

function firstHit(
  port: PerceptionPort,
  origin: Point3,
  direction: Point3,
  maxDistance: number,
  property: 'visible' | 'occludes',
  budget?: ScanBudget,
): RayHit | 'unloaded' | null {
  for (let distance = STEP; distance <= maxDistance; distance += STEP) {
    if (budget) budget.work++
    const voxel = {
      x: Math.floor(origin.x + direction.x * distance),
      y: Math.floor(origin.y + direction.y * distance),
      z: Math.floor(origin.z + direction.z * distance),
    }
    const block = port.blockAt(voxel)
    if (block === 'unloaded') return 'unloaded'
    if (block[property]) return { voxel, name: block.name }
  }
  return null
}

/** Dispatches the two candidate-based predicates. Both see the same candidate, so they are comparable. */
function isVisibleCandidate(
  port: PerceptionPort,
  eye: Point3,
  voxel: Point3,
  distance: number,
  predicate: VisibilityPredicate | undefined,
  budget?: ScanBudget,
): boolean {
  if (predicate === 'block_centre') {
    return hasExposedFace(port, voxel, budget) && lineReachesVoxel(port, eye, voxel, distance, budget)
  }
  return exposedFaceReachesEye(port, eye, voxel, budget)
}

const FACE_NORMALS: readonly Point3[] = [
  { x: 1, y: 0, z: 0 }, { x: -1, y: 0, z: 0 },
  { x: 0, y: 1, z: 0 }, { x: 0, y: -1, z: 0 },
  { x: 0, y: 0, z: 1 }, { x: 0, y: 0, z: -1 },
]
/** Nudges the aim past the face plane so the target sits in the neighbouring air, not on the boundary. */
const FACE_EPSILON = 0.01

/**
 * Is any face of this block both uncovered and reachable from the eye?
 *
 * This replaces "can a ray reach the block's centre", which asks about a point buried inside the
 * block rather than about anything a player can see. Aiming at a face centre instead means the
 * segment terminates on the surface, so it never has to penetrate half a block of solid material to
 * count as a hit.
 *
 * Only faces the eye is on the outer side of are considered — at most three of six — and they are
 * tried squarest-first, because the face a viewer is most nearly perpendicular to is the one most
 * likely unobstructed. Flat ground seen from above therefore succeeds on its first cast, where the
 * centre test spends a full-length ray and then fails.
 *
 * Known approximation: `lineIsClear` stops one step short of the target, so an occluder inside that
 * final step is not seen. The voxel holding the target has already been confirmed non-occluding,
 * which covers the ordinary case but not a partially-filled neighbour.
 */
function exposedFaceReachesEye(port: PerceptionPort, eye: Point3, voxel: Point3, budget?: ScanBudget): boolean {
  const centre = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const candidates: Array<{ target: Point3; squareness: number }> = []
  for (const normal of FACE_NORMALS) {
    const face = { x: centre.x + normal.x / 2, y: centre.y + normal.y / 2, z: centre.z + normal.z / 2 }
    const toEye = subtract(eye, face)
    const reach = length(toEye)
    // The eye is on the face itself: nothing can be between them.
    if (reach === 0) return true
    const squareness = dot(normal, toEye) / reach
    if (squareness <= 0) continue
    if (budget) budget.work++
    const neighbour = port.blockAt({ x: voxel.x + normal.x, y: voxel.y + normal.y, z: voxel.z + normal.z })
    if (neighbour === 'unloaded' || neighbour.occludes) continue
    candidates.push({
      target: {
        x: face.x + normal.x * FACE_EPSILON, y: face.y + normal.y * FACE_EPSILON, z: face.z + normal.z * FACE_EPSILON,
      },
      squareness,
    })
  }
  candidates.sort((left, right) => right.squareness - left.squareness)
  return candidates.some(candidate => lineIsClear(port, eye, candidate.target, budget))
}

function hasExposedFace(port: PerceptionPort, voxel: Point3, budget?: ScanBudget): boolean {
  return [
    [1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1],
  ].some(([x, y, z]) => {
    if (budget) budget.work++
    const block = port.blockAt({ x: voxel.x + x!, y: voxel.y + y!, z: voxel.z + z! })
    return block !== 'unloaded' && !block.occludes
  })
}

function lineReachesVoxel(port: PerceptionPort, eye: Point3, voxel: Point3, distance: number, budget?: ScanBudget): boolean {
  if (distance === 0) return true
  const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const hit = firstHit(port, eye, normalize(subtract(center, eye), distance), distance + STEP, 'occludes', budget)
  return hit === null || (hit !== 'unloaded' && sameVoxel(hit.voxel, voxel))
}

function lineIsClear(port: PerceptionPort, origin: Point3, target: Point3, budget?: ScanBudget): boolean {
  const delta = subtract(target, origin)
  const distance = length(delta)
  if (distance === 0) return true
  return firstHit(port, origin, normalize(delta, distance), Math.max(0, distance - STEP), 'occludes', budget) === null
}
function subtract(left: Point3, right: Point3): Point3 { return { x: left.x - right.x, y: left.y - right.y, z: left.z - right.z } }
function dot(left: Point3, right: Point3): number { return left.x * right.x + left.y * right.y + left.z * right.z }
function length(value: Point3): number { return Math.hypot(value.x, value.y, value.z) }
function normalize(value: Point3, magnitude: number): Point3 { return { x: value.x / magnitude, y: value.y / magnitude, z: value.z / magnitude } }
function sameVoxel(left: Point3, right: Point3): boolean { return left.x === right.x && left.y === right.y && left.z === right.z }
function roundTo(value: number, quantum: number): number {
  if (!Number.isFinite(quantum) || quantum <= 0) throw new RangeError('Relative-coordinate quantum must be positive')
  const result = Math.round(value / quantum) * quantum
  return Object.is(result, -0) ? 0 : result
}
