import { z } from 'zod'
import type { InformationProvider, InformationProviderContext, InformationProviderDefinition, ProviderAvailability, ProviderReadRequest, ProviderReadResult } from '../contracts/index.js'
import type { InventoryPort } from '../source-ports/inventory.js'

export interface InventoryValues {
  selectedHotbarSlot: number
  slots: Array<{ slot: number; itemName: string; count: number; metadata?: number; durabilityUsed?: number }>
}

const slotSchema = z.object({
  slot: z.number().int().min(0),
  itemName: z.string().min(1),
  count: z.number().int().positive(),
  metadata: z.number().int().optional(),
  durabilityUsed: z.number().int().optional(),
})

export class InventoryProvider implements InformationProvider<InventoryValues> {
  readonly #port: InventoryPort
  #revision = 0
  #lastSignature = ''

  constructor(port: InventoryPort) { this.#port = port }

  readonly definition: InformationProviderDefinition<InventoryValues> = {
    id: 'inventory_information',
    description: '站立不动时可直接得知的背包内容与当前选中快捷栏槽',
    schemaRevision: 'inventory-information:1',
    audiences: ['participant'] as const,
    fields: {
      selectedHotbarSlot: {
        description: '当前选中的快捷栏槽位（0-8）', valueSchema: z.number().int().min(0).max(8), valueType: 'number',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
      slots: {
        description: '背包中所有非空槽位', valueSchema: z.array(slotSchema), valueType: 'array',
        precision: 'exactly_displayed', sourceKinds: ['client_state'],
      },
    },
    scopeDependencies: ['connection', 'world'] as const,
    limits: { maxFieldsPerRead: 2, maxResultBytes: 16_384, timeoutMs: 2_000 },
  }

  availability(): ProviderAvailability<InventoryValues> {
    return { overall: 'available', informationRevision: this.#revisionFor(this.#port.current()), fields: {} }
  }

  async read(
    context: InformationProviderContext,
    request: ProviderReadRequest<InventoryValues, never, never>,
    _signal: AbortSignal,
  ): Promise<ProviderReadResult<InventoryValues, never>> {
    const inventory = this.#port.current()
    const revision = this.#revisionFor(inventory)
    const values: Partial<InventoryValues> = {}
    for (const field of request.fields) {
      if (field === 'selectedHotbarSlot') values.selectedHotbarSlot = inventory.selectedHotbarSlot
      if (field === 'slots') values.slots = inventory.slots
    }
    return {
      informationRevision: revision, values, unavailable: [],
      source: { kind: 'client_state', adapterRevision: 'inventory-provider.v1', sourceRevision: revision, acquisition: 'immediate_client_state' },
      observedAt: context.now, evidenceIds: [],
    }
  }

  #revisionFor(inventory: ReturnType<InventoryPort['current']>): number {
    const signature = JSON.stringify(inventory)
    if (signature !== this.#lastSignature) { this.#lastSignature = signature; this.#revision++ }
    return this.#revision
  }
}
