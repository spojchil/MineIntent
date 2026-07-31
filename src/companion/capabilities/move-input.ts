import type { JsonlEventJournal } from '../../events/index.js'
import { lookDirection, type ViewportValues } from '../../information/index.js'
import type { MinecraftBackendApi } from '../../minecraft/contracts.js'
import { z } from 'zod'
import {
  TOOL_RESULT_PROTOCOL, type CapabilityInvocation, type CapabilityScope, type ToolCapability,
} from './contracts.js'

const MOVE_EFFECT_EPSILON = 0.01

export const moveArgumentsSchema = z.strictObject({
  directions: z.array(z.enum(['forward', 'back', 'left', 'right'])).min(1).max(4)
    .refine(directions => new Set(directions).size === directions.length, '移动键不能重复。')
    .meta({ uniqueItems: true })
    .describe('同时按住的移动键，方向相对当前朝向；斜走时把两个键放在这里。'),
  duration_ms: z.number().int().min(50).max(1_500)
    .describe('整组移动键共同按住的时长，毫秒。步行大约每 250 毫秒走一格。'),
  sprint: z.boolean().optional()
    .describe('是否同时按住疾跑；同样时长内走得更远。'),
})

interface PoseSample {
  position: { x: number; y: number; z: number }
  yaw: number
  pitch: number
}

export function createMoveInputCapability(
  backend: Pick<MinecraftBackendApi, 'motor' | 'observationSource'>,
  journal: Pick<JsonlEventJournal, 'append'>,
  readViewport: (runId: string, signal: AbortSignal) => Promise<ViewportValues>,
  releaseBodyInputs: () => void,
): ToolCapability<'body'> {
  return {
    name: 'move_input',
    description:
      '想往一个方向挪一点、或斜着靠近已经看见的东西时，短暂按住一组真实移动键再一起松开，'
      + '随后返回实际移动效果和新的视野。前后键或左右键同时按会互相抵消，对应轴不会移动。'
      + '没有寻路也不会跳跃：一次最多走几格，障碍不会被自动绕开，返回时身体可能仍在滑行或下落。',
    argumentsSchema: moveArgumentsSchema,
    resource: 'body',
    execute: async (invocation, scope) => executeMove(
      backend, journal, readViewport, releaseBodyInputs, invocation, scope,
    ),
  }
}

async function executeMove(
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
    const args = moveArgumentsSchema.parse(invocation.arguments)
    await motor.move(args.directions, args.duration_ms, args.sprint, scope.signal)
    scope.assertCurrent()
    const after = backend.observationSource().selfPose()
    const viewport = await readViewport(invocation.runId, scope.signal)
    scope.assertCurrent()
    await journal.append('body_tool.completed', {
      actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
      tool: 'move_input', startedAt: invocation.startedAt,
      // Internal diagnostics may retain poses; they never cross the model result boundary.
      internal: { before, after },
    })
    scope.assertCurrent()
    return {
      protocol: TOOL_RESULT_PROTOCOL,
      status: 'completed',
      effect: measuredMoveEffect(before, after),
      viewport,
    }
  } catch (error) {
    if (scope.signal.aborted || !scope.isCurrent()) throw error
    scope.assertCurrent()
    const viewport = await readViewport(invocation.runId, scope.signal).catch(() => undefined)
    scope.assertCurrent()
    await journal.append('body_tool.failed', {
      actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
      tool: 'move_input',
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

/**
 * Says which frame it measured in, in place. The viewport declares every position it reports as
 * `minecraft_world_absolute`, and a model that carries that legend across to an action result would
 * read this triple as a world offset. It is not one: it is resolved against the pre-move facing, so
 * the same displacement means different things depending on where the companion was looking.
 */
function measuredMoveEffect(before: PoseSample, after: PoseSample) {
  const delta = {
    x: after.position.x - before.position.x,
    y: after.position.y - before.position.y,
    z: after.position.z - before.position.z,
  }
  const forward = lookDirection(before.yaw, 0)
  const right = { x: -forward.z, z: forward.x }
  const relativeDisplacement: [number, number, number] = [
    withoutNegativeZero(delta.x * right.x + delta.z * right.z),
    withoutNegativeZero(delta.y),
    withoutNegativeZero(delta.x * forward.x + delta.z * forward.z),
  ]
  const distance = Math.hypot(delta.x, delta.y, delta.z)
  return {
    coordinates: 'body_relative_before_move' as const,
    legend: 'relativeDisplacement 是 [右, 上, 前] 三个格数，相对移动前的朝向，不是世界绝对坐标',
    relativeDisplacement,
    distance: withoutNegativeZero(distance),
    movement: distance > MOVE_EFFECT_EPSILON ? 'changed' as const : 'no_effect' as const,
  }
}

function withoutNegativeZero(value: number): number { return Object.is(value, -0) ? 0 : value }
