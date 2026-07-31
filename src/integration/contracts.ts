export type IntegrationPhase = 'harness' | 'setup' | 'participant' | 'assertion' | 'cleanup'
export interface IntegrationRecord { sequence: number; at: string; scenario: string; phase: IntegrationPhase; type: string; detail: unknown }
export interface IntegrationRecorder { record(scenario: string, phase: IntegrationPhase, type: string, detail: unknown): void }
export interface PaperScenarioContext {
  signal: AbortSignal
  record(phase: IntegrationPhase, type: string, detail: unknown): void
}
export interface PaperScenario {
  name: string
  timeoutMs: number
  setup(ctx: PaperScenarioContext): Promise<void>
  run(ctx: PaperScenarioContext): Promise<void>
  cleanup(ctx: PaperScenarioContext): Promise<void>
}
export interface ScenarioResult { name: string; status: 'passed' | 'failed' | 'timed_out'; durationMs: number; error?: string }
