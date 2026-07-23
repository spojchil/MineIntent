import assert from 'node:assert/strict'
import { test } from 'node:test'
import { DebugStateStore, redactSensitive } from './debug-state.js'
import { LocalDebugServer } from './debug-server.js'

test('debug state is immutable, bounded and redacts sensitive values', () => {
  const store = new DebugStateStore()
  for (let index = 0; index < 12; index++) {
    store.failure({ at: new Date(index).toISOString(), source: 'runtime', code: `E${index}`, summary: 'safe' })
  }
  const snapshot = store.snapshot()
  assert.equal(snapshot.recentFailures.length, 10)
  assert.equal(snapshot.recentFailures[0]?.code, 'E2')
  assert.equal(Object.isFrozen(snapshot), true)
  assert.deepEqual(redactSensitive({ apiKey: 'sk-secret-value-long', nested: { authorization: 'Bearer abcdefghijklmnop', raw: 'private' } }), {
    apiKey: '[REDACTED]', nested: { authorization: '[REDACTED]', raw: '[REDACTED]' },
  })
})

test('local debug server only permits read-only GET routes', async () => {
  const store = new DebugStateStore()
  store.update({ currentBodyTool: { id: 'body-1', tool: 'move_input', purpose: 'D40 short input', startedAt: new Date(0).toISOString() } })
  const server = new LocalDebugServer(store, 0)
  const address = await server.start()
  try {
    const state = await fetch(`${address.url}/v1/state`)
    assert.equal(state.status, 200)
    assert.equal((await state.json() as { currentBodyTool: { tool: string } }).currentBodyTool.tool, 'move_input')
    const rejected = await fetch(`${address.url}/v1/state`, { method: 'POST', body: '{}' })
    assert.equal(rejected.status, 405)
    assert.deepEqual(await rejected.json(), { error: 'read_only' })
  } finally {
    await server.stop()
  }
})
