use std::{
    cmp::Ordering,
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use mineintent_contracts::minecraft::{BoxFuture, OperationControl};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::contracts::{
    InformationAudience, InformationFieldDefinition, InformationInterfaceId, InformationProvider,
    InformationProviderContext, InformationProviderDefinition, InformationProviderDescriptor,
    InformationProviderError, InformationProviderLimits, InformationProviderPagination,
    InformationProviderSelectors, ProviderAvailability, ProviderReadRequest, ProviderReadResult,
};

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationRegistryError {
    #[error("information registry is sealed")]
    Sealed,
    #[error("information registry is already sealed")]
    AlreadySealed,
    #[error("information registry must be sealed before use")]
    NotSealed,
    #[error("target Minecraft version is required")]
    TargetMinecraftVersionRequired,
    #[error("information registry has no providers")]
    NoProviders,
    #[error("duplicate information provider: {id}")]
    DuplicateProvider { id: String },
    #[error("provider {provider} has invalid definition: {reason}")]
    InvalidDefinition {
        provider: String,
        reason: InformationDefinitionError,
    },
    #[error("information registry lock was poisoned during {operation}")]
    LockPoisoned { operation: &'static str },
    #[error("failed to serialize the canonical information catalog: {message}")]
    CatalogSerialization { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationDefinitionError {
    #[error("description is empty")]
    MissingDescription,
    #[error("schema revision is empty")]
    MissingSchemaRevision,
    #[error("audiences are empty or duplicated")]
    InvalidAudiences,
    #[error("scope dependencies contain duplicates")]
    InvalidScopeDependencies,
    #[error("fields are empty")]
    MissingFields,
    #[error("field id is empty")]
    EmptyFieldId,
    #[error("field {field} has no description")]
    MissingFieldDescription { field: String },
    #[error("field {field} has no value type")]
    MissingFieldValueType { field: String },
    #[error("field {field} has empty or duplicated source kinds")]
    InvalidFieldSourceKinds { field: String },
    #[error("maxFieldsPerRead must be a positive integer")]
    InvalidFieldLimit,
    #[error("maxResultBytes must be a positive integer")]
    InvalidByteLimit,
    #[error("timeoutMs must be a positive integer")]
    InvalidTimeout,
    #[error("pagination limits are invalid")]
    InvalidPaginationLimits,
    #[error("selector kinds are empty or duplicated")]
    InvalidSelectorKinds,
}

pub struct RegisteredInformationProvider {
    definition: InformationProviderDefinition,
    provider: Arc<dyn InformationProvider>,
}

impl RegisteredInformationProvider {
    pub fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }
}

impl InformationProvider for RegisteredInformationProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, context: &InformationProviderContext<'_>) -> ProviderAvailability {
        self.provider.availability(context)
    }

    fn read<'a>(
        &'a self,
        context: InformationProviderContext<'a>,
        request: ProviderReadRequest,
        control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>> {
        self.provider.read(context, request, control)
    }
}

#[derive(Default)]
struct RegistryState {
    providers: BTreeMap<String, Arc<RegisteredInformationProvider>>,
    sealed: bool,
    target_minecraft_version: Option<String>,
    catalog_revision: Option<String>,
}

#[derive(Default)]
pub struct InformationRegistry {
    state: RwLock<RegistryState>,
}

impl InformationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks `sealed` without calling provider code, releases that read lock, then copies and
    /// validates the definition. The write-lock check below closes the concurrent-seal race, so
    /// provider code is never called while registry state is locked.
    pub fn register(
        &self,
        provider: Arc<dyn InformationProvider>,
    ) -> Result<(), InformationRegistryError> {
        {
            let state = self
                .state
                .read()
                .map_err(|_| InformationRegistryError::LockPoisoned {
                    operation: "preflight register",
                })?;
            if state.sealed {
                return Err(InformationRegistryError::Sealed);
            }
        }

        let definition = freeze_definition(provider.definition());
        validate_definition(&definition)?;
        let id = interface_id_name(definition.id).to_owned();
        let registered = Arc::new(RegisteredInformationProvider {
            definition,
            provider,
        });

        let mut state = self
            .state
            .write()
            .map_err(|_| InformationRegistryError::LockPoisoned {
                operation: "register",
            })?;
        if state.sealed {
            return Err(InformationRegistryError::Sealed);
        }
        if state.providers.contains_key(&id) {
            return Err(InformationRegistryError::DuplicateProvider { id });
        }
        state.providers.insert(id, registered);
        Ok(())
    }

    pub fn seal(&self, target_minecraft_version: &str) -> Result<(), InformationRegistryError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| InformationRegistryError::LockPoisoned { operation: "seal" })?;
        if state.sealed {
            return Err(InformationRegistryError::AlreadySealed);
        }
        if target_minecraft_version.trim().is_empty() {
            return Err(InformationRegistryError::TargetMinecraftVersionRequired);
        }
        if state.providers.is_empty() {
            return Err(InformationRegistryError::NoProviders);
        }

        let mut canonical = descriptors_unchecked(&state);
        for descriptor in &mut canonical {
            descriptor
                .audiences
                .sort_by(|left, right| js_string_cmp(audience_name(*left), audience_name(*right)));
            descriptor
                .field_ids
                .sort_by(|left, right| js_string_cmp(left, right));
        }
        let revision_input = CatalogRevisionInput {
            target_minecraft_version,
            providers: &canonical,
        };
        let encoded = serde_json::to_vec(&revision_input).map_err(|error| {
            InformationRegistryError::CatalogSerialization {
                message: error.to_string(),
            }
        })?;
        let digest = Sha256::digest(encoded);
        let hash = first_eight_bytes_hex(digest.as_slice());

        state.target_minecraft_version = Some(target_minecraft_version.to_owned());
        state.catalog_revision = Some(format!("catalog:{target_minecraft_version}:{hash}"));
        state.sealed = true;
        Ok(())
    }

    pub fn provider(
        &self,
        id: InformationInterfaceId,
    ) -> Result<Option<Arc<RegisteredInformationProvider>>, InformationRegistryError> {
        let state = self
            .state
            .read()
            .map_err(|_| InformationRegistryError::LockPoisoned {
                operation: "read provider",
            })?;
        require_sealed(&state)?;
        Ok(state.providers.get(interface_id_name(id)).cloned())
    }

    pub fn descriptors(
        &self,
    ) -> Result<Vec<InformationProviderDescriptor>, InformationRegistryError> {
        let state = self
            .state
            .read()
            .map_err(|_| InformationRegistryError::LockPoisoned {
                operation: "read descriptors",
            })?;
        Ok(descriptors_unchecked(&state))
    }

    pub fn catalog_revision(&self) -> Result<String, InformationRegistryError> {
        let state = self
            .state
            .read()
            .map_err(|_| InformationRegistryError::LockPoisoned {
                operation: "read catalog revision",
            })?;
        require_sealed(&state)?;
        state
            .catalog_revision
            .clone()
            .ok_or(InformationRegistryError::NotSealed)
    }

    pub fn target_minecraft_version(&self) -> Result<String, InformationRegistryError> {
        let state = self
            .state
            .read()
            .map_err(|_| InformationRegistryError::LockPoisoned {
                operation: "read target version",
            })?;
        require_sealed(&state)?;
        state
            .target_minecraft_version
            .clone()
            .ok_or(InformationRegistryError::NotSealed)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogRevisionInput<'a> {
    target_minecraft_version: &'a str,
    providers: &'a [InformationProviderDescriptor],
}

fn require_sealed(state: &RegistryState) -> Result<(), InformationRegistryError> {
    if state.sealed {
        Ok(())
    } else {
        Err(InformationRegistryError::NotSealed)
    }
}

fn descriptors_unchecked(state: &RegistryState) -> Vec<InformationProviderDescriptor> {
    state
        .providers
        .values()
        .map(|provider| {
            let definition = &provider.definition;
            let mut field_ids: Vec<_> = definition.fields.keys().cloned().collect();
            field_ids.sort_by(|left, right| js_string_cmp(left, right));
            InformationProviderDescriptor {
                id: definition.id,
                description: definition.description.clone(),
                schema_revision: definition.schema_revision.clone(),
                audiences: definition.audiences.clone(),
                field_ids,
            }
        })
        .collect()
}

fn validate_definition(
    definition: &InformationProviderDefinition,
) -> Result<(), InformationRegistryError> {
    let provider = interface_id_name(definition.id).to_owned();
    let invalid = |reason| InformationRegistryError::InvalidDefinition {
        provider: provider.clone(),
        reason,
    };

    if definition.description.trim().is_empty() {
        return Err(invalid(InformationDefinitionError::MissingDescription));
    }
    if definition.schema_revision.trim().is_empty() {
        return Err(invalid(InformationDefinitionError::MissingSchemaRevision));
    }
    if definition.audiences.is_empty() || has_duplicates(&definition.audiences) {
        return Err(invalid(InformationDefinitionError::InvalidAudiences));
    }
    if has_duplicates(&definition.scope_dependencies) {
        return Err(invalid(
            InformationDefinitionError::InvalidScopeDependencies,
        ));
    }
    if definition.fields.is_empty() {
        return Err(invalid(InformationDefinitionError::MissingFields));
    }
    for (field_id, field) in &definition.fields {
        if field_id.trim().is_empty() {
            return Err(invalid(InformationDefinitionError::EmptyFieldId));
        }
        if field.description.trim().is_empty() {
            return Err(invalid(
                InformationDefinitionError::MissingFieldDescription {
                    field: field_id.clone(),
                },
            ));
        }
        if field.value_type.trim().is_empty() {
            return Err(invalid(InformationDefinitionError::MissingFieldValueType {
                field: field_id.clone(),
            }));
        }
        if field.source_kinds.is_empty() || has_duplicates(&field.source_kinds) {
            return Err(invalid(
                InformationDefinitionError::InvalidFieldSourceKinds {
                    field: field_id.clone(),
                },
            ));
        }
    }
    if definition.limits.max_fields_per_read < 1 {
        return Err(invalid(InformationDefinitionError::InvalidFieldLimit));
    }
    if definition.limits.max_result_bytes < 1 {
        return Err(invalid(InformationDefinitionError::InvalidByteLimit));
    }
    if definition.limits.timeout_ms < 1 {
        return Err(invalid(InformationDefinitionError::InvalidTimeout));
    }
    if let Some(pagination) = &definition.pagination {
        if pagination.default_limit < 1 || pagination.max_limit < pagination.default_limit {
            return Err(invalid(InformationDefinitionError::InvalidPaginationLimits));
        }
    }
    if let Some(selectors) = &definition.selectors {
        if selectors.accepts_kinds.is_empty() || has_duplicates(&selectors.accepts_kinds) {
            return Err(invalid(InformationDefinitionError::InvalidSelectorKinds));
        }
    }
    Ok(())
}

fn freeze_definition(definition: &InformationProviderDefinition) -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: definition.id,
        description: definition.description.clone(),
        schema_revision: definition.schema_revision.clone(),
        audiences: definition.audiences.clone(),
        fields: definition
            .fields
            .iter()
            .map(|(id, field)| (id.clone(), clone_field_definition(field)))
            .collect(),
        scope_dependencies: definition.scope_dependencies.clone(),
        selectors: definition.selectors.as_ref().map(clone_selectors),
        pagination: definition.pagination.as_ref().map(clone_pagination),
        limits: clone_limits(&definition.limits),
    }
}

fn clone_field_definition(field: &InformationFieldDefinition) -> InformationFieldDefinition {
    InformationFieldDefinition {
        description: field.description.clone(),
        value_schema: Arc::clone(&field.value_schema),
        value_type: field.value_type.clone(),
        unit: field.unit.clone(),
        precision: field.precision,
        source_kinds: field.source_kinds.clone(),
        requires: field.requires.clone(),
        notes: field.notes.clone(),
    }
}

fn clone_selectors(selectors: &InformationProviderSelectors) -> InformationProviderSelectors {
    InformationProviderSelectors {
        required: selectors.required,
        accepts_kinds: selectors.accepts_kinds.clone(),
    }
}

fn clone_pagination(pagination: &InformationProviderPagination) -> InformationProviderPagination {
    InformationProviderPagination {
        default_limit: pagination.default_limit,
        max_limit: pagination.max_limit,
    }
}

fn clone_limits(limits: &InformationProviderLimits) -> InformationProviderLimits {
    InformationProviderLimits {
        max_fields_per_read: limits.max_fields_per_read,
        max_result_bytes: limits.max_result_bytes,
        timeout_ms: limits.timeout_ms,
    }
}

fn has_duplicates<T>(values: &[T]) -> bool
where
    T: Eq,
{
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}

fn js_string_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn first_eight_bytes_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn audience_name(audience: InformationAudience) -> &'static str {
    match audience {
        InformationAudience::Participant => "participant",
        InformationAudience::Controller => "controller",
        InformationAudience::Operator => "operator",
    }
}

fn interface_id_name(id: InformationInterfaceId) -> &'static str {
    match id {
        InformationInterfaceId::UiContext => "ui_context",
        InformationInterfaceId::CurrentStatus => "current_status",
        InformationInterfaceId::HotbarInformation => "hotbar_information",
        InformationInterfaceId::InventoryInformation => "inventory_information",
        InformationInterfaceId::ItemTooltipInformation => "item_tooltip_information",
        InformationInterfaceId::F3Information => "f3_information",
        InformationInterfaceId::CrosshairInformation => "crosshair_information",
        InformationInterfaceId::HudInformation => "hud_information",
        InformationInterfaceId::ChatInformation => "chat_information",
        InformationInterfaceId::PlayerListInformation => "player_list_information",
        InformationInterfaceId::CurrentScreenInformation => "current_screen_information",
        InformationInterfaceId::AdvancementInformation => "advancement_information",
        InformationInterfaceId::RecipeBookInformation => "recipe_book_information",
        InformationInterfaceId::ViewportInformation => "viewport_information",
        InformationInterfaceId::SoundInformation => "sound_information",
        InformationInterfaceId::LifecycleInformation => "lifecycle_information",
        InformationInterfaceId::ClientDiagnostics => "client_diagnostics",
    }
}
