import { z } from 'zod'
import type { PassiveObservations } from '../information/index.js'
import type { CompanionProfile } from '../companion/profile.js'

export const d40DecisionSchema = z.strictObject({
  protocol: z.literal('mineintent.d40-decision.v1'),
  speech: z.string().trim().min(1).max(500).nullable(),
})
export type D40Decision = z.infer<typeof d40DecisionSchema>

/** The structured body observation exposes no world coordinates or refs; free-form memory text is not rewritten. */
export interface D40DecisionContext {
  protocol: 'mineintent.d40-context.v1'
  player: { username: string; text: string }
  profile: Pick<CompanionProfile, 'content'>
  world: { dimension: string; timeOfDay?: number }
  observations: PassiveObservations
  recentEvents: Array<{ type: string; summary: string }>
  memories: Array<{ kind: string; summary: string; createdAt: string }>
}

export interface ModelRunResult {
  decision: D40Decision
  model: string
  usage?: { inputTokens?: number; outputTokens?: number }
}
export interface ModelProvider {
  run(input: { runId: string; context: D40DecisionContext }, signal: AbortSignal): Promise<ModelRunResult>
}

export const d40ToolInvocationSchema = z.strictObject({
  runId: z.string().min(1).max(128),
  name: z.enum(['look_relative', 'move_input']),
  arguments: z.record(z.string(), z.unknown()),
})
export type D40ToolInvocation = z.infer<typeof d40ToolInvocationSchema>
