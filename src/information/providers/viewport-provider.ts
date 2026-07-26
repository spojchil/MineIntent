import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import { nearbyTrackedEntities, raycastLookedAtBlock, standingOnBlock, visibleBlocks, type PerceptionPort } from '../source-ports/perception.js'

export interface ViewportValues {
  standingOnBlock: { name: string } | null
  lookedAtBlock: { name: string; distance: number } | null
  nearbyTrackedEntities: Array<{
    type: string
    name?: string
    username?: string
    distance: number
    direction: 'ahead' | 'right' | 'behind' | 'left'
  }>
  /** Each block is a positional [offsetX, offsetY, offsetZ, name] tuple, nearest first — not a keyed object, to avoid repeating the same four key names in every entry. */
  visibleBlocks: { blocks: Array<[number, number, number, string]>; truncated: boolean }
}

const standingOnBlockSchema = z.object({ name: z.string().min(1) }).nullable()
const lookedAtBlockSchema = z.object({ name: z.string().min(1), distance: z.number().min(0) }).nullable()
const nearbyEntitySchema = z.array(z.object({
  type: z.string().min(1),
  name: z.string().min(1).optional(),
  username: z.string().min(1).optional(),
  distance: z.number().min(0),
  direction: z.enum(['ahead', 'right', 'behind', 'left']),
}))
const visibleBlocksSchema = z.object({
  blocks: z.array(z.tuple([z.number().int(), z.number().int(), z.number().int(), z.string().min(1)])),
  truncated: z.boolean(),
})

// Matches vanilla's default block_interaction_range (4.5 blocks outside Creative).
const MAX_LOOK_DISTANCE = 4.5
const MAX_ENTITY_DISTANCE = 16
const MAX_ENTITIES = 10
const VERTICAL_FOV_DEGREES = 70
const ASSUMED_ASPECT_RATIO = 16 / 9
const VERTICAL_HALF_ANGLE = (VERTICAL_FOV_DEGREES / 2) * Math.PI / 180
const VIEW_FRUSTUM = {
  verticalHalfAngle: VERTICAL_HALF_ANGLE,
  horizontalHalfAngle: Math.atan(Math.tan(VERTICAL_HALF_ANGLE) * ASSUMED_ASPECT_RATIO),
} as const
const VISIBLE_BLOCKS_OPTIONS = {
  horizontalRadius: 32, verticalRadius: 4, maxDistance: 32, // 32 blocks == 2 chunks (chunks are a horizontal 16x16 division only)
  frustum: VIEW_FRUSTUM,
  limit: 24,
  predicate: 'exposed_face',
} as const

/**
 * "Reasonable vision" for a standing-still player: what block you're standing on, a precise
 * crosshair-style raycast for what's directly ahead (matching vanilla's targeted-block
 * mechanic), a distance-sorted list of protocol-tracked nearby entities, and a layered-filter
 * approximation of what's visible in front of you (rectangular frustum+distance cull → reachable
 * exposed-face test, using the same stepped rays as lookedAtBlock). Visibility and occlusion stay
 * binary per voxel — partial shapes (stairs, fences, torches, carpets), true translucency layering
 * and multi-point corner sampling are known, deliberately deferred refinements, not the
 * full FOV/DDA/material system from the shelved viewport design (issue #34/#68).
 */
export class ViewportInformationProvider implements InformationProvider<ViewportValues> {
  readonly #port: PerceptionPort
  #revision = 0
  #lastSignature = ''

  constructor(port: PerceptionPort) { this.#port = port }

  readonly definition: InformationProviderDefinition<ViewportValues> = {
    id: 'viewport_information',
    description: '站立不动时的合理视觉：脚下方块、准星精确指向的方块、附近协议追踪到的实体，以及正前方视野内的方块（分层过滤近似）',
    schemaRevision: 'viewport-information:5',
    audiences: ['companion'] as const,
    fields: {
      standingOnBlock: {
        description: '脚下（自身位置正下方）的方块；无法得知时为 null',
        valueSchema: standingOnBlockSchema, valueType: 'object', precision: 'exactly_displayed', sourceKinds: ['viewport_projection'],
      },
      lookedAtBlock: {
        description: '准星精确指向的最近非空气方块，对应原版 block_interaction_range（约4.5格）；' +
          '这是精确的单点指向，不是一个视野范围，没对准任何方块时为 null 是常见且正常的结果',
        valueSchema: lookedAtBlockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'],
      },
      nearbyTrackedEntities: {
        description: '16格内协议追踪到的实体，按距离排序；这是候选列表，不代表一定在视线内可见',
        valueSchema: nearbyEntitySchema, valueType: 'array', precision: 'inferred', sourceKinds: ['viewport_projection'],
      },
      visibleBlocks: {
        description: '正前方矩形视野（垂直约70度、按16:9推导水平角）、32格内、至少一个暴露面无遮挡可达的方块列表；blocks 每项是 ' +
          '[offsetX, offsetY, offsetZ, name] 四元组，坐标是相对自身位置的整数偏移量，不是世界绝对坐标，也不含距离字段' +
          '（可自行从偏移量估算远近）；按距离由近到远排序，可能被截断（truncated）；遮挡按整格二值化处理，是近似值不是精确渲染结果',
        valueSchema: visibleBlocksSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'],
      },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 4, maxResultBytes: 32_768, timeoutMs: 5_000 },
  }

  availability(): ProviderAvailability<ViewportValues> {
    return { overall: 'available', informationRevision: this.#revisionFor(), fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<ViewportValues, never, never>,
    signal: AbortSignal,
  ): Promise<ProviderReadResult<ViewportValues, never>> {
    const revision = this.#revisionFor()
    const values: Partial<ViewportValues> = {}
    if (request.fields.includes('standingOnBlock')) values.standingOnBlock = standingOnBlock(this.#port)
    if (request.fields.includes('lookedAtBlock')) values.lookedAtBlock = raycastLookedAtBlock(this.#port, MAX_LOOK_DISTANCE)
    if (request.fields.includes('nearbyTrackedEntities')) {
      values.nearbyTrackedEntities = nearbyTrackedEntities(this.#port, MAX_ENTITY_DISTANCE, MAX_ENTITIES)
    }
    if (request.fields.includes('visibleBlocks')) {
      const pose = this.#port.selfPose()
      const selfVoxel = {
        x: Math.floor(pose.position.x), y: Math.floor(pose.position.y), z: Math.floor(pose.position.z),
      }
      const result = await visibleBlocks(this.#port, VISIBLE_BLOCKS_OPTIONS, signal)
      values.visibleBlocks = {
        blocks: result.blocks.map((block): [number, number, number, string] => [
          block.position.x - selfVoxel.x,
          block.position.y - selfVoxel.y,
          block.position.z - selfVoxel.z,
          block.name,
        ]),
        truncated: result.truncated,
      }
    }
    return {
      informationRevision: revision, values, unavailable: [],
      source: { kind: 'viewport_projection', adapterRevision: 'viewport-provider.v2', sourceRevision: revision, acquisition: 'current_perception' },
      observedAt: context.now, evidenceIds: [],
    }
  }

  #revisionFor(): number {
    const pose = this.#port.selfPose()
    const signature = JSON.stringify(pose)
    if (signature !== this.#lastSignature) { this.#lastSignature = signature; this.#revision++ }
    return this.#revision
  }
}
