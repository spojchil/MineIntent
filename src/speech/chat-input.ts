import type { BackendEventEnvelope, ProtocolChatEvent } from '../minecraft/contracts.js'
import type { ChatInputContext, PlayerChatMessage } from './contracts.js'

export function interpretPlayerChat(
  event: BackendEventEnvelope<ProtocolChatEvent>,
  context: ChatInputContext,
): PlayerChatMessage | undefined {
  if (event.kind !== 'chat' || event.payload.position !== 'chat' || !event.payload.senderUsername) return undefined
  const sender = event.payload.senderUsername
  const explicitName = mentionsName(event.payload.plainText, context.companionUsername)
  const ongoing = equalName(context.conversationActiveWith, sender)
  const onlinePlayers = context.onlinePlayerUsernames.filter(name => !equalName(name, context.companionUsername))
  // 单独在场规则是一个临时的中间层决定（按 W09 在此记录理由）：两个人的场合说话不必点名，
  // 对象不言自明。规则属于「唯一在场」这一处境，不属于任何特定玩家——同条件同规则（A10）。
  // 预期归宿：帧携带在线人数与消息统计，关注与关系由 AI 写在记忆和关注列表里、由 AI 自己
  // 决定听谁（W08a）；届时本规则应被到达/关注机制取代或重新评估，而不是长成第二套特权。
  const singleParty = onlinePlayers.length === 1 && equalName(onlinePlayers[0], sender)
  const evidence = [
    ...(explicitName ? ['explicit_name' as const] : []),
    ...(ongoing ? ['ongoing_conversation' as const] : []),
    ...(singleParty ? ['single_party' as const] : []),
  ]
  const addressed = evidence.length > 0
  return {
    protocol: 'mineintent.player-chat.v1',
    sourceEventId: event.id,
    occurredAt: event.occurredAt,
    sender: { username: sender },
    text: event.payload.plainText,
    ...(event.payload.verified === undefined ? {} : { verified: event.payload.verified }),
    addressing: { addressedToCompanion: addressed, evidence: evidence.length ? evidence : ['not_addressed'] },
    world: { worldId: event.worldId, ...(event.dimension ? { dimension: event.dimension } : {}), connectionEpoch: event.connectionEpoch },
  }
}

function equalName(a: string | undefined, b: string): boolean { return a?.toLocaleLowerCase() === b.toLocaleLowerCase() }
function mentionsName(text: string, name: string): boolean {
  return new RegExp(`(?:^|[@＠\\s，,：:])${escapeRegex(name)}(?:$|[\\s，,：:！!.?？])`, 'iu').test(text)
}
function escapeRegex(value: string): string { return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') }
