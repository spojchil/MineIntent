import type { InformationInterfaceId, TrustedInformationCaller } from './contracts/index.js'
import type { InformationRuntime } from './runtime.js'
import type { CurrentStatusValues } from './providers/current-status-provider.js'
import type { InventoryValues } from './providers/inventory-provider.js'
import type { SoundValues } from './providers/sound-provider.js'
import type { ViewportValues } from './providers/viewport-provider.js'

export interface PassiveObservations {
  currentStatus?: CurrentStatusValues
  inventory?: InventoryValues
  sound?: SoundValues
  viewport?: ViewportValues
  omissions: Array<{ interfaceId: InformationInterfaceId; reason: string }>
}

interface ReadPlanEntry {
  interfaceId: InformationInterfaceId
  schemaRevision: string
  fields: readonly string[]
}

const READ_PLAN: readonly ReadPlanEntry[] = [
  { interfaceId: 'current_status', schemaRevision: 'current-status:1', fields: ['health', 'food', 'foodSaturation', 'oxygen', 'experienceLevel', 'statusEffects'] },
  { interfaceId: 'inventory_information', schemaRevision: 'inventory-information:1', fields: ['selectedHotbarSlot', 'slots'] },
  { interfaceId: 'sound_information', schemaRevision: 'sound-information:1', fields: ['recentSounds'] },
  { interfaceId: 'viewport_information', schemaRevision: 'viewport-information:4', fields: ['standingOnBlock', 'lookedAtBlock', 'nearbyTrackedEntities', 'visibleBlocks'] },
]

/**
 * The deterministic, single-shot Context Composer: reads a fixed, known-small field set from
 * each passive-observation interface once per decision. Not a model-facing tool loop — see
 * docs/architecture/information-runtime.md section 15 for why that was dropped.
 */
export async function composePassiveObservations(
  runtime: InformationRuntime,
  caller: TrustedInformationCaller,
  signal: AbortSignal,
): Promise<PassiveObservations> {
  const observations: PassiveObservations = { omissions: [] }
  for (const plan of READ_PLAN) {
    const response = await runtime.query(caller, {
      interfaceId: plan.interfaceId, operation: 'read', schemaRevision: plan.schemaRevision, fields: [...plan.fields],
    }, signal)
    if (response.protocol !== 'mineintent.information-read.v1') {
      const reason = response.protocol === 'mineintent.information-error.v1' ? response.code : 'invalid_request'
      observations.omissions.push({ interfaceId: plan.interfaceId, reason })
      continue
    }
    switch (plan.interfaceId) {
      case 'current_status': observations.currentStatus = response.values as unknown as CurrentStatusValues; break
      case 'inventory_information': observations.inventory = response.values as unknown as InventoryValues; break
      case 'sound_information': observations.sound = response.values as unknown as SoundValues; break
      case 'viewport_information': observations.viewport = response.values as unknown as ViewportValues; break
    }
  }
  return observations
}
