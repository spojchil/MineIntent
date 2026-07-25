import assert from 'node:assert/strict'
import test from 'node:test'
import { ToolBridgeServer } from './tool-bridge.js'

test('tool bridge is loopback-only, authenticated and forwards strict invocations', async t => {
  let seen: unknown
  const bridge = new ToolBridgeServer(async invocation => { seen = invocation; return { result: { status: 'completed' } } })
  const address = await bridge.start()
  t.after(() => bridge.stop())
  const unauthorized = await fetch(address.url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' })
  assert.equal(unauthorized.status, 401)
  const response = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: 'call-1', roundId: 0, name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } }),
  })
  assert.equal(response.status, 200)
  // Enveloped: the agent loop has to tell a result from a frame riding alongside it, and a bare
  // result would give it no way to know whether the payload is one or the other.
  assert.deepEqual(await response.json(), { protocol: 'mineintent.tool-response.v1', result: { status: 'completed' } })
  assert.deepEqual(seen, { runId: 'run-1', toolCallId: 'call-1', roundId: 0, name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } })
})

test('a frame rides beside the result rather than inside it', async t => {
  const frame = { at: '2026-07-25T00:00:00.000Z', world: { dimension: 'overworld' }, events: [{ type: 'self.health.dropped', summary: '受到伤害' }], omissions: [] }
  const bridge = new ToolBridgeServer(async () => ({ result: { status: 'queued' }, frame }))
  const address = await bridge.start()
  t.after(() => bridge.stop())

  const response = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: 'call-1', roundId: 0, name: 'say', arguments: { text: '好' } }),
  })

  assert.deepEqual(await response.json(), { protocol: 'mineintent.tool-response.v1', result: { status: 'queued' }, frame })
})
