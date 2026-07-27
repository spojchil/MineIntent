import type { PassiveObservations } from '../information/index.js'
import type { BackendState, Vec3Value } from '../minecraft/contracts.js'

export interface DebugFailureSummary {
  at: string
  source: 'backend' | 'model' | 'body_tool' | 'memory' | 'runtime'
  code: string
  summary: string
}

export interface DebugContextSource {
  id: string
  kind: 'runtime' | 'event' | 'profile' | 'memory' | 'player' | 'summary'
  size: number
}

export interface CompanionDebugState {
  protocol: 'mineintent.debug-state.v1'
  revision: number
  capturedAt: string
  connection: Readonly<BackendState>
  body?: {
    position: Vec3Value
    health: number
    food: number
    inventory: Array<{ itemName: string; count: number }>
  }
  currentBodyTool?: { id: string; tool: string; purpose: string; startedAt: string }
  recentFailures: readonly DebugFailureSummary[]
  observations?: PassiveObservations
  decision?: {
    status: 'idle' | 'running' | 'failed'
    runId?: string
    model?: string
    startedAt?: string
    contextSources: readonly DebugContextSource[]
    retrievedMemoryIds: readonly string[]
  }
}

export type DebugStateInput = Omit<CompanionDebugState, 'protocol' | 'revision' | 'capturedAt'>
