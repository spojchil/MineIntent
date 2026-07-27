import type { ExecutionResource } from '../../execution/index.js'
import type { WireToolDefinition } from '../../models/index.js'
import { z } from 'zod'

export const TOOL_RESULT_PROTOCOL = 'mineintent.tool-result.v1'

/** Facts minted by the run/round host for one capability invocation. */
export interface CapabilityInvocation {
  runId: string
  toolCallId: string
  roundId: string
  arguments: Record<string, unknown>
  actionId: string
  startedAt: string
}

/**
 * The lifecycle boundary shared by tool capabilities. It carries only scope facts and guards;
 * concrete services are injected into each capability factory separately.
 */
export interface CapabilityScope {
  signal: AbortSignal
  worldId: string
  chatEventId: string
  assertCurrent(): void
  isCurrent(): boolean
}

export interface ToolCapability<
  Resource extends ExecutionResource | undefined = ExecutionResource | undefined,
> {
  name: string
  description: string
  argumentsSchema: z.ZodType
  resource: Resource
  execute(invocation: CapabilityInvocation, scope: CapabilityScope): Promise<unknown> | unknown
}

/**
 * The sole catalog and dispatch index. Both views are derived from the same capability instances,
 * so registering a model-visible contract without its executor is not a representable state.
 */
export class ToolCapabilityRegistry {
  readonly #capabilities: readonly ToolCapability[]
  readonly #dispatch: ReadonlyMap<string, ToolCapability>

  constructor(capabilities: readonly ToolCapability[]) {
    const names = capabilities.map(capability => capability.name)
    if (new Set(names).size !== names.length) throw new Error('duplicate_tool_capability')

    this.#capabilities = [...capabilities]
    this.#dispatch = new Map(capabilities.map(capability => [capability.name, capability]))

    // This is intentionally independent of the duplicate-name check. If dispatch construction is
    // ever changed and silently skips a registered capability, startup fails before a model can be
    // shown a tool that would later return unknown_tool.
    for (const capability of capabilities) {
      if (this.#dispatch.get(capability.name) !== capability) {
        throw new Error(`tool_capability_dispatch_incomplete:${capability.name}`)
      }
    }
  }

  resolve(name: string): ToolCapability | undefined { return this.#dispatch.get(name) }

  definitions(): WireToolDefinition[] {
    return this.#capabilities.map(capability => ({
      type: 'function',
      function: {
        name: capability.name,
        description: capability.description,
        parameters: z.toJSONSchema(capability.argumentsSchema) as Record<string, unknown>,
      },
    }))
  }
}
