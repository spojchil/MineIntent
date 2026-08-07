//! Minecraft runtime composition and the stable facade-facing entrypoints.

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use azalea::{
    accept_resource_packs::AcceptResourcePacksPlugin,
    app::{App, AppExit, Plugin, PluginGroup, PostUpdate, Update},
    auto_reconnect::AutoReconnectPlugin,
    auto_respawn::AutoRespawnPlugin,
    bot::DefaultBotPlugins,
    ecs::{
        message::{MessageReader, MessageWriter},
        prelude::{Add, Commands, On, Query, With},
        system::Res,
    },
    entity::{dimensions::EntityDimensions, Dead, LocalEntity, Physics, Position},
    prelude::{bevy_ecs, Account, Component, Resource},
    protocol::address::{ResolvedAddr, ServerAddr},
    swarm::{DefaultSwarmPlugins, Swarm, SwarmBuilder, SwarmEvent},
    Client, DefaultPlugins, Event, SprintDirection, WalkDirection,
};
use bevy_ecs::schedule::IntoScheduleConfigs;
use mineintent_contracts::capability::validate_directed_positions;
use mineintent_contracts::minecraft::{
    parse_block_property_value, BackendClose, BackendCloseError, BackendError,
    BackendEventEnvelope as ContractBackendEventEnvelope,
    BackendEventKind as ContractBackendEventKind,
    BackendEventMetadata as ContractBackendEventMetadata, BackendEventPayload, BackendFailure,
    BackendFailureCode, BackendKick, BackendLifecyclePayload, BackendState, BackendTimeouts,
    BlockBoundingBox as ContractBlockBoundingBox, BlockInfoPresenterRegistry,
    BlockPosition as ContractBlockPosition, BlockReadResult as ContractBlockReadResult, BoxFuture,
    ChatPosition, DirectedViewportError, DirectedViewportProjection,
    EntityEquipmentSnapshot as ContractEntityEquipmentSnapshot,
    EntityRemovalReason as ContractEntityRemovalReason, FactSource as ContractFactSource,
    HeardSoundType as ContractHeardSoundType, ObservationEvent, ObservationEventListener,
    OperationControl, ProtocolBlockEvent as ContractProtocolBlockEvent,
    ProtocolBlockSnapshot as ContractProtocolBlockSnapshot,
    ProtocolChatEvent as ContractProtocolChatEvent,
    ProtocolEntityEvent as ContractProtocolEntityEvent,
    ProtocolEntitySnapshot as ContractProtocolEntitySnapshot, ProtocolObservationSource,
    ProtocolPlayerListEvent as ContractProtocolPlayerListEvent,
    ProtocolSelfEvent as ContractProtocolSelfEvent,
    ProtocolSnapshotChangedEvent as ContractProtocolSnapshotChangedEvent,
    ProtocolSoundPayload as ContractProtocolSoundPayload,
    ProtocolSoundSource as ContractProtocolSoundSource, ReconnectPolicy, RelativeMovementFlags,
    SelfPose as ContractSelfPose, Subscription, Vec3Value as ContractVec3Value,
    ViewportBlock as ContractViewportBlock, ViewportFrame as ContractViewportFrame,
    ViewportLegend as ContractViewportLegend, ViewportProjection as ContractViewportProjection,
    ViewportRead as ContractViewportRead, ViewportSelfPose as ContractViewportSelfPose,
    VisibleBlocksView as ContractVisibleBlocksView,
    VisibleEntitiesView as ContractVisibleEntitiesView,
    VisibleEntityView as ContractVisibleEntityView,
};
use tokio::sync::{oneshot, Notify};

use crate::{
    entity_events::{
        compact_pitch_degrees, compact_rotation_degrees, EntityIdentity, EntityMovePatch,
        EntityProducerCache, EntityProducerInput, EntityProducerToken, NormalizedEntityEvent,
        NormalizedEntitySnapshot,
    },
    protocol::{
        now_utc, BackendCommand, BackendCommandEnvelope, BackendEventEnvelope, BackendEventKind,
        FactSource, MotorDirection, BACKEND_COMMAND_PROTOCOL,
    },
    snapshot::{
        block_probe, block_snapshot, canonical_entity_type, capture, BlockBoundingBox,
        BlockPosition, BlockProbe, BlockReadResult, MinecraftSnapshotV1, PoseSnapshot,
        ProtocolBlockSnapshot, ProtocolEntitySnapshot, TrackedPlayerSnapshot, Vec3Value,
    },
    viewport::{
        project as project_viewport, project_directed_with_presenters as project_directed_viewport,
        project_with_checkpoint_and_presenters as project_viewport_with_checkpoint, ViewportBlock,
        ViewportOptions, ViewportProjection, WorldHeightBounds, WorldReader,
    },
};

mod command;
mod config;
mod driver;
mod dto;
mod entity;
mod events;
mod frame;
mod handle;
mod lifecycle;
mod observation;
mod ownership;
mod producers;
mod shutdown;
mod state;

#[cfg(test)]
use command::release_active_movement_and_finish;
pub use command::CommandCompletion;
#[allow(unused_imports)]
pub(crate) use command::CommandCompletionCancellation;
use command::{
    command_component_failure, finish_command, process_pending_commands, try_set_movement_flags,
    validate_command, CommandCompletionState, QueuedCommand,
};
pub use config::RunConfig;
use config::{
    checked_atomic_increment, next_reconnect_random, reconnect_schedule_at, PhaseDeadlineToken,
    StableResetToken, StopWatchdogToken, TransportPhase,
};
#[cfg(test)]
use driver::validate_run_config;
use driver::SwarmState;
pub use driver::{run, run_with_handle};
use dto::{
    backend_error_from_directed, contract_block_read_result, contract_block_snapshot,
    contract_entity_snapshot, contract_fact_source, contract_self_pose,
    contract_viewport_projection, observation_event_from_backend,
};
use entity::merge_refreshed_tracked_entities;
#[cfg(test)]
use entity::{normalized_entity_snapshot_to_protocol, record_entity_residual};
use events::{EventDispatchState, ObservationSubscriber, RuntimeEventQueue};
#[cfg(test)]
use events::{
    RuntimeDispatchAdmission, RUNTIME_BROKER_ORDINARY_CAPACITY, RUNTIME_BROKER_OVERFLOW_CAPACITY,
};
pub use events::{RuntimeEventReceiver, RuntimeObservationSubscription};
#[cfg(test)]
pub(crate) use events::{
    RUNTIME_BROKER_CONTROL_CAPACITY, RUNTIME_DISPATCH_CONTROL_CAPACITY,
    RUNTIME_DISPATCH_ORDINARY_CAPACITY, RUNTIME_DISPATCH_OVERFLOW_CAPACITY,
};
#[cfg(test)]
use frame::{calculate_armor_values, floor_block_coordinate, CachedLightChunk};
use frame::{probe_block_from_world, read_block_from_world, LightCache, LightSectionGeometry};
pub use handle::{RuntimeFrameFacts, RuntimeHandle};
pub use observation::RuntimeObservationSource;
#[cfg(test)]
use producers::{produce_entity_packet_events, prove_has_skylight, CanonicalPacketSourceMetadata};
use producers::{BlockSoundProducerPlugin, EntityProducerPlugin, ServerPositionCorrectionPlugin};
use state::*;

#[cfg(test)]
mod entity_events_owner_tests;

#[cfg(test)]
mod tests;
