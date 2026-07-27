import type { BackendEventEnvelope, ProtocolChatEvent } from '../minecraft/contracts.js'
import type { ChatInputContext, PlayerChatMessage } from './contracts.js'

export function interpretPlayerChat(
  event: BackendEventEnvelope<ProtocolChatEvent>,
  context: ChatInputContext,
): PlayerChatMessage | undefined {
  if (event.kind !== 'chat' || event.payload.position !== 'chat' || !event.payload.senderUsername) return undefined
  const sender = event.payload.senderUsername
  const isPrimaryPlayer = context.primaryPlayerUsernames.some(name => equalName(name, sender))
  const explicitName = mentionsName(event.payload.plainText, context.companionUsername)
  const ongoing = equalName(context.conversationActiveWith, sender)
  const singleParty = isPrimaryPlayer && context.onlinePlayerUsernames.filter(name => !equalName(name, context.companionUsername)).length === 1
  const evidence = [
    ...(explicitName ? ['explicit_name' as const] : []),
    ...(ongoing ? ['ongoing_conversation' as const] : []),
    ...(singleParty ? ['primary_player_single_party' as const] : []),
  ]
  const addressed = evidence.length > 0
  return {
    protocol: 'mineintent.player-chat.v1',
    sourceEventId: event.id,
    occurredAt: event.occurredAt,
    sender: { username: sender, isPrimaryPlayer },
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
