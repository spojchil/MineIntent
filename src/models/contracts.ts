import { z } from 'zod'
import type { PassiveObservations } from '../information/index.js'
import type { CompanionProfile } from '../companion/profile.js'
import type { WireToolDefinition } from './agent-tools.js'

/**
 * One appended observation of the world.
 *
 * Frames are the only way volatile state enters the conversation, and they enter it by being
 * *appended* — never re-rendered. That distinction is the whole design: under prefix caching a
 * timestamp appended once costs nothing, while the same timestamp re-rendered into the system
 * prompt on every request moves the entire prompt out of cache. So an older frame is never edited
 * or dropped while the conversation it belongs to is live; it stays a true record of what was
 * observed then, and `at` tells the model which frame is newest.
 *
 * The viewport is deliberately absent. It is the one observation large enough to dominate the
 * prompt, and the model pulls it with a tool when it wants to look.
 */
export interface AgentFrame {
  at: string
  /** Present on the frame that opens a run: the message that caused it. */
  player?: { username: string; text: string }
  world: { dimension: string; timeOfDay?: number }
  self?: { position: [number, number, number]; yawDegrees: number; pitchDegrees: number }
  status?: PassiveObservations['currentStatus']
  inventory?: PassiveObservations['inventory']
  sound?: PassiveObservations['sound']
  /**
   * What happened since the previous frame. This is the channel for everything the model would
   * never think to ask about — a pull-only design cannot report damage, because nothing tells the
   * model there is damage to look up.
   */
  events: Array<{ type: string; summary: string }>
  /** Set when the pending-event buffer overflowed. Staying silent about that would be a lie. */
  omittedEvents?: number
  omissions: PassiveObservations['omissions']
}

/**
 * What one request offers the model, split by how fast it changes rather than by topic.
 *
 * `stable` is rendered into the system message and `frame` is appended after it, because prefix
 * caching is prefix-only: whatever changes first invalidates everything after it. Profile changes
 * on the order of days and memory on the order of tool calls, while the world changes every tick.
 */
export interface AgentDecisionContext {
  protocol: 'mineintent.agent-context.v2'
  stable: {
    profile: Pick<CompanionProfile, 'content'>
    memories: Array<{ kind: string; summary: string; createdAt: string }>
  }
  frame: AgentFrame
}

/**
 * A tool answer, plus the frame the world produced while the tool ran, if there is one.
 *
 * The frame travels beside the result rather than inside it: an event like taking damage has
 * nothing to do with whichever tool happened to be running, and folding it into the result would
 * teach the model that tools report unrelated news.
 */
export interface ToolExecution {
  /** Middle-layer identity returned to the agent loop as opaque transport metadata. */
  roundId: string
  result: unknown
  frame?: AgentFrame
}

/**
 * Completion report for one agent run. Speech is not part of it: talking happens through the
 * `say` tool while the run is still going, and silence is simply the absence of a `say` call.
 */
export interface ModelRunResult {
  model: string
  /**
   * `cacheReadTokens` is the prefix the provider served from its cache. It is reported because our
   * prompt shape decides it: every provider refuses to cache a prefix under a floor, so a run can
   * legitimately report zero, and a hit rate is only auditable against `inputTokens` from the same
   * run. Absent when the provider reported nothing.
   */
  usage?: { inputTokens?: number; outputTokens?: number; cacheReadTokens?: number; cacheWriteTokens?: number }
}
export interface ModelProvider {
  run(
    input: { runId: string; context: AgentDecisionContext; tools: readonly WireToolDefinition[] },
    signal: AbortSignal,
  ): Promise<ModelRunResult>
}

/**
 * Names stay an open string: the tool backend answers unknown names with a failed result.
 *
 * `toolCallId` carries the model-side identity of the call so the internal chain runs unbroken
 * from the model's tool call through the action to the journal (D06). The agent loop declares the
 * first call in a response as a new round, but it cannot name that round: the middle layer returns
 * the identity and the loop only echoes that opaque value on the remaining calls.
 */
export const toolInvocationSchema = z.strictObject({
  runId: z.string().min(1).max(128),
  toolCallId: z.string().min(1).max(128),
  round: z.union([
    z.strictObject({ new: z.literal(true) }),
    z.strictObject({ id: z.string().min(1).max(128) }),
  ]),
  name: z.string().min(1).max(64),
  arguments: z.record(z.string(), z.unknown()),
})
export type ToolInvocation = z.infer<typeof toolInvocationSchema>
