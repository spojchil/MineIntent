import assert from 'node:assert/strict'
import test from 'node:test'
import { MemoryIntegrationRecorder } from './recorder.js'
import { PaperScenarioRunner } from './scenario-runner.js'

test('scenario records setup, participant behavior and cleanup as distinct phases', async () => {
  const recorder = new MemoryIntegrationRecorder()
  const result = await new PaperScenarioRunner(recorder).run({
    name: 'movement', timeoutMs: 100,
    setup: async ctx => ctx.record('setup', 'command', 'build fixture'),
    run: async ctx => ctx.record('participant', 'action', 'walk'),
    cleanup: async ctx => ctx.record('cleanup', 'command', 'remove fixture'),
  })
  assert.equal(result.status, 'passed')
  assert.deepEqual(recorder.records.map(record => record.phase), ['harness', 'setup', 'participant', 'cleanup', 'harness'])
})

test('timeout still runs cleanup and records a terminal result', async () => {
  const recorder = new MemoryIntegrationRecorder()
  let cleaned = false
  const result = await new PaperScenarioRunner(recorder).run({
    name: 'timeout', timeoutMs: 5, setup: async () => {},
    run: async ctx => new Promise<void>((_resolve, reject) => ctx.signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true })),
    cleanup: async () => { cleaned = true },
  })
  assert.equal(result.status, 'timed_out')
  assert.equal(cleaned, true)
  assert.equal(recorder.records.at(-1)?.type, 'scenario_terminal')
})
