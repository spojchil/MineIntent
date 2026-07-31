import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import type { SoundHistoryPort } from '../source-ports/sound.js'

export interface SoundValues {
  recentSounds: Array<{
    soundName?: string
    category?: string
    distance: number
    direction: 'ahead' | 'right' | 'behind' | 'left'
    volume: number
    pitch: number
    observedAt: string
  }>
}

const soundSchema = z.object({
  soundName: z.string().min(1).optional(),
  category: z.string().min(1).optional(),
  distance: z.number().min(0),
  direction: z.enum(['ahead', 'right', 'behind', 'left']),
  volume: z.number().min(0),
  pitch: z.number(),
  observedAt: z.iso.datetime(),
})

const RECENT_SOUND_LIMIT = 20

export class SoundInformationProvider implements InformationProvider<SoundValues> {
  readonly #port: SoundHistoryPort

  constructor(port: SoundHistoryPort) { this.#port = port }

  readonly definition: InformationProviderDefinition<SoundValues> = {
    id: 'sound_information',
    description: '站立不动时能听到的最近声音，含相对距离和方向',
    schemaRevision: 'sound-information:1',
    audiences: ['participant'] as const,
    fields: {
      recentSounds: {
        description: '最近听到的声音，按时间从新到旧排列', valueSchema: z.array(soundSchema), valueType: 'array',
        precision: 'quantized', sourceKinds: ['sound_projection'],
        notes: '距离和方向是从协议声音包位置换算得到的近似值，不是精确音源坐标',
      },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 1, maxResultBytes: 16_384, timeoutMs: 2_000 },
  }

  availability(): ProviderAvailability<SoundValues> {
    return { overall: 'available', informationRevision: this.#port.revision(), fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<SoundValues, never, never>,
    _signal: AbortSignal,
  ): Promise<ProviderReadResult<SoundValues, never>> {
    const values: Partial<SoundValues> = {}
    if (request.fields.includes('recentSounds')) values.recentSounds = [...this.#port.recent(RECENT_SOUND_LIMIT)]
    const revision = this.#port.revision()
    return {
      informationRevision: revision, values, unavailable: [],
      source: { kind: 'sound_projection', adapterRevision: 'sound-provider.v1', sourceRevision: revision, acquisition: 'immediate_client_state' },
      observedAt: context.now, evidenceIds: [],
    }
  }
}
