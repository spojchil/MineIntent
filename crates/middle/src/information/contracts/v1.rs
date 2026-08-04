use std::{collections::BTreeMap, sync::Arc};

use mineintent_contracts::{
    information::InformationUnavailableReason as FacadeUnavailableReason,
    minecraft::{BoxFuture, OperationControl},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use mineintent_contracts::information::{InformationConnectionState, InformationScopeSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InformationInterfaceId {
    #[serde(rename = "ui_context")]
    UiContext,
    #[serde(rename = "current_status")]
    CurrentStatus,
    #[serde(rename = "hotbar_information")]
    HotbarInformation,
    #[serde(rename = "inventory_information")]
    InventoryInformation,
    #[serde(rename = "item_tooltip_information")]
    ItemTooltipInformation,
    #[serde(rename = "f3_information")]
    F3Information,
    #[serde(rename = "crosshair_information")]
    CrosshairInformation,
    #[serde(rename = "hud_information")]
    HudInformation,
    #[serde(rename = "chat_information")]
    ChatInformation,
    #[serde(rename = "player_list_information")]
    PlayerListInformation,
    #[serde(rename = "current_screen_information")]
    CurrentScreenInformation,
    #[serde(rename = "advancement_information")]
    AdvancementInformation,
    #[serde(rename = "recipe_book_information")]
    RecipeBookInformation,
    #[serde(rename = "viewport_information")]
    ViewportInformation,
    #[serde(rename = "sound_information")]
    SoundInformation,
    #[serde(rename = "lifecycle_information")]
    LifecycleInformation,
    #[serde(rename = "client_diagnostics")]
    ClientDiagnostics,
}

pub const INFORMATION_INTERFACE_IDS: [InformationInterfaceId; 17] = [
    InformationInterfaceId::UiContext,
    InformationInterfaceId::CurrentStatus,
    InformationInterfaceId::HotbarInformation,
    InformationInterfaceId::InventoryInformation,
    InformationInterfaceId::ItemTooltipInformation,
    InformationInterfaceId::F3Information,
    InformationInterfaceId::CrosshairInformation,
    InformationInterfaceId::HudInformation,
    InformationInterfaceId::ChatInformation,
    InformationInterfaceId::PlayerListInformation,
    InformationInterfaceId::CurrentScreenInformation,
    InformationInterfaceId::AdvancementInformation,
    InformationInterfaceId::RecipeBookInformation,
    InformationInterfaceId::ViewportInformation,
    InformationInterfaceId::SoundInformation,
    InformationInterfaceId::LifecycleInformation,
    InformationInterfaceId::ClientDiagnostics,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationAudience {
    Participant,
    Controller,
    Operator,
}

pub const INFORMATION_AUDIENCES: [InformationAudience; 3] = [
    InformationAudience::Participant,
    InformationAudience::Controller,
    InformationAudience::Operator,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationSourceKind {
    ClientState,
    HudProjection,
    DebugProjection,
    ScreenProjection,
    ViewportProjection,
    SoundProjection,
    LifecycleEvent,
    OperatorDiagnostic,
}

pub const INFORMATION_SOURCE_KINDS: [InformationSourceKind; 8] = [
    InformationSourceKind::ClientState,
    InformationSourceKind::HudProjection,
    InformationSourceKind::DebugProjection,
    InformationSourceKind::ScreenProjection,
    InformationSourceKind::ViewportProjection,
    InformationSourceKind::SoundProjection,
    InformationSourceKind::LifecycleEvent,
    InformationSourceKind::OperatorDiagnostic,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationAvailability {
    Available,
    NotConnected,
    ScreenNotOpen,
    NotCurrentlyDisplayed,
    BlockedByReducedDebug,
    UnsupportedGameMode,
    PermissionRequired,
    NotSupported,
    NotExposed,
}

pub const INFORMATION_AVAILABILITIES: [InformationAvailability; 9] = [
    InformationAvailability::Available,
    InformationAvailability::NotConnected,
    InformationAvailability::ScreenNotOpen,
    InformationAvailability::NotCurrentlyDisplayed,
    InformationAvailability::BlockedByReducedDebug,
    InformationAvailability::UnsupportedGameMode,
    InformationAvailability::PermissionRequired,
    InformationAvailability::NotSupported,
    InformationAvailability::NotExposed,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationUnavailableReason {
    NotConnected,
    ScreenNotOpen,
    NotCurrentlyDisplayed,
    BlockedByReducedDebug,
    UnsupportedGameMode,
    PermissionRequired,
    NotSupported,
    NotExposed,
}

impl From<InformationUnavailableReason> for FacadeUnavailableReason {
    fn from(reason: InformationUnavailableReason) -> Self {
        match reason {
            InformationUnavailableReason::NotConnected => Self::NotConnected,
            InformationUnavailableReason::ScreenNotOpen => Self::ScreenNotOpen,
            InformationUnavailableReason::NotCurrentlyDisplayed => Self::NotCurrentlyDisplayed,
            InformationUnavailableReason::BlockedByReducedDebug => Self::BlockedByReducedDebug,
            InformationUnavailableReason::UnsupportedGameMode => Self::UnsupportedGameMode,
            InformationUnavailableReason::PermissionRequired => Self::PermissionRequired,
            InformationUnavailableReason::NotSupported => Self::NotSupported,
            InformationUnavailableReason::NotExposed => Self::NotExposed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationReadUnavailableReason {
    NotConnected,
    ScreenNotOpen,
    NotCurrentlyDisplayed,
    BlockedByReducedDebug,
    UnsupportedGameMode,
    PermissionRequired,
    NotSupported,
    NotExposed,
    StaleSelector,
    WrongWorld,
    WrongScreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationScopeDependency {
    Connection,
    World,
    Dimension,
    Ui,
    Screen,
}

pub const INFORMATION_SCOPE_DEPENDENCIES: [InformationScopeDependency; 5] = [
    InformationScopeDependency::Connection,
    InformationScopeDependency::World,
    InformationScopeDependency::Dimension,
    InformationScopeDependency::Ui,
    InformationScopeDependency::Screen,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationCatalogOperation {
    #[serde(rename = "list_interfaces")]
    ListInterfaces,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationCatalogRequest {
    pub operation: InformationCatalogOperation,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub known_catalog_revision: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationCatalogEntryAvailability {
    Available,
    PartiallyAvailable,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationCatalogEntry {
    pub id: InformationInterfaceId,
    pub description: String,
    pub schema_revision: String,
    pub audiences: Vec<InformationAudience>,
    pub availability: InformationCatalogEntryAvailability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationCatalogProtocol {
    #[serde(rename = "mineintent.information-catalog.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationCatalogOkStatus {
    Ok,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationCatalogNotModifiedStatus {
    NotModified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationCatalogOk {
    pub protocol: InformationCatalogProtocol,
    pub status: InformationCatalogOkStatus,
    pub target_minecraft_version: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub negotiated_minecraft_version: Option<String>,
    pub catalog_revision: String,
    pub interfaces: Vec<InformationCatalogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationCatalogNotModified {
    pub protocol: InformationCatalogProtocol,
    pub status: InformationCatalogNotModifiedStatus,
    pub catalog_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InformationCatalogResult {
    Ok(InformationCatalogOk),
    NotModified(InformationCatalogNotModified),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationPageRequest {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub cursor: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_js_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationSelectorRefProtocol {
    #[serde(rename = "mineintent.information-selector-ref.v1")]
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationSelectorRef {
    pub protocol: InformationSelectorRefProtocol,
    pub id: String,
    pub interface_id: InformationInterfaceId,
    #[serde(deserialize_with = "deserialize_js_integer")]
    pub connection_epoch: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub world_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub screen_instance_id: Option<String>,
    #[serde(deserialize_with = "deserialize_js_integer")]
    pub based_on_information_revision: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub valid_until: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationHelpOperation {
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationReadOperation {
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationAvailabilityMode {
    All,
    Current,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationHelpRequest {
    pub interface_id: InformationInterfaceId,
    pub operation: InformationHelpOperation,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub availability: Option<InformationAvailabilityMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub search: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub fields: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationReadRequest {
    pub interface_id: InformationInterfaceId,
    pub operation: InformationReadOperation,
    pub schema_revision: String,
    pub fields: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub selector: Option<InformationSelectorRef>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub page: Option<InformationPageRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InformationQueryRequest {
    Help(InformationHelpRequest),
    Read(InformationReadRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationPrecision {
    Displayed,
    Quantized,
    ExactlyDisplayed,
    Inferred,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationFieldHelp {
    pub id: String,
    pub description: String,
    pub value_type: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub unit: Option<String>,
    pub precision: InformationPrecision,
    pub interface_id: InformationInterfaceId,
    pub source_kinds: Vec<InformationSourceKind>,
    pub availability: InformationAvailability,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub requires: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub notes: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationHelpProtocol {
    #[serde(rename = "mineintent.information-help.v1")]
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationHelpResult {
    pub protocol: InformationHelpProtocol,
    pub interface_id: InformationInterfaceId,
    pub schema_revision: String,
    pub availability_mode: InformationAvailabilityMode,
    pub fields: Vec<InformationFieldHelp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationReadProtocol {
    #[serde(rename = "mineintent.information-read.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationAcquisition {
    ImmediateClientState,
    StructuredUiEquivalent,
    CurrentScreen,
    CurrentPerception,
    OperatorOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationReadSource {
    pub kind: InformationSourceKind,
    pub adapter_revision: String,
    pub source_revision: u64,
    pub acquisition: InformationAcquisition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationUnavailableField {
    pub field: String,
    pub reason: InformationReadUnavailableReason,
}

/// The TypeScript `Partial<T>` is erased at the heterogeneous provider boundary into JSON fields.
pub type InformationValues = BTreeMap<String, Value>;
pub type InformationFieldId = String;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationReadResult {
    pub protocol: InformationReadProtocol,
    pub read_id: String,
    pub interface_id: InformationInterfaceId,
    pub schema_revision: String,
    pub information_revision: u64,
    pub connection_epoch: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub world_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub dimension: Option<String>,
    pub observed_at: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub valid_until: Option<String>,
    pub source: InformationReadSource,
    pub values: InformationValues,
    pub unavailable: Vec<InformationUnavailableField>,
    pub evidence_ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationErrorCode {
    InvalidRequest,
    UnknownInterface,
    StaleSchema,
    UnknownField,
    InvalidSelector,
    InvalidPage,
    AudienceDenied,
    ScopeChanged,
    BudgetExceeded,
    DeadlineExceeded,
    ProviderFailed,
}

pub const INFORMATION_ERROR_CODES: [InformationErrorCode; 11] = [
    InformationErrorCode::InvalidRequest,
    InformationErrorCode::UnknownInterface,
    InformationErrorCode::StaleSchema,
    InformationErrorCode::UnknownField,
    InformationErrorCode::InvalidSelector,
    InformationErrorCode::InvalidPage,
    InformationErrorCode::AudienceDenied,
    InformationErrorCode::ScopeChanged,
    InformationErrorCode::BudgetExceeded,
    InformationErrorCode::DeadlineExceeded,
    InformationErrorCode::ProviderFailed,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationErrorProtocol {
    #[serde(rename = "mineintent.information-error.v1")]
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationRequestError {
    pub protocol: InformationErrorProtocol,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub interface_id: Option<InformationInterfaceId>,
    pub code: InformationErrorCode,
    pub message: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_catalog_revision: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_schema_revision: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub rejected_fields: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InformationToolResult {
    Help(InformationHelpResult),
    Read(InformationReadResult),
    Error(InformationRequestError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InformationAllInterfaces {
    #[serde(rename = "*")]
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InformationAllowedInterfaces {
    All(InformationAllInterfaces),
    Interfaces(Vec<InformationInterfaceId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationGrantPurpose {
    ParticipantContext,
    ModelTool,
    Controller,
    Operator,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationGrant {
    pub id: String,
    pub principal_id: String,
    pub audience: InformationAudience,
    pub allowed_interfaces: InformationAllowedInterfaces,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_fields: Option<BTreeMap<InformationInterfaceId, Vec<String>>>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub connection_epoch: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub world_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub screen_instance_id: Option<String>,
    pub purpose: InformationGrantPurpose,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub valid_until: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustedInformationCaller {
    pub principal_id: String,
    pub grant_id: String,
    pub purpose: InformationGrantPurpose,
    pub correlation_id: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub decision_run_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub controller_lease_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationProviderSelectors {
    pub required: bool,
    pub accepts_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationProviderPagination {
    pub default_limit: u64,
    pub max_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationProviderLimits {
    pub max_fields_per_read: u64,
    pub max_result_bytes: u64,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct InformationValueSchemaError {
    pub message: String,
}

/// Object-safe replacement for the TypeScript `ZodType<Value>` held by provider definitions.
pub trait InformationValueSchema: Send + Sync {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError>;
}

pub struct InformationFieldDefinition {
    pub description: String,
    pub value_schema: Arc<dyn InformationValueSchema>,
    pub value_type: String,
    pub unit: Option<String>,
    pub precision: InformationPrecision,
    pub source_kinds: Vec<InformationSourceKind>,
    pub requires: Option<Vec<String>>,
    pub notes: Option<String>,
}

pub struct InformationProviderDefinition {
    pub id: InformationInterfaceId,
    pub description: String,
    pub schema_revision: String,
    pub audiences: Vec<InformationAudience>,
    pub fields: BTreeMap<String, InformationFieldDefinition>,
    pub scope_dependencies: Vec<InformationScopeDependency>,
    pub selectors: Option<InformationProviderSelectors>,
    pub pagination: Option<InformationProviderPagination>,
    pub limits: InformationProviderLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAvailability {
    pub overall: InformationCatalogEntryAvailability,
    pub information_revision: u64,
    pub fields: BTreeMap<String, InformationUnavailableReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPageRequest {
    pub limit: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub state: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderReadRequest {
    pub fields: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub selector: Option<Value>,
    pub page: ProviderPageRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderReadResult {
    pub information_revision: u64,
    pub values: InformationValues,
    pub unavailable: Vec<InformationUnavailableField>,
    pub source: InformationReadSource,
    pub observed_at: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub valid_until: Option<String>,
    pub evidence_ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_page_state: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationReferenceIssueRequest {
    pub kind: String,
    pub payload: Value,
    pub allowed_interfaces: Vec<InformationInterfaceId>,
    pub based_on_information_revision: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub valid_until: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub bind_to_screen: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationReferenceIssueError {
    #[error("information reference per-read issue limit exceeded")]
    PerIssuerLimitExceeded,
    #[error("information reference requires an allowed target interface")]
    AllowedTargetRequired,
    #[error("information reference metadata is invalid")]
    InvalidMetadata,
    #[error("screen-bound information reference requires an active screen revision")]
    ActiveScreenRevisionRequired,
    #[error("information reference capacity exceeded")]
    CapacityExceeded,
    /// Parity-reserved for the TypeScript `unknown` input; the Rust contract already owns a
    /// `serde_json::Value`, so this cannot be produced by current public callers.
    #[error("information reference payload must be JSON serializable")]
    PayloadNotJsonSerializable,
    #[error("information reference payload exceeds its byte limit ({actual} > {maximum})")]
    PayloadByteLimitExceeded { actual: usize, maximum: usize },
    #[error("information reference lifetime exceeds its limit")]
    LifetimeExceeded,
    #[error("information reference timestamp is outside the supported UTC range")]
    TimestampOutOfRange,
    #[error("information reference store is unavailable while issuing")]
    StoreUnavailable,
}

pub trait InformationReferenceIssuer: Send + Sync {
    fn issue(
        &self,
        request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InformationProviderCaller {
    pub audience: InformationAudience,
    pub purpose: InformationGrantPurpose,
}

pub struct InformationProviderContext<'a> {
    pub now: &'a str,
    pub scope: &'a InformationScopeSnapshot,
    pub caller: InformationProviderCaller,
    pub refs: &'a dyn InformationReferenceIssuer,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationProviderError {
    #[error("information provider operation was cancelled")]
    Cancelled,
    #[error("information provider operation exceeded its deadline")]
    DeadlineExceeded,
    #[error("information provider failed: {message}")]
    Failed { message: String },
}

pub trait InformationProvider: Send + Sync {
    fn definition(&self) -> &InformationProviderDefinition;

    fn availability(&self, context: &InformationProviderContext<'_>) -> ProviderAvailability;

    fn read<'a>(
        &'a self,
        context: InformationProviderContext<'a>,
        request: ProviderReadRequest,
        control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationProviderDescriptor {
    pub id: InformationInterfaceId,
    pub description: String,
    pub schema_revision: String,
    pub audiences: Vec<InformationAudience>,
    pub field_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationToolSessionBudget {
    pub max_calls: u64,
    pub max_read_calls: u64,
    pub max_returned_bytes: u64,
    pub deadline_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationToolSessionContext {
    pub session_id: String,
    pub decision_run_id: String,
    pub correlation_id: String,
    pub principal_id: String,
    pub grant_id: String,
    pub budget: InformationToolSessionBudget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum InformationInvalidationEvent {
    ConnectionChanged {
        connection_epoch: u64,
    },
    WorldChanged {
        #[serde(
            default,
            deserialize_with = "deserialize_optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        world_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        dimension: Option<String>,
    },
    ScreenChanged {
        #[serde(
            default,
            deserialize_with = "deserialize_optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        screen_instance_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        screen_revision: Option<u64>,
    },
    GrantEnded {
        grant_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationTraceRecord {
    pub read_id: String,
    pub interface_id: InformationInterfaceId,
    pub fields: Vec<String>,
    pub source_kind: InformationSourceKind,
    pub source_revision: u64,
    pub evidence_ids: Vec<String>,
    pub correlation_id: String,
    pub observed_at: String,
}

fn deserialize_js_integer<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Unexpected, Visitor};
    use std::fmt;

    struct JsIntegerVisitor;

    impl<'de> Visitor<'de> for JsIntegerVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a non-negative JavaScript safe integer")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            checked_js_integer(value).ok_or_else(|| {
                Error::invalid_value(Unexpected::Unsigned(value), &"a JavaScript safe integer")
            })
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            let value = u64::try_from(value)
                .map_err(|_| Error::invalid_value(Unexpected::Signed(value), &self))?;
            self.visit_u64(value)
        }

        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            if value.is_finite()
                && value >= 0.0
                && value.fract() == 0.0
                && value <= JS_MAX_SAFE_INTEGER as f64
            {
                Ok(value as u64)
            } else {
                Err(Error::invalid_value(Unexpected::Float(value), &self))
            }
        }
    }

    deserializer.deserialize_any(JsIntegerVisitor)
}

fn deserialize_optional_js_integer<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct OptionalJsIntegerVisitor;

    impl<'de> serde::de::Visitor<'de> for OptionalJsIntegerVisitor {
        type Value = Option<u64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an optional non-negative JavaScript safe integer")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Err(E::custom("explicit null is not an optional field value"))
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserialize_js_integer(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalJsIntegerVisitor)
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    use serde::de::Error;

    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(D::Error::custom(
            "explicit null is not an optional field value",
        ));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(D::Error::custom)
}

const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn checked_js_integer(value: u64) -> Option<u64> {
    (value <= JS_MAX_SAFE_INTEGER).then_some(value)
}
