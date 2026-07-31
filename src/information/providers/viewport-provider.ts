import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import { raycastLookedAtBlock, standingOnBlock, visibleBlocks, visibleEntities, type PerceptionPort } from '../source-ports/perception.js'
import type { Point3 } from '../geometry.js'

type WorldPosition = [number, number, number]

/**
 * Three facts kept apart instead of collapsed into one label.
 *
 * The previous single string was `username ?? name ?? type`, which made a player called `sheep`
 * indistinguishable from a sheep — and that is not a cosmetic collision, since one of them is
 * someone to talk to. Keeping `player` as its own optional field means its presence *is* the
 * answer to "is this a person", with no sentinel value to misread.
 *
 * None of this widens what the companion may know. A vanilla player reads a species off the model
 * and a name off the nameplate above it; the entity's handles — `entityKey`, `uuid`, the protocol
 * id — stay behind the port, as they do everywhere else.
 */
interface VisibleEntityView {
  /** Vanilla entity type: `sheep`, `creeper`, `item`. Players are `player`. */
  type: string
  /** A player's name, present only for players — so having it also says this is one. */
  player?: string
  position: WorldPosition
}

/**
 * Absolute world coordinates, not body-relative ones.
 *
 * A body-relative frame renumbers every entry whenever the companion turns or steps, which makes
 * an incremental read impossible and forces the model to re-register its whole picture from
 * discrete text every observation. World coordinates are also not privileged information: any vanilla
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
  /** Nearest first, and bounded: `truncated` says whether farther entities were left out. */
  visibleEntities: { items: VisibleEntityView[]; truncated: boolean }
  /** Compact [block_name, x, y, z] tuples, nearest first. */
  visibleBlocks: { blocks: Array<[string, number, number, number]>; truncated: boolean }
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
const visibleEntitiesSchema = z.object({
  items: z.array(z.strictObject({
    type: z.string().min(1),
    player: z.string().min(1).optional(),
    position: worldPositionSchema,
  })),
  truncated: z.boolean(),
})
const visibleBlocksSchema = z.object({
  blocks: z.array(z.tuple([z.string().min(1), z.number(), z.number(), z.number()])), truncated: z.boolean(),
})

const LEGEND: ViewportValues['frame']['legend'] = {
  // Legends ride with the data instead of living in the agent prompt, so a schema change here
  // cannot silently outdate a description written in another language and process.
  visibleEntities: 'items 每项为 {type, player?, position}：type 是原版实体类型（玩家为 player），'
    + 'player 只有玩家才有，position 是 Minecraft 世界绝对坐标。按距离从近到远，'
    + 'truncated 为真表示更远处还有实体没列出',
  // Nearest-first is part of the contract, not an implementation detail: the cap keeps the closest
  // blocks, so a truncated read is "everything out to some radius" rather than an arbitrary sample.
  visibleBlocks: '[block_name, x, y, z]，同一坐标系的整数体素，按距离从近到远，可能截断',
}

/** How many entities a read reports. Named because `truncated` is only meaningful against it. */
const ENTITY_LIMIT = 8

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
const VISIBLE_BLOCKS_OPTIONS = {
  horizontalRadius: 32,
  verticalRadius: 20,
  maxDistance: 32,
  frustum: VIEW_FRUSTUM,
  limit: 256,
  predicate: 'exposed_face',
} as const

export class ViewportInformationProvider implements InformationProvider<ViewportValues> {
  /**
   * Counts published projections, not world states.
   *
   * A revision earns its name only if equal revisions guarantee equal content, and this one cannot
   * make that promise. It used to be signed from the pose plus `port.revision()`, which resolves to
   * the backend's `snapshotRevision` — and that counter advances on the companion's own vitals, held
   * item, window updates, players joining or leaving, death, dimension change and readiness.
   * Nothing advances it when a block is placed or broken, or when a mob takes a step. So for the two
   * fields that dominate this read, a pose held still meant a frozen revision over changing content,
   * every time rather than occasionally.
   *
   * Minting a fresh number per read is honest instead of nearly-right: it never claims two reads
   * agree. The cost is that the number carries no cache signal, which costs nothing real here —
   * `acquisition: 'current_perception'` already says this is a fresh sample, and the interface is
   * unpaginated and issues no refs, so nothing downstream compares two of these for equality.
   *
   * A revision that genuinely tracked content would need the backend to subscribe to block updates
   * and entity movement. That is a real design question — entity movement fires every tick per
   * entity, so the honest version has an event-storm problem — and it is filed rather than guessed.
   */
  #revision = 0
  constructor(private readonly port: PerceptionPort) {}

  readonly definition: InformationProviderDefinition<ViewportValues> = {
    id: 'viewport_information',
    description: '粗略第一人称视野；所有位置都使用 Minecraft 世界绝对坐标，方块为整数体素',
    schemaRevision: 'viewport-information:10',
    audiences: ['companion'] as const,
    fields: {
      frame: { description: '本次观察的姿态与坐标系图例', valueSchema: frameSchema, valueType: 'object', precision: 'exactly_displayed', sourceKinds: ['viewport_projection'] },
      standingOnBlock: { description: '脚下可见方块及其绝对体素坐标', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      lookedAtBlock: { description: '准星射线首先命中的可见方块及其绝对体素坐标', valueSchema: blockSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleEntities: { description: '可见实体；items 每项为{type,player?,position}，按距离从近到远，truncated 表示更远处还有未列出的', valueSchema: visibleEntitiesSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
      visibleBlocks: { description: '可见方块（朝观察者的暴露面无遮挡可达）；每项为[名称,x,y,z]整数体素，按距离从近到远，可能截断', valueSchema: visibleBlocksSchema, valueType: 'object', precision: 'inferred', sourceKinds: ['viewport_projection'] },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 5, maxResultBytes: 65_536, timeoutMs: 5_000 },
  }

  /**
   * Reports the last published projection and does not mint a new one. Availability is the runtime's
   * probe for whether a held reference is still current, so it has to be free of side effects: a
   * counter that advanced on being asked would make every reference look stale the moment it was
   * checked, and two consecutive probes disagree.
   */
  availability(): ProviderAvailability<ViewportValues> {
    return { overall: 'available', informationRevision: this.#revision, fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<ViewportValues, never, never>,
    signal: AbortSignal,
  ): Promise<ProviderReadResult<ViewportValues, never>> {
    const revision = ++this.#revision
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
      const result = await visibleEntities(this.port, 32, VIEW_FRUSTUM, ENTITY_LIMIT, signal)
      values.visibleEntities = {
        items: result.entities.map(entity => ({
          // `name` is the registry species (`sheep`), while `type` is the library's broad category
          // (`mob`, `object`). The species is what a player would say, so it leads; the category is
          // only a fallback for an entity the registry did not recognise.
          type: entity.name ?? entity.type,
          ...(entity.username === undefined ? {} : { player: entity.username }),
          position: roundPosition(entity.position),
        })),
        truncated: result.truncated,
      }
    }
    if (request.fields.includes('visibleBlocks')) {
      const result = await visibleBlocks(this.port, VISIBLE_BLOCKS_OPTIONS, signal)
      values.visibleBlocks = {
        // Block coordinates are exact integers, which is what makes an incremental read possible:
        // the same block keeps the same key no matter where the companion stands.
        blocks: result.blocks.map(block => {
          const [x, y, z] = voxel(block.position)
          return [block.name, x, y, z]
        }),
        truncated: result.truncated,
      }
    }
    return {
      informationRevision: revision, values, unavailable: [],
      source: {
        kind: 'viewport_projection', adapterRevision: 'viewport-provider.v3',
        // The backend's own counter, kept where it belongs. It is a weak signal about the world —
        // see the note on `#revision` for what it does and does not advance on — but this is the
        // field that is meant to describe the source, so it should report the source's number and
        // not the projection's.
        sourceRevision: this.port.revision(),
        acquisition: 'current_perception',
      },
      observedAt: context.now, evidenceIds: [],
    }
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
