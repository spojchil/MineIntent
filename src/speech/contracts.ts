export type AddressingEvidence =
  | 'explicit_name'
  | 'explicit_reply'
  | 'single_party'
  | 'ongoing_conversation'
  | 'not_addressed'

export interface PlayerChatMessage {
  protocol: 'mineintent.player-chat.v1'
  sourceEventId: string
  occurredAt: string
  sender: { username: string }
  text: string
  verified?: boolean
  addressing: { addressedToParticipant: boolean; evidence: AddressingEvidence[] }
  world: { worldId: string; dimension?: string; connectionEpoch: number }
}

export interface ChatInputContext {
  participantUsername: string
  onlinePlayerUsernames: readonly string[]
  conversationActiveWith?: string
}

export interface SpeechRequest {
  id: string
  text: string
}

export type SpeechEvent =
  | { type: 'scheduled'; requestId: string; segments: number }
  | { type: 'sent'; requestId: string; segment: number; text: string }
  | { type: 'cancelled'; requestId: string; reason: string }
  | { type: 'failed'; requestId: string; reason: string }

export interface SpeechTransport { send(message: string): void }
