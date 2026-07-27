import type { JsonlEventJournal } from '../../events/index.js'
import type { SpeechScheduler } from '../../speech/index.js'
import { z } from 'zod'
import { TOOL_RESULT_PROTOCOL, type ToolCapability } from './contracts.js'

export const sayArgumentsSchema = z.strictObject({
  text: z.string().min(1).max(500)
    .describe('要说的话，一次一句。'),
})

export function createSayCapability(
  speech: Pick<SpeechScheduler, 'schedule'>,
  journal: Pick<JsonlEventJournal, 'append'>,
  recordQueuedSay: (runId: string) => void,
): ToolCapability<'chat'> {
  return {
    name: 'say',
    description:
      '把一句话交给聊天发送队列。返回只表示已排队，不表示玩家已经看到：长句会被切成几条依次发出，'
      + '发送有间隔，离开当前世界会取消未发出的部分。想说话时调用；不需要说、或想保持沉默时，'
      + '不调用即可。动作要花时间，行动前先简短说一句往往更自然。',
    argumentsSchema: sayArgumentsSchema,
    resource: 'chat',
    execute(invocation) {
      const parsed = sayArgumentsSchema.safeParse(invocation.arguments)
      const text = parsed.success ? parsed.data.text.trim() : ''
      if (!text) {
        return { protocol: TOOL_RESULT_PROTOCOL, status: 'failed', summary: 'say requires a non-empty text' }
      }
      let segments: number
      try {
        // actionId doubles as the speech request id so scheduler events correlate with this call.
        segments = speech.schedule({ id: invocation.actionId, text })
      } catch (error) {
        return {
          protocol: TOOL_RESULT_PROTOCOL, status: 'failed',
          summary: error instanceof Error ? error.message.slice(0, 300) : 'say_failed',
        }
      }
      recordQueuedSay(invocation.runId)
      void journal.append('say.queued', {
        actionId: invocation.actionId, runId: invocation.runId, toolCallId: invocation.toolCallId,
        roundId: invocation.roundId, segments, characters: text.length,
      })
      // `queued`, not `completed`: the scheduler segments and rate-limits, so the player has not
      // seen this yet and a later scope change can still cancel it.
      return { protocol: TOOL_RESULT_PROTOCOL, status: 'queued', segments }
    },
  }
}
