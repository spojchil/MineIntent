//! Deterministic composition of the four passive Information observations.
//!
//! This is deliberately an adapter over the existing `InformationRuntimePort`.  It owns the
//! fixed observation plan, but it does not own a registry, provider, policy, viewport adapter, or
//! a second query trait.  A later facade can use this adapter while retaining its own atomic
//! viewport read entry point.

use std::collections::HashSet;

use mineintent_contracts::{
    information::{
        CurrentStatusValues, InformationFieldOmission, InformationOmission,
        InformationOmissionReason, InformationUnavailableReason as FacadeUnavailableReason,
        InventoryValues, PassiveInterfaceId, PassiveObservations, PassiveViewportValues,
        SoundValues,
    },
    minecraft::{BoxFuture, OperationControl},
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::{
    contracts::{
        InformationErrorCode, InformationInterfaceId, InformationReadUnavailableReason,
        InformationRequestError, InformationToolResult, TrustedInformationCaller,
    },
    tool_session::InformationRuntimePort,
};

#[derive(Clone, Copy)]
struct ReadPlanEntry {
    interface_id: InformationInterfaceId,
    facade_interface_id: PassiveInterfaceId,
    schema_revision: &'static str,
    fields: &'static [&'static str],
    request: &'static str,
}

const READ_PLAN: [ReadPlanEntry; 4] = [
    ReadPlanEntry {
        interface_id: InformationInterfaceId::CurrentStatus,
        facade_interface_id: PassiveInterfaceId::CurrentStatus,
        schema_revision: "current-status:1",
        fields: &[
            "health",
            "food",
            "foodSaturation",
            "oxygen",
            "experienceLevel",
            "statusEffects",
        ],
        request: r#"{"interfaceId":"current_status","operation":"read","schemaRevision":"current-status:1","fields":["health","food","foodSaturation","oxygen","experienceLevel","statusEffects"]}"#,
    },
    ReadPlanEntry {
        interface_id: InformationInterfaceId::InventoryInformation,
        facade_interface_id: PassiveInterfaceId::InventoryInformation,
        schema_revision: "inventory-information:1",
        fields: &["selectedHotbarSlot", "slots"],
        request: r#"{"interfaceId":"inventory_information","operation":"read","schemaRevision":"inventory-information:1","fields":["selectedHotbarSlot","slots"]}"#,
    },
    ReadPlanEntry {
        interface_id: InformationInterfaceId::SoundInformation,
        facade_interface_id: PassiveInterfaceId::SoundInformation,
        schema_revision: "sound-information:1",
        fields: &["recentSounds"],
        request: r#"{"interfaceId":"sound_information","operation":"read","schemaRevision":"sound-information:1","fields":["recentSounds"]}"#,
    },
    ReadPlanEntry {
        interface_id: InformationInterfaceId::ViewportInformation,
        facade_interface_id: PassiveInterfaceId::ViewportInformation,
        schema_revision: "viewport-information:10",
        fields: &["frame"],
        request: r#"{"interfaceId":"viewport_information","operation":"read","schemaRevision":"viewport-information:10","fields":["frame"]}"#,
    },
];

/// A reusable passive-observation assembler over the existing Information runtime port.
pub struct InformationContextComposer<'a> {
    runtime: &'a dyn InformationRuntimePort,
}

impl<'a> InformationContextComposer<'a> {
    pub fn new(runtime: &'a dyn InformationRuntimePort) -> Self {
        Self { runtime }
    }

    pub fn compose_passive_observations<'call>(
        &'call self,
        caller: &'call TrustedInformationCaller,
        control: OperationControl,
    ) -> BoxFuture<'call, PassiveObservations>
    where
        'a: 'call,
    {
        compose_passive_observations(self.runtime, caller, control)
    }
}

/// Compose the fixed four-interface passive observation set.
///
/// Every plan entry is queried exactly once and in declaration order.  The cloned controls all
/// refer to the same cancellation/deadline handles; no child control, timer, or token is created
/// here.  Individual failures become facade omissions so a later interface cannot erase an
/// earlier successful value.
pub fn compose_passive_observations<'a>(
    runtime: &'a dyn InformationRuntimePort,
    caller: &'a TrustedInformationCaller,
    control: OperationControl,
) -> BoxFuture<'a, PassiveObservations> {
    Box::pin(async move {
        let mut observations = PassiveObservations {
            omissions: Vec::new(),
            ..PassiveObservations::default()
        };

        for plan in READ_PLAN {
            let response = runtime.query(caller, plan.request, control.clone()).await;
            append_response(&mut observations, plan, response);
        }

        observations
    })
}

enum DecodedValues {
    CurrentStatus(CurrentStatusValues),
    Inventory(InventoryValues),
    Sound(SoundValues),
    Viewport(PassiveViewportValues),
}

struct DecodedRead {
    values: Option<DecodedValues>,
    unavailable: Vec<InformationFieldOmission>,
}

fn append_response(
    observations: &mut PassiveObservations,
    plan: ReadPlanEntry,
    response: InformationToolResult,
) {
    match response {
        InformationToolResult::Read(read) => match decode_read(plan, read) {
            Ok(decoded) => {
                if let Some(values) = decoded.values {
                    assign_values(observations, values);
                }
                if !decoded.unavailable.is_empty() {
                    observations.omissions.push(InformationOmission {
                        interface_id: plan.facade_interface_id,
                        reason: if observations_has_values(observations, plan.facade_interface_id) {
                            InformationOmissionReason::Partial
                        } else {
                            InformationOmissionReason::Unavailable
                        },
                        fields: decoded.unavailable,
                        message: None,
                    });
                }
            }
            Err(message) => push_failure(observations, plan, message),
        },
        InformationToolResult::Error(error) => push_error(observations, plan, error),
        InformationToolResult::Help(_) => push_failure(
            observations,
            plan,
            "The passive composer received a help result for a read request.".to_owned(),
        ),
    }
}

fn decode_read(
    plan: ReadPlanEntry,
    read: super::contracts::InformationReadResult,
) -> Result<DecodedRead, String> {
    if read.interface_id != plan.interface_id {
        return Err(format!(
            "passive read returned interface {:?}, expected {:?}",
            read.interface_id, plan.interface_id
        ));
    }
    if read.schema_revision != plan.schema_revision {
        return Err(format!(
            "passive read returned schema {}, expected {}",
            read.schema_revision, plan.schema_revision
        ));
    }
    if read.next_cursor.is_some() {
        return Err("passive read unexpectedly returned a cursor".to_owned());
    }

    for (field, value) in &read.values {
        if !plan.fields.iter().any(|expected| *expected == field) {
            return Err(format!("passive read returned unplanned field {field}"));
        }
        if contains_null(value) {
            return Err(format!(
                "passive read returned an explicit null in field {field}"
            ));
        }
    }

    let mut unavailable_fields = HashSet::new();
    let mut unavailable = Vec::with_capacity(read.unavailable.len());
    for field in read.unavailable {
        if !plan.fields.iter().any(|expected| *expected == field.field) {
            return Err(format!(
                "passive read returned unplanned unavailable field {}",
                field.field
            ));
        }
        if !unavailable_fields.insert(field.field.clone()) {
            return Err(format!(
                "passive read repeated unavailable field {}",
                field.field
            ));
        }
        if read.values.contains_key(&field.field) {
            return Err(format!(
                "passive read returned field {} as both available and unavailable",
                field.field
            ));
        }
        unavailable.push(InformationFieldOmission {
            field: field.field,
            reason: map_unavailable_reason(field.reason),
        });
    }

    for field in plan.fields {
        let present = read.values.keys().any(|returned| returned == field)
            || unavailable_fields.contains(*field);
        if !present {
            return Err(format!(
                "passive read omitted field {field} without an explanation"
            ));
        }
    }

    let values = if read.values.is_empty() {
        None
    } else {
        let object: Map<String, Value> = read.values.into_iter().collect();
        let value = Value::Object(object);
        Some(match plan.interface_id {
            InformationInterfaceId::CurrentStatus => {
                DecodedValues::CurrentStatus(parse_values(value, "current_status")?)
            }
            InformationInterfaceId::InventoryInformation => {
                DecodedValues::Inventory(parse_values(value, "inventory_information")?)
            }
            InformationInterfaceId::SoundInformation => {
                DecodedValues::Sound(parse_values(value, "sound_information")?)
            }
            InformationInterfaceId::ViewportInformation => {
                DecodedValues::Viewport(parse_values(value, "viewport_information")?)
            }
            other => {
                return Err(format!(
                    "passive composer has no typed DTO for interface {other:?}"
                ));
            }
        })
    };

    Ok(DecodedRead {
        values,
        unavailable,
    })
}

fn parse_values<T>(value: Value, interface_id: &str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|error| {
        format!("{interface_id} provider values failed strict passive DTO validation: {error}")
    })
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) => values.iter().any(contains_null),
        Value::Object(values) => values.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn assign_values(observations: &mut PassiveObservations, values: DecodedValues) {
    match values {
        DecodedValues::CurrentStatus(values) => observations.current_status = Some(values),
        DecodedValues::Inventory(values) => observations.inventory = Some(values),
        DecodedValues::Sound(values) => observations.sound = Some(values),
        DecodedValues::Viewport(values) => observations.viewport = Some(values),
    }
}

fn observations_has_values(
    observations: &PassiveObservations,
    interface_id: PassiveInterfaceId,
) -> bool {
    match interface_id {
        PassiveInterfaceId::CurrentStatus => observations.current_status.is_some(),
        PassiveInterfaceId::InventoryInformation => observations.inventory.is_some(),
        PassiveInterfaceId::SoundInformation => observations.sound.is_some(),
        PassiveInterfaceId::ViewportInformation => observations.viewport.is_some(),
    }
}

fn push_error(
    observations: &mut PassiveObservations,
    plan: ReadPlanEntry,
    error: InformationRequestError,
) {
    let reason = match error.code {
        InformationErrorCode::AudienceDenied => InformationOmissionReason::AudienceDenied,
        InformationErrorCode::UnknownInterface => InformationOmissionReason::Unavailable,
        InformationErrorCode::DeadlineExceeded => InformationOmissionReason::DeadlineExceeded,
        InformationErrorCode::ScopeChanged => InformationOmissionReason::ScopeChanged,
        InformationErrorCode::ProviderFailed
        | InformationErrorCode::InvalidRequest
        | InformationErrorCode::StaleSchema
        | InformationErrorCode::UnknownField
        | InformationErrorCode::InvalidSelector
        | InformationErrorCode::InvalidPage
        | InformationErrorCode::BudgetExceeded => InformationOmissionReason::ProviderFailed,
    };
    observations.omissions.push(InformationOmission {
        interface_id: plan.facade_interface_id,
        reason,
        fields: Vec::new(),
        message: Some(error.message),
    });
}

fn push_failure(observations: &mut PassiveObservations, plan: ReadPlanEntry, message: String) {
    observations.omissions.push(InformationOmission {
        interface_id: plan.facade_interface_id,
        reason: InformationOmissionReason::ProviderFailed,
        fields: Vec::new(),
        message: Some(message),
    });
}

fn map_unavailable_reason(reason: InformationReadUnavailableReason) -> FacadeUnavailableReason {
    match reason {
        InformationReadUnavailableReason::NotConnected => FacadeUnavailableReason::NotConnected,
        InformationReadUnavailableReason::ScreenNotOpen => FacadeUnavailableReason::ScreenNotOpen,
        InformationReadUnavailableReason::NotCurrentlyDisplayed => {
            FacadeUnavailableReason::NotCurrentlyDisplayed
        }
        InformationReadUnavailableReason::BlockedByReducedDebug => {
            FacadeUnavailableReason::BlockedByReducedDebug
        }
        InformationReadUnavailableReason::UnsupportedGameMode => {
            FacadeUnavailableReason::UnsupportedGameMode
        }
        InformationReadUnavailableReason::PermissionRequired => {
            FacadeUnavailableReason::PermissionRequired
        }
        InformationReadUnavailableReason::NotSupported => FacadeUnavailableReason::NotSupported,
        InformationReadUnavailableReason::NotExposed => FacadeUnavailableReason::NotExposed,
        InformationReadUnavailableReason::StaleSelector => FacadeUnavailableReason::StaleSelector,
        InformationReadUnavailableReason::WrongWorld => FacadeUnavailableReason::WrongWorld,
        InformationReadUnavailableReason::WrongScreen => FacadeUnavailableReason::WrongScreen,
    }
}
