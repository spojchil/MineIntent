import { z } from 'zod'
import { d40DecisionSchema, type D40DecisionContext, type ModelProvider, type ModelRunResult } from './contracts.js'

interface FetchLike { (input: string | URL, init?: RequestInit): Promise<Response> }
const MAX_RESPONSE_BYTES = 64 * 1_024
const MAX_ERROR_CHARACTERS = 300
const CANCEL_TIMEOUT_MS = 2_000

export interface AgentServiceOptions {
  baseUrl: string
  serviceToken: string
  toolCallbackUrl: string
  toolCallbackToken: string
  timeoutMs?: number
  fetch?: FetchLike
}

const responseSchema = z.strictObject({
  decision: d40DecisionSchema,
  model: z.string().min(1).max(256),
  usage: z.strictObject({
    inputTokens: z.number().int().nonnegative().optional(),
    outputTokens: z.number().int().nonnegative().optional(),
  }).optional(),
})

export class AgentServiceModelProvider implements ModelProvider {
  readonly #endpoint: URL
  readonly #cancelEndpoint: URL
  readonly #serviceToken: string
  readonly #callbackUrl: string
  readonly #callbackToken: string
  readonly #timeoutMs: number
  readonly #fetch: FetchLike

  constructor(options: AgentServiceOptions) {
    const baseUrl = validateAgentServiceBaseUrl(options.baseUrl)
    this.#endpoint = new URL('/v1/decide', baseUrl)
    this.#cancelEndpoint = new URL('/v1/cancel', baseUrl)
    this.#serviceToken = validateToken(options.serviceToken, 'Agent service token', 32)
    this.#callbackUrl = validateLoopbackUrl(options.toolCallbackUrl)
    this.#callbackToken = validateToken(options.toolCallbackToken, 'Tool callback token', 16)
    this.#timeoutMs = options.timeoutMs ?? 180_000
    this.#fetch = options.fetch ?? fetch
  }

  async run(input: { runId: string; context: D40DecisionContext }, signal: AbortSignal): Promise<ModelRunResult> {
    signal.throwIfAborted()
    const requestSignal = AbortSignal.any([signal, AbortSignal.timeout(this.#timeoutMs)])
    let cancelStarted = false
    const cancelRun = (): void => {
      if (cancelStarted) return
      cancelStarted = true
      void this.#cancelRun(input.runId)
    }
    requestSignal.addEventListener('abort', cancelRun, { once: true })
    try {
      const response = await this.#fetch(this.#endpoint, {
        method: 'POST',
        redirect: 'error',
        signal: requestSignal,
        headers: {
          authorization: `Bearer ${this.#serviceToken}`,
          'content-type': 'application/json',
          'x-mineintent-tool-executor-url': this.#callbackUrl,
          'x-mineintent-tool-executor-token': this.#callbackToken,
        },
        body: JSON.stringify(input),
      })
      const payload = await readBoundedJson(response)
      if (!response.ok) {
        const message = payload && typeof payload === 'object' && 'error' in payload
          ? String(payload.error).slice(0, MAX_ERROR_CHARACTERS)
          : 'unknown error'
        throw new Error(`Agent service request failed (${response.status}): ${message}`)
      }
      return responseSchema.parse(payload)
    } finally {
      requestSignal.removeEventListener('abort', cancelRun)
    }
  }

  async #cancelRun(runId: string): Promise<void> {
    try {
      const response = await this.#fetch(this.#cancelEndpoint, {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.timeout(CANCEL_TIMEOUT_MS),
        headers: {
          authorization: `Bearer ${this.#serviceToken}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify({ runId }),
      })
      await response.body?.cancel()
    } catch {
      // Cancellation is best-effort here; a newer run also supersedes this run server-side.
    }
  }
}

function validateLoopbackUrl(value: string): string {
  if (value.length > 2_048 || /[\r\n]/u.test(value)) throw new TypeError('Tool callback URL is invalid')
  const url = new URL(value)
  const loopback = url.hostname === '127.0.0.1' || url.hostname === '[::1]' || url.hostname === 'localhost'
  if (url.protocol !== 'http:' || !loopback || url.username || url.password || url.hash) {
    throw new TypeError('Tool callback must be an uncredentialed loopback HTTP URL')
  }
  return url.href
}
function validateToken(value: string, label: string, minimumLength: number): string {
  if (value.length < minimumLength || value.length > 512 || [...value].some(character => {
    const codePoint = character.codePointAt(0)!
    return codePoint < 0x21 || codePoint > 0x7e
  })) throw new TypeError(`${label} is invalid`)
  return value
}

function validateAgentServiceBaseUrl(value: string): string {
  if (value.length > 2_048 || /[\r\n]/u.test(value)) throw new TypeError('Agent service URL is invalid')
  const url = new URL(value)
  const loopback = url.hostname === '127.0.0.1' || url.hostname === '[::1]' || url.hostname === 'localhost'
  if (url.protocol !== 'http:' || !loopback || url.username || url.password || url.hash) {
    throw new TypeError('Agent service must be an uncredentialed loopback HTTP URL')
  }
  return url.href
}

async function readBoundedJson(response: Response): Promise<unknown> {
  const declaredLength = response.headers.get('content-length')
  if (declaredLength !== null) {
    const size = Number(declaredLength)
    if (Number.isFinite(size) && size > MAX_RESPONSE_BYTES) {
      await response.body?.cancel()
      throw new Error('Agent service response exceeded its size limit')
    }
  }
  if (!response.body) throw new Error('Agent service response body is missing')

  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_RESPONSE_BYTES) {
        await reader.cancel()
        throw new Error('Agent service response exceeded its size limit')
      }
      chunks.push(value)
    }
  } finally {
    reader.releaseLock()
  }

  const combined = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) { combined.set(chunk, offset); offset += chunk.byteLength }
  let text: string
  try { text = new TextDecoder('utf-8', { fatal: true }).decode(combined) }
  catch { throw new Error('Agent service returned invalid UTF-8') }
  try { return JSON.parse(text) as unknown }
  catch { throw new Error('Agent service returned invalid JSON') }
}
