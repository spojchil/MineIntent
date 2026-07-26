import type { RememberedBlocks, VisibleBlocksResult } from './source-ports/perception.js'
import type { Point3 } from './geometry.js'

export type BlockTuple = [string, number, number, number]

export interface ViewportDiff {
  added: BlockTuple[]
  removed: BlockTuple[]
  /**
   * Remembered blocks nearby that this look did not confirm — behind the companion, occluded, in an
   * unloaded chunk, or seen but past the output budget. They stay remembered and nothing is said
   * about them.
   *
   * "Not confirmed" is deliberately from the model's side rather than the scan's: a block the scan
   * saw but could not fit under the cap was, as far as the model is concerned, not confirmed either.
   */
  unverified: number
}

const SECTION_SIZE = 16

/**
 * What the companion has already been told about blocks, in the form that makes an incremental read
 * possible.
 *
 * The mirror is not a second source of truth about the world. It records what has been *said* in this
 * conversation, so it is reset whenever that conversation restarts and voided whenever world scope
 * changes. If it ever outlived the messages it mirrors it would start suppressing `added` for blocks
 * the model no longer knows about — the same class of bug as holding a target outside the context and
 * letting the two drift apart.
 *
 * It answers the scan's questions rather than re-asking them afterwards. A scan visits every voxel in
 * the candidate box anyway, and at each one it already knows whether the voxel is in view, what
 * occupies it and whether it is visible, so consulting memory there costs nothing. The previous
 * two-pass design re-derived all of it per remembered voxel — a second frustum test, a second
 * `blockAt`, often a second ray — and needed a cap on those re-checks to stay affordable. Both that
 * cap and the "how far did this scan verify" radius are gone: every voxel inside the frustum now gets
 * a verdict regardless of the output budget, so a removal is exact instead of budget-dependent.
 *
 * Storage is indexed by 16³ section so counting nearby memories walks only what is within reach
 * rather than everything ever seen.
 */
export class ViewportMirror implements RememberedBlocks {
  readonly #sections = new Map<string, Map<string, string>>()

  get size(): number {
    let total = 0
    for (const section of this.#sections.values()) total += section.size
    return total
  }

  clear(): void { this.#sections.clear() }

  recall(voxel: Point3): string | undefined {
    return this.#sections.get(sectionKeyOf(voxel))?.get(voxelKey(voxel))
  }

  /**
   * Folds a completed scan in and reports what to tell the model.
   *
   * Only ever called after a scan finished: a cancelled scan throws, so a partially walked box can
   * never leave the mirror half-updated.
   */
  fold(input: {
    blocks: VisibleBlocksResult['blocks']
    vanished: VisibleBlocksResult['vanished']
    eye: Point3
    reach: number
  }): ViewportDiff {
    const added: BlockTuple[] = []
    const removed: BlockTuple[] = []
    const confirmed = new Set<string>()

    for (const block of input.blocks) {
      const key = voxelKey(block.position)
      confirmed.add(key)
      const known = this.#sections.get(sectionKeyOf(block.position))?.get(key)
      if (known === block.name) continue
      // A replacement is one removal plus one addition, so the model never has to work out that two
      // lines describe the same place.
      if (known !== undefined) removed.push([known, block.position.x, block.position.y, block.position.z])
      added.push([block.name, block.position.x, block.position.y, block.position.z])
    }

    for (const gone of input.vanished) {
      const key = voxelKey(gone.position)
      // A voxel cannot be both occupied and empty in one scan; guarded anyway so a port that
      // contradicts itself cannot produce a removal for something just reported as present.
      if (confirmed.has(key)) continue
      confirmed.add(key)
      removed.push([gone.name, gone.position.x, gone.position.y, gone.position.z])
    }

    let unverified = 0
    for (const [sectionKey, section] of this.#sections) {
      if (!sectionWithinReach(sectionKey, input.eye, input.reach)) continue
      for (const key of section.keys()) if (!confirmed.has(key)) unverified += 1
    }

    for (const [, x, y, z] of removed) this.#forget({ x, y, z })
    for (const block of input.blocks) this.#remember(block.position, block.name)
    return { added, removed, unverified }
  }

  #remember(voxel: Point3, name: string): void {
    const sectionKey = sectionKeyOf(voxel)
    const section = this.#sections.get(sectionKey)
    if (section === undefined) this.#sections.set(sectionKey, new Map([[voxelKey(voxel), name]]))
    else section.set(voxelKey(voxel), name)
  }

  #forget(voxel: Point3): void {
    const sectionKey = sectionKeyOf(voxel)
    const section = this.#sections.get(sectionKey)
    if (section === undefined) return
    section.delete(voxelKey(voxel))
    if (section.size === 0) this.#sections.delete(sectionKey)
  }
}

function voxelKey(voxel: Point3): string { return `${voxel.x},${voxel.y},${voxel.z}` }

function sectionKeyOf(voxel: Point3): string {
  return `${Math.floor(voxel.x / SECTION_SIZE)},${Math.floor(voxel.y / SECTION_SIZE)},${Math.floor(voxel.z / SECTION_SIZE)}`
}

/**
 * Distance from the eye to the section's box, against the scan radius. A sphere test on purpose: this
 * only bounds which memories are worth counting, and the scan has already applied the exact frustum
 * per voxel. Duplicating the frustum here would create a second place that has to agree about what
 * "in view" means.
 */
function sectionWithinReach(sectionKey: string, eye: Point3, reach: number): boolean {
  const [sx, sy, sz] = sectionKey.split(',').map(Number) as [number, number, number]
  const min = { x: sx * SECTION_SIZE, y: sy * SECTION_SIZE, z: sz * SECTION_SIZE }
  const dx = Math.max(min.x - eye.x, 0, eye.x - (min.x + SECTION_SIZE))
  const dy = Math.max(min.y - eye.y, 0, eye.y - (min.y + SECTION_SIZE))
  const dz = Math.max(min.z - eye.z, 0, eye.z - (min.z + SECTION_SIZE))
  return Math.hypot(dx, dy, dz) <= reach
}
