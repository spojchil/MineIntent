import type { CompanionDebugState, DebugFailureSummary, DebugStateInput } from './contracts.js'

export class DebugStateStore {
  #revision = 0
  #state: DebugStateInput = {
    connection: { status: 'idle' },
    recentFailures: [],
    decision: { status: 'idle', contextSources: [], retrievedMemoryIds: [] },
  }

  update(update: Partial<DebugStateInput>): void {
    this.#state = structuredClone({ ...this.#state, ...update })
    this.#revision++
  }

  failure(failure: DebugFailureSummary): void {
    this.update({ recentFailures: [...this.#state.recentFailures, failure].slice(-10) })
  }

  snapshot(): Readonly<CompanionDebugState> {
    const state: CompanionDebugState = {
      protocol: 'mineintent.debug-state.v1',
      revision: this.#revision,
      capturedAt: new Date().toISOString(),
      ...structuredClone(this.#state),
    }
    return deepFreeze(redactSensitive(state)) as Readonly<CompanionDebugState>
  }
}

const SENSITIVE_KEY = /(?:api[-_]?key|authorization|cookie|credential|password|profiles?folder|secret|token)$/iu
const PRIVATE_RAW_KEY = /^(?:content|messages?|prompt|raw|transcript)$/iu
const SECRET_VALUE = /(?:bearer\s+[a-z0-9._~+\/-]{12,}|sk-[a-z0-9_-]{12,})/giu

export function redactSensitive<T>(input: T): T {
  const visit = (value: unknown, key?: string): unknown => {
    if (key && (SENSITIVE_KEY.test(key) || PRIVATE_RAW_KEY.test(key))) return '[REDACTED]'
    if (typeof value === 'string') return value.replace(SECRET_VALUE, '[REDACTED]')
    if (Array.isArray(value)) return value.map(item => visit(item))
    if (value && typeof value === 'object') {
      return Object.fromEntries(Object.entries(value).map(([childKey, child]) => [childKey, visit(child, childKey)]))
    }
    return value
  }
  return visit(structuredClone(input)) as T
}

function deepFreeze<T>(value: T): T {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value)
    for (const child of Object.values(value)) deepFreeze(child)
  }
  return value
}
