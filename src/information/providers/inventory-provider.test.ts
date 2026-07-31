import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { InventoryPort, InventoryStateSnapshot } from '../source-ports/inventory.js'
import { assertInformationProviderContract } from '../testing/provider-contract.js'
import { InventoryProvider } from './inventory-provider.js'

class FakeInventoryPort implements InventoryPort {
  constructor(public state: InventoryStateSnapshot) {}
  current(): InventoryStateSnapshot { return this.state }
}

function context() {
  return {
    now: new Date().toISOString(),
    scope: { processSessionId: 's', connectionState: 'play' as const, connectionEpoch: 1, uiRevision: 0, capturedAt: new Date().toISOString() },
    caller: { audience: 'participant' as const, purpose: 'participant_context' as const },
    refs: { issue: () => { throw new Error('not used') } },
  }
}

test('inventory provider satisfies the provider contract', async () => {
  const port = new FakeInventoryPort({ selectedHotbarSlot: 0, slots: [{ slot: 9, itemName: 'oak_log', count: 4 }] })
  const provider = new InventoryProvider(port)
  await assertInformationProviderContract(provider, {
    context: context(),
    request: { fields: ['selectedHotbarSlot', 'slots'], page: { limit: 1 } },
  })
})

test('inventory provider reports current slots and selected hotbar slot', async () => {
  const port = new FakeInventoryPort({ selectedHotbarSlot: 3, slots: [{ slot: 36, itemName: 'stone', count: 64 }] })
  const provider = new InventoryProvider(port)
  const result = await provider.read(context(), { fields: ['selectedHotbarSlot', 'slots'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.selectedHotbarSlot, 3)
  assert.deepEqual(result.values.slots, [{ slot: 36, itemName: 'stone', count: 64 }])
})
