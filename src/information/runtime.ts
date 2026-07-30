import { createHash, randomUUID } from 'node:crypto'
import {
  INFORMATION_AVAILABILITIES,
  INFORMATION_SOURCE_KINDS,
  informationCatalogRequestSchema,
  informationQueryRequestSchema,
  type InformationCatalogRequest,
  type InformationCatalogResult,
  type InformationFieldDefinition,
  type InformationFieldHelp,
  type InformationGrant,
  type InformationHelpResult,
  type InformationInterfaceId,
  type InformationInvalidationEvent,
  type InformationProviderContext,
  type InformationProviderDescriptor,
  type InformationQueryRequest,
  type InformationReadResult,
  type InformationRequestError,
  type ResolvedInformationReference,
  type InformationScopeSnapshot,
  type InformationSelectorRef,
  type InformationToolResult,
  type ProviderReadResult,
  type TrustedInformationCaller,
} from './contracts/index.js'
import type { InformationAccessPolicy } from './access-policy.js'
import { InformationCursorStore } from './cursor-store.js'
import { InformationRefStore } from './ref-store.js'
import { InformationRegistry, type RegisteredInformationProvider } from './registry.js'
import { scopeChanged, type InformationScopeSource } from './scope.js'
import { noopInformationTrace, type InformationTraceSink } from './trace.js'

export interface InformationRuntimeOptions {
  registry: InformationRegistry
  accessPolicy: InformationAccessPolicy
  scopeSource: InformationScopeSource
  refStore?: InformationRefStore
  cursorStore?: InformationCursorStore
  trace?: InformationTraceSink
  negotiatedMinecraftVersion?: string
  now?: () => Date
}

export class InformationRuntime {
  readonly #registry: InformationRegistry
  readonly #accessPolicy: InformationAccessPolicy
  readonly #scopeSource: InformationScopeSource
  readonly #refStore: InformationRefStore
  readonly #cursorStore: InformationCursorStore
  readonly #trace: InformationTraceSink
  readonly #negotiatedMinecraftVersion?: string
  readonly #now: () => Date

  constructor(options: InformationRuntimeOptions) {
    this.#registry = options.registry
    this.#accessPolicy = options.accessPolicy
    this.#scopeSource = options.scopeSource
    this.#refStore = options.refStore ?? new InformationRefStore()
    this.#cursorStore = options.cursorStore ?? new InformationCursorStore()
    this.#trace = options.trace ?? noopInformationTrace
    this.#negotiatedMinecraftVersion = options.negotiatedMinecraftVersion
    this.#now = options.now ?? (() => new Date())
    this.#registry.catalogRevision()
  }

  catalog(
    caller: TrustedInformationCaller,
    rawRequest: unknown,
  ): InformationCatalogResult | InformationRequestError {
    const parsed = informationCatalogRequestSchema.safeParse(rawRequest)
    if (!parsed.success) return error('invalid_request', 'Invalid information catalog request.')
    const request: InformationCatalogRequest = parsed.data
    const scope = this.#scopeSource.capture()
    const grant = this.#accessPolicy.resolve(caller.grantId, caller.principalId)
    if (!grant || grant.purpose !== caller.purpose) {
      return error('audience_denied', 'The caller has no valid information grant.')
    }
    const interfaces = []
    const revisionEntries: Array<{
      id: InformationInterfaceId
      schemaRevision: string
      fieldIds: string[]
    }> = []
    for (const descriptor of this.#registry.descriptors()) {
      const provider = this.#registry.provider(descriptor.id)!
      if (!this.#accessPolicy.authorize(grant, descriptor, 'catalog', [], scope).allowed) continue
      const visibleFieldIds = descriptor.fieldIds.filter((field) =>
        this.#accessPolicy.authorize(grant, descriptor, 'help', [field], scope).allowed)
      if (visibleFieldIds.length === 0) continue
      let availability: ReturnType<RegisteredInformationProvider['availability']>
      try {
        availability = provider.availability(this.#providerContext(
          descriptor.id,
          caller,
          grant,
          scope,
        ))
        validateAvailability(provider, availability)
      } catch {
        return error('provider_failed', 'An information provider could not report availability.')
      }
      interfaces.push({
        id: descriptor.id,
        description: descriptor.description,
        schemaRevision: descriptor.schemaRevision,
        audiences: [...descriptor.audiences],
        availability: availability.overall,
      })
      revisionEntries.push({
        id: descriptor.id,
        schemaRevision: descriptor.schemaRevision,
        fieldIds: visibleFieldIds,
      })
    }

    const catalogRevision = visibleCatalogRevision(
      this.#registry.catalogRevision(),
      revisionEntries,
    )
    if (request.knownCatalogRevision === catalogRevision) {
      return {
        protocol: 'mineintent.information-catalog.v1',
        status: 'not_modified',
        catalogRevision,
      }
    }

    return {
      protocol: 'mineintent.information-catalog.v1',
      status: 'ok',
      targetMinecraftVersion: this.#registry.targetMinecraftVersion(),
      ...(this.#negotiatedMinecraftVersion
        ? { negotiatedMinecraftVersion: this.#negotiatedMinecraftVersion }
        : {}),
      catalogRevision,
      interfaces,
    }
  }

  async query(
    caller: TrustedInformationCaller,
    rawRequest: unknown,
    signal: AbortSignal,
  ): Promise<InformationToolResult> {
    const parsed = informationQueryRequestSchema.safeParse(rawRequest)
    if (!parsed.success) return error('invalid_request', 'Invalid information query request.')
    const request: InformationQueryRequest = parsed.data
    const provider = this.#registry.provider(request.interfaceId)
    if (!provider) {
      return error('unknown_interface', 'Unknown information interface.', request.interfaceId)
    }
    const descriptor = this.#descriptor(request.interfaceId)
    const scope = this.#scopeSource.capture()
    const grant = this.#accessPolicy.resolve(caller.grantId, caller.principalId)
    if (!grant || grant.purpose !== caller.purpose) {
      return error('audience_denied', 'The caller has no valid information grant.', request.interfaceId)
    }
    return request.operation === 'help'
      ? this.#help(caller, grant, provider, descriptor, request, scope)
      : this.#read(caller, grant, provider, descriptor, request, scope, signal)
  }

  invalidate(event: InformationInvalidationEvent): void {
    this.#refStore.invalidate(event)
    this.#cursorStore.invalidate(event)
  }

  resolveContextReference<Payload>(
    caller: TrustedInformationCaller,
    id: string,
    acceptedKinds?: readonly string[],
  ): ResolvedInformationReference<Payload> | undefined {
    const scope = this.#scopeSource.capture()
    const grant = this.#accessPolicy.resolve(caller.grantId, caller.principalId)
    if (!grant || grant.purpose !== caller.purpose) return undefined
    const resolved = this.#refStore.resolveById<Payload>({
      id,
      principalId: caller.principalId,
      grant,
      scope,
      ...(acceptedKinds ? { acceptedKinds } : {}),
    })
    // A reference copied from a verified decision context names an observation that
    // already happened. Unlike a selector for a new read, it must not become
    // unusable merely because the aggregate source revision advanced afterwards.
    // Ref-store scope and TTL checks still apply, and Grounding separately proves
    // that the exact ref was conveyed by the current context before resolving it.
    return resolved
  }

  #help(
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    provider: RegisteredInformationProvider,
    descriptor: InformationProviderDescriptor,
    request: Extract<InformationQueryRequest, { operation: 'help' }>,
    scope: InformationScopeSnapshot,
  ): InformationHelpResult | InformationRequestError {
    const allFieldIds = descriptor.fieldIds
    const requestedFields = request.fields ?? allFieldIds.filter((field) =>
      this.#accessPolicy.authorize(grant, descriptor, 'help', [field], scope).allowed)
    const unknownFields = requestedFields.filter((field) => !allFieldIds.includes(field))
    if (unknownFields.length > 0) {
      return error('unknown_field', 'One or more information fields are unknown.', request.interfaceId, {
        rejectedFields: unknownFields,
        currentSchemaRevision: descriptor.schemaRevision,
      })
    }
    if (!this.#accessPolicy.authorize(grant, descriptor, 'help', requestedFields, scope).allowed) {
      return error('audience_denied', 'The requested information fields are not allowed.', request.interfaceId)
    }

    let currentAvailability: ReturnType<RegisteredInformationProvider['availability']> | undefined
    const availabilityMode = request.availability ?? 'all'
    if (availabilityMode === 'current') {
      try {
        currentAvailability = provider.availability(this.#providerContext(
          request.interfaceId,
          caller,
          grant,
          scope,
        ))
        validateAvailability(provider, currentAvailability)
      } catch {
        return error('provider_failed', 'The information provider could not report availability.', request.interfaceId)
      }
    }

    const search = request.search?.toLocaleLowerCase()
    const fields = requestedFields
      .map((fieldId) => toFieldHelp(
        request.interfaceId,
        fieldId,
        provider.definition.fields[fieldId]!,
        currentAvailability?.fields[fieldId] ?? 'available',
      ))
      .filter((field) => !search ||
        field.id.toLocaleLowerCase().includes(search) ||
        field.description.toLocaleLowerCase().includes(search))

    return {
      protocol: 'mineintent.information-help.v1',
      interfaceId: request.interfaceId,
      schemaRevision: descriptor.schemaRevision,
      availabilityMode,
      fields,
    }
  }

  async #read(
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    provider: RegisteredInformationProvider,
    descriptor: InformationProviderDescriptor,
    request: Extract<InformationQueryRequest, { operation: 'read' }>,
    scopeBefore: InformationScopeSnapshot,
    signal: AbortSignal,
  ): Promise<InformationToolResult> {
    if (request.schemaRevision !== descriptor.schemaRevision) {
      return error('stale_schema', 'The information schema changed; call help again.', request.interfaceId, {
        currentSchemaRevision: descriptor.schemaRevision,
      })
    }
    const fields = [...new Set(request.fields)]
    if (fields.length !== request.fields.length) {
      return error('invalid_request', 'Duplicate information fields are not allowed.', request.interfaceId)
    }
    const unknownFields = fields.filter((field) => !descriptor.fieldIds.includes(field))
    if (unknownFields.length > 0) {
      return error('unknown_field', 'One or more information fields are unknown.', request.interfaceId, {
        rejectedFields: unknownFields,
        currentSchemaRevision: descriptor.schemaRevision,
      })
    }
    if (fields.length > provider.definition.limits.maxFieldsPerRead) {
      return error('invalid_request', 'The information field limit was exceeded.', request.interfaceId)
    }
    if (!this.#accessPolicy.authorize(grant, descriptor, 'read', fields, scopeBefore).allowed) {
      return error('audience_denied', 'The requested information fields are not allowed.', request.interfaceId)
    }

    const selector = this.#resolveSelector(
      caller,
      grant,
      provider,
      request.interfaceId,
      request.selector,
      scopeBefore,
    )
    if ('error' in selector) return selector.error

    const pagination = this.#resolvePage(
      caller,
      grant,
      provider,
      request,
      fields,
      scopeBefore,
    )
    if ('error' in pagination) return pagination.error

    const context = this.#providerContext(request.interfaceId, caller, grant, scopeBefore)
    let internal: ProviderReadResult<Record<string, unknown>, unknown>
    try {
      internal = await withTimeout(
        (operationSignal) => provider.read(context, {
          fields,
          ...(selector.payload !== undefined ? { selector: selector.payload } : {}),
          page: {
            limit: pagination.limit,
            ...(pagination.state !== undefined ? { state: pagination.state } : {}),
          },
        }, operationSignal),
        provider.definition.limits.timeoutMs,
        signal,
      )
    } catch (caught) {
      const code = caught === deadlineMarker || signal.aborted
        ? 'deadline_exceeded'
        : 'provider_failed'
      return error(code, code === 'deadline_exceeded'
        ? 'The information read deadline elapsed.'
        : 'The information provider failed.', request.interfaceId)
    }

    const validation = validateProviderResult(provider, fields, internal)
    if (!validation.ok) return error('provider_failed', validation.message, request.interfaceId)
    if (pagination.expectedInformationRevision !== undefined &&
        pagination.expectedInformationRevision !== internal.informationRevision) {
      return error('invalid_page', 'The paged information changed.', request.interfaceId)
    }
    const scopeAfter = this.#scopeSource.capture()
    if (scopeChanged(scopeBefore, scopeAfter, provider.definition.scopeDependencies)) {
      return error('scope_changed', 'The information scope changed during the read.', request.interfaceId)
    }
    if (request.selector &&
        !this.#selectorSourceIsCurrent(caller, grant, request.selector, scopeAfter)) {
      return error('invalid_selector', 'The selector source changed during the read.', request.interfaceId)
    }

    const readId = `read_${randomUUID()}`
    let nextCursor: string | undefined
    try {
      if (internal.nextPageState !== undefined) {
        nextCursor = this.#cursorStore.issue({
          interfaceId: request.interfaceId,
          fields,
          ...(request.selector ? { selector: request.selector } : {}),
          informationRevision: internal.informationRevision,
          limit: pagination.limit,
          pageState: internal.nextPageState,
          principalId: caller.principalId,
          grant,
          scope: scopeAfter,
        })
      }
    } catch {
      return error('provider_failed', 'The information provider returned invalid page state.', request.interfaceId)
    }
    const result: InformationReadResult<Record<string, unknown>> = {
      protocol: 'mineintent.information-read.v1',
      readId,
      interfaceId: request.interfaceId,
      schemaRevision: descriptor.schemaRevision,
      informationRevision: internal.informationRevision,
      connectionEpoch: scopeAfter.connectionEpoch,
      ...(scopeAfter.worldId ? { worldId: scopeAfter.worldId } : {}),
      ...(scopeAfter.dimension ? { dimension: scopeAfter.dimension } : {}),
      observedAt: internal.observedAt,
      ...(internal.validUntil ? { validUntil: internal.validUntil } : {}),
      source: {
        kind: internal.source.kind,
        adapterRevision: internal.source.adapterRevision,
        sourceRevision: internal.source.sourceRevision,
        acquisition: internal.source.acquisition,
      },
      values: validation.values,
      unavailable: internal.unavailable.map(({ field, reason }) => ({ field, reason })),
      evidenceIds: [...internal.evidenceIds],
      ...(nextCursor ? { nextCursor } : {}),
    }
    if (Buffer.byteLength(JSON.stringify(result), 'utf8') > provider.definition.limits.maxResultBytes) {
      return error('provider_failed', 'The information provider exceeded its result limit.', request.interfaceId)
    }
    this.#trace.append({
      readId,
      interfaceId: request.interfaceId,
      fields,
      sourceKind: internal.source.kind,
      sourceRevision: internal.source.sourceRevision,
      evidenceIds: internal.evidenceIds,
      correlationId: caller.correlationId,
      observedAt: internal.observedAt,
    })
    return result
  }

  #resolveSelector(
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    provider: RegisteredInformationProvider,
    interfaceId: InformationInterfaceId,
    ref: InformationSelectorRef | undefined,
    scope: InformationScopeSnapshot,
  ): { payload?: unknown } | { error: InformationRequestError } {
    const selectorDefinition = provider.definition.selectors
    if (selectorDefinition?.required && !ref) {
      return { error: error('invalid_selector', 'This information interface requires a selector.', interfaceId) }
    }
    if (!selectorDefinition && ref) {
      return { error: error('invalid_selector', 'This information interface does not accept selectors.', interfaceId) }
    }
    if (!ref) return {}
    const payload = this.#refStore.resolve({
      ref,
      targetInterface: interfaceId,
      principalId: caller.principalId,
      grant,
      scope,
      acceptedKinds: selectorDefinition?.acceptsKinds,
    })
    if (payload === undefined) {
      return { error: error('invalid_selector', 'The information selector is invalid or stale.', interfaceId) }
    }
    if (!this.#selectorSourceIsCurrent(caller, grant, ref, scope)) {
      return { error: error('invalid_selector', 'The selector source is no longer available.', interfaceId) }
    }
    return { payload }
  }

  #selectorSourceIsCurrent(
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    ref: InformationSelectorRef,
    scope: InformationScopeSnapshot,
  ): boolean {
    const sourceProvider = this.#registry.provider(ref.interfaceId)
    const sourceDescriptor = this.#registry.descriptors().find(({ id }) => id === ref.interfaceId)
    if (!sourceProvider || !sourceDescriptor ||
        !this.#accessPolicy.authorize(grant, sourceDescriptor, 'help', [], scope).allowed) return false
    try {
      const availability = sourceProvider.availability(this.#providerContext(
        ref.interfaceId,
        caller,
        grant,
        scope,
      ))
      validateAvailability(sourceProvider, availability)
      return availability.overall !== 'unavailable' &&
        availability.informationRevision === ref.basedOnInformationRevision
    } catch {
      return false
    }
  }

  #resolvePage(
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    provider: RegisteredInformationProvider,
    request: Extract<InformationQueryRequest, { operation: 'read' }>,
    fields: readonly string[],
    scope: InformationScopeSnapshot,
  ):
    | { limit: number; state?: unknown; expectedInformationRevision?: number }
    | { error: InformationRequestError } {
    const definition = provider.definition.pagination
    if (!definition) {
      return request.page
        ? { error: error('invalid_page', 'This information interface is not paginated.', request.interfaceId) }
        : { limit: 1 }
    }
    const limit = request.page?.limit ?? definition.defaultLimit
    if (limit < 1 || limit > definition.maxLimit) {
      return { error: error('invalid_page', 'The information page limit is invalid.', request.interfaceId) }
    }
    if (!request.page?.cursor) return { limit }
    const resolved = this.#cursorStore.resolve<unknown>({
      cursor: request.page.cursor,
      interfaceId: request.interfaceId,
      fields,
      ...(request.selector ? { selector: request.selector } : {}),
      limit,
      principalId: caller.principalId,
      grant,
      scope,
    })
    return resolved
      ? { limit, state: resolved.state, expectedInformationRevision: resolved.informationRevision }
      : { error: error('invalid_page', 'The information cursor is invalid or stale.', request.interfaceId) }
  }

  #providerContext(
    interfaceId: InformationInterfaceId,
    caller: TrustedInformationCaller,
    grant: InformationGrant,
    scope: InformationScopeSnapshot,
  ): InformationProviderContext {
    return {
      now: this.#now().toISOString(),
      scope,
      caller: { audience: grant.audience, purpose: grant.purpose },
      refs: this.#refStore.issuer({
        interfaceId,
        principalId: caller.principalId,
        grant,
        scope,
      }),
    }
  }

  #descriptor(interfaceId: InformationInterfaceId): InformationProviderDescriptor {
    return this.#registry.descriptors().find((item) => item.id === interfaceId)!
  }
}

function toFieldHelp(
  interfaceId: InformationInterfaceId,
  fieldId: string,
  definition: InformationFieldDefinition<unknown>,
  availability: InformationFieldHelp['availability'],
): InformationFieldHelp {
  return {
    id: fieldId,
    description: definition.description,
    valueType: definition.valueType,
    ...(definition.unit ? { unit: definition.unit } : {}),
    precision: definition.precision,
    interfaceId,
    sourceKinds: [...definition.sourceKinds],
    availability,
    ...(definition.requires ? { requires: [...definition.requires] } : {}),
    ...(definition.notes ? { notes: definition.notes } : {}),
  }
}

function validateAvailability(
  provider: RegisteredInformationProvider,
  availability: ReturnType<RegisteredInformationProvider['availability']>,
): void {
  if (!Number.isInteger(availability.informationRevision) || availability.informationRevision < 0) {
    throw new Error('Invalid information revision')
  }
  const knownFields = Object.keys(provider.definition.fields)
  for (const field of Object.keys(availability.fields)) {
    if (!knownFields.includes(field)) throw new Error('Invalid field availability')
  }
  if (availability.overall === 'unavailable' &&
      knownFields.some((field) => availability.fields[field] === undefined)) {
    throw new Error('Unavailable provider must explain every field')
  }
}

function validateProviderResult(
  provider: RegisteredInformationProvider,
  requestedFields: readonly string[],
  result: ProviderReadResult<Record<string, unknown>, unknown>,
):
  | { ok: true; values: Record<string, unknown> }
  | { ok: false; message: string } {
  if (!Number.isInteger(result.informationRevision) || result.informationRevision < 0) {
    return { ok: false, message: 'The information provider returned an invalid revision.' }
  }
  const returnedFields = Object.keys(result.values)
  if (returnedFields.some((field) => !requestedFields.includes(field))) {
    return { ok: false, message: 'The information provider returned unrequested fields.' }
  }
  const parsedValues: Record<string, unknown> = {}
  for (const field of returnedFields) {
    const definition = provider.definition.fields[field]
    const parsed = definition?.valueSchema.safeParse(result.values[field])
    if (!definition || !parsed?.success) {
      return { ok: false, message: 'The information provider returned an invalid field value.' }
    }
    if (!definition.sourceKinds.includes(result.source.kind)) {
      return { ok: false, message: 'The information provider returned a disallowed field source.' }
    }
    parsedValues[field] = parsed.data
  }
  const unavailableFields = new Set<string>()
  for (const unavailable of result.unavailable) {
    if (!requestedFields.includes(unavailable.field) || unavailableFields.has(unavailable.field)) {
      return { ok: false, message: 'The information provider returned invalid unavailable fields.' }
    }
    unavailableFields.add(unavailable.field)
  }
  if (returnedFields.some((field) => unavailableFields.has(field))) {
    return { ok: false, message: 'The information provider returned a field as both available and unavailable.' }
  }
  if (requestedFields.some((field) => !returnedFields.includes(field) && !unavailableFields.has(field))) {
    return { ok: false, message: 'The information provider omitted a requested field without explanation.' }
  }
  if (!Number.isInteger(result.source.sourceRevision) || result.source.sourceRevision < 0 ||
      !result.source.adapterRevision ||
      !INFORMATION_SOURCE_KINDS.includes(result.source.kind) ||
      ![
        'immediate_client_state',
        'structured_ui_equivalent',
        'current_screen',
        'current_perception',
        'operator_only',
      ].includes(result.source.acquisition) ||
      Number.isNaN(Date.parse(result.observedAt)) ||
      (result.validUntil !== undefined && Number.isNaN(Date.parse(result.validUntil)))) {
    return { ok: false, message: 'The information provider returned invalid source metadata.' }
  }
  const allowedUnavailable = new Set<string>([
    ...INFORMATION_AVAILABILITIES.filter((value) => value !== 'available'),
    'stale_selector',
    'wrong_world',
    'wrong_screen',
  ])
  if (result.unavailable.some(({ reason }) => !allowedUnavailable.has(reason)) ||
      result.evidenceIds.some((id) => typeof id !== 'string' || id.length === 0 || id.length > 256)) {
    return { ok: false, message: 'The information provider returned invalid evidence metadata.' }
  }
  try {
    const values = cloneJson(parsedValues)
    if (Object.keys(values).length !== returnedFields.length) {
      return { ok: false, message: 'The information provider returned a non-JSON field value.' }
    }
    return { ok: true, values }
  } catch {
    return { ok: false, message: 'The information provider returned a non-JSON field value.' }
  }
}

function visibleCatalogRevision(
  baseRevision: string,
  visibleInterfaces: readonly {
    id: string
    schemaRevision: string
    fieldIds: readonly string[]
  }[],
): string {
  const digest = createHash('sha256')
    .update(JSON.stringify(visibleInterfaces))
    .digest('hex')
    .slice(0, 12)
  return `${baseRevision}:${digest}`
}

function cloneJson<T>(value: T): T {
  const serialized = JSON.stringify(value)
  if (serialized === undefined) throw new Error('Value is not JSON serializable')
  return JSON.parse(serialized) as T
}

function error(
  code: InformationRequestError['code'],
  message: string,
  interfaceId?: InformationInterfaceId,
  extra: Pick<
    InformationRequestError,
    'currentCatalogRevision' | 'currentSchemaRevision' | 'rejectedFields'
  > = {},
): InformationRequestError {
  return {
    protocol: 'mineintent.information-error.v1',
    ...(interfaceId ? { interfaceId } : {}),
    code,
    message,
    ...extra,
  }
}

const deadlineMarker = Symbol('information-deadline')

async function withTimeout<T>(
  operation: (signal: AbortSignal) => Promise<T>,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<T> {
  if (signal.aborted) throw deadlineMarker
  const timeoutController = new AbortController()
  const operationSignal = AbortSignal.any([signal, timeoutController.signal])
  let timeout: NodeJS.Timeout | undefined
  let abortHandler: (() => void) | undefined
  const deadline = new Promise<never>((_resolve, reject) => {
    timeout = setTimeout(() => {
      timeoutController.abort(deadlineMarker)
      reject(deadlineMarker)
    }, timeoutMs)
    abortHandler = () => reject(deadlineMarker)
    signal.addEventListener('abort', abortHandler, { once: true })
  })
  try {
    return await Promise.race([operation(operationSignal), deadline])
  } finally {
    if (timeout) clearTimeout(timeout)
    if (abortHandler) signal.removeEventListener('abort', abortHandler)
  }
}
