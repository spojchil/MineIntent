import { randomUUID } from 'node:crypto'
import path from 'node:path'
import { CompanionRuntime, loadCompanionProfile } from '../companion/index.js'
import { JsonlEventJournal } from '../events/index.js'
import { FileMemoryStore } from '../memory/index.js'
import { MinecraftBackend } from '../minecraft/index.js'
import { AgentServiceModelProvider, D40ToolBridgeServer } from '../models/index.js'
import { DebugStateStore, LocalDebugServer } from '../telemetry/index.js'
import type { AppConfig } from './config.js'

export class MineIntentApp {
  readonly #config: AppConfig
  #runtime?: CompanionRuntime
  #debugServer?: LocalDebugServer
  #toolBridge?: D40ToolBridgeServer

  constructor(config: AppConfig) { this.#config = config }

  async start(): Promise<{ debugUrl: string }> {
    let runtime: CompanionRuntime | undefined
    let debugServer: LocalDebugServer | undefined
    let toolBridge: D40ToolBridgeServer | undefined
    try {
      const profile = await loadCompanionProfile(this.#config.profileFile)
      const debug = new DebugStateStore()
      const backend = new MinecraftBackend(this.#config.minecraft)
      const memory = new FileMemoryStore(path.join(this.#config.dataDirectory, 'memories.json'))
      const journal = new JsonlEventJournal(path.join(this.#config.dataDirectory, 'events.jsonl'), this.#config.minecraft.worldId, randomUUID())
      toolBridge = new D40ToolBridgeServer(async invocation => {
        if (!runtime) throw new Error('runtime_not_ready')
        return runtime.executeBodyTool(invocation)
      })
      const callback = await toolBridge.start()
      const model = new AgentServiceModelProvider({
        baseUrl: this.#config.agentServiceUrl,
        serviceToken: this.#config.agentServiceToken,
        toolCallbackUrl: callback.url,
        toolCallbackToken: callback.token,
      })
      runtime = new CompanionRuntime({ backend, model, memory, journal, profile, debug, primaryPlayer: this.#config.primaryPlayer })
      debugServer = new LocalDebugServer(debug, this.#config.debugPort)
      const address = await debugServer.start()
      await runtime.start()
      this.#runtime = runtime
      this.#debugServer = debugServer
      this.#toolBridge = toolBridge
      return { debugUrl: `${address.url}/v1/state` }
    } catch (error) {
      await closeBestEffort([
        () => runtime?.stop('app_start_failed'),
        () => debugServer?.stop(),
        () => toolBridge?.stop(),
      ])
      throw error
    }
  }

  async stop(reason = 'app_stopped'): Promise<void> {
    const runtime = this.#runtime
    const debugServer = this.#debugServer
    const toolBridge = this.#toolBridge
    this.#runtime = undefined
    this.#debugServer = undefined
    this.#toolBridge = undefined
    const errors = await closeBestEffort([
      () => runtime?.stop(reason),
      () => debugServer?.stop(),
      () => toolBridge?.stop(),
    ])
    if (errors.length > 0) throw new AggregateError(errors, 'MineIntent shutdown failed')
  }
}

async function closeBestEffort(steps: Array<() => Promise<unknown> | undefined>): Promise<unknown[]> {
  const errors: unknown[] = []
  for (const step of steps) {
    try { await step() } catch (error) { errors.push(error) }
  }
  return errors
}
