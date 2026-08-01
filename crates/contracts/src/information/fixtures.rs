use serde::{Deserialize, Serialize};

use crate::minecraft::{
    fixture_snapshot, FactSource, ViewportCoordinateSystem, ViewportFrame, ViewportLegend,
    ViewportProjection, ViewportRead, ViewportSelfPose, VisibleBlocksView, VisibleEntitiesView,
    VisibleEntityView,
};

use super::{
    CurrentStatusValues, InformationConnectionState, InformationFieldOmission, InformationOmission,
    InformationOmissionReason, InformationScopeSnapshot, InformationUnavailableReason,
    InventoryValues, PassiveInterfaceId, PassiveObservations, PassiveViewportValues,
    RelativeDirection, SoundObservation, SoundValues,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPlainValueFixtures {
    pub current_status: CurrentStatusValues,
    pub inventory: InventoryValues,
    pub sound: SoundValues,
    pub viewport: ViewportProjection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationFixtureSet {
    pub scope: InformationScopeSnapshot,
    pub plain_values: ProviderPlainValueFixtures,
    pub viewport_read: ViewportRead,
    pub composed: PassiveObservations,
    pub denied: PassiveObservations,
    pub unavailable: PassiveObservations,
    pub partial: PassiveObservations,
    pub timeout: PassiveObservations,
}

/// Deterministic DTO-only builder for middle/Agent tests; it owns no provider or runtime state.
#[derive(Clone, Debug)]
pub struct InformationFixtureBuilder {
    fixtures: InformationFixtureSet,
}

impl InformationFixtureBuilder {
    pub fn canonical() -> Self {
        Self {
            fixtures: fixture_information_set(),
        }
    }

    pub fn scope(mut self, scope: InformationScopeSnapshot) -> Self {
        self.fixtures.scope = scope;
        self
    }

    pub fn composed(mut self, composed: PassiveObservations) -> Self {
        self.fixtures.composed = composed;
        self
    }

    pub fn build(self) -> InformationFixtureSet {
        self.fixtures
    }
}

pub fn fixture_information_set() -> InformationFixtureSet {
    let snapshot = fixture_snapshot();
    let current_status = CurrentStatusValues {
        health: Some(snapshot.self_snapshot.health),
        food: Some(snapshot.self_snapshot.food),
        food_saturation: Some(snapshot.self_snapshot.food_saturation),
        oxygen: snapshot.self_snapshot.oxygen,
        experience_level: snapshot
            .self_snapshot
            .experience
            .as_ref()
            .map(|experience| experience.level),
        status_effects: Some(snapshot.self_snapshot.effects.clone()),
    };
    let inventory = InventoryValues {
        selected_hotbar_slot: Some(snapshot.inventory.selected_hotbar_slot),
        slots: Some(snapshot.inventory.slots.clone()),
    };
    let sound = SoundValues {
        recent_sounds: Some(vec![SoundObservation {
            sound_name: Some("block.note_block.harp".to_owned()),
            category: Some("record".to_owned()),
            distance: 4.5,
            direction: RelativeDirection::Ahead,
            volume: 1.0,
            pitch: 0.8,
            observed_at: "2026-08-01T00:00:20Z".to_owned(),
        }]),
    };
    let viewport = fixture_viewport_projection();
    let viewport_read = ViewportRead {
        projection: viewport.clone(),
        source: FactSource::ServerObserved,
        revision: 9,
    };
    let scope = InformationScopeSnapshot {
        process_session_id: snapshot.process_session_id,
        connection_state: InformationConnectionState::Play,
        connection_epoch: snapshot.connection_epoch,
        world_id: Some(snapshot.world.world_id),
        dimension: Some(snapshot.world.dimension),
        ui_revision: 0,
        screen_instance_id: None,
        screen_revision: None,
        captured_at: "2026-08-01T00:00:21Z".to_owned(),
    };
    let composed = PassiveObservations {
        current_status: Some(current_status.clone()),
        inventory: Some(inventory.clone()),
        sound: Some(sound.clone()),
        viewport: Some(PassiveViewportValues {
            frame: Some(viewport.frame.clone()),
        }),
        omissions: Vec::new(),
    };
    let denied = PassiveObservations {
        omissions: vec![InformationOmission {
            interface_id: PassiveInterfaceId::CurrentStatus,
            reason: InformationOmissionReason::AudienceDenied,
            fields: Vec::new(),
            message: Some("participant grant does not allow current_status".to_owned()),
        }],
        ..PassiveObservations::default()
    };
    let unavailable = PassiveObservations {
        omissions: vec![InformationOmission {
            interface_id: PassiveInterfaceId::ViewportInformation,
            reason: InformationOmissionReason::Unavailable,
            fields: vec![InformationFieldOmission {
                field: "frame".to_owned(),
                reason: InformationUnavailableReason::NotConnected,
            }],
            message: None,
        }],
        ..PassiveObservations::default()
    };
    let partial = PassiveObservations {
        current_status: Some(CurrentStatusValues {
            health: Some(18.0),
            ..CurrentStatusValues::default()
        }),
        omissions: vec![InformationOmission {
            interface_id: PassiveInterfaceId::CurrentStatus,
            reason: InformationOmissionReason::Partial,
            fields: vec![
                InformationFieldOmission {
                    field: "oxygen".to_owned(),
                    reason: InformationUnavailableReason::NotExposed,
                },
                InformationFieldOmission {
                    field: "experienceLevel".to_owned(),
                    reason: InformationUnavailableReason::NotExposed,
                },
            ],
            message: None,
        }],
        ..PassiveObservations::default()
    };
    let timeout = PassiveObservations {
        omissions: vec![InformationOmission {
            interface_id: PassiveInterfaceId::ViewportInformation,
            reason: InformationOmissionReason::DeadlineExceeded,
            fields: Vec::new(),
            message: Some("viewport read exceeded its deadline".to_owned()),
        }],
        ..PassiveObservations::default()
    };
    InformationFixtureSet {
        scope,
        plain_values: ProviderPlainValueFixtures {
            current_status,
            inventory,
            sound,
            viewport,
        },
        viewport_read,
        composed,
        denied,
        unavailable,
        partial,
        timeout,
    }
}

fn fixture_viewport_projection() -> ViewportProjection {
    ViewportProjection {
        frame: ViewportFrame {
            coordinates: ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ViewportSelfPose {
                position: [1.5, 64.0, -2.5],
                yaw_degrees: 28.6,
                pitch_degrees: -14.3,
            },
            legend: ViewportLegend {
                visible_entities: "items: {type, player?, position}; nearest first".to_owned(),
                visible_blocks: "[block_name, x, y, z]; nearest first".to_owned(),
            },
        },
        standing_on_block: Some(crate::minecraft::ViewportBlock {
            name: "stone".to_owned(),
            position: [1.0, 63.0, -3.0],
        }),
        looked_at_block: Some(crate::minecraft::ViewportBlock {
            name: "glass".to_owned(),
            position: [1.0, 65.0, -5.0],
        }),
        visible_entities: VisibleEntitiesView {
            items: vec![VisibleEntityView {
                entity_type: "player".to_owned(),
                player: Some("Observer".to_owned()),
                position: [4.0, 64.0, -2.0],
            }],
            truncated: false,
        },
        visible_blocks: VisibleBlocksView {
            blocks: vec![("glass".to_owned(), 1, 65, -5)],
            truncated: false,
        },
    }
}
