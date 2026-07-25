import { z } from 'zod'
import type { PassiveObservations } from '../information/index.js'
import type { CompanionProfile } from '../companion/profile.js'
import type { WireToolDefinition } from './agent-tools.js'

/** The structured body observation exposes no world coordinates or refs; free-form memory text is not rewritten. */
export interface AgentDecisionContext {
  protocol: 'mineintent.agent-context.v1'
  player: { username: string; text: string }
  profile: Pick<CompanionProfile, 'content'>
  world: { dimension: string; timeOfDay?: number }
  observations: PassiveObservations
  recentEvents: Array<{ type: string; summary: string }>
  memories: Array<{ kind: string; summary: string; createdAt: string }>
}

/**
 * Completion report for one agent run. Speech is not part of it: talking happens through the
 * `say` tool while the run is still going, and silence is simply the absence of a `say` call.
 */
export interface ModelRunResult {
  model: string
  usage?: { inputTokens?: number; outputTokens?: number }
}
export interface ModelProvider {
  run(
    input: { runId: string; context: AgentDecisionContext; tools: readonly WireToolDefinition[] },
    signal: AbortSignal,
  ): Promise<ModelRunResult>
}

/** Names stay an open string: the tool backend answers unknown names with a failed result. */
export const toolInvocationSchema = z.strictObject({
  runId: z.string().min(1).max(128),
  name: z.string().min(1).max(64),
  arguments: z.record(z.string(), z.unknown()),
})
export type ToolInvocation = z.infer<typeof toolInvocationSchema>
