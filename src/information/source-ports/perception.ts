import { setImmediate as yieldToEventLoop } from 'node:timers/promises'
import { lookDirection, type Point3 } from '../geometry.js'

export interface PerceptionPose {
  position: Point3
  yaw: number
  pitch: number
}

export interface PerceptionBlock {
  name: string
  /** The voxel has something a player can see, including transparent and non-colliding blocks. */
  visible: boolean
  /** The voxel blocks a visibility ray. Transparent blocks remain visible without occluding behind them. */
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
  /** Returns 'unloaded' when the position is outside the currently loaded/known world. */
  blockAt(position: Point3): PerceptionBlock | 'unloaded'
  /** Protocol-tracked entities excluding self; a candidate list, not proof of line of sight. */
  nearbyEntities(): readonly PerceptionEntityCandidate[]
}

export interface LookedAtBlock {
  name: string
  position: Point3
}

export interface VisibleEntity {
  type: string
  name?: string
  username?: string
  position: Point3
  distance: number
}

export interface VisibleBlock { name: string; position: Point3; distance: number }

/**
 * Bounded like the block result, and for the same reason: a caller cannot tell "eight entities are
 * nearby" from "the nearest eight of many" by looking at a list of eight.
 */
export interface VisibleEntitiesResult { entities: VisibleEntity[]; truncated: boolean }

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

/** The old centre-ray predicate is retained only as a measurable comparison baseline. */
export type VisibilityPredicate = 'block_centre' | 'exposed_face'

export interface VisibleBlocksOptions {
  horizontalRadius: number
  verticalRadius: number
  maxDistance: number
  frustum: ViewFrustum
  limit: number
  /** Defaults to the legacy centre ray; production opts into `exposed_face` explicitly. */
  predicate?: VisibilityPredicate
  /** Filled in when supplied so section culling can be verified rather than merely assumed. */
  metrics?: ScanMetrics
}

export interface ScanMetrics {
  sectionsTested: number
  sectionsSkipped: number
  voxelsExamined: number
}

export const EYE_HEIGHT = 1.62
const SECTION_SIZE = 16
const STEP = 0.25
/**
 * Work units between yields. Exported because it is the unit of cancellation latency: after a
 * signal aborts, a scan may do up to one more quantum before it stops, and the tests assert
 * against that bound rather than a magic number.
 */
export const YIELD_EVERY_WORK_UNITS = 2_048

interface RayHit { voxel: Point3; name: string; distance: number }
interface AxisAlignedBox { min: Point3; max: Point3 }
/**
 * Work counter shared by every scan loop and every raycast they fire. Counting only the outer
 * loop understates the real cost by orders of magnitude: each candidate voxel and each entity
 * hitbox sample casts a ray whose steps used to be invisible to the yield budget, so a dense
 * scene could run thousands of blockAt calls between two yields. Rays increment the same counter
 * and callers yield *between* rays, which bounds uninterrupted work at one raycast.
 */
interface ScanBudget { work: number }
interface ViewAxes { right: Point3; up: Point3; forward: Point3 }

const sectionOf = (value: number): number => Math.floor(value / SECTION_SIZE)

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

/** Camera axes for a roll-free first-person view: right stays level while up tilts with pitch. */
function viewAxes(yaw: number, pitch: number): ViewAxes {
  const forward = lookDirection(yaw, pitch)
  const level = lookDirection(yaw, 0)
  const right = { x: -level.z, y: 0, z: level.x }
  return { right, up: cross(right, forward), forward }
}

/**
 * Rectangular frustum test in camera space, replacing the cone's single dot-product threshold.
 * Comparing against z · tan(half) keeps it to two multiplies instead of two arctangents.
 */
function insideFrustum(axes: ViewAxes, delta: Point3, frustum: ViewFrustum): boolean {
  const depth = dot(delta, axes.forward)
  if (depth <= 0) return false
  return Math.abs(dot(delta, axes.right)) <= depth * Math.tan(frustum.horizontalHalfAngle)
    && Math.abs(dot(delta, axes.up)) <= depth * Math.tan(frustum.verticalHalfAngle)
}

function cross(a: Point3, b: Point3): Point3 {
  return { x: a.y * b.z - a.z * b.y, y: a.z * b.x - a.x * b.z, z: a.x * b.y - a.y * b.x }
}

/**
 * A single ray along the exact current yaw/pitch, matching vanilla's targeted-block mechanic.
 * It stops at the first visible voxel, so glass and other non-occluding blocks can still be looked at.
 */
export function raycastLookedAtBlock(port: PerceptionPort, maxDistance: number): LookedAtBlock | null {
  const pose = port.selfPose()
  const eye: Point3 = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const hit = firstHit(port, eye, lookDirection(pose.yaw, pose.pitch), maxDistance, 'visible')
  return hit === 'unloaded' || hit === null ? null : { name: hit.name, position: hit.voxel }
}

/**
 * Enumerates loaded visible voxels inside the rectangular view frustum, then asks whether a surface
 * of each candidate can actually be reached from the eye. The scan yields under a shared work budget
 * so deadlines, disconnects and scope invalidation can cancel it even in dense scenes.
 */
export async function visibleBlocks(
  port: PerceptionPort,
  options: VisibleBlocksOptions,
  signal?: AbortSignal,
): Promise<{ blocks: VisibleBlock[]; truncated: boolean }> {
  const pose = port.selfPose()
  const eye: Point3 = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const axes = viewAxes(pose.yaw, pose.pitch)
  const selfVoxel: Point3 = {
    x: Math.floor(pose.position.x), y: Math.floor(pose.position.y), z: Math.floor(pose.position.z),
  }
  const candidates: VisibleBlock[] = []
  const budget: ScanBudget = { work: 0 }
  signal?.throwIfAborted()

  // A forward frustum occupies only a fraction of the cube-shaped candidate range. Reject whole
  // Minecraft sections first; the AABB tests are conservative, so any section containing an
  // in-range voxel centre survives and observable results remain identical to per-voxel culling.
  const lowest = {
    x: selfVoxel.x - options.horizontalRadius,
    y: selfVoxel.y - options.verticalRadius,
    z: selfVoxel.z - options.horizontalRadius,
  }
  const highest = {
    x: selfVoxel.x + options.horizontalRadius,
    y: selfVoxel.y + options.verticalRadius,
    z: selfVoxel.z + options.horizontalRadius,
  }

  for (let sx = sectionOf(lowest.x); sx <= sectionOf(highest.x); sx++) {
    for (let sz = sectionOf(lowest.z); sz <= sectionOf(highest.z); sz++) {
      for (let sy = sectionOf(lowest.y); sy <= sectionOf(highest.y); sy++) {
        const bounds: AxisAlignedBox = {
          min: { x: sx * SECTION_SIZE, y: sy * SECTION_SIZE, z: sz * SECTION_SIZE },
          max: {
            x: sx * SECTION_SIZE + SECTION_SIZE,
            y: sy * SECTION_SIZE + SECTION_SIZE,
            z: sz * SECTION_SIZE + SECTION_SIZE,
          },
        }
        if (options.metrics) options.metrics.sectionsTested++
        if (distanceToBox(eye, bounds) > options.maxDistance
          || !boxIntersectsFrustum(axes, eye, bounds, options.frustum)) {
          if (options.metrics) options.metrics.sectionsSkipped++
          continue
        }

        for (let x = Math.max(bounds.min.x, lowest.x); x <= Math.min(bounds.max.x - 1, highest.x); x++) {
          for (let z = Math.max(bounds.min.z, lowest.z); z <= Math.min(bounds.max.z - 1, highest.z); z++) {
            for (let y = Math.max(bounds.min.y, lowest.y); y <= Math.min(bounds.max.y - 1, highest.y); y++) {
              if (budget.work >= YIELD_EVERY_WORK_UNITS) await spendBudget(budget, signal)
              budget.work++
              if (options.metrics) options.metrics.voxelsExamined++

              const voxel = { x, y, z }
              const center = { x: x + 0.5, y: y + 0.5, z: z + 0.5 }
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
      }
    }
  }

  signal?.throwIfAborted()
  candidates.sort((left, right) => left.distance - right.distance)
  return {
    blocks: candidates.slice(0, options.limit),
    truncated: candidates.length > options.limit,
  }
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
): Promise<VisibleEntitiesResult> {
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
  // Nearest-first before slicing, so what the cap drops is always the farthest.
  return { entities: candidates.slice(0, limit), truncated: candidates.length > limit }
}

function isVisibleCandidate(
  port: PerceptionPort,
  eye: Point3,
  voxel: Point3,
  distance: number,
  predicate: VisibilityPredicate | undefined,
  budget: ScanBudget,
): boolean {
  if (predicate === 'exposed_face') return exposedFaceReachesEye(port, eye, voxel, budget)
  return hasExposedFace(port, voxel, budget) && lineReachesVoxel(port, eye, voxel, distance, budget)
}

const FACE_NORMALS: readonly Point3[] = [
  { x: 1, y: 0, z: 0 }, { x: -1, y: 0, z: 0 },
  { x: 0, y: 1, z: 0 }, { x: 0, y: -1, z: 0 },
  { x: 0, y: 0, z: 1 }, { x: 0, y: 0, z: -1 },
]
/** Puts the target just outside the face instead of ambiguously on a voxel boundary. */
const FACE_EPSILON = 0.01

/**
 * A block is visible when any face toward the eye is both uncovered and reachable. A centre ray is
 * the wrong predicate: on flat ground it aims half a block below the surface and hits nearer ground.
 * At most three faces point toward the eye, and trying the squarest one first keeps the common case
 * to one ray.
 *
 * `squareness > 0` is a strict test, and the boundary case it excludes is excluded correctly rather
 * than conservatively. A dot product of exactly zero means the eye lies in that face's own plane, so
 * the face is edge-on: its projected area is zero and there is nothing to see. The all-transparent
 * comparison fixture reaches this with an eye at (0, 65.62, 0), which sits on the shared boundary of
 * (-1,65,-1) and (0,65,-1) — every outward normal there is either negative or exactly zero, so both
 * voxels are omitted. The centre-ray predicate counts them, which is why the two predicates differ
 * by two voxels in that fixture; counting a face you are coplanar with is the error, not omitting it.
 *
 * The same geometry bites test fixtures: a wall built in the eye's own plane is edge-on and correctly
 * invisible, so an occlusion fixture placed there passes while testing nothing.
 */
function exposedFaceReachesEye(port: PerceptionPort, eye: Point3, voxel: Point3, budget: ScanBudget): boolean {
  const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const candidates: Array<{ target: Point3; squareness: number }> = []

  for (const normal of FACE_NORMALS) {
    const face = {
      x: center.x + normal.x / 2,
      y: center.y + normal.y / 2,
      z: center.z + normal.z / 2,
    }
    const toEye = subtract(eye, face)
    const reach = length(toEye)
    if (reach === 0) return true
    const squareness = dot(normal, toEye) / reach
    if (squareness <= 0) continue

    budget.work++
    const neighbor = port.blockAt({
      x: voxel.x + normal.x,
      y: voxel.y + normal.y,
      z: voxel.z + normal.z,
    })
    if (neighbor === 'unloaded' || neighbor.occludes) continue
    candidates.push({
      target: {
        x: face.x + normal.x * FACE_EPSILON,
        y: face.y + normal.y * FACE_EPSILON,
        z: face.z + normal.z * FACE_EPSILON,
      },
      squareness,
    })
  }

  candidates.sort((left, right) => right.squareness - left.squareness)
  return candidates.some(candidate => lineIsClear(port, eye, candidate.target, budget))
}

function hasExposedFace(port: PerceptionPort, voxel: Point3, budget: ScanBudget): boolean {
  return FACE_NORMALS.some(normal => {
    budget.work++
    const neighbor = port.blockAt({
      x: voxel.x + normal.x,
      y: voxel.y + normal.y,
      z: voxel.z + normal.z,
    })
    return neighbor !== 'unloaded' && !neighbor.occludes
  })
}

function lineReachesVoxel(
  port: PerceptionPort,
  eye: Point3,
  voxel: Point3,
  distance: number,
  budget: ScanBudget,
): boolean {
  if (distance === 0) return true
  const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const hit = firstHit(port, eye, normalize(subtract(center, eye), distance), distance + STEP, 'occludes', budget)
  return hit === null || (hit !== 'unloaded' && sameVoxel(hit.voxel, voxel))
}

function lineIsClear(port: PerceptionPort, origin: Point3, target: Point3, budget: ScanBudget): boolean {
  const delta = subtract(target, origin)
  const distance = length(delta)
  if (distance === 0) return true
  return firstHit(port, origin, normalize(delta, distance), Math.max(0, distance - STEP), 'occludes', budget) === null
}

function firstHit(
  port: PerceptionPort,
  origin: Point3,
  direction: Point3,
  maxDistance: number,
  property: 'visible' | 'occludes',
  budget?: ScanBudget,
): RayHit | 'unloaded' | null {
  const steps = Math.floor(maxDistance / STEP)
  for (let step = 1; step <= steps; step++) {
    const distance = step * STEP
    if (budget) budget.work++
    const voxel = {
      x: Math.floor(origin.x + direction.x * distance),
      y: Math.floor(origin.y + direction.y * distance),
      z: Math.floor(origin.z + direction.z * distance),
    }
    const block = port.blockAt(voxel)
    if (block === 'unloaded') return 'unloaded'
    if (block[property]) return { voxel, name: block.name, distance }
  }
  return null
}

/**
 * Conservative AABB/frustum rejection. A box is outside only when all eight corners lie beyond
 * the same frustum plane; checking whether any one corner is inside would miss a frustum passing
 * through the middle of a close or large box. The same conservative test serves entity hitboxes
 * and the section-level visible-block prefilter.
 */
function boxIntersectsFrustum(axes: ViewAxes, eye: Point3, bounds: AxisAlignedBox, frustum: ViewFrustum): boolean {
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
  const position = {
    x: Math.floor(pose.position.x), y: Math.floor(pose.position.y) - 1, z: Math.floor(pose.position.z),
  }
  const block = port.blockAt(position)
  return block === 'unloaded' || !block.visible ? null : { name: block.name, position }
}

/** Quantized body-local [right, up, forward]; never a world-coordinate channel. */
export function viewRelativePosition(
  pose: PerceptionPose,
  position: Point3,
  quantum = 0.5,
): [number, number, number] {
  const offset = subtract(position, pose.position)
  const forward = lookDirection(pose.yaw, 0)
  const right = { x: -forward.z, z: forward.x }
  return [
    roundTo(offset.x * right.x + offset.z * right.z, quantum),
    roundTo(offset.y, quantum),
    roundTo(offset.x * forward.x + offset.z * forward.z, quantum),
  ]
}

function subtract(left: Point3, right: Point3): Point3 {
  return { x: left.x - right.x, y: left.y - right.y, z: left.z - right.z }
}
function dot(left: Point3, right: Point3): number { return left.x * right.x + left.y * right.y + left.z * right.z }
function length(value: Point3): number { return Math.hypot(value.x, value.y, value.z) }
function normalize(value: Point3, magnitude: number): Point3 {
  return { x: value.x / magnitude, y: value.y / magnitude, z: value.z / magnitude }
}
function sameVoxel(left: Point3, right: Point3): boolean {
  return left.x === right.x && left.y === right.y && left.z === right.z
}
function roundTo(value: number, quantum: number): number {
  if (!Number.isFinite(quantum) || quantum <= 0) throw new RangeError('Relative-coordinate quantum must be positive')
  const result = Math.round(value / quantum) * quantum
  return Object.is(result, -0) ? 0 : result
}
