import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import { InMemoryInformationAccessPolicy } from './access-policy.js'
import type {
  InformationGrant,
  InformationInterfaceId,
  InformationProvider,
  InformationProviderDefinition,
  InformationScopeSnapshot,
  InformationToolResult,
  ProviderReadResult,
} from './contracts/index.js'
import { InformationRegistry } from './registry.js'
import { InformationRuntime } from './runtime.js'
import { MutableInformationScopeSource } from './scope.js'
import { FakeInformationProvider } from './testing/fake-provider.js'
import { assertInformationProviderContract } from './testing/provider-contract.js'
import { InformationTool, InformationToolSession, type InformationRuntimePort } from './tool-session.js'
import { InMemoryInformationTrace } from './trace.js'

interface StatusValues {
  health: number
  food_display: number
}

const initialScope: InformationScopeSnapshot = {
  processSessionId: 'process-1',
  connectionState: 'play',
  connectionEpoch: 2,
  worldId: 'world-1',
  dimension: 'minecraft:overworld',
  uiRevision: 1,
  capturedAt: '2026-07-14T00:00:00.000Z',
}

const participantGrant: InformationGrant = {
  id: 'grant-participant',
  principalId: 'participant-model',
  audience: 'participant',
  allowedInterfaces: '*',
  purpose: 'model_tool',
}

const caller = {
  principalId: 'participant-model',
  grantId: 'grant-participant',
  purpose: 'model_tool' as const,
  correlationId: 'correlation-1',
}

function statusDefinition(
  id: InformationInterfaceId = 'current_status',
  audiences: InformationProviderDefinition<StatusValues>['audiences'] = ['participant'],
): InformationProviderDefinition<StatusValues> {
  return {
    id,
    description: 'Visible current status',
    schemaRevision: `${id}:1`,
    audiences,
    scopeDependencies: ['connection', 'world'],
    fields: {
      health: {
        description: 'Displayed health',
        valueSchema: z.number().min(0).max(20),
        valueType: 'number',
        unit: 'half_heart',
        precision: 'displayed',
        sourceKinds: ['hud_projection'],
      },
      food_display: {
        description: 'Displayed food',
        valueSchema: z.number().int().min(0).max(20),
        valueType: 'number',
        precision: 'exactly_displayed',
        sourceKinds: ['hud_projection'],
      },
    },
    limits: { maxFieldsPerRead: 2, maxResultBytes: 4_096, timeoutMs: 100 },
  }
}

function statusProvider(options: {
  id?: InformationInterfaceId
  audiences?: InformationProviderDefinition<StatusValues>['audiences']
  beforeRead?: () => void
  mutateResult?: (result: ProviderReadResult<StatusValues, never>) => void
} = {}) {
  return new FakeInformationProvider<StatusValues>({
    definition: statusDefinition(options.id, options.audiences),
    availability: () => ({
      overall: 'available',
      informationRevision: 7,
      fields: {},
    }),
    read: async (_context, request) => {
      options.beforeRead?.()
      const values: Partial<StatusValues> = {}
      if (request.fields.includes('health')) values.health = 18
      if (request.fields.includes('food_display')) values.food_display = 14
      const result: ProviderReadResult<StatusValues, never> = {
        informationRevision: 7,
        values,
        unavailable: [],
        source: {
          kind: 'hud_projection',
          adapterRevision: 'fake-status:1',
          sourceRevision: 12,
          acquisition: 'immediate_client_state',
        },
        observedAt: '2026-07-14T00:00:00.000Z',
        evidenceIds: ['evidence-1'],
      }
      options.mutateResult?.(result)
      return result
    },
  })
}

function setup(providers = [statusProvider()]) {
  const registry = new InformationRegistry()
  for (const provider of providers) registry.register(provider)
  registry.seal('1.21.1')
  const policy = new InMemoryInformationAccessPolicy()
  policy.put(participantGrant)
  const scope = new MutableInformationScopeSource(initialScope)
  const trace = new InMemoryInformationTrace()
  const runtime = new InformationRuntime({ registry, accessPolicy: policy, scopeSource: scope, trace })
  return { runtime, registry, policy, scope, trace }
}

function setupSingle<Values extends object, Selector, PageState>(
  provider: InformationProvider<Values, Selector, PageState>,
) {
  const registry = new InformationRegistry()
  registry.register(provider)
  registry.seal('1.21.1')
  const policy = new InMemoryInformationAccessPolicy()
  policy.put(participantGrant)
  return new InformationRuntime({
    registry,
    accessPolicy: policy,
    scopeSource: new MutableInformationScopeSource(initialScope),
  })
}

test('runtime filters catalog by audience and serves Catalog → Help → Read', async () => {
  const participant = statusProvider()
  const diagnostics = statusProvider({ id: 'client_diagnostics', audiences: ['operator'] })
  const { runtime, trace } = setup([participant, diagnostics])

  const catalog = runtime.catalog(caller, { operation: 'list_interfaces' })
  assert.equal(catalog.protocol, 'mineintent.information-catalog.v1')
  assert.equal('interfaces' in catalog ? catalog.interfaces.length : -1, 1)
  assert.equal('interfaces' in catalog ? catalog.interfaces[0]?.id : undefined, 'current_status')

  const help = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'help',
    availability: 'current',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal(help.protocol, 'mineintent.information-help.v1')
  assert.deepEqual('fields' in help ? help.fields.map(({ id }) => id) : [], ['health'])
  const read = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal(read.protocol, 'mineintent.information-read.v1')
  assert.deepEqual('values' in read ? read.values : {}, { health: 18 })
  assert.equal('source' in read ? read.source.sourceRevision : undefined, 12)
  assert.equal(trace.records().length, 1)
  assert.deepEqual(trace.records()[0]?.fields, ['health'])
  assert.equal('values' in trace.records()[0]!, false)
})

test('effective catalog revisions change with grant-visible fields and purpose is bound', async () => {
  const { runtime, policy } = setup()
  policy.put({
    ...participantGrant,
    allowedFields: { current_status: ['health'] },
  })
  const first = runtime.catalog(caller, { operation: 'list_interfaces' })
  assert.equal('status' in first ? first.status : undefined, 'ok')
  const firstRevision = 'catalogRevision' in first ? first.catalogRevision : undefined

  const help = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'help',
  }, new AbortController().signal)
  assert.deepEqual('fields' in help ? help.fields.map(({ id }) => id) : [], ['health'])
  const deniedRead = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['food_display'],
  }, new AbortController().signal)
  assert.equal('code' in deniedRead ? deniedRead.code : undefined, 'audience_denied')

  policy.put({
    ...participantGrant,
    allowedFields: { current_status: ['food_display'] },
  })
  const changed = runtime.catalog(caller, {
    operation: 'list_interfaces',
    knownCatalogRevision: firstRevision,
  })
  assert.equal('status' in changed ? changed.status : undefined, 'ok')
  assert.notEqual('catalogRevision' in changed ? changed.catalogRevision : undefined, firstRevision)

  const wrongPurpose = runtime.catalog({ ...caller, purpose: 'operator' }, {
    operation: 'list_interfaces',
  })
  assert.equal('code' in wrongPurpose ? wrongPurpose.code : undefined, 'audience_denied')
})

test('runtime rejects stale schemas, unknown fields and forged scope input', async () => {
  const { runtime } = setup()
  const stale = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'old',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal('code' in stale ? stale.code : undefined, 'stale_schema')

  const unknown = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['saturation'],
  }, new AbortController().signal)
  assert.equal('code' in unknown ? unknown.code : undefined, 'unknown_field')

  const forged = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'help',
    worldId: 'forged',
  }, new AbortController().signal)
  assert.equal('code' in forged ? forged.code : undefined, 'invalid_request')
})

test('runtime preserves partial reads without filling unavailable fields', async () => {
  const partial = new FakeInformationProvider<StatusValues>({
    definition: statusDefinition(),
    availability: () => ({
      overall: 'partially_available',
      informationRevision: 8,
      fields: { food_display: 'not_currently_displayed' },
    }),
    read: async () => ({
      informationRevision: 8,
      values: { health: 18 },
      unavailable: [{ field: 'food_display', reason: 'not_currently_displayed' }],
      source: {
        kind: 'hud_projection',
        adapterRevision: 'fake-partial:1',
        sourceRevision: 13,
        acquisition: 'immediate_client_state',
      },
      observedAt: initialScope.capturedAt,
      evidenceIds: [],
    }),
  })
  const result = await setup([partial]).runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health', 'food_display'],
  }, new AbortController().signal)
  assert.deepEqual('values' in result ? result.values : {}, { health: 18 })
  assert.deepEqual('unavailable' in result ? result.unavailable : [], [
    { field: 'food_display', reason: 'not_currently_displayed' },
  ])
})

test('runtime discards provider leaks and reads racing a scope change', async () => {
  const leaking = statusProvider({
    mutateResult: (result) => {
      Object.assign(result.values, { hidden_saturation: 4.2 })
    },
  })
  const leaked = await setup([leaking]).runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal('code' in leaked ? leaked.code : undefined, 'provider_failed')

  const scope = new MutableInformationScopeSource(initialScope)
  const racing = statusProvider({
    beforeRead: () => scope.update({
      ...initialScope,
      connectionEpoch: 3,
      capturedAt: '2026-07-14T00:00:01.000Z',
    }),
  })
  const registry = new InformationRegistry()
  registry.register(racing)
  registry.seal('1.21.1')
  const policy = new InMemoryInformationAccessPolicy()
  policy.put(participantGrant)
  const runtime = new InformationRuntime({ registry, accessPolicy: policy, scopeSource: scope })
  const raced = await runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal('code' in raced ? raced.code : undefined, 'scope_changed')
})

test('runtime rebuilds nested fields from parsed Zod data and enforces declared sources', async () => {
  interface NestedValues { health: { current: number } }
  const definition = {
    id: 'current_status',
    description: 'Nested visible status',
    schemaRevision: 'nested:1',
    audiences: ['participant'],
    scopeDependencies: ['connection'],
    fields: {
      health: {
        description: 'Visible health object',
        valueSchema: z.object({ current: z.number() }),
        valueType: 'object',
        precision: 'displayed',
        sourceKinds: ['hud_projection'],
      },
    },
    limits: { maxFieldsPerRead: 1, maxResultBytes: 4_096, timeoutMs: 100 },
  } satisfies InformationProviderDefinition<NestedValues>
  const nested = new FakeInformationProvider<NestedValues>({
    definition,
    availability: () => ({ overall: 'available', informationRevision: 1, fields: {} }),
    read: async () => ({
      informationRevision: 1,
      values: { health: { current: 18, hiddenSaturation: 4.2 } },
      unavailable: [],
      source: {
        kind: 'hud_projection',
        adapterRevision: 'nested:1',
        sourceRevision: 1,
        acquisition: 'immediate_client_state',
      },
      observedAt: initialScope.capturedAt,
      evidenceIds: [],
    }),
  })
  const cleaned = await setupSingle(nested).query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'nested:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.deepEqual('values' in cleaned ? cleaned.values : {}, { health: { current: 18 } })

  const wrongSource = new FakeInformationProvider<NestedValues>({
    definition,
    availability: () => ({ overall: 'available', informationRevision: 1, fields: {} }),
    read: async () => ({
      informationRevision: 1,
      values: { health: { current: 18 } },
      unavailable: [],
      source: {
        kind: 'operator_diagnostic',
        adapterRevision: 'nested:1',
        sourceRevision: 1,
        acquisition: 'operator_only',
      },
      observedAt: initialScope.capturedAt,
      evidenceIds: [],
    }),
  })
  const rejected = await setupSingle(wrongSource).query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'nested:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal('code' in rejected ? rejected.code : undefined, 'provider_failed')
})

test('runtime aborts the provider when its deadline elapses', async () => {
  let aborted = false
  const definition = {
    ...statusDefinition(),
    limits: { maxFieldsPerRead: 2, maxResultBytes: 4_096, timeoutMs: 10 },
  }
  const slow = new FakeInformationProvider<StatusValues>({
    definition,
    availability: () => ({ overall: 'available', informationRevision: 1, fields: {} }),
    read: async (_context, _request, signal) => new Promise((_resolve, reject) => {
      signal.addEventListener('abort', () => {
        aborted = true
        reject(new Error('aborted'))
      }, { once: true })
    }),
  })
  const result = await setup([slow]).runtime.query(caller, {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, new AbortController().signal)
  assert.equal('code' in result ? result.code : undefined, 'deadline_exceeded')
  assert.equal(aborted, true)
})

test('the reusable provider contract validates a legal provider fixture', async () => {
  const provider = statusProvider()
  await assertInformationProviderContract(provider, {
    context: {
      now: initialScope.capturedAt,
      scope: initialScope,
      caller: { audience: 'participant', purpose: 'model_tool' },
      refs: { issue: () => { throw new Error('not used') } },
    },
    request: { fields: ['health'], page: { limit: 1 } },
  })
})

test('tool sessions enforce read-call and byte budgets before returning results', async () => {
  let calls = 0
  const port: InformationRuntimePort = {
    catalog: () => ({
      protocol: 'mineintent.information-catalog.v1',
      status: 'not_modified',
      catalogRevision: 'catalog:1',
    }),
    query: async (): Promise<InformationToolResult> => {
      calls += 1
      return {
        protocol: 'mineintent.information-error.v1',
        code: 'unknown_field',
        message: 'x',
      }
    },
  }
  const tool = new InformationTool(port)
  const session = new InformationToolSession({
    sessionId: 'session-1',
    decisionRunId: 'run-1',
    correlationId: 'correlation-1',
    principalId: 'participant-model',
    grantId: 'grant-participant',
    budget: {
      maxCalls: 2,
      maxReadCalls: 1,
      maxReturnedBytes: 1_024,
      deadlineAt: '2099-01-01T00:00:00.000Z',
    },
  })
  const request = {
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }
  await tool.invoke(request, session, new AbortController().signal)
  const rejected = await tool.invoke(request, session, new AbortController().signal)
  assert.equal('code' in rejected ? rejected.code : undefined, 'budget_exceeded')
  assert.equal(calls, 1)

  const byteLimited = new InformationToolSession({
    sessionId: 'session-2',
    decisionRunId: 'run-2',
    correlationId: 'correlation-2',
    principalId: 'participant-model',
    grantId: 'grant-participant',
    budget: {
      maxCalls: 1,
      maxReadCalls: 1,
      maxReturnedBytes: 1,
      deadlineAt: '2099-01-01T00:00:00.000Z',
    },
  })
  const byteRejected = await tool.invoke(request, byteLimited, new AbortController().signal)
  assert.equal('code' in byteRejected ? byteRejected.code : undefined, 'budget_exceeded')
})

test('tool session deadline aborts a read already in progress', async () => {
  let aborted = false
  const port: InformationRuntimePort = {
    catalog: () => ({
      protocol: 'mineintent.information-catalog.v1',
      status: 'not_modified',
      catalogRevision: 'catalog:1',
    }),
    query: async (_caller, _request, signal) => new Promise((resolve) => {
      signal.addEventListener('abort', () => {
        aborted = true
        resolve({
          protocol: 'mineintent.information-error.v1',
          code: 'deadline_exceeded',
          message: 'deadline',
        })
      }, { once: true })
    }),
  }
  const session = new InformationToolSession({
    sessionId: 'session-deadline',
    decisionRunId: 'run-deadline',
    correlationId: 'correlation-deadline',
    principalId: 'participant-model',
    grantId: 'grant-participant',
    budget: {
      maxCalls: 1,
      maxReadCalls: 1,
      maxReturnedBytes: 1_024,
      deadlineAt: new Date(Date.now() + 20).toISOString(),
    },
  })
  const result = await new InformationTool(port).invoke({
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, session, new AbortController().signal)
  assert.equal('code' in result ? result.code : undefined, 'deadline_exceeded')
  assert.equal(aborted, true)
})

test('tool session forwards upstream cancellation to a read already in progress', async () => {
  let abortReason: unknown
  const port: InformationRuntimePort = {
    catalog: () => ({
      protocol: 'mineintent.information-catalog.v1',
      status: 'not_modified',
      catalogRevision: 'catalog:1',
    }),
    query: async (_caller, _request, signal) => new Promise((resolve) => {
      signal.addEventListener('abort', () => {
        abortReason = signal.reason
        resolve({
          protocol: 'mineintent.information-error.v1',
          code: 'deadline_exceeded',
          message: 'cancelled',
        })
      }, { once: true })
    }),
  }
  const session = new InformationToolSession({
    sessionId: 'session-upstream-abort',
    decisionRunId: 'run-upstream-abort',
    correlationId: 'correlation-upstream-abort',
    principalId: 'participant-model',
    grantId: 'grant-participant',
    budget: {
      maxCalls: 1,
      maxReadCalls: 1,
      maxReturnedBytes: 1_024,
      deadlineAt: new Date(Date.now() + 60_000).toISOString(),
    },
  })
  const upstream = new AbortController()
  const expectedReason = new Error('caller cancelled')
  const pending = new InformationTool(port).invoke({
    interfaceId: 'current_status',
    operation: 'read',
    schemaRevision: 'current_status:1',
    fields: ['health'],
  }, session, upstream.signal)
  upstream.abort(expectedReason)

  const result = await pending
  assert.equal('code' in result ? result.code : undefined, 'deadline_exceeded')
  assert.equal(abortReason, expectedReason)
})
