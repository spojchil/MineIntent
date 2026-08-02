//! Information v1 runtime control plane.
//!
//! The runtime is intentionally a thin orchestrator over the registry, policy, scope, ref,
//! cursor and trace components.  Provider code is called only after all synchronous checks have
//! completed and never while one of those component locks is held.

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use mineintent_contracts::minecraft::{BoxFuture, OperationControl};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{sleep, timeout, Duration};
use uuid::Uuid;

use super::{
    access_policy::{
        InformationAccessPolicy, InformationAuthorizationOperation, InformationAuthorizationResult,
    },
    contracts::{
        parse_information_catalog_request, parse_information_query_request, InformationAcquisition,
        InformationAvailability, InformationAvailabilityMode, InformationCatalogEntry,
        InformationCatalogEntryAvailability, InformationCatalogNotModified,
        InformationCatalogNotModifiedStatus, InformationCatalogOk, InformationCatalogOkStatus,
        InformationCatalogProtocol, InformationCatalogResult, InformationErrorCode,
        InformationErrorProtocol, InformationFieldHelp, InformationGrant, InformationHelpProtocol,
        InformationHelpResult, InformationInterfaceId, InformationProvider,
        InformationProviderContext, InformationProviderDescriptor, InformationProviderError,
        InformationReadProtocol, InformationReadResult, InformationRequestError,
        InformationScopeSnapshot, InformationSelectorRef, InformationToolResult,
        InformationUnavailableReason, InformationValues, ProviderAvailability, ProviderPageRequest,
        ProviderReadRequest, ProviderReadResult, TrustedInformationCaller,
    },
    control::{child_operation_control, pending_unit, RuntimeCancellation, RuntimeDeadline},
    cursor_store::{
        InformationCursorIssueInput, InformationCursorResolveInput, InformationCursorStore,
    },
    ref_store::{InformationRefResolveInput, InformationRefStore},
    registry::{InformationRegistry, InformationRegistryError, RegisteredInformationProvider},
    scope::{scope_changed, InformationScopeSource},
    support::{format_utc_millis, javascript_json_bytes, parse_javascript_date_millis},
    trace::{InformationTraceSink, NoopInformationTrace},
    InformationClock, SystemInformationClock,
};

/// Runtime construction is fallible because the registry must already be sealed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationRuntimeInitError {
    #[error("information runtime registry is unavailable: {0}")]
    Registry(#[from] InformationRegistryError),
}

pub struct InformationRuntimeOptions {
    pub registry: Arc<InformationRegistry>,
    pub access_policy: Arc<dyn InformationAccessPolicy>,
    pub scope_source: Arc<dyn InformationScopeSource>,
    pub ref_store: InformationRefStore,
    pub cursor_store: InformationCursorStore,
    pub trace: Arc<dyn InformationTraceSink>,
    pub negotiated_minecraft_version: Option<String>,
    pub clock: Arc<dyn InformationClock>,
}

impl InformationRuntimeOptions {
    pub fn new(
        registry: Arc<InformationRegistry>,
        access_policy: Arc<dyn InformationAccessPolicy>,
        scope_source: Arc<dyn InformationScopeSource>,
    ) -> Self {
        Self {
            registry,
            access_policy,
            scope_source,
            ref_store: InformationRefStore::default(),
            cursor_store: InformationCursorStore::default(),
            trace: Arc::new(NoopInformationTrace),
            negotiated_minecraft_version: None,
            clock: Arc::new(SystemInformationClock),
        }
    }
}

pub struct InformationRuntime {
    registry: Arc<InformationRegistry>,
    access_policy: Arc<dyn InformationAccessPolicy>,
    scope_source: Arc<dyn InformationScopeSource>,
    ref_store: InformationRefStore,
    cursor_store: InformationCursorStore,
    trace: Arc<dyn InformationTraceSink>,
    negotiated_minecraft_version: Option<String>,
    clock: Arc<dyn InformationClock>,
}

impl InformationRuntime {
    pub fn new(options: InformationRuntimeOptions) -> Result<Self, InformationRuntimeInitError> {
        // This also makes the sealed-registry invariant explicit at the runtime boundary.
        options.registry.catalog_revision()?;
        Ok(Self {
            registry: options.registry,
            access_policy: options.access_policy,
            scope_source: options.scope_source,
            ref_store: options.ref_store,
            cursor_store: options.cursor_store,
            trace: options.trace,
            negotiated_minecraft_version: options.negotiated_minecraft_version,
            clock: options.clock,
        })
    }

    pub fn catalog(
        &self,
        caller: &TrustedInformationCaller,
        raw_request: &str,
    ) -> Result<InformationCatalogResult, InformationRequestError> {
        let request = parse_information_catalog_request(raw_request).map_err(|_| {
            error(
                InformationErrorCode::InvalidRequest,
                "Invalid information catalog request.",
                None,
            )
        })?;
        let scope = self.capture_scope(None)?;
        let grant = self.resolve_grant(caller, None)?;
        let descriptors = self.registry.descriptors().map_err(|_| {
            error(
                InformationErrorCode::ProviderFailed,
                "An information provider could not report availability.",
                None,
            )
        })?;

        let mut interfaces = Vec::new();
        let mut revision_entries = Vec::new();
        for descriptor in descriptors {
            let Some(provider) = self.provider(descriptor.id, None)? else {
                continue;
            };
            if !self.authorize(
                &grant,
                &descriptor,
                InformationAuthorizationOperation::Catalog,
                &[],
                &scope,
            ) {
                continue;
            }
            let visible_field_ids = descriptor
                .field_ids
                .iter()
                .filter(|field| {
                    self.authorize(
                        &grant,
                        &descriptor,
                        InformationAuthorizationOperation::Help,
                        std::slice::from_ref(*field),
                        &scope,
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            if visible_field_ids.is_empty() {
                continue;
            }

            let availability = self
                .provider_availability(caller, &grant, &scope, &provider, descriptor.id)
                .map_err(|_| {
                    error(
                        InformationErrorCode::ProviderFailed,
                        "An information provider could not report availability.",
                        None,
                    )
                })?;
            interfaces.push(InformationCatalogEntry {
                id: descriptor.id,
                description: descriptor.description,
                schema_revision: descriptor.schema_revision.clone(),
                audiences: descriptor.audiences,
                availability: availability.overall,
            });
            revision_entries.push(VisibleCatalogRevision {
                id: descriptor.id,
                schema_revision: descriptor.schema_revision,
                field_ids: visible_field_ids,
            });
        }

        let catalog_revision = visible_catalog_revision(
            &self.registry.catalog_revision().map_err(|_| {
                error(
                    InformationErrorCode::ProviderFailed,
                    "An information provider could not report availability.",
                    None,
                )
            })?,
            &revision_entries,
        )?;
        if request.known_catalog_revision.as_deref() == Some(catalog_revision.as_str()) {
            return Ok(InformationCatalogResult::NotModified(
                InformationCatalogNotModified {
                    protocol: InformationCatalogProtocol::V1,
                    status: InformationCatalogNotModifiedStatus::NotModified,
                    catalog_revision,
                },
            ));
        }
        let target_minecraft_version = self.registry.target_minecraft_version().map_err(|_| {
            error(
                InformationErrorCode::ProviderFailed,
                "An information provider could not report availability.",
                None,
            )
        })?;
        Ok(InformationCatalogResult::Ok(InformationCatalogOk {
            protocol: InformationCatalogProtocol::V1,
            status: InformationCatalogOkStatus::Ok,
            target_minecraft_version,
            negotiated_minecraft_version: self.negotiated_minecraft_version.clone(),
            catalog_revision,
            interfaces,
        }))
    }

    pub fn query<'a>(
        &'a self,
        caller: &'a TrustedInformationCaller,
        raw_request: &'a str,
        control: OperationControl,
    ) -> BoxFuture<'a, InformationToolResult> {
        Box::pin(async move {
            let request = match parse_information_query_request(raw_request) {
                Ok(request) => request,
                Err(_) => {
                    return InformationToolResult::Error(error(
                        InformationErrorCode::InvalidRequest,
                        "Invalid information query request.",
                        None,
                    ));
                }
            };
            let interface_id = match &request {
                super::contracts::InformationQueryRequest::Help(request) => request.interface_id,
                super::contracts::InformationQueryRequest::Read(request) => request.interface_id,
            };
            let Some(provider) = (match self.provider(interface_id, Some(interface_id)) {
                Ok(provider) => provider,
                Err(error) => return InformationToolResult::Error(error),
            }) else {
                return InformationToolResult::Error(error(
                    InformationErrorCode::UnknownInterface,
                    "Unknown information interface.",
                    Some(interface_id),
                ));
            };
            let descriptor = match self.descriptor(interface_id) {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => {
                    return InformationToolResult::Error(error(
                        InformationErrorCode::UnknownInterface,
                        "Unknown information interface.",
                        Some(interface_id),
                    ));
                }
                Err(error) => return InformationToolResult::Error(error),
            };
            let scope = match self.capture_scope(Some(interface_id)) {
                Ok(scope) => scope,
                Err(error) => return InformationToolResult::Error(error),
            };
            let grant = match self.resolve_grant(caller, Some(interface_id)) {
                Ok(grant) => grant,
                Err(error) => return InformationToolResult::Error(error),
            };
            match request {
                super::contracts::InformationQueryRequest::Help(request) => {
                    match self.help(caller, &grant, &provider, &descriptor, &request, &scope) {
                        Ok(result) => InformationToolResult::Help(result),
                        Err(error) => InformationToolResult::Error(error),
                    }
                }
                super::contracts::InformationQueryRequest::Read(request) => {
                    self.read(
                        caller,
                        &grant,
                        &provider,
                        &descriptor,
                        &request,
                        scope,
                        control,
                    )
                    .await
                }
            }
        })
    }

    pub fn invalidate(&self, event: &super::contracts::InformationInvalidationEvent) {
        let _ = self.ref_store.invalidate(event);
        let _ = self.cursor_store.invalidate(event);
    }

    fn provider(
        &self,
        interface_id: InformationInterfaceId,
        error_interface: Option<InformationInterfaceId>,
    ) -> Result<Option<Arc<RegisteredInformationProvider>>, InformationRequestError> {
        self.registry.provider(interface_id).map_err(|_| {
            error(
                InformationErrorCode::ProviderFailed,
                "The information registry is unavailable.",
                error_interface,
            )
        })
    }

    fn descriptor(
        &self,
        interface_id: InformationInterfaceId,
    ) -> Result<Option<InformationProviderDescriptor>, InformationRequestError> {
        self.registry
            .descriptors()
            .map_err(|_| {
                error(
                    InformationErrorCode::ProviderFailed,
                    "The information registry is unavailable.",
                    Some(interface_id),
                )
            })
            .map(|descriptors| {
                descriptors
                    .into_iter()
                    .find(|descriptor| descriptor.id == interface_id)
            })
    }

    fn capture_scope(
        &self,
        interface_id: Option<InformationInterfaceId>,
    ) -> Result<InformationScopeSnapshot, InformationRequestError> {
        catch_unwind(AssertUnwindSafe(|| self.scope_source.capture())).map_err(|_| {
            error(
                InformationErrorCode::ProviderFailed,
                "The information scope is unavailable.",
                interface_id,
            )
        })
    }

    fn resolve_grant(
        &self,
        caller: &TrustedInformationCaller,
        interface_id: Option<InformationInterfaceId>,
    ) -> Result<InformationGrant, InformationRequestError> {
        let grant = catch_unwind(AssertUnwindSafe(|| {
            self.access_policy
                .resolve(&caller.grant_id, &caller.principal_id)
        }))
        .ok()
        .and_then(Result::ok)
        .flatten();
        match grant {
            Some(grant) if grant.purpose == caller.purpose => Ok(grant),
            _ => Err(error(
                InformationErrorCode::AudienceDenied,
                "The caller has no valid information grant.",
                interface_id,
            )),
        }
    }

    fn authorize(
        &self,
        grant: &InformationGrant,
        descriptor: &InformationProviderDescriptor,
        operation: InformationAuthorizationOperation,
        fields: &[String],
        scope: &InformationScopeSnapshot,
    ) -> bool {
        catch_unwind(AssertUnwindSafe(|| {
            self.access_policy
                .authorize(grant, descriptor, operation, fields, scope)
        }))
        .map_or(false, |result| {
            matches!(result, InformationAuthorizationResult::Allowed)
        })
    }

    fn provider_availability(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        scope: &InformationScopeSnapshot,
        provider: &Arc<RegisteredInformationProvider>,
        interface_id: InformationInterfaceId,
    ) -> Result<ProviderAvailability, ()> {
        let now = format_utc_millis(self.clock.now_millis(), || scope.captured_at.clone())
            .unwrap_or_else(|_| scope.captured_at.clone());
        let issuer = self
            .ref_store
            .issuer(super::ref_store::InformationRefIssuerInput {
                interface_id,
                principal_id: caller.principal_id.clone(),
                grant: grant.clone(),
                scope: scope.clone(),
            });
        let context = InformationProviderContext {
            now: &now,
            scope,
            caller: super::contracts::InformationProviderCaller {
                audience: grant.audience,
                purpose: grant.purpose,
            },
            refs: &issuer,
        };
        let availability =
            catch_unwind(AssertUnwindSafe(|| provider.availability(&context))).map_err(|_| ())?;
        validate_availability(provider, &availability).map_err(|_| ())?;
        Ok(availability)
    }

    fn help(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        provider: &Arc<RegisteredInformationProvider>,
        descriptor: &InformationProviderDescriptor,
        request: &super::contracts::InformationHelpRequest,
        scope: &InformationScopeSnapshot,
    ) -> Result<InformationHelpResult, InformationRequestError> {
        let all_field_ids = &descriptor.field_ids;
        let requested_fields = match &request.fields {
            Some(fields) => fields.clone(),
            None => all_field_ids
                .iter()
                .filter(|field| {
                    self.authorize(
                        grant,
                        descriptor,
                        InformationAuthorizationOperation::Help,
                        std::slice::from_ref(*field),
                        scope,
                    )
                })
                .cloned()
                .collect(),
        };
        let unknown_fields = requested_fields
            .iter()
            .filter(|field| !all_field_ids.contains(field))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown_fields.is_empty() {
            return Err(error_with(
                InformationErrorCode::UnknownField,
                "One or more information fields are unknown.",
                Some(request.interface_id),
                None,
                Some(descriptor.schema_revision.clone()),
                Some(unknown_fields),
            ));
        }
        if !self.authorize(
            grant,
            descriptor,
            InformationAuthorizationOperation::Help,
            &requested_fields,
            scope,
        ) {
            return Err(error(
                InformationErrorCode::AudienceDenied,
                "The requested information fields are not allowed.",
                Some(request.interface_id),
            ));
        }

        let availability = if request.availability == Some(InformationAvailabilityMode::Current) {
            Some(
                self.provider_availability(caller, grant, scope, provider, request.interface_id)
                    .map_err(|_| {
                        error(
                            InformationErrorCode::ProviderFailed,
                            "The information provider could not report availability.",
                            Some(request.interface_id),
                        )
                    })?,
            )
        } else {
            None
        };
        let search = request.search.as_deref().map(str::to_lowercase);
        let definition = provider.definition();
        let fields = requested_fields
            .iter()
            .filter_map(|field_id| {
                let field = definition.fields.get(field_id)?;
                let availability = availability
                    .as_ref()
                    .and_then(|availability| availability.fields.get(field_id).copied())
                    .map_or(InformationAvailability::Available, Into::into);
                let help = InformationFieldHelp {
                    id: field_id.clone(),
                    description: field.description.clone(),
                    value_type: field.value_type.clone(),
                    unit: field.unit.clone(),
                    precision: field.precision,
                    interface_id: request.interface_id,
                    source_kinds: field.source_kinds.clone(),
                    availability,
                    requires: field.requires.clone(),
                    notes: field.notes.clone(),
                };
                let matches = search.as_ref().is_none_or(|search| {
                    help.id.to_lowercase().contains(search)
                        || help.description.to_lowercase().contains(search)
                });
                matches.then_some(help)
            })
            .collect();
        Ok(InformationHelpResult {
            protocol: InformationHelpProtocol::V1,
            interface_id: request.interface_id,
            schema_revision: descriptor.schema_revision.clone(),
            availability_mode: request
                .availability
                .unwrap_or(InformationAvailabilityMode::All),
            fields,
        })
    }

    async fn read(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        provider: &Arc<RegisteredInformationProvider>,
        descriptor: &InformationProviderDescriptor,
        request: &super::contracts::InformationReadRequest,
        scope_before: InformationScopeSnapshot,
        control: OperationControl,
    ) -> InformationToolResult {
        if request.schema_revision != descriptor.schema_revision {
            return InformationToolResult::Error(error_with(
                InformationErrorCode::StaleSchema,
                "The information schema changed; call help again.",
                Some(request.interface_id),
                None,
                Some(descriptor.schema_revision.clone()),
                None,
            ));
        }
        let mut fields = Vec::with_capacity(request.fields.len());
        for field in &request.fields {
            if fields.iter().any(|existing| existing == field) {
                return InformationToolResult::Error(error(
                    InformationErrorCode::InvalidRequest,
                    "Duplicate information fields are not allowed.",
                    Some(request.interface_id),
                ));
            }
            fields.push(field.clone());
        }
        let unknown_fields = fields
            .iter()
            .filter(|field| !descriptor.field_ids.contains(field))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown_fields.is_empty() {
            return InformationToolResult::Error(error_with(
                InformationErrorCode::UnknownField,
                "One or more information fields are unknown.",
                Some(request.interface_id),
                None,
                Some(descriptor.schema_revision.clone()),
                Some(unknown_fields),
            ));
        }
        if fields.len() as u64 > provider.definition().limits.max_fields_per_read {
            return InformationToolResult::Error(error(
                InformationErrorCode::InvalidRequest,
                "The information field limit was exceeded.",
                Some(request.interface_id),
            ));
        }
        if !self.authorize(
            grant,
            descriptor,
            InformationAuthorizationOperation::Read,
            &fields,
            &scope_before,
        ) {
            return InformationToolResult::Error(error(
                InformationErrorCode::AudienceDenied,
                "The requested information fields are not allowed.",
                Some(request.interface_id),
            ));
        }

        let selector = match self.resolve_selector(
            caller,
            grant,
            provider,
            request.interface_id,
            request.selector.as_ref(),
            &scope_before,
        ) {
            Ok(selector) => selector,
            Err(error) => return InformationToolResult::Error(error),
        };
        let pagination =
            match self.resolve_page(caller, grant, provider, request, &fields, &scope_before) {
                Ok(pagination) => pagination,
                Err(error) => return InformationToolResult::Error(error),
            };

        // Match the TypeScript runtime's `withTimeout` boundary: schema, field,
        // authorization, selector, and cursor validation happen before an already-cancelled
        // signal is observed.  In particular, resolving a cursor here intentionally consumes
        // its one-shot entry before provider execution begins.
        if control.cancellation().is_cancelled()
            || control
                .deadline()
                .is_some_and(|deadline| deadline.has_elapsed())
        {
            return InformationToolResult::Error(error(
                InformationErrorCode::DeadlineExceeded,
                "The information read deadline elapsed.",
                Some(request.interface_id),
            ));
        }

        let now = format_utc_millis(self.clock.now_millis(), || scope_before.captured_at.clone())
            .unwrap_or_else(|_| scope_before.captured_at.clone());
        let issuer = self
            .ref_store
            .issuer(super::ref_store::InformationRefIssuerInput {
                interface_id: request.interface_id,
                principal_id: caller.principal_id.clone(),
                grant: grant.clone(),
                scope: scope_before.clone(),
            });
        let context = InformationProviderContext {
            now: &now,
            scope: &scope_before,
            caller: super::contracts::InformationProviderCaller {
                audience: grant.audience,
                purpose: grant.purpose,
            },
            refs: &issuer,
        };
        let provider_request = ProviderReadRequest {
            fields: fields.clone(),
            selector: selector.clone(),
            page: ProviderPageRequest {
                limit: pagination.limit,
                state: pagination.state.clone(),
            },
        };
        let (child_control, child_cancel, child_deadline) = child_operation_control();
        let provider_future = match catch_unwind(AssertUnwindSafe(|| {
            provider.read(context, provider_request, child_control)
        })) {
            Ok(future) => future,
            Err(_) => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::ProviderFailed,
                    "The information provider failed.",
                    Some(request.interface_id),
                ));
            }
        };
        let provider_result = await_provider(
            provider_future,
            &control,
            child_cancel,
            child_deadline,
            provider.definition().limits.timeout_ms,
        )
        .await;
        let internal = match provider_result {
            ProviderCallResult::Returned(Ok(result)) => result,
            ProviderCallResult::Returned(Err(InformationProviderError::Cancelled))
            | ProviderCallResult::Returned(Err(InformationProviderError::DeadlineExceeded))
            | ProviderCallResult::Deadline => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::DeadlineExceeded,
                    "The information read deadline elapsed.",
                    Some(request.interface_id),
                ));
            }
            ProviderCallResult::Returned(Err(InformationProviderError::Failed { .. })) => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::ProviderFailed,
                    "The information provider failed.",
                    Some(request.interface_id),
                ));
            }
            ProviderCallResult::Panic => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::ProviderFailed,
                    "The information provider failed.",
                    Some(request.interface_id),
                ));
            }
        };

        let values = match validate_provider_result(provider, &fields, &internal) {
            Ok(values) => values,
            Err(message) => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::ProviderFailed,
                    message,
                    Some(request.interface_id),
                ));
            }
        };
        if let Some(expected) = pagination.expected_information_revision {
            if expected != internal.information_revision {
                return InformationToolResult::Error(error(
                    InformationErrorCode::InvalidPage,
                    "The paged information changed.",
                    Some(request.interface_id),
                ));
            }
        }
        let scope_after = match self.capture_scope(Some(request.interface_id)) {
            Ok(scope) => scope,
            Err(error) => return InformationToolResult::Error(error),
        };
        if scope_changed(
            &scope_before,
            &scope_after,
            &provider.definition().scope_dependencies,
        ) {
            return InformationToolResult::Error(error(
                InformationErrorCode::ScopeChanged,
                "The information scope changed during the read.",
                Some(request.interface_id),
            ));
        }
        if let Some(reference) = request.selector.as_ref() {
            if !self.selector_source_is_current(caller, grant, reference, &scope_after) {
                return InformationToolResult::Error(error(
                    InformationErrorCode::InvalidSelector,
                    "The selector source changed during the read.",
                    Some(request.interface_id),
                ));
            }
        }

        let read_id = format!("read_{}", Uuid::new_v4());
        let next_cursor = if let Some(page_state) = internal.next_page_state.clone() {
            match self.cursor_store.issue(InformationCursorIssueInput {
                interface_id: request.interface_id,
                fields: fields.clone(),
                selector: request.selector.clone(),
                information_revision: internal.information_revision,
                limit: pagination.limit,
                page_state,
                principal_id: caller.principal_id.clone(),
                grant: grant.clone(),
                scope: scope_after.clone(),
            }) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return InformationToolResult::Error(error(
                        InformationErrorCode::ProviderFailed,
                        "The information provider returned invalid page state.",
                        Some(request.interface_id),
                    ));
                }
            }
        } else {
            None
        };
        let result = InformationReadResult {
            protocol: InformationReadProtocol::V1,
            read_id: read_id.clone(),
            interface_id: request.interface_id,
            schema_revision: descriptor.schema_revision.clone(),
            information_revision: internal.information_revision,
            connection_epoch: scope_after.connection_epoch,
            world_id: scope_after.world_id.clone(),
            dimension: scope_after.dimension.clone(),
            observed_at: internal.observed_at.clone(),
            valid_until: internal.valid_until.clone(),
            source: internal.source.clone(),
            values,
            unavailable: internal.unavailable.clone(),
            evidence_ids: internal.evidence_ids.clone(),
            next_cursor,
        };
        let result_bytes = match javascript_json_bytes(&result) {
            Ok(bytes) => bytes,
            Err(_) => {
                return InformationToolResult::Error(error(
                    InformationErrorCode::ProviderFailed,
                    "The information provider returned a non-JSON result.",
                    Some(request.interface_id),
                ));
            }
        };
        if result_bytes.len() as u64 > provider.definition().limits.max_result_bytes {
            return InformationToolResult::Error(error(
                InformationErrorCode::ProviderFailed,
                "The information provider exceeded its result limit.",
                Some(request.interface_id),
            ));
        }
        let trace_record = super::contracts::InformationTraceRecord {
            read_id,
            interface_id: request.interface_id,
            fields,
            source_kind: result.source.kind,
            source_revision: result.source.source_revision,
            evidence_ids: result.evidence_ids.clone(),
            correlation_id: caller.correlation_id.clone(),
            observed_at: result.observed_at.clone(),
        };
        if catch_unwind(AssertUnwindSafe(|| self.trace.append(trace_record))).is_err() {
            return InformationToolResult::Error(error(
                InformationErrorCode::ProviderFailed,
                "The information trace sink failed.",
                Some(request.interface_id),
            ));
        }
        InformationToolResult::Read(result)
    }

    fn resolve_selector(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        provider: &Arc<RegisteredInformationProvider>,
        interface_id: InformationInterfaceId,
        reference: Option<&InformationSelectorRef>,
        scope: &InformationScopeSnapshot,
    ) -> Result<Option<Value>, InformationRequestError> {
        let selector_definition = provider.definition().selectors.as_ref();
        if selector_definition.is_some_and(|definition| definition.required) && reference.is_none()
        {
            return Err(error(
                InformationErrorCode::InvalidSelector,
                "This information interface requires a selector.",
                Some(interface_id),
            ));
        }
        if selector_definition.is_none() && reference.is_some() {
            return Err(error(
                InformationErrorCode::InvalidSelector,
                "This information interface does not accept selectors.",
                Some(interface_id),
            ));
        }
        let Some(reference) = reference else {
            return Ok(None);
        };
        let payload = self
            .ref_store
            .resolve(InformationRefResolveInput {
                reference: reference.clone(),
                target_interface: interface_id,
                principal_id: caller.principal_id.clone(),
                grant: grant.clone(),
                scope: scope.clone(),
                accepted_kinds: selector_definition
                    .map(|definition| definition.accepts_kinds.clone()),
            })
            .map_err(|_| {
                error(
                    InformationErrorCode::InvalidSelector,
                    "The information selector is invalid or stale.",
                    Some(interface_id),
                )
            })?
            .ok_or_else(|| {
                error(
                    InformationErrorCode::InvalidSelector,
                    "The information selector is invalid or stale.",
                    Some(interface_id),
                )
            })?;
        if !self.selector_source_is_current(caller, grant, reference, scope) {
            return Err(error(
                InformationErrorCode::InvalidSelector,
                "The selector source is no longer available.",
                Some(interface_id),
            ));
        }
        Ok(Some(payload))
    }

    fn selector_source_is_current(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        reference: &InformationSelectorRef,
        scope: &InformationScopeSnapshot,
    ) -> bool {
        let provider = match self.provider(reference.interface_id, Some(reference.interface_id)) {
            Ok(Some(provider)) => provider,
            _ => return false,
        };
        let descriptor = match self.descriptor(reference.interface_id) {
            Ok(Some(descriptor)) => descriptor,
            _ => return false,
        };
        if !self.authorize(
            grant,
            &descriptor,
            InformationAuthorizationOperation::Help,
            &[],
            scope,
        ) {
            return false;
        }
        self.provider_availability(caller, grant, scope, &provider, reference.interface_id)
            .map(|availability| {
                availability.overall != InformationCatalogEntryAvailability::Unavailable
                    && availability.information_revision == reference.based_on_information_revision
            })
            .unwrap_or(false)
    }

    fn resolve_page(
        &self,
        caller: &TrustedInformationCaller,
        grant: &InformationGrant,
        provider: &Arc<RegisteredInformationProvider>,
        request: &super::contracts::InformationReadRequest,
        fields: &[String],
        scope: &InformationScopeSnapshot,
    ) -> Result<ResolvedPage, InformationRequestError> {
        let definition = provider.definition().pagination.as_ref();
        let Some(definition) = definition else {
            if request.page.is_some() {
                return Err(error(
                    InformationErrorCode::InvalidPage,
                    "This information interface is not paginated.",
                    Some(request.interface_id),
                ));
            }
            return Ok(ResolvedPage {
                limit: 1,
                state: None,
                expected_information_revision: None,
            });
        };
        let limit = request
            .page
            .as_ref()
            .and_then(|page| page.limit)
            .unwrap_or(definition.default_limit);
        if limit < 1 || limit > definition.max_limit {
            return Err(error(
                InformationErrorCode::InvalidPage,
                "The information page limit is invalid.",
                Some(request.interface_id),
            ));
        }
        let Some(cursor) = request.page.as_ref().and_then(|page| page.cursor.as_ref()) else {
            return Ok(ResolvedPage {
                limit,
                state: None,
                expected_information_revision: None,
            });
        };
        let resolved = self
            .cursor_store
            .resolve(InformationCursorResolveInput {
                cursor: cursor.clone(),
                interface_id: request.interface_id,
                fields: fields.to_vec(),
                selector: request.selector.clone(),
                limit,
                principal_id: caller.principal_id.clone(),
                grant: grant.clone(),
                scope: scope.clone(),
            })
            .map_err(|_| {
                error(
                    InformationErrorCode::InvalidPage,
                    "The information cursor is invalid or stale.",
                    Some(request.interface_id),
                )
            })?
            .ok_or_else(|| {
                error(
                    InformationErrorCode::InvalidPage,
                    "The information cursor is invalid or stale.",
                    Some(request.interface_id),
                )
            })?;
        Ok(ResolvedPage {
            limit,
            state: Some(resolved.state),
            expected_information_revision: Some(resolved.information_revision),
        })
    }
}

impl super::tool_session::InformationRuntimePort for InformationRuntime {
    fn catalog(
        &self,
        caller: &TrustedInformationCaller,
        request: &str,
    ) -> Result<InformationCatalogResult, InformationRequestError> {
        InformationRuntime::catalog(self, caller, request)
    }

    fn query<'a>(
        &'a self,
        caller: &'a TrustedInformationCaller,
        request: &'a str,
        control: OperationControl,
    ) -> BoxFuture<'a, InformationToolResult> {
        InformationRuntime::query(self, caller, request, control)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ResolvedPage {
    limit: u64,
    state: Option<Value>,
    expected_information_revision: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VisibleCatalogRevision {
    id: InformationInterfaceId,
    schema_revision: String,
    field_ids: Vec<String>,
}

fn visible_catalog_revision(
    base_revision: &str,
    entries: &[VisibleCatalogRevision],
) -> Result<String, InformationRequestError> {
    let encoded = serde_json::to_vec(entries).map_err(|_| {
        error(
            InformationErrorCode::ProviderFailed,
            "The information catalog could not be serialized.",
            None,
        )
    })?;
    let digest = Sha256::digest(encoded);
    let mut suffix = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        suffix.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        suffix.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    Ok(format!("{base_revision}:{suffix}"))
}

fn validate_availability(
    provider: &RegisteredInformationProvider,
    availability: &ProviderAvailability,
) -> Result<(), &'static str> {
    let known_fields = provider.definition().fields.keys().collect::<HashSet<_>>();
    if availability
        .fields
        .keys()
        .any(|field| !known_fields.contains(field))
    {
        return Err("invalid field availability");
    }
    if availability.overall == InformationCatalogEntryAvailability::Unavailable
        && provider
            .definition()
            .fields
            .keys()
            .any(|field| !availability.fields.contains_key(field))
    {
        return Err("unavailable provider must explain every field");
    }
    Ok(())
}

fn validate_provider_result(
    provider: &RegisteredInformationProvider,
    requested_fields: &[String],
    result: &ProviderReadResult,
) -> Result<InformationValues, &'static str> {
    let returned_fields = result.values.keys().cloned().collect::<Vec<_>>();
    if returned_fields
        .iter()
        .any(|field| !requested_fields.contains(field))
    {
        return Err("The information provider returned unrequested fields.");
    }

    let mut parsed_values = BTreeMap::new();
    for field in &returned_fields {
        let Some(definition) = provider.definition().fields.get(field) else {
            return Err("The information provider returned an invalid field value.");
        };
        let value = result
            .values
            .get(field)
            .cloned()
            .ok_or("The information provider returned an invalid field value.")?;
        let parsed = match catch_unwind(AssertUnwindSafe(|| definition.value_schema.parse(value))) {
            Ok(Ok(value)) => value,
            Ok(Err(_)) | Err(_) => {
                return Err("The information provider returned an invalid field value.");
            }
        };
        if !definition.source_kinds.contains(&result.source.kind) {
            return Err("The information provider returned a disallowed field source.");
        }
        let cloned = serde_json::to_vec(&parsed)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .ok_or("The information provider returned a non-JSON field value.")?;
        parsed_values.insert(field.clone(), cloned);
    }

    let mut unavailable_fields = HashSet::new();
    for unavailable in &result.unavailable {
        if !requested_fields.contains(&unavailable.field)
            || !unavailable_fields.insert(unavailable.field.clone())
        {
            return Err("The information provider returned invalid unavailable fields.");
        }
    }
    if returned_fields
        .iter()
        .any(|field| unavailable_fields.contains(field))
    {
        return Err("The information provider returned a field as both available and unavailable.");
    }
    if requested_fields
        .iter()
        .any(|field| !returned_fields.contains(field) && !unavailable_fields.contains(field))
    {
        return Err("The information provider omitted a requested field without explanation.");
    }
    if result.source.adapter_revision.is_empty()
        || parse_javascript_date_millis(&result.observed_at).is_none()
        || result
            .valid_until
            .as_deref()
            .is_some_and(|timestamp| parse_javascript_date_millis(timestamp).is_none())
        || result
            .evidence_ids
            .iter()
            .any(|id| id.is_empty() || id.encode_utf16().count() > 256)
    {
        return Err("The information provider returned invalid source metadata.");
    }
    // All enum variants are contract-validated at deserialization/construction time.  Keep the
    // explicit source-kind/acquisition checks here so this boundary remains auditable if the
    // contract gains a dynamically decoded representation later.
    if !super::contracts::INFORMATION_SOURCE_KINDS.contains(&result.source.kind)
        || !matches!(
            result.source.acquisition,
            InformationAcquisition::ImmediateClientState
                | InformationAcquisition::StructuredUiEquivalent
                | InformationAcquisition::CurrentScreen
                | InformationAcquisition::CurrentPerception
                | InformationAcquisition::OperatorOnly
        )
    {
        return Err("The information provider returned invalid source metadata.");
    }
    Ok(parsed_values)
}

enum ProviderCallResult {
    Returned(Result<ProviderReadResult, InformationProviderError>),
    Deadline,
    Panic,
}

async fn await_provider<'a>(
    future: BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>>,
    parent: &OperationControl,
    child_cancellation: Arc<RuntimeCancellation>,
    child_deadline: Arc<RuntimeDeadline>,
    timeout_ms: u64,
) -> ProviderCallResult {
    let future = CatchUnwindFuture::new(future);
    let mut parent_cancelled = parent.cancelled();
    let mut parent_deadline = parent.deadline_elapsed().unwrap_or_else(pending_unit);
    let mut timer = Box::pin(sleep(Duration::from_millis(timeout_ms.max(1))));
    tokio::pin!(future);
    let result = tokio::select! {
        result = &mut future => match result {
            Ok(result) => ProviderCallResult::Returned(result),
            Err(()) => ProviderCallResult::Panic,
        },
        _ = &mut parent_cancelled => {
            child_cancellation.trigger();
            drain_provider(&mut future).await;
            ProviderCallResult::Deadline
        },
        _ = &mut parent_deadline => {
            child_deadline.trigger();
            drain_provider(&mut future).await;
            ProviderCallResult::Deadline
        },
        _ = &mut timer => {
            child_deadline.trigger();
            drain_provider(&mut future).await;
            ProviderCallResult::Deadline
        },
    };
    result
}

async fn drain_provider<F>(future: &mut Pin<&mut CatchUnwindFuture<F>>)
where
    F: Future + Send,
{
    let _ = timeout(Duration::from_millis(50), future).await;
}

/// `std` has no async catch-unwind adapter.  Catching each poll protects the runtime from both a
/// synchronous panic in `read` and a panic later inside a provider future.
struct CatchUnwindFuture<F> {
    future: Pin<Box<F>>,
}

impl<F> CatchUnwindFuture<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}

impl<F: Future> Future for CatchUnwindFuture<F> {
    type Output = Result<F::Output, ()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(context)))
            .map_or(Poll::Ready(Err(())), |poll| poll.map(Ok))
    }
}

fn error(
    code: InformationErrorCode,
    message: &str,
    interface_id: Option<InformationInterfaceId>,
) -> InformationRequestError {
    error_with(code, message, interface_id, None, None, None)
}

fn error_with(
    code: InformationErrorCode,
    message: &str,
    interface_id: Option<InformationInterfaceId>,
    current_catalog_revision: Option<String>,
    current_schema_revision: Option<String>,
    rejected_fields: Option<Vec<String>>,
) -> InformationRequestError {
    InformationRequestError {
        protocol: InformationErrorProtocol::V1,
        interface_id,
        code,
        message: message.to_owned(),
        current_catalog_revision,
        current_schema_revision,
        rejected_fields,
    }
}

impl From<InformationUnavailableReason> for InformationAvailability {
    fn from(reason: InformationUnavailableReason) -> Self {
        match reason {
            InformationUnavailableReason::NotConnected => InformationAvailability::NotConnected,
            InformationUnavailableReason::ScreenNotOpen => InformationAvailability::ScreenNotOpen,
            InformationUnavailableReason::NotCurrentlyDisplayed => {
                InformationAvailability::NotCurrentlyDisplayed
            }
            InformationUnavailableReason::BlockedByReducedDebug => {
                InformationAvailability::BlockedByReducedDebug
            }
            InformationUnavailableReason::UnsupportedGameMode => {
                InformationAvailability::UnsupportedGameMode
            }
            InformationUnavailableReason::PermissionRequired => {
                InformationAvailability::PermissionRequired
            }
            InformationUnavailableReason::NotSupported => InformationAvailability::NotSupported,
            InformationUnavailableReason::NotExposed => InformationAvailability::NotExposed,
        }
    }
}
