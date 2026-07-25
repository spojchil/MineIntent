import { randomUUID } from 'node:crypto'
import type { ExecutionRefusal, ExecutionResource, JobOutcome, JobState, ResourceLease } from './contracts.js'

/** Movement inputs that cancel each other when the model asks for them in the same round. */
const OPPOSING_DIRECTION: Record<string, string> = {
  forward: 'back', back: 'forward', left: 'right', right: 'left',
}

interface RoundMoves { roundId: number; directions: Set<string> }

interface Job {
  jobId: string
  resource: ExecutionResource
  runId: string
  toolName: string
  state: JobState
  controller: AbortController
  startedAt: string
}

/**
 * Arbitrates access to the body, chat and memory, and tracks the jobs that hold them.
 *
 * Every refusal is returned, never thrown: a held resource is a fact about the world, and a tool
 * that throws here would surface as a bridge 500 and destroy a whole run over a transient
 * conflict. The agent decides what to do about a refusal — wait, cancel, or do something else —
 * because that is a judgement; this layer only reports what is true.
 */
export class ExecutionArbiter {
  readonly #leases = new Map<ExecutionResource, ResourceLease>()
  readonly #jobs = new Map<string, Job>()
  readonly #roundMoves = new Map<string, RoundMoves>()
  #epoch = 0

  /** Bumped by the entry layer on scope loss; every lease and job from an older epoch is void. */
  invalidate(reason: string): void {
    this.#epoch += 1
    for (const job of this.#jobs.values()) {
      if (job.state === 'running') {
        job.state = 'cancelled'
        job.controller.abort(reason)
      }
    }
    this.#leases.clear()
    this.#roundMoves.clear()
  }

  get epoch(): number { return this.#epoch }

  leaseFor(resource: ExecutionResource): ResourceLease | undefined { return this.#leases.get(resource) }

  /**
   * Reserves a resource for one call. Returns a refusal instead of throwing so the caller can hand
   * the model an honest failed tool result.
   */
  acquire(input: {
    resource: ExecutionResource
    runId: string
    toolName: string
  }): { lease: ResourceLease; release: () => void } | ExecutionRefusal {
    const held = this.#leases.get(input.resource)
    if (held !== undefined) {
      return {
        code: 'resource_busy',
        summary: `resource_busy:${input.resource} is held by ${held.toolName}`,
      }
    }
    const lease: ResourceLease = {
      resource: input.resource, actionId: randomUUID(), runId: input.runId,
      toolName: input.toolName, acquiredAt: new Date().toISOString(),
    }
    this.#leases.set(input.resource, lease)
    let released = false
    return {
      lease,
      release: () => {
        if (released) return
        released = true
        if (this.#leases.get(input.resource) === lease) this.#leases.delete(input.resource)
      },
    }
  }

  /**
   * Judges whether a movement input contradicts one the model already asked for in this round.
   *
   * Same-round is the meaningful unit: those calls arrived in one model response, so the model
   * meant them together. `forward` plus `left` is how anyone walks diagonally; `forward` plus
   * `back` is not an action any player performs. A different round is a new decision and starts
   * from a clean slate.
   */
  admitMove(runId: string, roundId: number, direction: string): ExecutionRefusal | undefined {
    const current = this.#roundMoves.get(runId)
    const round = current !== undefined && current.roundId === roundId
      ? current
      : { roundId, directions: new Set<string>() }
    this.#roundMoves.set(runId, round)
    const opposing = OPPOSING_DIRECTION[direction]
    if (opposing !== undefined && round.directions.has(opposing)) {
      return {
        code: 'opposing_move',
        summary: `opposing_move:${direction} cancels ${opposing} already held this round`,
      }
    }
    round.directions.add(direction)
    return undefined
  }

  /** Forgets per-round movement state for a finished run so the map cannot grow unbounded. */
  forgetRun(runId: string): void { this.#roundMoves.delete(runId) }

  /**
   * Registers continuous work that outlives the call which started it. The agent gets a handle
   * immediately, exactly like a background shell command, and learns the outcome later rather than
   * blocking on it.
   */
  startJob(input: { resource: ExecutionResource; runId: string; toolName: string }): Job {
    const job: Job = {
      jobId: randomUUID(), resource: input.resource, runId: input.runId, toolName: input.toolName,
      state: 'running', controller: new AbortController(), startedAt: new Date().toISOString(),
    }
    this.#jobs.set(job.jobId, job)
    return job
  }

  settleJob(jobId: string, state: Exclude<JobState, 'running'>, summary?: string): JobOutcome | undefined {
    const job = this.#jobs.get(jobId)
    if (job === undefined) return undefined
    if (job.state === 'running') job.state = state
    if (state === 'cancelled') job.controller.abort('job_cancelled')
    return { jobId, state: job.state, ...(summary === undefined ? {} : { summary }) }
  }

  cancelJob(jobId: string): JobOutcome | undefined { return this.settleJob(jobId, 'cancelled') }

  jobsFor(runId: string): JobOutcome[] {
    return [...this.#jobs.values()]
      .filter(job => job.runId === runId)
      .map(job => ({ jobId: job.jobId, state: job.state }))
  }

  /** Drops settled jobs; running ones are kept so their outcome can still be reported. */
  pruneSettledJobs(): void {
    for (const [jobId, job] of this.#jobs) {
      if (job.state !== 'running') this.#jobs.delete(jobId)
    }
  }
}
