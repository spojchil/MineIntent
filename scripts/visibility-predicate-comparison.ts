/**
 * Compares the candidate-based visibility predicates on the synthetic fixtures from the scan
 * experiment, so the choice in issue #82 rests on measurement that can be re-derived rather than on
 * numbers quoted from a document.
 *
 * Correctness is the primary axis. For the flat-ground fixture the correct answer is available in
 * closed form — the eye sits above a solid half-space, so a face is visible exactly when its centre
 * is inside the frustum and within range, and no ray descending to the surface plane can dip below
 * it. That oracle is written here independently of the implementation under test.
 *
 * Cost is secondary and reported only to show where it comes from. Per the experiment's own
 * discipline, call counts do not decide anything: end-to-end time on a real backend does.
 *
 *   npx tsx scripts/visibility-predicate-comparison.ts
 */
import { visibleBlocks, type PerceptionBlock, type PerceptionPort, type PerceptionPose, type ScanMetrics, type VisibilityPredicate } from '../src/information/source-ports/perception.js'

const EYE_HEIGHT = 1.62
const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const opaque = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
const glass: PerceptionBlock = { name: 'glass', visible: true, occludes: false }

const SCAN = {
  horizontalRadius: 32,
  verticalRadius: 20,
  maxDistance: 32,
  frustum: {
    verticalHalfAngle: (35 * Math.PI) / 180,
    horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)),
  },
  // Raised far above the production 256 so the cap cannot mask a difference in what each predicate
  // can see. The cap is a budget question, not a correctness one.
  limit: 1_000_000,
}

/** Counts what each predicate actually costs, and how much of it is repeated reads. */
class CountingPort implements PerceptionPort {
  reads = 0
  readonly distinct = new Set<string>()
  constructor(
    readonly pose: PerceptionPose,
    private readonly lookup: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded',
  ) {}
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    this.reads += 1
    this.distinct.add(`${position.x},${position.y},${position.z}`)
    return this.lookup(position)
  }
  nearbyEntities() { return [] }
}

const pose = (pitch: number): PerceptionPose => ({ position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch })

interface Fixture {
  name: string
  pitch: number
  lookup: (p: PerceptionPose['position']) => PerceptionBlock | 'unloaded'
  /** Closed-form expected surface count, where one exists. */
  oracle?: (eye: PerceptionPose['position']) => number
}

/**
 * Every visible top face of the `y = 63` layer, by the argument in the file header.
 *
 * Culling is measured to the **voxel centre**, not the face, because that is what the enumeration
 * does before any visibility test runs. Using the face here instead reports 912 and makes a correct
 * implementation look like it misses 12 — the gap is a disagreement about which point represents a
 * block, not a missed surface.
 *
 * That disagreement is itself a (small, conservative) bias worth knowing about: a block whose face
 * is inside the frustum but whose centre is just outside is culled before its face is ever tested,
 * so the boundary under-reports. Fixing it means culling by the block's box rather than its centre.
 */
function flatGroundOracle(eye: PerceptionPose['position']): number {
  let count = 0
  for (let x = -SCAN.horizontalRadius; x <= SCAN.horizontalRadius; x++) {
    for (let z = -SCAN.horizontalRadius; z <= SCAN.horizontalRadius; z++) {
      const delta = { x: x + 0.5 - eye.x, y: 63.5 - eye.y, z: z + 0.5 - eye.z }
      const depth = -delta.z
      if (depth <= 0) continue
      if (Math.hypot(delta.x, delta.y, delta.z) > SCAN.maxDistance) continue
      if (Math.abs(delta.x) > depth * Math.tan(SCAN.frustum.horizontalHalfAngle)) continue
      if (Math.abs(delta.y) > depth * Math.tan(SCAN.frustum.verticalHalfAngle)) continue
      count += 1
    }
  }
  return count
}

const FIXTURES: Fixture[] = [
  { name: '空气视野', pitch: 0, lookup: () => AIR, oracle: () => 0 },
  { name: '开阔平地（水平）', pitch: 0, lookup: p => (p.y <= 63 ? opaque('grass_block') : AIR), oracle: flatGroundOracle },
  { name: '开阔平地（俯视 -1.2rad）', pitch: -1.2, lookup: p => (p.y <= 63 ? opaque('grass_block') : AIR) },
  { name: '前方 8 格墙', pitch: 0, lookup: p => (p.z === -8 ? opaque('stone') : AIR) },
  { name: '全透明体', pitch: 0, lookup: () => glass },
]

const PREDICATES: VisibilityPredicate[] = ['block_centre', 'exposed_face']  // both stated; neither relies on the default

async function main(): Promise<void> {
  const rows: string[] = []
  for (const fixture of FIXTURES) {
    const eye = { x: 0, y: 64 + EYE_HEIGHT, z: 0 }
    const expected = fixture.oracle?.(eye)
    for (const predicate of PREDICATES) {
      const port = new CountingPort(pose(fixture.pitch), fixture.lookup)
      const metrics: ScanMetrics = { sectionsTested: 0, sectionsSkipped: 0, voxelsExamined: 0 }
      const started = process.hrtime.bigint()
      const result = await visibleBlocks(port, { ...SCAN, predicate, metrics })
      const ms = Number(process.hrtime.bigint() - started) / 1e6
      const recall = expected === undefined ? '—'
        : expected === 0 ? (result.blocks.length === 0 ? '100%' : `假阳 ${result.blocks.length}`)
        : `${((result.blocks.length / expected) * 100).toFixed(1)}%`
      rows.push([
        fixture.name.padEnd(24),
        predicate.padEnd(14),
        String(result.blocks.length).padStart(7),
        (expected === undefined ? '—' : String(expected)).padStart(7),
        recall.padStart(9),
        String(port.reads).padStart(10),
        `${metrics.voxelsExamined}`.padStart(9),
        `${metrics.sectionsSkipped}/${metrics.sectionsTested}`.padStart(8),
        `${ms.toFixed(0)}ms`.padStart(7),
      ].join(''))
    }
  }
  console.log(
    '夹具'.padEnd(22) + '判据'.padEnd(15) + '报告数'.padStart(6) + '真值'.padStart(7)
    + '召回'.padStart(9) + 'blockAt'.padStart(10) + '遍历体素'.padStart(7)
    + 'section剔除'.padStart(6) + '耗时'.padStart(7),
  )
  for (const row of rows) console.log(row)
}

void main()
