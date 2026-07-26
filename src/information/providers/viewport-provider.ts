import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import { raycastLookedAtBlock, standingOnBlock, visibleBlocks, visibleEntities, type PerceptionPort } from '../source-ports/perception.js'
import type { Point3 } from '../geometry.js'

type WorldPosition = [number, number, number]
type EntityTuple = [string, number, number, number]

/**
 * Absolute world coordinates, not body-relative ones.
 *
 * A body-relative frame renumbers every entry whenever the companion turns or steps, which makes
 * an incremental read impossible and forces the model to re-register its whole picture from
 * discrete text every round. World coordinates are also not privileged information: any vanilla
 * player reads their own position and the targeted block's position straight off the F3 screen and
 * can derive the rest. The no-cheating boundary therefore stays where the register puts it — on
 * what is observable and visible, never on how it is represented.
 */
export interface ViewportValues {
  frame: {
    coordinates: 'minecraft_world_absolute'
    /** Where the companion is and where it faces, so relative bearings stay derivable. */
    self: { position: WorldPosition; yawDegrees: number; pitchDegrees: number }
    legend: { visibleEntities: string; visibleBlocks: string }
  }
  standingOnBlock: { name: string; position: WorldPosition } | null
  lookedAtBlock: { name: string; position: WorldPosition } | null
  /** Compact [entity_name_or_player, x, y, z] tuples, nearest first. */
  visibleEntities: EntityTuple[]
  /**
   * Compact [block_name, x, y, z] tuples, nearest first. `verifiedDistance` is how far this scan can
   * be trusted: with a truncated result it is the last kept block's distance, and beyond it the scan
   * looked at nothing, so an absence there proves nothing.
   */
  visibleBlocks: { blocks: Array<[string, number, number, number]>; truncated: boolean; verifiedDistance: number }
}

const worldPositionSchema = z.tuple([z.number(), z.number(), z.number()])
const frameSchema = z.strictObject({
  coordinates: z.literal('minecraft_world_absolute'),
  self: z.strictObject({
    position: worldPositionSchema,
    yawDegrees: z.number(),
    pitchDegrees: z.number(),
  }),
  legend: z.strictObject({ visibleEntities: z.string().min(1), visibleBlocks: z.string().min(1) }),
})
const blockSchema = z.object({ name: z.string().min(1), position: worldPositionSchema }).nullable()
const visibleEntitiesSchema = z.array(z.tuple([z.string().min(1), z.number(), z.number(), z.number()]))
const visibleBlocksSchema = z.object({
  blocks: z.array(z.tuple([z.string().min(1), z.number(), z.number(), z.number()])), truncated: z.boolean(),
  verifiedDistance: z.number(),
})

const LEGEND: ViewportValues['frame']['legend'] = {
  // Legends ride with the data instead of living in the agent prompt, so a schema change here
  // cannot silently outdate a description written in another language and process.
  visibleEntities: '[entity_name_or_player, x, y, z]，Minecraft 世界绝对坐标，按距离从近到远',
  // Nearest-first is part of the contract, not an implementation detail: the cap keeps the closest
  // blocks, so a truncated read is "everything out to some radius" rather than an arbitrary sample.
  visibleBlocks: '[block_name, x, y, z]，同一坐标系的整数体素，按距离从近到远，可能截断',
}

/**
 * Vanilla's default FOV slider is 70°, and that slider is the *vertical* field of view. The
 * horizontal angle is derived from the window aspect ratio rather than hardcoded, so the shape
 * stays honest if the assumed ratio ever changes: at 16:9 it works out to 102.45° (half 51.22°),
 * at 4:3 to 86.07°. Sprint and the FOV-effects slider widen this in game; that is deliberately
 * not modelled, since the companion has no camera of its own to widen.
 */
const VERTICAL_FOV_DEGREES = 70
const ASSUMED_ASPECT_RATIO = 16 / 9
const VIEW_FRUSTUM = {
  verticalHalfAngle: (VERTICAL_FOV_DEGREES / 2) * Math.PI / 180,
  horizontalHalfAngle: Math.atan(Math.tan((VERTICAL_FOV_DEGREES / 2) * Math.PI / 180) * ASSUMED_ASPECT_RATIO),
} as const

/**
 * Exported because an incremental read has to judge a remembered voxel against the *same* bounds the
 * scan used. Two copies of these numbers would let "in view" mean one thing when a block is found
 * and another when its disappearance is judged, which is exactly how a diff starts lying.
 */
export const VIEWPORT_SCAN = {
  horizontalRadius: 32, verticalRadius: 20, maxDistance: 32, frustum: VIEW_FRUSTUM, limit: 256,
  // Stated rather than defaulted: which predicate ran decides what a missing block means, so
  // reading it here should not require knowing a default elsewhere.
  predicate: 'exposed_face',
} as const

export class ViewportInformationProvider implements InformationProvider<ViewportValues> {
  #revision = 0
  #lastSignature = ''
  constructor(private readonly port: PerceptionPort) {}

  readonly definition: InformationProviderDefinition<ViewportValues> = {
    id: 'viewport_information',
    description: '粗略第一人称视野；所有位置都使用 Minecraft 世界绝对坐标，方块为整数体素',
    schemaRevision: 'viewport-information:8',
    audiences: ['companion'] as const,
    fields: {
      frame: { description: '本次观察的姿态与坐标系图例', valueSchema: frameSchema, valueType: 'object', precision: 'exactly_displayed', sourceKinds: ['viewport_projection'] },
      standingOnBlock: { description: '脚下可见方块及其绝对体素坐标', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      lookedAtBlock: { description: '准星射线首先命中的可见方块及其绝对体素坐标', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleEntities: { description: '可见实体；每项为[实体名或玩家名,x,y,z]，按距离从近到远', valueSchema: visibleEntitiesSchema, valueType: 'array', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleBlocks: { description: '可见方块；每项为[名称,x,y,z]整数体素，按距离从近到远，可能截断', valueSchema: visibleBlocksSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 5, maxResultBytes: 65_536, timeoutMs: 5_000 },
  }

  availability(): ProviderAvailability<ViewportValues> {
    return { overall: 'available', informationRevision: this.revisionFor(), fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<ViewportValues, never, never>,
    signal: AbortSignal,
  ): Promise<ProviderReadResult<ViewportValues, never>> {
    const revision = this.revisionFor()
    const pose = this.port.selfPose()
    const values: Partial<ViewportValues> = {}
    if (request.fields.includes('frame')) {
      values.frame = {
        coordinates: 'minecraft_world_absolute',
        self: {
          position: roundPosition(pose.position),
          yawDegrees: withoutNegativeZero(round(radiansToDegrees(pose.yaw), 1)),
          pitchDegrees: withoutNegativeZero(round(radiansToDegrees(pose.pitch), 1)),
        },
        legend: { ...LEGEND },
      }
    }
    if (request.fields.includes('standingOnBlock')) {
      const block = standingOnBlock(this.port)
      values.standingOnBlock = block ? { name: block.name, position: voxel(block.position) } : null
    }
    if (request.fields.includes('lookedAtBlock')) {
      const block = raycastLookedAtBlock(this.port, 4.5)
      values.lookedAtBlock = block ? { name: block.name, position: voxel(block.position) } : null
    }
    if (request.fields.includes('visibleEntities')) {
      const entities = await visibleEntities(this.port, 32, VIEW_FRUSTUM, 8, signal)
      values.visibleEntities = entities.map(entity => {
        const [x, y, z] = roundPosition(entity.position)
        return [entity.username ?? entity.name ?? entity.type, x, y, z]
      })
    }
    if (request.fields.includes('visibleBlocks')) {
      const result = await visibleBlocks(this.port, VIEWPORT_SCAN, signal)
      values.visibleBlocks = {
        // Block coordinates are exact integers, which is what makes an incremental read possible:
        // the same block keeps the same key no matter where the companion stands.
        blocks: result.blocks.map(block => {
          const [x, y, z] = voxel(block.position)
          return [block.name, x, y, z]
        }),
        truncated: result.truncated,
        verifiedDistance: round(result.verifiedDistance, 2),
      }
    }
    return {
      informationRevision: revision, values, unavailable: [],
      source: { kind: 'viewport_projection', adapterRevision: 'viewport-provider.v2', sourceRevision: revision, acquisition: 'current_perception' },
      observedAt: context.now, evidenceIds: [],
    }
  }

  private revisionFor(): number {
    const signature = JSON.stringify({ pose: this.port.selfPose(), sourceRevision: this.port.revision() })
    if (signature !== this.#lastSignature) { this.#lastSignature = signature; this.#revision++ }
    return this.#revision
  }
}

function voxel(position: Point3): [number, number, number] {
  return [Math.floor(position.x), Math.floor(position.y), Math.floor(position.z)]
}
/** Entity and self positions are continuous; one decimal is finer than a player reads off F3. */
function roundPosition(position: Point3): [number, number, number] {
  return [
    withoutNegativeZero(round(position.x, 1)),
    withoutNegativeZero(round(position.y, 1)),
    withoutNegativeZero(round(position.z, 1)),
  ]
}
function round(value: number, decimals: number): number {
  const factor = 10 ** decimals
  return Math.round(value * factor) / factor
}
function radiansToDegrees(value: number): number { return value * 180 / Math.PI }
function withoutNegativeZero(value: number): number { return Object.is(value, -0) ? 0 : value }
