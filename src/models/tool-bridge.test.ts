import assert from 'node:assert/strict'
import test from 'node:test'
import { ToolBridgeServer } from './tool-bridge.js'

test('tool bridge is loopback-only, authenticated and forwards strict invocations', async t => {
  let seen: unknown
  const bridge = new ToolBridgeServer(async invocation => {
    seen = invocation
    return { result: { status: 'completed' }, observationAfter: null }
  })
  const address = await bridge.start()
  t.after(() => bridge.stop())
  const unauthorized = await fetch(address.url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' })
  assert.equal(unauthorized.status, 401)
  const response = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: 'call-1', name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } }),
  })
  assert.equal(response.status, 200)
  assert.deepEqual(await response.json(), {
    protocol: 'mineintent.tool-response.v2', result: { status: 'completed' }, observationAfter: null,
  })
  assert.deepEqual(seen, { runId: 'run-1', toolCallId: 'call-1', name: 'look_relative', arguments: { yaw_degrees: 10, pitch_degrees: 0 } })

  const legacyRound = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: 'call-2', round: { new: true }, name: 'say', arguments: { text: '好' } }),
  })
  assert.equal(legacyRound.status, 400)

  const nonAsciiCallId = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: '😀'.repeat(65), name: 'say', arguments: { text: '好' } }),
  })
  assert.equal(nonAsciiCallId.status, 400)
})

test('the tool response carries a post-handling observation without claiming causation', async t => {
  const observationAfter = { at: '2026-07-25T00:00:00.000Z', world: { dimension: 'overworld' }, events: [{ type: 'self.health.dropped', summary: '受到伤害' }], omissions: [] }
  const bridge = new ToolBridgeServer(async () => ({ result: { status: 'queued' }, observationAfter }))
  const address = await bridge.start()
  t.after(() => bridge.stop())

  const response = await fetch(address.url, {
    method: 'POST', headers: { authorization: `Bearer ${address.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runId: 'run-1', toolCallId: 'call-1', name: 'say', arguments: { text: '好' } }),
  })

  assert.deepEqual(await response.json(), {
    protocol: 'mineintent.tool-response.v2', result: { status: 'queued' }, observationAfter,
  })
})
