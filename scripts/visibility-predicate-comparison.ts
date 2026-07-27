/**
 * Re-runnable comparison of the legacy block-centre ray and the exposed-face predicate.
 * Correctness comes from the flat-ground closed-form oracle; blockAt counts and scan metrics are
 * diagnostic cost evidence only.
 *
 *   pnpm exec tsx scripts/visibility-predicate-comparison.ts
 */
import {
  visibleBlocks,
  type PerceptionBlock,
  type PerceptionPort,
  type PerceptionPose,
  type ScanMetrics,
  type VisibilityPredicate,
} from '../src/information/source-ports/perception.js'

const EYE_HEIGHT = 1.62
const AIR: PerceptionBlock = { name: 'air', visible: false, occludes: false }
const opaque = (name: string): PerceptionBlock => ({ name, visible: true, occludes: true })
const GLASS: PerceptionBlock = { name: 'glass', visible: true, occludes: false }

const SCAN = {
  horizontalRadius: 32,
  verticalRadius: 20,
  maxDistance: 32,
  frustum: {
    verticalHalfAngle: (35 * Math.PI) / 180,
    horizontalHalfAngle: Math.atan(Math.tan((35 * Math.PI) / 180) * (16 / 9)),
  },
  // Keep the cap out of the comparison: it is an output budget, not a visibility predicate.
  limit: 1_000_000,
}

class CountingPort implements PerceptionPort {
  reads = 0
  constructor(
    readonly pose: PerceptionPose,
    private readonly lookup: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded',
  ) {}
  selfPose() { return this.pose }
  revision() { return 1 }
  blockAt(position: PerceptionPose['position']) {
    this.reads++
    return this.lookup(position)
  }
  nearbyEntities() { return [] }
}

const pose = (pitch: number): PerceptionPose => ({ position: { x: 0, y: 64, z: 0 }, yaw: 0, pitch })

interface Fixture {
  name: string
  pitch: number
  lookup: (position: PerceptionPose['position']) => PerceptionBlock | 'unloaded'
  oracle?: (eye: PerceptionPose['position']) => number
}

/**
 * Every exposed top surface whose voxel centre passes the scan's declared range/frustum cull.
 * This is independent geometry, not a second raycasting implementation.
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
      count++
    }
  }
  return count
}

const FIXTURES: Fixture[] = [
  { name: '空气视野', pitch: 0, lookup: () => AIR, oracle: () => 0 },
  { name: '开阔平地（水平）', pitch: 0, lookup: p => p.y <= 63 ? opaque('grass_block') : AIR, oracle: flatGroundOracle },
  { name: '开阔平地（俯视 -1.2rad）', pitch: -1.2, lookup: p => p.y <= 63 ? opaque('grass_block') : AIR },
  { name: '前方 8 格墙', pitch: 0, lookup: p => p.z === -8 ? opaque('stone') : AIR },
  { name: '全透明体', pitch: 0, lookup: () => GLASS },
]

const PREDICATES: VisibilityPredicate[] = ['block_centre', 'exposed_face']

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
      const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6
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
        String(metrics.voxelsExamined).padStart(9),
        `${metrics.sectionsSkipped}/${metrics.sectionsTested}`.padStart(8),
        `${elapsedMs.toFixed(0)}ms`.padStart(7),
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
