import assert from 'node:assert/strict'
import test from 'node:test'
import type { BackendEventEnvelope, ProtocolChatEvent } from '../minecraft/contracts.js'
import { interpretPlayerChat } from './chat-input.js'
import { segmentChat, SpeechScheduler } from './speech-scheduler.js'

function chat(text: string, sender = 'spojchil'): BackendEventEnvelope<ProtocolChatEvent> {
  return {
    protocol: 'mineintent.minecraft.backend-event.v1', id: 'chat-1', kind: 'chat', occurredAt: '2026-07-12T00:00:00.000Z',
    processSessionId: 'session', connectionEpoch: 3, connectionAttemptId: 'attempt', worldId: 'world', dimension: 'overworld',
    payload: { senderUsername: sender, plainText: text, position: 'chat', verified: true },
  }
}

const context = {
  companionUsername: 'MineIntentBot',
  primaryPlayerUsernames: ['spojchil'],
  onlinePlayerUsernames: ['spojchil', 'MineIntentBot'],
}

test('chat input records sender, addressing evidence, time and world context', () => {
  const message = interpretPlayerChat(chat('MineIntentBot，你在吗'), context)!
  assert.equal(message.sender.isPrimaryPlayer, true)
  assert.equal(message.addressing.addressedToCompanion, true)
  assert.equal(message.world.dimension, 'overworld')
  assert.equal(message.occurredAt, '2026-07-12T00:00:00.000Z')
})

test('stop wording remains ordinary addressed player text', () => {
  const message = interpretPlayerChat(chat('MineIntentBot，停一下'), context)
  assert.equal(message?.text, 'MineIntentBot，停一下')
  assert.equal(message?.addressing.addressedToCompanion, true)
})

test('segmentChat respects Unicode length and keeps ordered content', () => {
  const segments = segmentChat('这是第一句话。这是第二句话，需要被安全分开。', 10)
  assert.equal(segments.every(segment => [...segment].length <= 10), true)
  assert.equal(segments.join('').replaceAll(' ', ''), '这是第一句话。这是第二句话，需要被安全分开。')
})

test('scheduler rate limits and preserves segment order', async () => {
  const sent: string[] = []
  const events: string[] = []
  const scheduler = new SpeechScheduler({ send: message => sent.push(message) }, {
    maxSegmentLength: 5,
    minimumIntervalMs: 5,
    onEvent: event => events.push(`${event.type}:${'requestId' in event ? event.requestId : ''}`),
  })
  scheduler.schedule({ id: 'reply', text: '我去拿一些木头回来' })
  await wait(30)
  assert.equal(sent.join(''), '我去拿一些木头回来')
  assert.equal(events[0], 'scheduled:reply')
  scheduler.stop()
})

test('scheduler stop cancels queued speech before it is sent', async () => {
  const sent: string[] = []
  const events: string[] = []
  const scheduler = new SpeechScheduler({ send: message => sent.push(message) }, {
    minimumIntervalMs: 0,
    onEvent: event => events.push(event.type),
  })
  scheduler.schedule({ id: 'reply', text: '这里风景不错' })
  scheduler.stop('test_stopped')
  await wait(5)
  assert.deepEqual(sent, [])
  assert.deepEqual(events, ['scheduled', 'cancelled'])
})

function wait(ms: number): Promise<void> { return new Promise(resolve => setTimeout(resolve, ms)) }
