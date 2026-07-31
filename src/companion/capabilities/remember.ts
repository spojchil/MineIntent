import type { JsonlEventJournal } from '../../events/index.js'
import type { FileMemoryStore } from '../../memory/index.js'
import { z } from 'zod'
import { TOOL_RESULT_PROTOCOL, type ToolCapability } from './contracts.js'

export const rememberArgumentsSchema = z.strictObject({
  summary: z.string().min(1).max(300)
    .describe('要记住的内容，一两句话说清楚。'),
})

export function createRememberCapability(
  memory: Pick<FileMemoryStore, 'remember'>,
  journal: Pick<JsonlEventJournal, 'append'>,
): ToolCapability<'memory'> {
  return {
    name: 'remember',
    description: '把你决定长期保留的内容写进持久记忆，供以后回忆。用一两句话清楚写下要保留的内容。',
    argumentsSchema: rememberArgumentsSchema,
    resource: 'memory',
    async execute(invocation, scope) {
      const parsed = rememberArgumentsSchema.safeParse(invocation.arguments)
      const summary = parsed.success ? parsed.data.summary.trim() : ''
      if (!summary) {
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: 'remember requires a non-empty summary' }
      }
      try {
        await memory.remember({
          worldId: scope.worldId,
          kind: 'episode',
          summary,
          evidence: [{ kind: 'event', id: scope.chatEventId }],
        })
        scope.assertCurrent()
        // The summary itself stays out of the journal, matching the redaction stance on speech.
        await journal.append('memory.remembered', {
          actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
        })
        scope.assertCurrent()
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'completed' }
      } catch (error) {
        if (scope.signal.aborted || !scope.isCurrent()) throw error
        return {
          protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
          summary: error instanceof Error ? error.message.slice(0, 300) : 'remember_failed',
        }
      }
    },
  }
}
