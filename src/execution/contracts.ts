/**
 * The execution and resource-management layer: the small internal control plane B08 asked for
 * after the original Action Runtime was deleted (ADR 0003 is `accepted` but `diverged`).
 *
 * It owns exactly four things — resource leases, same-round arbitration, job lifecycle, and epoch
 * binding — and deliberately knows nothing about goals or targets. "Hold forward until I say stop
 * or 800ms elapse, and tell me when you stop" is in scope; "walk to the sheep" is not. Holding a
 * target is what grew the previous Action Runtime into a planner, and D01 has since ruled that
 * path out, so the boundary is the whole point of rebuilding this smaller.
 */

/**
 * Independent resources a tool may need. Locks are per resource, never per tool.
 *
 * `senses` is not about exclusive access to reality — looking twice hurts nobody. It exists because
 * a viewport scan walks a six-figure number of voxels, so two overlapping ones would compete for the
 * event loop and make both slower than running them in turn.
 */
export type ExecutionResource = 'body' | 'chat' | 'memory' | 'senses'

export type JobState = 'running' | 'completed' | 'failed' | 'cancelled'

export interface ResourceLease {
  resource: ExecutionResource
  /** Internal action identity; also the correlation key carried into the journal (D06). */
  actionId: string
  runId: string
  toolName: string
  acquiredAt: string
}

export interface ExecutionRefusal {
  /** Machine-readable so the agent can distinguish "wait" from "never". */
  code: 'resource_busy' | 'opposing_move' | 'unknown_tool' | 'scope_invalid'
  summary: string
}

/** What a finished job reports back. Kept deliberately narrow: no goal, no target, no plan. */
export interface JobOutcome {
  jobId: string
  state: JobState
  summary?: string
}
