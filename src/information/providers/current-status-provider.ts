import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import type { SelfVitalsPort } from '../source-ports/self-vitals.js'

export interface CurrentStatusValues {
  health: number
  food: number
  foodSaturation: number
  oxygen: number
  experienceLevel: number
  statusEffects: Array<{ name: string; amplifier: number; durationTicks?: number }>
}

const statusEffectSchema = z.object({
  name: z.string().min(1),
  amplifier: z.number().int(),
  durationTicks: z.number().int().optional(),
})

export class CurrentStatusProvider implements InformationProvider<CurrentStatusValues> {
  readonly #port: SelfVitalsPort
  #revision = 0
  #lastSignature = ''

  constructor(port: SelfVitalsPort) { this.#port = port }

  readonly definition: InformationProviderDefinition<CurrentStatusValues> = {
    id: 'current_status',
    description: '站立不动时可直接得知的自身状态：生命、饥饿、氧气、经验和药水效果',
    schemaRevision: 'current-status:1',
    audiences: ['participant'] as const,
    fields: {
      health: {
        description: '当前生命值', valueSchema: z.number().min(0), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      food: {
        description: '当前饥饿值（0-20）', valueSchema: z.number().min(0).max(20), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      foodSaturation: {
        description: '当前饱和度', valueSchema: z.number().min(0), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      oxygen: {
        description: '当前氧气值；不在水下通常为满值', valueSchema: z.number().min(0), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      experienceLevel: {
        description: '当前经验等级', valueSchema: z.number().int().min(0), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      statusEffects: {
        description: '当前生效的药水/状态效果', valueSchema: z.array(statusEffectSchema), valueType: 'array',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 6, maxResultBytes: 8_192, timeoutMs: 2_000 },
  }

  availability(): ProviderAvailability<CurrentStatusValues> {
    return { overall: 'available', informationRevision: this.#revisionFor(this.#port.current()), fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<CurrentStatusValues, never, never>,
    _signal: AbortSignal,
  ): Promise<ProviderReadResult<CurrentStatusValues, never>> {
    const vitals = this.#port.current()
    const revision = this.#revisionFor(vitals)
    const values: Partial<CurrentStatusValues> = {}
    for (const field of request.fields) {
      switch (field) {
        case 'health': values.health = vitals.health; break
        case 'food': values.food = vitals.food; break
        case 'foodSaturation': values.foodSaturation = vitals.foodSaturation; break
        case 'oxygen': values.oxygen = vitals.oxygen ?? 20; break
        case 'experienceLevel': values.experienceLevel = vitals.experience?.level ?? 0; break
        case 'statusEffects': values.statusEffects = vitals.effects; break
      }
    }
    return {
      informationRevision: revision, values, unavailable: [],
      source: { kind: 'client_state', adapterRevision: 'current-status-provider.v1', sourceRevision: revision, acquisition: 'immediate_client_state' },
      observedAt: context.now, evidenceIds: [],
    }
  }

  #revisionFor(vitals: ReturnType<SelfVitalsPort['current']>): number {
    const signature = JSON.stringify(vitals)
    if (signature !== this.#lastSignature) { this.#lastSignature = signature; this.#revision++ }
    return this.#revision
  }
}
