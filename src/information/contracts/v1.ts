import type { ZodType } from 'zod'

export const INFORMATION_INTERFACE_IDS = [
  'ui_context',
  'current_status',
  'hotbar_information',
  'inventory_information',
  'item_tooltip_information',
  'f3_information',
  'crosshair_information',
  'hud_information',
  'chat_information',
  'player_list_information',
  'current_screen_information',
  'advancement_information',
  'recipe_book_information',
  'viewport_information',
  'sound_information',
  'lifecycle_information',
  'client_diagnostics',
] as const

export type InformationInterfaceId = (typeof INFORMATION_INTERFACE_IDS)[number]

export const INFORMATION_AUDIENCES = ['participant', 'controller', 'operator'] as const
export type InformationAudience = (typeof INFORMATION_AUDIENCES)[number]

export const INFORMATION_SOURCE_KINDS = [
  'client_state',
  'hud_projection',
  'debug_projection',
  'screen_projection',
  'viewport_projection',
  'sound_projection',
  'lifecycle_event',
  'operator_diagnostic',
] as const
export type InformationSourceKind = (typeof INFORMATION_SOURCE_KINDS)[number]

export const INFORMATION_AVAILABILITIES = [
  'available',
  'not_connected',
  'screen_not_open',
  'not_currently_displayed',
  'blocked_by_reduced_debug',
  'unsupported_game_mode',
  'permission_required',
  'not_supported',
  'not_exposed',
] as const
export type InformationAvailability = (typeof INFORMATION_AVAILABILITIES)[number]
export type InformationUnavailableReason = Exclude<InformationAvailability, 'available'>

export const INFORMATION_SCOPE_DEPENDENCIES = [
  'connection',
  'world',
  'dimension',
  'ui',
  'screen',
] as const
export type InformationScopeDependency = (typeof INFORMATION_SCOPE_DEPENDENCIES)[number]

export interface InformationCatalogRequest {
  operation: 'list_interfaces'
  knownCatalogRevision?: string
}

export interface InformationCatalogEntry {
  id: InformationInterfaceId
  description: string
  schemaRevision: string
  audiences: InformationAudience[]
  availability: 'available' | 'partially_available' | 'unavailable'
}

export type InformationCatalogResult =
  | {
      protocol: 'mineintent.information-catalog.v1'
      status: 'ok'
      targetMinecraftVersion: string
      negotiatedMinecraftVersion?: string
      catalogRevision: string
      interfaces: InformationCatalogEntry[]
    }
  | {
      protocol: 'mineintent.information-catalog.v1'
      status: 'not_modified'
      catalogRevision: string
    }

export interface InformationPageRequest {
  cursor?: string
  limit?: number
}

export interface InformationSelectorRef {
  protocol: 'mineintent.information-selector-ref.v1'
  id: string
  interfaceId: InformationInterfaceId
  connectionEpoch: number
  worldId?: string
  screenInstanceId?: string
  basedOnInformationRevision: number
  validUntil?: string
}

export type InformationQueryRequest =
  | {
      interfaceId: InformationInterfaceId
      operation: 'help'
      availability?: 'all' | 'current'
      search?: string
      fields?: string[]
    }
  | {
      interfaceId: InformationInterfaceId
      operation: 'read'
      schemaRevision: string
      fields: string[]
      selector?: InformationSelectorRef
      page?: InformationPageRequest
    }

export interface InformationFieldHelp {
  id: string
  description: string
  valueType: string
  unit?: string
  precision: 'displayed' | 'quantized' | 'exactly_displayed' | 'inferred'
  interfaceId: InformationInterfaceId
  sourceKinds: InformationSourceKind[]
  availability: InformationAvailability
  requires?: string[]
  notes?: string
}

export interface InformationHelpResult {
  protocol: 'mineintent.information-help.v1'
  interfaceId: InformationInterfaceId
  schemaRevision: string
  availabilityMode: 'all' | 'current'
  fields: InformationFieldHelp[]
}

export interface InformationReadResult<T extends object> {
  protocol: 'mineintent.information-read.v1'
  readId: string
  interfaceId: InformationInterfaceId
  schemaRevision: string
  informationRevision: number
  connectionEpoch: number
  worldId?: string
  dimension?: string
  observedAt: string
  validUntil?: string
  source: {
    kind: InformationSourceKind
    adapterRevision: string
    sourceRevision: number
    acquisition:
      | 'immediate_client_state'
      | 'structured_ui_equivalent'
      | 'current_screen'
      | 'current_perception'
      | 'operator_only'
  }
  values: Partial<T>
  unavailable: Array<{
    field: string
    reason: InformationUnavailableReason | 'stale_selector' | 'wrong_world' | 'wrong_screen'
  }>
  evidenceIds: string[]
  nextCursor?: string
}

export const INFORMATION_ERROR_CODES = [
  'invalid_request',
  'unknown_interface',
  'stale_schema',
  'unknown_field',
  'invalid_selector',
  'invalid_page',
  'audience_denied',
  'scope_changed',
  'budget_exceeded',
  'deadline_exceeded',
  'provider_failed',
] as const
export type InformationErrorCode = (typeof INFORMATION_ERROR_CODES)[number]

export interface InformationRequestError {
  protocol: 'mineintent.information-error.v1'
  interfaceId?: InformationInterfaceId
  code: InformationErrorCode
  message: string
  currentCatalogRevision?: string
  currentSchemaRevision?: string
  rejectedFields?: string[]
}

export type InformationToolResult<T extends object = Record<string, unknown>> =
  | InformationHelpResult
  | InformationReadResult<T>
  | InformationRequestError

export interface InformationScopeSnapshot {
  processSessionId: string
  connectionState: 'disconnected' | 'connecting' | 'configuration' | 'play'
  connectionEpoch: number
  worldId?: string
  dimension?: string
  uiRevision: number
  screenInstanceId?: string
  screenRevision?: number
  capturedAt: string
}

export interface InformationGrant {
  id: string
  principalId: string
  audience: InformationAudience
  allowedInterfaces: '*' | readonly InformationInterfaceId[]
  allowedFields?: Partial<Record<InformationInterfaceId, readonly string[]>>
  connectionEpoch?: number
  worldId?: string
  screenInstanceId?: string
  purpose: 'participant_context' | 'model_tool' | 'controller' | 'operator'
  validUntil?: string
}

export interface TrustedInformationCaller {
  principalId: string
  grantId: string
  purpose: InformationGrant['purpose']
  correlationId: string
  decisionRunId?: string
  controllerLeaseId?: string
}

export type FieldId<Values extends object> = Extract<keyof Values, string>

export interface InformationFieldDefinition<Value> {
  description: string
  valueSchema: ZodType<Value>
  valueType: string
  unit?: string
  precision: 'displayed' | 'quantized' | 'exactly_displayed' | 'inferred'
  sourceKinds: readonly InformationSourceKind[]
  requires?: readonly string[]
  notes?: string
}

export interface InformationProviderDefinition<Values extends object> {
  id: InformationInterfaceId
  description: string
  schemaRevision: string
  audiences: readonly InformationAudience[]
  fields: {
    readonly [Field in FieldId<Values>]: InformationFieldDefinition<Values[Field]>
  }
  scopeDependencies: readonly InformationScopeDependency[]
  selectors?: {
    required: boolean
    acceptsKinds: readonly string[]
  }
  pagination?: {
    defaultLimit: number
    maxLimit: number
  }
  limits: {
    maxFieldsPerRead: number
    maxResultBytes: number
    timeoutMs: number
  }
}

export interface ProviderAvailability<Values extends object> {
  overall: 'available' | 'partially_available' | 'unavailable'
  informationRevision: number
  fields: Partial<Record<FieldId<Values>, InformationUnavailableReason>>
}

export interface ProviderReadRequest<Values extends object, Selector, PageState> {
  fields: readonly FieldId<Values>[]
  selector?: Selector
  page: { limit: number; state?: PageState }
}

export interface ProviderReadResult<Values extends object, PageState> {
  informationRevision: number
  values: Partial<Values>
  unavailable: Array<{
    field: FieldId<Values>
    reason: InformationUnavailableReason | 'stale_selector' | 'wrong_world' | 'wrong_screen'
  }>
  source: InformationReadResult<Record<string, unknown>>['source']
  observedAt: string
  validUntil?: string
  evidenceIds: string[]
  nextPageState?: PageState
}

export interface InformationReferenceIssueRequest<Payload> {
  kind: string
  payload: Payload
  allowedInterfaces: readonly InformationInterfaceId[]
  basedOnInformationRevision: number
  validUntil?: string
  bindToScreen?: boolean
}

export interface InformationReferenceIssuer {
  issue<Payload>(request: InformationReferenceIssueRequest<Payload>): InformationSelectorRef
}

export interface InformationProviderContext {
  now: string
  scope: Readonly<InformationScopeSnapshot>
  caller: Readonly<{
    audience: InformationAudience
    purpose: InformationGrant['purpose']
  }>
  refs: InformationReferenceIssuer
}

export interface InformationProvider<
  Values extends object,
  Selector = never,
  PageState = never,
> {
  readonly definition: InformationProviderDefinition<Values>
  availability(context: InformationProviderContext): ProviderAvailability<Values>
  read(
    context: InformationProviderContext,
    request: ProviderReadRequest<Values, Selector, PageState>,
    signal: AbortSignal,
  ): Promise<ProviderReadResult<Values, PageState>>
}

export interface InformationProviderDescriptor {
  id: InformationInterfaceId
  description: string
  schemaRevision: string
  audiences: readonly InformationAudience[]
  fieldIds: readonly string[]
}

export interface InformationToolSessionBudget {
  maxCalls: number
  maxReadCalls: number
  maxReturnedBytes: number
  deadlineAt: string
}

export interface InformationToolSessionContext {
  sessionId: string
  decisionRunId: string
  correlationId: string
  principalId: string
  grantId: string
  budget: InformationToolSessionBudget
}

export type InformationInvalidationEvent =
  | { kind: 'connection_changed'; connectionEpoch: number }
  | { kind: 'world_changed'; worldId?: string; dimension?: string }
  | { kind: 'screen_changed'; screenInstanceId?: string; screenRevision?: number }
  | { kind: 'grant_ended'; grantId: string }

export interface InformationTraceRecord {
  readId: string
  interfaceId: InformationInterfaceId
  fields: readonly string[]
  sourceKind: InformationSourceKind
  sourceRevision: number
  evidenceIds: readonly string[]
  correlationId: string
  observedAt: string
}
