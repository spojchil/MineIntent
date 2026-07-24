import assert from 'node:assert/strict'
import test from 'node:test'
import { AgentServiceModelProvider } from './agent-service-client.js'
import type { D40DecisionContext } from './contracts.js'

const transport = {
  serviceToken: 'agent-service-test-token-0123456789',
  toolCallbackUrl: 'http://127.0.0.1:32123/v1/d40/tool',
  toolCallbackToken: '0123456789abcdef',
}

test('agent service receives internal run id, safe context and callback credentials', async () => {
  let body: unknown
  const provider = new AgentServiceModelProvider({ baseUrl: 'http://127.0.0.1:8765', ...transport, fetch: async (_input, init) => {
    body = JSON.parse(String(init?.body))
    assert.equal((init?.headers as Record<string, string>).authorization, `Bearer ${transport.serviceToken}`)
    assert.equal((init?.headers as Record<string, string>)['x-mineintent-tool-executor-token'], transport.toolCallbackToken)
    return new Response(JSON.stringify({
      model: 'deepseek-chat', usage: { inputTokens: 12, outputTokens: 8 },
      decision: { protocol: 'mineintent.d40-decision.v1', speech: '看见了。' },
    }), { status: 200, headers: { 'content-type': 'application/json' } })
  } })
  const result = await provider.run({ runId: 'run-1', context: context() }, new AbortController().signal)
  assert.equal((body as { runId: string }).runId, 'run-1')
  assert.equal(result.decision.speech, '看见了。')
})

test('agent service rejects non-loopback callbacks and surfaces service errors', async () => {
  assert.throws(() => new AgentServiceModelProvider({ baseUrl: 'http://127.0.0.1:8765', ...transport, toolCallbackUrl: 'https://example.com/tool' }))
  assert.throws(() => new AgentServiceModelProvider({ baseUrl: 'https://example.com', ...transport }), /loopback/u)
  assert.throws(() => new AgentServiceModelProvider({ baseUrl: 'http://127.0.0.1:8765', ...transport, serviceToken: '含有非ASCII字符的令牌_012345678901234567890123456789' }), /token/u)
  const provider = new AgentServiceModelProvider({ baseUrl: 'http://127.0.0.1:8765', ...transport, fetch: async () =>
    new Response(JSON.stringify({ error: 'model failed' }), { status: 502 }) })
  await assert.rejects(provider.run({ runId: 'run-1', context: context() }, new AbortController().signal), /model failed/u)
})

test('agent service response is bounded and error text is truncated', async () => {
  const oversized = new AgentServiceModelProvider({
    baseUrl: 'http://127.0.0.1:8765', ...transport,
    fetch: async () => new Response('x'.repeat(65 * 1_024), { status: 502 }),
  })
  await assert.rejects(oversized.run({ runId: 'run-1', context: context() }, new AbortController().signal), /size limit/u)

  const verboseError = new AgentServiceModelProvider({
    baseUrl: 'http://127.0.0.1:8765', ...transport,
    fetch: async () => new Response(JSON.stringify({ error: 'x'.repeat(1_000) }), { status: 502 }),
  })
  await assert.rejects(
    verboseError.run({ runId: 'run-1', context: context() }, new AbortController().signal),
    error => error instanceof Error && error.message.length < 400 && !error.message.includes('x'.repeat(301)),
  )
})

test('aborting a decision notifies the service with the exact run id', async () => {
  const controller = new AbortController()
  let decisionStarted!: () => void
  const started = new Promise<void>(resolve => { decisionStarted = resolve })
  let cancelBody: unknown
  let cancelSeen!: () => void
  const cancelled = new Promise<void>(resolve => { cancelSeen = resolve })
  const provider = new AgentServiceModelProvider({
    baseUrl: 'http://127.0.0.1:8765', ...transport,
    fetch: async (input, init) => {
      const url = new URL(input)
      if (url.pathname === '/v1/cancel') {
        cancelBody = JSON.parse(String(init?.body))
        assert.equal((init?.headers as Record<string, string>).authorization, `Bearer ${transport.serviceToken}`)
        cancelSeen()
        return new Response(JSON.stringify({ cancelled: true }), { status: 200 })
      }
      decisionStarted()
      return await new Promise<Response>((_resolve, reject) => {
        const requestSignal = init?.signal
        const abort = (): void => reject(new DOMException('aborted', 'AbortError'))
        requestSignal?.addEventListener('abort', abort, { once: true })
        if (requestSignal?.aborted === true) abort()
      })
    },
  })

  const running = provider.run({ runId: 'run-to-cancel', context: context() }, controller.signal)
  await started
  controller.abort('world_scope_changed')
  await assert.rejects(running, error => error instanceof DOMException && error.name === 'AbortError')
  await cancelled
  assert.deepEqual(cancelBody, { runId: 'run-to-cancel' })
})

function context(): D40DecisionContext {
  return {
    protocol: 'mineintent.d40-context.v1', player: { username: 'Alex', text: '看看那只羊' },
    profile: { content: '你是伙伴。' }, world: { dimension: 'overworld' }, observations: { omissions: [] },
    recentEvents: [], memories: [],
  }
}
