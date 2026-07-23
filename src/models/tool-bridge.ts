import { randomBytes, timingSafeEqual } from 'node:crypto'
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http'
import { d40ToolInvocationSchema, type D40ToolInvocation } from './contracts.js'

const HOST = '127.0.0.1'
const ROUTE = '/v1/d40/tool'
const MAX_REQUEST_BYTES = 32_768
const MAX_RESPONSE_BYTES = 262_144

export interface ToolBridgeAddress { url: string; token: string }

export class D40ToolBridgeServer {
  readonly #token = randomBytes(32).toString('base64url')
  #server?: Server
  constructor(private readonly handler: (invocation: D40ToolInvocation) => Promise<unknown>) {}

  async start(): Promise<ToolBridgeAddress> {
    if (this.#server) return this.address()
    const server = createServer((request, response) => { void this.#handle(request, response) })
    this.#server = server
    try {
      await new Promise<void>((resolve, reject) => {
        server.once('error', reject)
        server.listen(0, HOST, () => { server.off('error', reject); resolve() })
      })
    } catch (error) { this.#server = undefined; throw error }
    return this.address()
  }

  address(): ToolBridgeAddress {
    const address = this.#server?.address()
    if (!address || typeof address === 'string') throw new Error('Tool bridge is not listening')
    return { url: `http://${HOST}:${address.port}${ROUTE}`, token: this.#token }
  }

  async stop(): Promise<void> {
    const server = this.#server
    this.#server = undefined
    if (!server) return
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }

  async #handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
    try {
      if (request.method !== 'POST' || request.url !== ROUTE) return send(response, request.method === 'POST' ? 404 : 405, { error: 'not_found' })
      if (!authorized(request.headers.authorization, this.#token)) return send(response, 401, { error: 'unauthorized' })
      if (request.headers['content-type']?.split(';', 1)[0]?.trim().toLowerCase() !== 'application/json') {
        return send(response, 415, { error: 'content_type_must_be_application_json' })
      }
      const raw = await readBody(request)
      let value: unknown
      try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(raw)) }
      catch { return send(response, 400, { error: 'invalid_json' }) }
      const invocation = d40ToolInvocationSchema.safeParse(value)
      if (!invocation.success) return send(response, 400, { error: 'invalid_tool_invocation' })
      send(response, 200, await this.handler(invocation.data))
    } catch (error) {
      send(response, 500, { error: error instanceof Error ? error.message.slice(0, 500) : 'tool_handler_failed' })
    }
  }
}

async function readBody(request: IncomingMessage): Promise<Uint8Array> {
  const chunks: Buffer[] = []
  let size = 0
  for await (const chunk of request) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as Uint8Array)
    size += buffer.length
    if (size > MAX_REQUEST_BYTES) throw new Error('request_too_large')
    chunks.push(buffer)
  }
  return Buffer.concat(chunks, size)
}
function authorized(header: string | undefined, token: string): boolean {
  if (!header) return false
  const actual = Buffer.from(header)
  const expected = Buffer.from(`Bearer ${token}`)
  return actual.length === expected.length && timingSafeEqual(actual, expected)
}
function send(response: ServerResponse, status: number, value: unknown): void {
  if (response.headersSent || response.destroyed) return
  let payload = Buffer.from(JSON.stringify(value), 'utf8')
  if (payload.length > MAX_RESPONSE_BYTES) { status = 500; payload = Buffer.from('{"error":"response_too_large"}', 'utf8') }
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8', 'content-length': String(payload.length), 'cache-control': 'no-store',
  }).end(payload)
}
