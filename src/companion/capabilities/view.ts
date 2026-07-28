import type { ViewportValues } from '../../information/index.js'
import { z } from 'zod'
import { TOOL_RESULT_PROTOCOL, type ToolCapability } from './contracts.js'

export const viewArgumentsSchema = z.strictObject({})

export function createViewCapability(
  readViewport: (runId: string, signal: AbortSignal) => Promise<ViewportValues>,
): ToolCapability<'viewport'> {
  return {
    name: 'view',
    description:
      '原地看一眼当前视野，不转头也不挪步。想先看看眼前再决定做什么，'
      + '或被提醒周围有变化时调用。',
    argumentsSchema: viewArgumentsSchema,
    // A full projection can scan six figures of voxels. It must serialize with another viewport
    // scan, but sharing `body` would make looking wait behind walking even though players can do both.
    resource: 'viewport',
    async execute(invocation, scope) {
      scope.signal.throwIfAborted()
      scope.assertCurrent()
      if (!viewArgumentsSchema.safeParse(invocation.arguments).success) {
        return {
          protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
          summary: 'view accepts no arguments',
        }
      }
      try {
        const viewport = await readViewport(invocation.runId, scope.signal)
        scope.assertCurrent()
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'completed', viewport }
      } catch (error) {
        if (scope.signal.aborted || !scope.isCurrent()) throw error
        return {
          protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
          summary: error instanceof Error ? error.message.slice(0, 300) : 'view_failed',
        }
      }
    },
  }
}
