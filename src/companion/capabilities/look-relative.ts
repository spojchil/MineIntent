import type { JsonlEventJournal } from '../../events/index.js'
import type { ViewportValues } from '../../information/index.js'
import type { MinecraftBackendApi } from '../../minecraft/contracts.js'
import { z } from 'zod'
import {
  TOOL_RESULT_PROTOCOL, type CapabilityInvocation, type CapabilityScope, type ToolCapability,
} from './contracts.js'

const LOOK_EFFECT_EPSILON_DEGREES = 0.01

export const lookArgumentsSchema = z.strictObject({
  yaw_degrees: z.number()
    .min(-90).max(90)
    .describe('相对当前视线的水平转角，度。正值向右，负值向左。'),
  pitch_degrees: z.number()
    .min(-90).max(90)
    .describe('相对当前视线的垂直转角，度。正值向下，负值向上。'),
})

interface PoseSample {
  position: { x: number; y: number; z: number }
  yaw: number
  pitch: number
}

export function createLookRelativeCapability(
  backend: Pick<MinecraftBackendApi, 'motor' | 'observationSource'>,
  journal: Pick<JsonlEventJournal, 'append'>,
  readViewport: (runId: string, signal: AbortSignal) => Promise<ViewportValues>,
  releaseBodyInputs: () => void,
): ToolCapability<'body'> {
  return {
    name: 'look_relative',
    description:
      '相对当前视线转动一次视角，随后返回新的视野。玩家提到的东西不在当前视野里，'
      + '或行动前需要先转向别处时调用。',
    argumentsSchema: lookArgumentsSchema,
    resource: 'body',
    execute: async (invocation, scope) => executeLook(
      backend, journal, readViewport, releaseBodyInputs, invocation, scope,
    ),
  }
}

async function executeLook(
  backend: Pick<MinecraftBackendApi, 'motor' | 'observationSource'>,
  journal: Pick<JsonlEventJournal, 'append'>,
  readViewport: (runId: string, signal: AbortSignal) => Promise<ViewportValues>,
  releaseBodyInputs: () => void,
  invocation: CapabilityInvocation,
  scope: CapabilityScope,
): Promise<unknown> {
  let motor: ReturnType<MinecraftBackendApi['motor']> | undefined
  try {
    motor = backend.motor()
    const before = backend.observationSource().selfPose()
    const args = lookArgumentsSchema.parse(invocation.arguments)
    await motor.lookRelative(args.yaw_degrees, args.pitch_degrees, scope.signal)
    scope.assertCurrent()
    const after = backend.observationSource().selfPose()
    const viewport = await readViewport(invocation.runId, scope.signal)
    scope.assertCurrent()
    await journal.append('body_tool.completed', {
      actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
      roundId: invocation.roundId, tool: 'look_relative', startedAt: invocation.startedAt,
      // Internal diagnostics may retain poses; they never cross the model result boundary.
      internal: { before, after },
    })
    scope.assertCurrent()
    return {
      protocol: TOOL_RESULT_PROTOCOL,
      status: 'completed',
      effect: measuredLookEffect(before, after),
      viewport,
    }
  } catch (error) {
    if (scope.signal.aborted || !scope.isCurrent()) throw error
    scope.assertCurrent()
    const viewport = await readViewport(invocation.runId, scope.signal).catch(() => undefined)
    scope.assertCurrent()
    await journal.append('body_tool.failed', {
      actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
      roundId: invocation.roundId, tool: 'look_relative',
      summary: error instanceof Error ? error.message : String(error),
    })
    scope.assertCurrent()
    return {
      protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
      summary: error instanceof Error ? error.message.slice(0, 300) : 'tool_failed',
      ...(viewport ? { viewport } : {}),
    }
  } finally {
    try { if (motor) motor.releaseAll(); else releaseBodyInputs() } catch { /* best effort */ }
  }
}

function measuredLookEffect(before: PoseSample, after: PoseSample) {
  const yawDegrees = radiansToDegrees(normalizeRadians(before.yaw - after.yaw))
  const pitchDegrees = radiansToDegrees(before.pitch - after.pitch)
  return {
    relativeTurnDegrees: { yaw: withoutNegativeZero(yawDegrees), pitch: withoutNegativeZero(pitchDegrees) },
    turned: Math.hypot(yawDegrees, pitchDegrees) > LOOK_EFFECT_EPSILON_DEGREES,
  }
}

function normalizeRadians(value: number): number {
  let normalized = value % (Math.PI * 2)
  if (normalized > Math.PI) normalized -= Math.PI * 2
  if (normalized < -Math.PI) normalized += Math.PI * 2
  return normalized
}

function radiansToDegrees(value: number): number { return value * 180 / Math.PI }
function withoutNegativeZero(value: number): number { return Object.is(value, -0) ? 0 : value }
