import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import { InMemoryInformationAccessPolicy } from './access-policy.js'
import {
  informationSelectorRefSchema,
  type InformationGrant,
  type InformationProviderDefinition,
  type InformationScopeSnapshot,
  type InformationSelectorRef,
} from './contracts/index.js'
import { InformationRegistry } from './registry.js'
import { InformationRuntime } from './runtime.js'
import { MutableInformationScopeSource } from './scope.js'
import { FakeInformationProvider } from './testing/fake-provider.js'

const scope: InformationScopeSnapshot = {
  processSessionId: 'process-refs',
  connectionState: 'play',
  connectionEpoch: 8,
  worldId: 'world-refs',
  dimension: 'minecraft:overworld',
  uiRevision: 1,
  capturedAt: '2026-07-14T00:00:00.000Z',
}

const grant: InformationGrant = {
  id: 'grant-refs',
  principalId: 'model-refs',
  audience: 'companion',
  allowedInterfaces: '*',
  purpose: 'model_tool',
}

const caller = {
  principalId: 'model-refs',
  grantId: 'grant-refs',
  purpose: 'model_tool' as const,
  correlationId: 'correlation-refs',
}

function runtimeWith(...providers: Array<FakeInformationProvider<object, unknown, unknown>>) {
  const registry = new InformationRegistry()
  for (const provider of providers) registry.register(provider)
  registry.seal('1.21.1')
  const policy = new InMemoryInformationAccessPolicy()
  policy.put(grant)
  return new InformationRuntime({
    registry,
    accessPolicy: policy,
    scopeSource: new MutableInformationScopeSource(scope),
  })
}

test('runtime keeps observed context refs while rejecting stale or wrongly targeted selectors', async () => {
  let inventoryRevision = 2
  let changeSourceDuringTooltipRead = false
  interface InventoryValues { item_refs: InformationSelectorRef[] }
  const inventoryDefinition = {
    id: 'inventory_information',
    description: 'Visible inventory items',
    schemaRevision: 'inventory:1',
    audiences: ['companion'],
    scopeDependencies: ['connection', 'world'],
    fields: {
      item_refs: {
        description: 'Opaque references to visible items',
        valueSchema: z.array(informationSelectorRefSchema),
        valueType: 'array',
        precision: 'exactly_displayed',
        sourceKinds: ['screen_projection'],
      },
    },
    limits: { maxFieldsPerRead: 1, maxResultBytes: 4_096, timeoutMs: 100 },
  } satisfies InformationProviderDefinition<InventoryValues>
  const inventory = new FakeInformationProvider<InventoryValues>({
    definition: inventoryDefinition,
    availability: () => ({ overall: 'available', informationRevision: inventoryRevision, fields: {} }),
    read: async (context) => ({
      informationRevision: 2,
      values: {
        item_refs: [context.refs.issue({
          kind: 'item',
          payload: { slot: 2 },
          allowedInterfaces: ['item_tooltip_information'],
          basedOnInformationRevision: 2,
        })],
      },
      unavailable: [],
      source: {
        kind: 'screen_projection',
        adapterRevision: 'fake-inventory:1',
        sourceRevision: 3,
        acquisition: 'structured_ui_equivalent',
      },
      observedAt: scope.capturedAt,
      evidenceIds: [],
    }),
  })

  interface TooltipValues { display_name: string }
  const tooltipDefinition = {
    id: 'item_tooltip_information',
    description: 'Visible item tooltip',
    schemaRevision: 'tooltip:1',
    audiences: ['companion'],
    scopeDependencies: ['connection', 'world'],
    selectors: { required: true, acceptsKinds: ['item'] },
    fields: {
      display_name: {
        description: 'Displayed item name',
        valueSchema: z.string(),
        valueType: 'string',
        precision: 'exactly_displayed',
        sourceKinds: ['screen_projection'],
      },
    },
    limits: { maxFieldsPerRead: 1, maxResultBytes: 4_096, timeoutMs: 100 },
  } satisfies InformationProviderDefinition<TooltipValues>
  const tooltip = new FakeInformationProvider<TooltipValues, { slot: number }>({
    definition: tooltipDefinition,
    availability: () => ({ overall: 'available', informationRevision: 1, fields: {} }),
    read: async (_context, request) => {
      if (changeSourceDuringTooltipRead) inventoryRevision += 1
      return {
        informationRevision: 1,
        values: { display_name: request.selector?.slot === 2 ? 'Oak Log' : 'unexpected' },
        unavailable: [],
        source: {
          kind: 'screen_projection',
          adapterRevision: 'fake-tooltip:1',
          sourceRevision: 1,
          acquisition: 'structured_ui_equivalent',
        },
        observedAt: scope.capturedAt,
        evidenceIds: [],
      }
    },
  })

  const runtime = runtimeWith(
    inventory as unknown as FakeInformationProvider<object, unknown, unknown>,
    tooltip as unknown as FakeInformationProvider<object, unknown, unknown>,
  )
  const inventoryRead = await runtime.query(caller, {
    interfaceId: 'inventory_information',
    operation: 'read',
    schemaRevision: 'inventory:1',
    fields: ['item_refs'],
  }, new AbortController().signal)
  assert.equal(inventoryRead.protocol, 'mineintent.information-read.v1')
  const ref = 'values' in inventoryRead
    ? (inventoryRead.values.item_refs as InformationSelectorRef[])[0]
    : undefined
  assert.ok(ref)

  assert.deepEqual(runtime.resolveContextReference<{ slot: number }>(caller, ref.id, ['item'])?.payload, { slot: 2 })
  assert.equal(runtime.resolveContextReference(caller, ref.id, ['viewport.block']), undefined)

  const tooltipRead = await runtime.query(caller, {
    interfaceId: 'item_tooltip_information',
    operation: 'read',
    schemaRevision: 'tooltip:1',
    fields: ['display_name'],
    selector: ref,
  }, new AbortController().signal)
  assert.deepEqual('values' in tooltipRead ? tooltipRead.values : {}, { display_name: 'Oak Log' })

  changeSourceDuringTooltipRead = true
  const stale = await runtime.query(caller, {
    interfaceId: 'item_tooltip_information',
    operation: 'read',
    schemaRevision: 'tooltip:1',
    fields: ['display_name'],
    selector: ref,
  }, new AbortController().signal)
  assert.equal('code' in stale ? stale.code : undefined, 'invalid_selector')
  assert.deepEqual(runtime.resolveContextReference<{ slot: number }>(caller, ref.id, ['item'])?.payload, { slot: 2 })

  const wrongTarget = await runtime.query(caller, {
    interfaceId: 'inventory_information',
    operation: 'read',
    schemaRevision: 'inventory:1',
    fields: ['item_refs'],
    selector: ref,
  }, new AbortController().signal)
  assert.equal('code' in wrongTarget ? wrongTarget.code : undefined, 'invalid_selector')
})

test('runtime issues and consumes scope-bound continuation cursors', async () => {
  interface PagedValues { entries: number[] }
  interface PageState { offset: number }
  const definition = {
    id: 'inventory_information',
    description: 'Paged visible entries',
    schemaRevision: 'paged:1',
    audiences: ['companion'],
    scopeDependencies: ['connection', 'world'],
    pagination: { defaultLimit: 2, maxLimit: 2 },
    fields: {
      entries: {
        description: 'Visible entries',
        valueSchema: z.array(z.number().int()),
        valueType: 'array',
        precision: 'exactly_displayed',
        sourceKinds: ['client_state'],
      },
    },
    limits: { maxFieldsPerRead: 1, maxResultBytes: 4_096, timeoutMs: 100 },
  } satisfies InformationProviderDefinition<PagedValues>
  const provider = new FakeInformationProvider<PagedValues, never, PageState>({
    definition,
    availability: () => ({ overall: 'available', informationRevision: 5, fields: {} }),
    read: async (_context, request) => {
      const offset = request.page.state?.offset ?? 0
      return {
        informationRevision: 5,
        values: { entries: [0, 1, 2, 3].slice(offset, offset + request.page.limit) },
        unavailable: [],
        source: {
          kind: 'client_state',
          adapterRevision: 'fake-page:1',
          sourceRevision: 5,
          acquisition: 'immediate_client_state',
        },
        observedAt: scope.capturedAt,
        evidenceIds: [],
        ...(offset + request.page.limit < 4
          ? { nextPageState: { offset: offset + request.page.limit } }
          : {}),
      }
    },
  })
  const runtime = runtimeWith(
    provider as unknown as FakeInformationProvider<object, unknown, unknown>,
  )
  const first = await runtime.query(caller, {
    interfaceId: 'inventory_information',
    operation: 'read',
    schemaRevision: 'paged:1',
    fields: ['entries'],
    page: { limit: 2 },
  }, new AbortController().signal)
  assert.deepEqual('values' in first ? first.values : {}, { entries: [0, 1] })
  const cursor = 'nextCursor' in first ? first.nextCursor : undefined
  assert.ok(cursor)

  const second = await runtime.query(caller, {
    interfaceId: 'inventory_information',
    operation: 'read',
    schemaRevision: 'paged:1',
    fields: ['entries'],
    page: { cursor, limit: 2 },
  }, new AbortController().signal)
  assert.deepEqual('values' in second ? second.values : {}, { entries: [2, 3] })
  assert.equal('nextCursor' in second ? second.nextCursor : undefined, undefined)
})
