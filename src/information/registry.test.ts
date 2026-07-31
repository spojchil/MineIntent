import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import type { InformationInterfaceId, InformationProviderDefinition } from './contracts/index.js'
import { InformationRegistry } from './registry.js'
import { FakeInformationProvider } from './testing/fake-provider.js'

interface RegistryValues {
  value: number
}

function provider(id: InformationInterfaceId) {
  const definition = {
    id,
    description: `Provider ${id}`,
    schemaRevision: `${id}:1`,
    audiences: ['participant'],
    scopeDependencies: ['connection'],
    fields: {
      value: {
        description: 'Visible value',
        valueSchema: z.number(),
        valueType: 'number',
        precision: 'displayed',
        sourceKinds: ['client_state'],
      },
    },
    limits: { maxFieldsPerRead: 1, maxResultBytes: 1_024, timeoutMs: 100 },
  } satisfies InformationProviderDefinition<RegistryValues>
  return new FakeInformationProvider<RegistryValues>({
    definition,
    availability: () => ({ overall: 'available', informationRevision: 1, fields: {} }),
    read: async () => ({
      informationRevision: 1,
      values: { value: 1 },
      unavailable: [],
      source: {
        kind: 'client_state',
        adapterRevision: 'fake:1',
        sourceRevision: 1,
        acquisition: 'immediate_client_state',
      },
      observedAt: '2026-07-14T00:00:00.000Z',
      evidenceIds: [],
    }),
  })
}

test('registry is deterministic, sealed and rejects duplicate providers', () => {
  const left = new InformationRegistry()
  left.register(provider('current_status'))
  left.register(provider('ui_context'))
  left.seal('1.21.1')

  const right = new InformationRegistry()
  right.register(provider('ui_context'))
  right.register(provider('current_status'))
  right.seal('1.21.1')

  assert.equal(left.catalogRevision(), right.catalogRevision())
  assert.deepEqual(left.descriptors().map(({ id }) => id), ['current_status', 'ui_context'])
  assert.throws(() => left.register(provider('f3_information')), /sealed/)

  const duplicate = new InformationRegistry()
  duplicate.register(provider('current_status'))
  assert.throws(() => duplicate.register(provider('current_status')), /Duplicate/)
  assert.throws(() => duplicate.provider('current_status'), /sealed/)
})
