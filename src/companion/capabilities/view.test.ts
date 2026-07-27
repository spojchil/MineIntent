import assert from 'node:assert/strict'
import test from 'node:test'
import type { ViewportValues } from '../../information/index.js'
import { z } from 'zod'
import type { CapabilityInvocation, CapabilityScope } from './contracts.js'
import { createViewCapability, viewArgumentsSchema } from './view.js'

const viewport: ViewportValues = {
  frame: {
    coordinates: 'minecraft_world_absolute',
    self: { position: [0, 64, 0], yawDegrees: 0, pitchDegrees: 0 },
    legend: { visibleEntities: 'entities', visibleBlocks: 'blocks' },
  },
  standingOnBlock: { name: 'grass_block', position: [0, 63, 0] },
  lookedAtBlock: null,
  visibleEntities: { items: [], truncated: false },
  visibleBlocks: { blocks: [['grass_block', 0, 63, 0]], truncated: false },
}

const invocation: CapabilityInvocation = {
  runId: 'run', toolCallId: 'call', roundId: 'round', arguments: {},
  actionId: 'action', startedAt: '2026-07-27T00:00:00.000Z',
}

function scope(signal: AbortSignal): CapabilityScope {
  return {
    signal, worldId: 'world', chatEventId: 'chat',
    assertCurrent: () => signal.throwIfAborted(),
    isCurrent: () => !signal.aborted,
  }
}

test('view declares one full read with an empty argument object and its own scan resource', () => {
  const capability = createViewCapability(async () => viewport)
  assert.deepEqual(z.toJSONSchema(viewArgumentsSchema), {
    $schema: 'https://json-schema.org/draft/2020-12/schema',
    type: 'object', properties: {}, additionalProperties: false,
  })
  assert.equal(viewArgumentsSchema.safeParse({ direction: 'north' }).success, false)
  assert.equal(capability.resource, 'viewport')
})

test('view rejects an already-cancelled signal before starting the scan', async () => {
  const controller = new AbortController()
  let reads = 0
  const capability = createViewCapability(async () => {
    reads += 1
    return viewport
  })
  controller.abort('deadline')

  await assert.rejects(async () => capability.execute(invocation, scope(controller.signal)))
  assert.equal(reads, 0)
})
