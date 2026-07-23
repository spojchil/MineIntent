import assert from 'node:assert/strict'
import test from 'node:test'
import { D40ToolBridgeServer } from './tool-bridge.js'

test('tool bridge is loopback-only, authenticated and forwards strict invocations', async t => {
  let seen: unknown
  const bridge = new D40ToolBridgeServer(async invocation => { seen = invocation; return { status: 'completed' } })
  const address = await bridge.start()
  t.after(() => bridge.stop())
  const unauthorized = await fetch(address.url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' })
  assert.equal(unauthorized.status, 401)
  const response = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } }),
  })
  assert.equal(response.status, 200)
  assert.deepEqual(await response.json(), { status: 'completed' })
  assert.deepEqual(seen, { runId: 'run-1', name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } })
})
