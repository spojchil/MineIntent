import { classifyVoxel, type PerceptionPort, type VisibleBlocksOptions } from './source-ports/perception.js'
import type { Point3 } from './geometry.js'

export type BlockTuple = [string, number, number, number]

export interface MirrorObservation {
  blocks: ReadonlyArray<{ name: string; position: Point3 }>
  /** How far this scan actually looked. Beyond it, an absence proves nothing. */
  verifiedDistance: number
}

export interface ViewportDiff {
  added: BlockTuple[]
  removed: BlockTuple[]
  /**
   * Remembered blocks that were nearby but could not be judged this time — out of the frustum,
   * behind something, in an unloaded chunk, or past the re-check budget. They stay remembered.
   */
  unverified: number
}

const SECTION_SIZE = 16
/**
 * Ceiling on rays spent re-checking blocks that went missing while still in view.
 *
 * The common case is free: a head turn puts everything outside the frustum, which is settled by
 * arithmetic. The expensive case is real though — looking down at flat ground and back up leaves
 * hundreds of ground voxels in view yet missing, because a ray to a distant block's *centre* passes
 * through nearer ground before reaching it. Exceeding the budget degrades safely: an unjudged block
 * stays remembered and reports nothing, which is exactly the conservative direction.
 */
const MAX_REVERIFICATIONS = 96

/**
 * What the companion has been told about blocks, in the form that makes an incremental read possible.
 *
 * The mirror is not a second source of truth about the world. It is a record of what has already
 * been said in this conversation, so it is reset whenever that conversation restarts and voided
 * whenever the world scope changes. If it ever outlived the messages it mirrors, it would start
 * suppressing `added` for blocks the model no longer knows about — the same class of bug as holding
 * a target outside the context and letting the two drift apart.
 *
 * Storage is indexed by 16³ section so a scan only reconsiders blocks near the companion. A long
 * session accumulates thousands of remembered voxels across a whole world; each read only cares
 * about the handful of sections within reach.
 */
export class ViewportMirror {
  readonly #sections = new Map<string, Map<string, string>>()

  get size(): number {
    let total = 0
    for (const section of this.#sections.values()) total += section.size
    return total
  }

  clear(): void { this.#sections.clear() }

  /**
   * Folds one observation in and reports what changed. Order matters: nothing is forgotten before
   * the current scan has been consulted, so a block seen again keeps its place silently.
   */
  diff(port: PerceptionPort, options: VisibleBlocksOptions, observation: MirrorObservation): ViewportDiff {
    const seen = new Map<string, string>()
    for (const block of observation.blocks) seen.set(voxelKey(block.position), block.name)

    const added: BlockTuple[] = []
    const removed: BlockTuple[] = []
    let unverified = 0
    let reverifications = 0
    const budget = { work: 0 }
    const verify = { ...options, verifiedDistance: observation.verifiedDistance }
    const eye = port.selfPose().position

    for (const [sectionKey, section] of this.#sections) {
      if (!sectionWithinReach(sectionKey, eye, options.maxDistance)) continue
      for (const [key, name] of section) {
        const current = seen.get(key)
        if (current === name) continue
        if (current !== undefined) {
          // Same voxel, different block: a replacement is one removal plus one addition, so the
          // model never has to infer that two lines are about the same place.
          removed.push(tuple(name, key))
          continue
        }
        if (reverifications >= MAX_REVERIFICATIONS) { unverified += 1; continue }
        reverifications += 1
        // Only a voxel proven empty is reported gone. Every other verdict — out of view, occluded,
        // chunk unloaded, or the scan disagreeing with a fresh look — leaves the block remembered.
        if (classifyVoxel(port, verify, parseKey(key), budget) === 'absent') removed.push(tuple(name, key))
        else unverified += 1
      }
    }

    for (const [key, name] of seen) {
      const section = this.#sections.get(sectionKeyFor(key))
      if (section?.get(key) !== name) added.push(tuple(name, key))
    }

    for (const removal of removed) this.#forget(voxelKey({ x: removal[1], y: removal[2], z: removal[3] }))
    for (const [key, name] of seen) this.#remember(key, name)
    return { added, removed, unverified }
  }

  #remember(key: string, name: string): void {
    const sectionKey = sectionKeyFor(key)
    const section = this.#sections.get(sectionKey)
    if (section === undefined) this.#sections.set(sectionKey, new Map([[key, name]]))
    else section.set(key, name)
  }

  #forget(key: string): void {
    const sectionKey = sectionKeyFor(key)
    const section = this.#sections.get(sectionKey)
    if (section === undefined) return
    section.delete(key)
    if (section.size === 0) this.#sections.delete(sectionKey)
  }
}

function voxelKey(position: Point3): string { return `${position.x},${position.y},${position.z}` }

function parseKey(key: string): Point3 {
  const [x, y, z] = key.split(',').map(Number) as [number, number, number]
  return { x, y, z }
}

function tuple(name: string, key: string): BlockTuple {
  const { x, y, z } = parseKey(key)
  return [name, x, y, z]
}

function sectionKeyFor(key: string): string {
  const { x, y, z } = parseKey(key)
  return `${Math.floor(x / SECTION_SIZE)},${Math.floor(y / SECTION_SIZE)},${Math.floor(z / SECTION_SIZE)}`
}

/**
 * Distance from the eye to the section's box, against the scan radius.
 *
 * A sphere test rather than a frustum test on purpose: this only has to be a cheap bound, and
 * `classifyVoxel` applies the exact frustum per voxel afterwards. Duplicating the frustum here
 * would mean two places that must agree about what "in view" means.
 */
function sectionWithinReach(sectionKey: string, eye: Point3, radius: number): boolean {
  const { x, y, z } = parseKey(sectionKey)
  const min = { x: x * SECTION_SIZE, y: y * SECTION_SIZE, z: z * SECTION_SIZE }
  const dx = Math.max(min.x - eye.x, 0, eye.x - (min.x + SECTION_SIZE))
  const dy = Math.max(min.y - eye.y, 0, eye.y - (min.y + SECTION_SIZE))
  const dz = Math.max(min.z - eye.z, 0, eye.z - (min.z + SECTION_SIZE))
  return Math.hypot(dx, dy, dz) <= radius
}
