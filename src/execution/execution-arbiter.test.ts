import assert from 'node:assert/strict'
import test from 'node:test'
import { ExecutionArbiter } from './execution-arbiter.js'

test('leases are per resource, so chat and memory stay free while the body is held', () => {
  const arbiter = new ExecutionArbiter()
  const body = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.ok(!('code' in body))

  // The whole point of `say` existing mid-loop is speaking before an action lands; a body lease
  // must not block it, or the tool description would be teaching an impossible move.
  for (const resource of ['chat', 'memory'] as const) {
    const other = arbiter.acquire({ resource, runId: 'r', toolName: 'say' })
    assert.ok(!('code' in other), `${resource} must not be blocked by the body`)
  }

  const second = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'look_relative' })
  assert.ok('code' in second)
  assert.equal(second.code, 'resource_busy')
  assert.match(second.summary, /body is held by move_input/u)
})

test('a refusal is returned rather than thrown so one conflict cannot kill a run', () => {
  const arbiter = new ExecutionArbiter()
  arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'move_input' })
  // Not assert.throws: the agent must receive this as an ordinary failed tool result.
  const refused = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.ok('code' in refused)
})

test('releasing is idempotent and frees the resource exactly once', () => {
  const arbiter = new ExecutionArbiter()
  const first = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.ok(!('code' in first))
  first.release()
  first.release()

  const second = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'look_relative' })
  assert.ok(!('code' in second))
  // A late release from the stale lease must not evict the live one.
  first.release()
  assert.equal(arbiter.leaseFor('body')?.toolName, 'look_relative')
})

test('a stale release after invalidation cannot evict the lease that replaced it', () => {
  const arbiter = new ExecutionArbiter()
  const stale = arbiter.acquire({ resource: 'body', runId: 'old', toolName: 'move_input' })
  assert.ok(!('code' in stale))

  // Scope loss clears the map without running the closures, so `stale.release()` has never fired.
  arbiter.invalidate('world_scope_changed')
  const live = arbiter.acquire({ resource: 'body', runId: 'new', toolName: 'look_relative' })
  assert.ok(!('code' in live))

  // The abandoned call now unwinds. Without the identity check it would free the new run's body,
  // letting two runs drive the same resource at once.
  stale.release()

  assert.equal(arbiter.leaseFor('body')?.toolName, 'look_relative')
  assert.equal(arbiter.leaseFor('body')?.runId, 'new')
})

test('same-round movement refuses opposing pairs and admits diagonal ones', () => {
  const arbiter = new ExecutionArbiter()
  assert.equal(arbiter.admitMove('r', 0, 'forward'), undefined)
  // Diagonal: exactly what a player does holding W and A together.
  assert.equal(arbiter.admitMove('r', 0, 'left'), undefined)

  const refused = arbiter.admitMove('r', 0, 'back')
  assert.equal(refused?.code, 'opposing_move')
  assert.match(refused!.summary, /back cancels forward/u)

  // A new round is a new decision: the model saw the result and may legitimately reverse.
  assert.equal(arbiter.admitMove('r', 1, 'back'), undefined)
})

test('per-round state is scoped per run and forgotten when the run ends', () => {
  const arbiter = new ExecutionArbiter()
  arbiter.admitMove('run-a', 0, 'forward')
  // Another run's round 0 is unrelated; sharing the map across runs would refuse valid moves.
  assert.equal(arbiter.admitMove('run-b', 0, 'back'), undefined)

  arbiter.forgetRun('run-a')
  assert.equal(arbiter.admitMove('run-a', 0, 'back'), undefined)
})

test('a job hands back a handle immediately and reports its outcome later', () => {
  const arbiter = new ExecutionArbiter()
  const job = arbiter.startJob({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.equal(job.state, 'running')
  assert.deepEqual(arbiter.jobsFor('r'), [{ jobId: job.jobId, state: 'running' }])

  const settled = arbiter.settleJob(job.jobId, 'completed', 'walked 3 blocks')
  assert.deepEqual(settled, { jobId: job.jobId, state: 'completed', summary: 'walked 3 blocks' })
  // Already settled: a later cancel must not rewrite history.
  assert.equal(arbiter.cancelJob(job.jobId)?.state, 'completed')
  assert.equal(arbiter.settleJob('missing-job', 'completed'), undefined)
})

test('cancelling a running job aborts its signal', () => {
  const arbiter = new ExecutionArbiter()
  const job = arbiter.startJob({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.equal(job.controller.signal.aborted, false)
  arbiter.cancelJob(job.jobId)
  assert.equal(job.controller.signal.aborted, true)
})

test('scope loss voids every lease and running job in one step', () => {
  const arbiter = new ExecutionArbiter()
  const held = arbiter.acquire({ resource: 'body', runId: 'r', toolName: 'move_input' })
  assert.ok(!('code' in held))
  const job = arbiter.startJob({ resource: 'body', runId: 'r', toolName: 'move_input' })
  arbiter.admitMove('r', 0, 'forward')
  const before = arbiter.epoch

  arbiter.invalidate('world_scope_changed')

  assert.equal(arbiter.epoch, before + 1)
  assert.equal(arbiter.leaseFor('body'), undefined)
  assert.equal(job.state, 'cancelled')
  assert.equal(job.controller.signal.aborted, true)
  // Round history is void too: the pre-invalidation forward no longer constrains anything.
  assert.equal(arbiter.admitMove('r', 0, 'back'), undefined)
})

test('settled jobs are pruned while running ones survive to be reported', () => {
  const arbiter = new ExecutionArbiter()
  const done = arbiter.startJob({ resource: 'body', runId: 'r', toolName: 'move_input' })
  const running = arbiter.startJob({ resource: 'chat', runId: 'r', toolName: 'say' })
  arbiter.settleJob(done.jobId, 'completed')

  arbiter.pruneSettledJobs()

  assert.deepEqual(arbiter.jobsFor('r'), [{ jobId: running.jobId, state: 'running' }])
})
