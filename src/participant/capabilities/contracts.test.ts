import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import { ToolCapabilityRegistry, type ToolCapability } from './contracts.js'

function capability(name: string): ToolCapability<'body'> {
  return {
    name, description: `description:${name}`, resource: 'body',
    argumentsSchema: z.strictObject({ value: z.string() }),
    execute: () => ({ status: 'completed' }),
  }
}

test('one registration produces both the advertised contract and dispatch entry', () => {
  const first = capability('first')
  const second = capability('second')
  const registry = new ToolCapabilityRegistry([first, second])

  assert.deepEqual(registry.definitions().map(definition => definition.function.name), ['first', 'second'])
  assert.equal(registry.resolve('first'), first)
  assert.equal(registry.resolve('second'), second)
  assert.equal(registry.resolve('absent'), undefined)
})

test('duplicate capability names fail while the registry is constructed', () => {
  assert.throws(
    () => new ToolCapabilityRegistry([capability('same'), capability('same')]),
    /duplicate_tool_capability/u,
  )
})
