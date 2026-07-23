import type { SpeechEvent, SpeechRequest, SpeechTransport } from './contracts.js'

interface Queued { request: SpeechRequest; segments: string[]; next: number }

export interface SpeechSchedulerOptions {
  maxSegmentLength?: number
  minimumIntervalMs?: number
  now?: () => number
  onEvent?: (event: SpeechEvent) => void
}

export class SpeechScheduler {
  readonly #transport: SpeechTransport
  readonly #maxLength: number
  readonly #interval: number
  readonly #now: () => number
  readonly #onEvent: (event: SpeechEvent) => void
  #queue: Queued[] = []
  #timer?: ReturnType<typeof setTimeout>
  #lastSentAt = Number.NEGATIVE_INFINITY

  constructor(transport: SpeechTransport, options: SpeechSchedulerOptions = {}) {
    this.#transport = transport
    this.#maxLength = options.maxSegmentLength ?? 256
    this.#interval = options.minimumIntervalMs ?? 1_000
    this.#now = options.now ?? Date.now
    this.#onEvent = options.onEvent ?? (() => {})
    if (!Number.isInteger(this.#maxLength) || this.#maxLength < 1) throw new RangeError('maxSegmentLength must be positive')
  }

  schedule(request: SpeechRequest): void {
    if (!request.id || !request.text.trim()) throw new TypeError('Speech request requires id and text')
    if (this.#queue.some(item => item.request.id === request.id)) throw new Error(`Duplicate speech request: ${request.id}`)
    const segments = segmentChat(request.text, this.#maxLength)
    this.#queue.push({ request: { ...request }, segments, next: 0 })
    this.#onEvent({ type: 'scheduled', requestId: request.id, segments: segments.length })
    this.#pump()
  }

  stop(reason = 'scheduler_stopped'): void {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = undefined
    for (const item of this.#queue) this.#onEvent({ type: 'cancelled', requestId: item.request.id, reason })
    this.#queue = []
  }

  #pump(): void {
    if (this.#timer) return
    const item = this.#queue[0]
    if (!item) return
    const delay = Math.max(0, this.#interval - (this.#now() - this.#lastSentAt))
    this.#timer = setTimeout(() => {
      this.#timer = undefined
      const current = this.#queue[0]
      if (!current) return this.#pump()
      const text = current.segments[current.next]!
      try {
        this.#transport.send(text)
        this.#lastSentAt = this.#now()
        this.#onEvent({ type: 'sent', requestId: current.request.id, segment: current.next, text })
        current.next++
        if (current.next === current.segments.length) this.#queue.shift()
      } catch (error) {
        this.#queue.shift()
        this.#onEvent({ type: 'failed', requestId: current.request.id, reason: error instanceof Error ? error.message : String(error) })
      }
      this.#pump()
    }, delay)
  }
}

export function segmentChat(text: string, maxLength = 256): string[] {
  const normalized = text.replace(/[\r\n\0]+/g, ' ').replace(/\s+/g, ' ').trim()
  if (!normalized) throw new TypeError('Speech text becomes empty after normalization')
  const result: string[] = []
  let rest = normalized
  while ([...rest].length > maxLength) {
    const points = [...rest]
    let cut = maxLength
    for (let index = maxLength; index >= Math.floor(maxLength * 0.6); index--) {
      if (/[\s，。！？、,.!?;；:：]/u.test(points[index - 1] ?? '')) { cut = index; break }
    }
    result.push(points.slice(0, cut).join('').trim())
    rest = points.slice(cut).join('').trim()
  }
  if (rest) result.push(rest)
  return result
}
