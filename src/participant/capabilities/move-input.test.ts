import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import { moveArgumentsSchema } from './move-input.js'

test('move contract exposes one simultaneous key set and rejects duplicate keys', () => {
  const parameters = z.toJSONSchema(moveArgumentsSchema) as unknown as {
    properties: {
      directions: Record<string, unknown>
      duration_ms: Record<string, unknown>
    }
    required: string[]
  }
  assert.deepEqual(parameters.required, ['directions', 'duration_ms'])
  assert.deepEqual(parameters.properties.directions, {
    description: '同时按住的移动键，方向相对当前朝向；斜走时把两个键放在这里。',
    minItems: 1,
    maxItems: 4,
    type: 'array',
    items: { type: 'string', enum: ['forward', 'back', 'left', 'right'] },
    uniqueItems: true,
  })
  assert.equal(moveArgumentsSchema.safeParse({
    directions: ['forward', 'back'], duration_ms: 50,
  }).success, true)
  assert.equal(moveArgumentsSchema.safeParse({
    directions: ['forward', 'forward'], duration_ms: 50,
  }).success, false)
})
