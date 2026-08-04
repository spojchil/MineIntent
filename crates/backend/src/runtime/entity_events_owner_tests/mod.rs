use super::*;

/// The vendor `AttemptToken` bound by a test app's owner setup and used by
/// its packet queue helpers, so stamped canonical sources are exercised
/// end-to-end instead of falling back to the legacy fence.
#[derive(Clone, Copy, Resource)]
pub(super) struct TestAttemptToken(pub(super) azalea::join::AttemptToken);

fn snapshot(epoch: u64, protocol_id: i32, x: f64) -> NormalizedEntitySnapshot {
    NormalizedEntitySnapshot {
        identity: EntityIdentity::new(epoch, protocol_id),
        entity_type: "minecraft:pig".to_owned(),
        uuid: Some(format!("entity-{protocol_id}")),
        name: None,
        username: None,
        position: [x, 64.0, -3.0],
        velocity: [0.25, 0.0, -0.5],
        yaw: 45.0,
        pitch: -11.25,
        head_yaw: Some(90.0),
        width: 0.9,
        height: 0.9,
        on_ground: true,
        pose: None,
        held_item_name: None,
        equipment: Vec::new(),
        valid: true,
    }
}

fn token(epoch: u64, admission: u64) -> EntityProducerToken {
    EntityProducerToken::new(epoch, format!("packet:{admission}"))
}

fn scope_snapshot(epoch: u64) -> MinecraftSnapshotV1 {
    MinecraftSnapshotV1 {
        protocol: crate::snapshot::SNAPSHOT_PROTOCOL.to_owned(),
        snapshot_revision: 1,
        lifecycle_revision: 1,
        captured_at: now_utc(),
        process_session_id: "scope-test".to_owned(),
        connection_epoch: epoch,
        connection_attempt_id: format!("attempt-{epoch}"),
        world: crate::snapshot::WorldSnapshot {
            world_id: "scope-world".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
            minecraft_version: "26.1.2".to_owned(),
            protocol_version: 775,
            game_mode: "survival".to_owned(),
            min_y: -64,
            height: 384,
        },
        self_snapshot: crate::snapshot::SelfSnapshot {
            entity_key: "scope-self".to_owned(),
            username: "scope".to_owned(),
            position: Vec3Value {
                x: 0.0,
                y: 64.0,
                z: 0.0,
            },
            velocity: Vec3Value {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            alive: true,
            health: 20.0,
            food: 20,
            food_saturation: 5.0,
            experience: crate::snapshot::ExperienceSnapshot {
                level: 0,
                progress: 0.0,
                total: 0,
            },
        },
        inventory: crate::snapshot::InventorySnapshot {
            selected_hotbar_slot: 0,
            slots: Vec::new(),
        },
        tracked_players: Vec::new(),
    }
}

mod ownership;
mod packet_adapter;
