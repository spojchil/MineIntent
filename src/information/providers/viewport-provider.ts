import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import { raycastLookedAtBlock, standingOnBlock, viewRelativePosition, visibleBlocks, visibleEntities, type PerceptionPort } from '../source-ports/perception.js'

type RelativePosition = [number, number, number]
export interface ViewportValues {
  frame: {
    axes: ['right', 'up', 'forward']
    directions: { ahead: '+forward'; right: '+right'; behind: '-forward'; left: '-right' }
  }
  standingOnBlock: { name: string; relativePosition: RelativePosition } | null
  lookedAtBlock: { name: string; relativePosition: RelativePosition } | null
  visibleEntities: Array<{
    type: string
    name?: string
    username?: string
    distanceBand: 'very_near' | 'near' | 'medium' | 'far'
    direction: 'ahead' | 'right' | 'behind' | 'left'
    relativePosition: RelativePosition
  }>
  /** Compact [block_name, right, up, forward] tuples, nearest first. */
  visibleBlocks: { blocks: Array<[string, number, number, number]>; truncated: boolean }
}

const relativePositionSchema = z.tuple([z.number(), z.number(), z.number()])
const frameSchema = z.strictObject({
  axes: z.tuple([z.literal('right'), z.literal('up'), z.literal('forward')]),
  directions: z.strictObject({
    ahead: z.literal('+forward'), right: z.literal('+right'), behind: z.literal('-forward'), left: z.literal('-right'),
  }),
})
const blockSchema = z.object({ name: z.string().min(1), relativePosition: relativePositionSchema }).nullable()
const visibleEntitiesSchema = z.array(z.object({
  type: z.string().min(1), name: z.string().min(1).optional(), username: z.string().min(1).optional(),
  distanceBand: z.enum(['very_near', 'near', 'medium', 'far']),
  direction: z.enum(['ahead', 'right', 'behind', 'left']), relativePosition: relativePositionSchema,
}))
const visibleBlocksSchema = z.object({
  blocks: z.array(z.tuple([z.string().min(1), z.number(), z.number(), z.number()])), truncated: z.boolean(),
})

const FRAME: ViewportValues['frame'] = {
  axes: ['right', 'up', 'forward'],
  directions: { ahead: '+forward', right: '+right', behind: '-forward', left: '-right' },
}
const VIEW_HALF_ANGLE = 45 * Math.PI / 180

export class ViewportInformationProvider implements InformationProvider<ViewportValues> {
  #revision = 0
  #lastSignature = ''
  constructor(private readonly port: PerceptionPort) {}

  readonly definition: InformationProviderDefinition<ViewportValues> = {
    id: 'viewport_information',
    description: '粗略第一人称视野；所有位置都使用同一量化的[右,上,前]身体相对坐标',
    schemaRevision: 'viewport-information:5',
    audiences: ['companion'] as const,
    fields: {
      frame: { description: '相对坐标与方向图例', valueSchema: frameSchema, valueType: 'object', precision: 'exactly_displayed', sourceKinds: ['viewport_projection'] },
      standingOnBlock: { description: '脚下可见方块及其[右,上,前]相对位置', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      lookedAtBlock: { description: '准星射线首先命中的可见方块及其[右,上,前]相对位置', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleEntities: { description: '通过视锥和遮挡检查的实体；位置为[右,上,前]', valueSchema: visibleEntitiesSchema, valueType: 'array', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleBlocks: { description: '可见方块；每项为[名称,右,上,前]，可能截断', valueSchema: visibleBlocksSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 5, maxResultBytes: 32_768, timeoutMs: 5_000 },
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
    if (request.fields.includes('frame')) values.frame = structuredClone(FRAME)
    if (request.fields.includes('standingOnBlock')) {
      const block = standingOnBlock(this.port)
      values.standingOnBlock = block ? { name: block.name, relativePosition: viewRelativePosition(pose, block.position) } : null
    }
    if (request.fields.includes('lookedAtBlock')) {
      const block = raycastLookedAtBlock(this.port, 4.5)
      values.lookedAtBlock = block ? { name: block.name, relativePosition: viewRelativePosition(pose, block.position) } : null
    }
    if (request.fields.includes('visibleEntities')) {
      values.visibleEntities = visibleEntities(this.port, 32, VIEW_HALF_ANGLE, 8).map(entity => ({
        type: entity.type,
        ...(entity.name ? { name: entity.name } : {}),
        ...(entity.username ? { username: entity.username } : {}),
        distanceBand: entity.distanceBand,
        direction: entity.direction,
        relativePosition: viewRelativePosition(pose, entity.position),
      }))
    }
    if (request.fields.includes('visibleBlocks')) {
      const result = await visibleBlocks(this.port, {
        horizontalRadius: 32, verticalRadius: 4, maxDistance: 32, halfAngle: VIEW_HALF_ANGLE, limit: 24,
      }, signal)
      values.visibleBlocks = {
        blocks: result.blocks.map(block => {
          const [right, up, forward] = viewRelativePosition(pose, block.position)
          return [block.name, right, up, forward]
        }),
        truncated: result.truncated,
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
