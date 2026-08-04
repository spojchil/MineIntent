//! Entity producer normalization and connection-scoped state.
//!
//! The packet/ECS adapter is intentionally kept outside this module.  This
//! seam owns the invariants that must be true regardless of which Azalea
//! schedule supplies an observation: protocol-id identity, pre-remove
//! snapshots, scope isolation, and token-level idempotence.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::snapshot::protocol_entity_key;

/// Recent duplicate suppression is deliberately bounded.  Tokens only cover
/// overlap between packet observation paths and a repeated delivery of the
/// same packet; this is not a permanent exactly-once ledger.
pub(crate) const ENTITY_PRODUCER_DEDUPE_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntityIdentity {
    pub(crate) epoch: u64,
    pub(crate) protocol_id: i32,
}

impl EntityIdentity {
    pub(crate) fn new(epoch: u64, protocol_id: i32) -> Self {
        Self { epoch, protocol_id }
    }

    pub(crate) fn key(self) -> String {
        protocol_entity_key(self.epoch, self.protocol_id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NormalizedEntitySnapshot {
    pub(crate) identity: EntityIdentity,
    pub(crate) entity_type: String,
    pub(crate) uuid: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) position: [f64; 3],
    pub(crate) velocity: [f64; 3],
    pub(crate) yaw: f64,
    pub(crate) pitch: f64,
    pub(crate) head_yaw: Option<f64>,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) on_ground: bool,
    pub(crate) pose: Option<String>,
    pub(crate) held_item_name: Option<String>,
    pub(crate) equipment: Vec<(u32, String, u32)>,
    pub(crate) valid: bool,
}

impl NormalizedEntitySnapshot {
    pub(crate) fn entity_key(&self) -> String {
        self.identity.key()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntityProducerToken {
    epoch: u64,
    value: String,
}

impl EntityProducerToken {
    pub(crate) fn new(epoch: u64, value: impl Into<String>) -> Self {
        Self {
            epoch,
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NormalizedEntityEvent {
    Spawned {
        entity: NormalizedEntitySnapshot,
    },
    Moved {
        entity: NormalizedEntitySnapshot,
    },
    Updated {
        entity: NormalizedEntitySnapshot,
        changed: Vec<String>,
    },
    Animation {
        entity: EntityIdentity,
        animation: String,
    },
    Hurt {
        entity: EntityIdentity,
        possible_source: Option<EntityIdentity>,
    },
    Removed {
        entity: EntityIdentity,
        last: NormalizedEntitySnapshot,
    },
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EntityProducerInput {
    Spawn {
        token: EntityProducerToken,
        snapshot: NormalizedEntitySnapshot,
    },
    Move {
        token: EntityProducerToken,
        patch: EntityMovePatch,
    },
    Update {
        token: EntityProducerToken,
        snapshot: NormalizedEntitySnapshot,
    },
    Animation {
        token: EntityProducerToken,
        entity: EntityIdentity,
        animation: String,
    },
    Hurt {
        token: EntityProducerToken,
        entity: EntityIdentity,
        possible_source: Option<EntityIdentity>,
    },
    Remove {
        token: EntityProducerToken,
        entity: EntityIdentity,
    },
}

/// A single entity movement transition applied to the producer's shadow
/// state.  The ordinary move packets use `delta`/`compact_look`; teleport and
/// position-sync packets use the absolute fields plus their relative masks.
/// Missing fields retain their value from the preceding packet snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EntityMovePatch {
    pub(crate) entity: EntityIdentity,
    pub(crate) delta: Option<[i64; 3]>,
    pub(crate) compact_look: Option<[i8; 2]>,
    pub(crate) head_yaw: Option<f64>,
    pub(crate) absolute_position: Option<[f64; 3]>,
    pub(crate) absolute_look: Option<[f64; 2]>,
    pub(crate) relative_position: [bool; 3],
    pub(crate) relative_look: [bool; 2],
    pub(crate) velocity: Option<[f64; 3]>,
    pub(crate) relative_velocity: [bool; 3],
    pub(crate) rotate_velocity: bool,
    pub(crate) on_ground: Option<bool>,
}

impl EntityMovePatch {
    pub(crate) fn relative(
        entity: EntityIdentity,
        delta: Option<[i64; 3]>,
        compact_look: Option<[i8; 2]>,
        on_ground: bool,
    ) -> Self {
        Self {
            entity,
            delta,
            compact_look,
            head_yaw: None,
            absolute_position: None,
            absolute_look: None,
            relative_position: [false; 3],
            relative_look: [false; 2],
            velocity: None,
            relative_velocity: [false; 3],
            rotate_velocity: false,
            on_ground: Some(on_ground),
        }
    }

    pub(crate) fn teleport(
        entity: EntityIdentity,
        position: [f64; 3],
        look: [f64; 2],
        relative_position: [bool; 3],
        relative_look: [bool; 2],
        velocity: [f64; 3],
        relative_velocity: [bool; 3],
        rotate_velocity: bool,
        on_ground: bool,
    ) -> Self {
        Self {
            entity,
            delta: None,
            compact_look: None,
            head_yaw: None,
            absolute_position: Some(position),
            absolute_look: Some(look),
            relative_position,
            relative_look,
            velocity: Some(velocity),
            relative_velocity,
            rotate_velocity,
            on_ground: Some(on_ground),
        }
    }

    pub(crate) fn position_sync(
        entity: EntityIdentity,
        position: [f64; 3],
        look: [f64; 2],
        velocity: [f64; 3],
        on_ground: bool,
    ) -> Self {
        Self::teleport(
            entity, position, look, [false; 3], [false; 2], velocity, [false; 3], false, on_ground,
        )
    }

    pub(crate) fn rotate_head(entity: EntityIdentity, head_yaw: i8) -> Self {
        Self {
            entity,
            delta: None,
            compact_look: None,
            head_yaw: Some(compact_rotation_radians(head_yaw)),
            absolute_position: None,
            absolute_look: None,
            relative_position: [false; 3],
            relative_look: [false; 2],
            velocity: None,
            relative_velocity: [false; 3],
            rotate_velocity: false,
            on_ground: None,
        }
    }
}

pub(crate) fn compact_rotation_radians(value: i8) -> f64 {
    (f64::from(i32::from(value) * 360) / 256.0).to_radians()
}

pub(crate) fn compact_pitch_radians(value: i8) -> f64 {
    (f64::from(i32::from(value) * 360) / 256.0)
        .clamp(-90.0, 90.0)
        .to_radians()
}

fn clamp_pitch_radians(value: f64) -> f64 {
    value.clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2)
}

fn decode_relative_axis(base: f64, delta: i64) -> f64 {
    if delta == 0 {
        base
    } else {
        ((base * 4096.0).round() + delta as f64) / 4096.0
    }
}

#[derive(Default)]
pub(crate) struct EntityProducerCache {
    epoch: Option<u64>,
    snapshots: HashMap<EntityIdentity, NormalizedEntitySnapshot>,
    recent_tokens: VecDeque<EntityProducerToken>,
    recent_token_set: HashSet<EntityProducerToken>,
}

impl EntityProducerCache {
    pub(crate) fn reset_scope(&mut self, epoch: u64) {
        self.epoch = Some(epoch);
        self.snapshots.clear();
        self.recent_tokens.clear();
        self.recent_token_set.clear();
    }

    pub(crate) fn deactivate_scope(&mut self) {
        self.epoch = None;
        self.snapshots.clear();
        self.recent_tokens.clear();
        self.recent_token_set.clear();
    }

    #[cfg(test)]
    pub(crate) fn scope_epoch(&self) -> Option<u64> {
        self.epoch
    }

    pub(crate) fn apply(
        &mut self,
        epoch: u64,
        input: EntityProducerInput,
    ) -> Option<NormalizedEntityEvent> {
        // A packet cannot open, close, or roll back a connection scope.  The
        // lifecycle owner must call reset_scope before the first observation
        // and on every authoritative connection transition.
        if self.epoch != Some(epoch) || !self.input_matches_scope(epoch, &input) {
            return None;
        }

        // Relative movement and removal both require an established shadow.
        // Rejecting before token admission lets a caller retry the same
        // admission after the missing Spawn has been observed.
        let required_shadow = match &input {
            EntityProducerInput::Move { patch, .. } => Some(patch.entity),
            EntityProducerInput::Remove { entity, .. } => Some(*entity),
            _ => None,
        };
        if required_shadow.is_some_and(|identity| !self.snapshots.contains_key(&identity)) {
            return None;
        }

        let token = match &input {
            EntityProducerInput::Spawn { token, .. }
            | EntityProducerInput::Move { token, .. }
            | EntityProducerInput::Update { token, .. }
            | EntityProducerInput::Animation { token, .. }
            | EntityProducerInput::Hurt { token, .. }
            | EntityProducerInput::Remove { token, .. } => token.clone(),
        };

        if let EntityProducerInput::Spawn { snapshot, .. }
        | EntityProducerInput::Update { snapshot, .. } = &input
        {
            if !snapshot.is_finite() {
                return None;
            }
        }

        match input {
            EntityProducerInput::Spawn { snapshot, .. } => {
                if !self.remember_token(&token) {
                    return None;
                }
                self.store_snapshot(epoch, snapshot.clone());
                Some(NormalizedEntityEvent::Spawned { entity: snapshot })
            }
            EntityProducerInput::Move { patch, .. } => {
                let mut snapshot = self
                    .snapshots
                    .get(&patch.entity)
                    .cloned()
                    .expect("move shadow was checked before movement admission");
                apply_move_patch(&mut snapshot, patch);
                if !snapshot.is_finite() || !self.remember_token(&token) {
                    return None;
                }
                self.snapshots.insert(patch.entity, snapshot.clone());
                Some(NormalizedEntityEvent::Moved { entity: snapshot })
            }
            EntityProducerInput::Update { snapshot, .. } => {
                if !self.remember_token(&token) {
                    return None;
                }
                self.store_snapshot(epoch, snapshot.clone());
                Some(NormalizedEntityEvent::Updated {
                    entity: snapshot,
                    changed: Vec::new(),
                })
            }
            EntityProducerInput::Animation {
                entity, animation, ..
            } => {
                if entity.epoch != epoch || !self.remember_token(&token) {
                    return None;
                }
                Some(NormalizedEntityEvent::Animation { entity, animation })
            }
            EntityProducerInput::Hurt {
                entity,
                possible_source,
                ..
            } => {
                if (entity.epoch != epoch
                    || possible_source.is_some_and(|source| source.epoch != epoch))
                    || !self.remember_token(&token)
                {
                    return None;
                }
                Some(NormalizedEntityEvent::Hurt {
                    entity,
                    possible_source,
                })
            }
            EntityProducerInput::Remove { entity, .. } => {
                if !self.remember_token(&token) {
                    return None;
                }
                self.snapshots
                    .remove(&entity)
                    .map(|last| NormalizedEntityEvent::Removed { entity, last })
            }
        }
    }

    /// Apply SetEntityMotion's packet residual without producing an entity
    /// envelope. Mineflayer 4.37.1 updates velocity but does not emit
    /// `entityMoved`; the runtime mirrors that boundary while still making
    /// the public observation immediately readable.
    pub(crate) fn apply_velocity_residual(
        &mut self,
        epoch: u64,
        token: EntityProducerToken,
        entity: EntityIdentity,
        velocity: [f64; 3],
    ) -> Option<NormalizedEntitySnapshot> {
        if self.epoch != Some(epoch)
            || token.epoch != epoch
            || entity.epoch != epoch
            || !velocity.iter().all(|value| value.is_finite())
        {
            return None;
        }
        let mut snapshot = self.snapshots.get(&entity)?.clone();
        snapshot.velocity = velocity;
        if !snapshot.is_finite() || !self.remember_token(&token) {
            return None;
        }
        self.snapshots.insert(entity, snapshot.clone());
        Some(snapshot)
    }

    fn input_matches_scope(&self, epoch: u64, input: &EntityProducerInput) -> bool {
        match input {
            EntityProducerInput::Spawn { token, snapshot }
            | EntityProducerInput::Update { token, snapshot } => {
                token.epoch == epoch && snapshot.identity.epoch == epoch
            }
            EntityProducerInput::Move { token, patch } => {
                token.epoch == epoch && patch.entity.epoch == epoch
            }
            EntityProducerInput::Animation { token, entity, .. } => {
                token.epoch == epoch && entity.epoch == epoch
            }
            EntityProducerInput::Hurt {
                token,
                entity,
                possible_source,
                ..
            } => {
                token.epoch == epoch
                    && entity.epoch == epoch
                    && possible_source.is_none_or(|source| source.epoch == epoch)
            }
            EntityProducerInput::Remove { token, entity } => {
                token.epoch == epoch && entity.epoch == epoch
            }
        }
    }

    fn remember_token(&mut self, token: &EntityProducerToken) -> bool {
        if !self.recent_token_set.insert(token.clone()) {
            return false;
        }
        self.recent_tokens.push_back(token.clone());
        if self.recent_tokens.len() > ENTITY_PRODUCER_DEDUPE_CAPACITY {
            let evicted = self
                .recent_tokens
                .pop_front()
                .expect("dedupe queue length is above its capacity");
            self.recent_token_set.remove(&evicted);
        }
        true
    }

    fn store_snapshot(&mut self, epoch: u64, snapshot: NormalizedEntitySnapshot) {
        if snapshot.identity.epoch == epoch {
            self.snapshots.insert(snapshot.identity, snapshot);
        }
    }
}

impl NormalizedEntitySnapshot {
    fn is_finite(&self) -> bool {
        self.position
            .iter()
            .chain(self.velocity.iter())
            .copied()
            .all(f64::is_finite)
            && self.yaw.is_finite()
            && (self.yaw as f32).is_finite()
            && self.pitch.is_finite()
            && (self.pitch as f32).is_finite()
            && self
                .head_yaw
                .is_none_or(|value| value.is_finite() && (value as f32).is_finite())
            && self.width.is_finite()
            && (self.width as f32).is_finite()
            && self.height.is_finite()
            && (self.height as f32).is_finite()
    }
}

fn apply_move_patch(snapshot: &mut NormalizedEntitySnapshot, patch: EntityMovePatch) {
    let old_yaw = snapshot.yaw;
    let old_pitch = snapshot.pitch;
    let old_velocity = snapshot.velocity;
    let next_yaw = patch
        .compact_look
        .map(|[yaw, _]| compact_rotation_radians(yaw))
        .or_else(|| {
            patch.absolute_look.map(|look| {
                if patch.relative_look[0] {
                    snapshot.yaw + look[0]
                } else {
                    look[0]
                }
            })
        })
        .unwrap_or(snapshot.yaw);
    let next_pitch = patch
        .compact_look
        .map(|[_, pitch]| compact_pitch_radians(pitch))
        .or_else(|| {
            patch.absolute_look.map(|look| {
                clamp_pitch_radians(if patch.relative_look[1] {
                    snapshot.pitch + look[1]
                } else {
                    look[1]
                })
            })
        })
        .unwrap_or(snapshot.pitch);
    if let Some(delta) = patch.delta {
        for (axis, component) in snapshot.position.iter_mut().zip(delta) {
            *axis = decode_relative_axis(*axis, component);
        }
    }
    if let Some(position) = patch.absolute_position {
        for ((axis, value), relative) in snapshot
            .position
            .iter_mut()
            .zip(position)
            .zip(patch.relative_position)
        {
            *axis = if relative { *axis + value } else { value };
        }
    }
    if let Some([yaw, pitch]) = patch.compact_look {
        snapshot.yaw = compact_rotation_radians(yaw);
        snapshot.pitch = compact_pitch_radians(pitch);
    }
    if let Some(look) = patch.absolute_look {
        snapshot.yaw = if patch.relative_look[0] {
            snapshot.yaw + look[0]
        } else {
            look[0]
        };
        snapshot.pitch = clamp_pitch_radians(if patch.relative_look[1] {
            snapshot.pitch + look[1]
        } else {
            look[1]
        });
    }
    let mut next_velocity = old_velocity;
    if patch.rotate_velocity {
        // RelativeMovements::apply rotates the old velocity first, using the
        // old and newly resolved look directions, and only then applies the
        // packet's absolute/relative delta components.
        let rotated = azalea::Vec3::new(next_velocity[0], next_velocity[1], next_velocity[2])
            .x_rot((old_pitch - next_pitch) as f32)
            .y_rot((old_yaw - next_yaw) as f32);
        next_velocity = [rotated.x, rotated.y, rotated.z];
    }
    if let Some(velocity) = patch.velocity {
        for ((axis, value), relative) in next_velocity
            .iter_mut()
            .zip(velocity)
            .zip(patch.relative_velocity)
        {
            *axis = if relative { *axis + value } else { value };
        }
        snapshot.velocity = next_velocity;
    } else if patch.rotate_velocity {
        snapshot.velocity = next_velocity;
    }
    if let Some(head_yaw) = patch.head_yaw {
        snapshot.head_yaw = Some(head_yaw);
    }
    if let Some(on_ground) = patch.on_ground {
        snapshot.on_ground = on_ground;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(epoch: u64, id: i32, x: f64) -> NormalizedEntitySnapshot {
        NormalizedEntitySnapshot {
            identity: EntityIdentity::new(epoch, id),
            entity_type: "minecraft:zombie".to_owned(),
            uuid: Some("00000000-0000-0000-0000-000000000001".to_owned()),
            name: None,
            username: None,
            position: [x, 64.0, 2.0],
            velocity: [0.0, 0.0, 0.0],
            yaw: 90.0_f64.to_radians(),
            pitch: 10.0_f64.to_radians(),
            head_yaw: None,
            width: 0.6,
            height: 1.8,
            on_ground: false,
            pose: Some("standing".to_owned()),
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        }
    }

    fn token(epoch: u64, value: impl Into<String>) -> EntityProducerToken {
        EntityProducerToken::new(epoch, value)
    }

    fn move_patch(
        epoch: u64,
        id: i32,
        delta: Option<[i64; 3]>,
        compact_look: Option<[i8; 2]>,
        on_ground: bool,
    ) -> EntityMovePatch {
        EntityMovePatch::relative(
            EntityIdentity::new(epoch, id),
            delta,
            compact_look,
            on_ground,
        )
    }

    #[test]
    fn entity_key_is_epoch_and_protocol_id() {
        assert_eq!(EntityIdentity::new(1, 7).key(), "1:7");
        assert_ne!(EntityIdentity::new(2, 7).key(), "1:7");
    }

    #[test]
    fn six_event_sequence_uses_latest_snapshot_and_empty_update_fields() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(1);
        let identity = EntityIdentity::new(1, 7);

        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "add:7"),
                    snapshot: snapshot(1, 7, 1.0),
                },
            ),
            Some(NormalizedEntityEvent::Spawned { .. })
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "move:7:1"),
                    patch: move_patch(1, 7, Some([1024, 0, 0]), None, false),
                },
            ),
            Some(NormalizedEntityEvent::Moved { .. })
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Update {
                    token: token(1, "metadata:7:1"),
                    snapshot: snapshot(1, 7, 1.25),
                },
            ),
            Some(NormalizedEntityEvent::Updated { changed, .. }) if changed.is_empty()
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Animation {
                    token: token(1, "animate:7:1"),
                    entity: identity,
                    animation: "swing_main_hand".to_owned(),
                },
            ),
            Some(NormalizedEntityEvent::Animation { .. })
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Hurt {
                    token: token(1, "hurt:7:1"),
                    entity: identity,
                    possible_source: Some(EntityIdentity::new(1, 9)),
                },
            ),
            Some(NormalizedEntityEvent::Hurt { .. })
        ));

        let removed = cache.apply(
            1,
            EntityProducerInput::Remove {
                token: token(1, "remove:7:1"),
                entity: identity,
            },
        );
        assert!(matches!(
            removed,
            Some(NormalizedEntityEvent::Removed { last, .. })
                if last.position == [1.25, 64.0, 2.0]
        ));
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Remove {
                    token: token(1, "remove:7:2"),
                    entity: identity,
                },
            )
            .is_none());
    }

    #[test]
    fn duplicate_token_is_suppressed_without_suppressing_other_events() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(1);
        let first = EntityProducerInput::Spawn {
            token: token(1, "packet:1"),
            snapshot: snapshot(1, 1, 1.0),
        };
        assert!(cache.apply(1, first.clone()).is_some());
        assert!(cache.apply(1, first).is_none());
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "packet:2"),
                    patch: move_patch(1, 1, Some([4096, 0, 0]), None, false),
                },
            )
            .is_some());
    }

    #[test]
    fn compact_pitch_extreme_clamps_x_rot_100_to_half_pi() {
        assert!((compact_pitch_radians(100) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((compact_pitch_radians(-100) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);

        let mut cache = EntityProducerCache::default();
        cache.reset_scope(1);
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "pitch:spawn"),
                    snapshot: snapshot(1, 7, 1.0),
                },
            )
            .is_some());
        let moved = cache
            .apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "pitch:move"),
                    patch: move_patch(1, 7, None, Some([0, 100]), false),
                },
            )
            .expect("extreme compact pitch should still produce a move");
        assert!(matches!(
            moved,
            NormalizedEntityEvent::Moved { entity }
                if (entity.pitch - std::f64::consts::FRAC_PI_2).abs() < 1e-12
        ));
    }

    #[test]
    fn scope_reset_drops_old_snapshots_and_tokens() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(1);
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "packet:1"),
                    snapshot: snapshot(1, 7, 4.0),
                },
            )
            .is_some());

        cache.reset_scope(2);
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Remove {
                    token: token(2, "remove:7"),
                    entity: EntityIdentity::new(2, 7),
                },
            )
            .is_none());
        assert_eq!(cache.scope_epoch(), Some(2));
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Spawn {
                    token: token(2, "packet:1"),
                    snapshot: snapshot(2, 7, 8.0),
                },
            )
            .is_some());
    }

    #[test]
    fn stale_epoch_input_cannot_cross_scope() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(2);
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Animation {
                    token: token(1, "stale"),
                    entity: EntityIdentity::new(2, 7),
                    animation: "wake_up".to_owned(),
                },
            )
            .is_none());
    }

    #[test]
    fn late_epoch_input_cannot_roll_back_active_scope_or_remove_its_snapshot() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(2);
        let identity = EntityIdentity::new(2, 7);
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Spawn {
                    token: token(2, "spawn:2:7"),
                    snapshot: snapshot(2, 7, 20.0),
                },
            )
            .is_some());

        assert!(cache
            .apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "late:1:7"),
                    patch: move_patch(1, 7, Some([4096, 0, 0]), None, false),
                },
            )
            .is_none());
        assert_eq!(cache.scope_epoch(), Some(2));

        let removed = cache.apply(
            2,
            EntityProducerInput::Remove {
                token: token(2, "remove:2:7"),
                entity: identity,
            },
        );
        assert!(matches!(
            removed,
            Some(NormalizedEntityEvent::Removed { last, .. })
                if last.position == [20.0, 64.0, 2.0]
        ));
    }

    #[test]
    fn invalid_snapshot_identity_is_not_emitted_stored_or_deduped() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(2);
        let shared_token = token(2, "retry:7");

        assert!(cache
            .apply(
                2,
                EntityProducerInput::Spawn {
                    token: shared_token.clone(),
                    snapshot: snapshot(1, 7, 1.0),
                },
            )
            .is_none());
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Spawn {
                    token: shared_token,
                    snapshot: snapshot(2, 7, 2.0),
                },
            )
            .is_some());
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Remove {
                    token: token(2, "remove:7"),
                    entity: EntityIdentity::new(2, 7),
                },
            )
            .is_some());
    }

    #[test]
    fn invalid_hurt_source_is_not_deduped_and_valid_retry_emits() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(2);
        let shared_token = token(2, "hurt:7:1");
        let entity = EntityIdentity::new(2, 7);

        assert!(cache
            .apply(
                2,
                EntityProducerInput::Hurt {
                    token: shared_token.clone(),
                    entity,
                    possible_source: Some(EntityIdentity::new(1, 9)),
                },
            )
            .is_none());
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Hurt {
                    token: shared_token,
                    entity,
                    possible_source: Some(EntityIdentity::new(2, 9)),
                },
            )
            .is_some());
    }

    #[test]
    fn recent_token_fifo_evicts_old_tokens_at_declared_capacity() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(2);
        let entity = EntityIdentity::new(2, 7);
        let first_token = token(2, "animation:first");
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Animation {
                    token: first_token.clone(),
                    entity,
                    animation: "swing_main_hand".to_owned(),
                },
            )
            .is_some());

        for index in 0..ENTITY_PRODUCER_DEDUPE_CAPACITY {
            assert!(cache
                .apply(
                    2,
                    EntityProducerInput::Animation {
                        token: token(2, format!("animation:{index}")),
                        entity,
                        animation: "swing_off_hand".to_owned(),
                    },
                )
                .is_some());
        }

        // The FIFO holds exactly the most recent capacity tokens, so the
        // oldest overlap token is eligible again after eviction.
        assert!(cache
            .apply(
                2,
                EntityProducerInput::Animation {
                    token: first_token,
                    entity,
                    animation: "swing_main_hand".to_owned(),
                },
            )
            .is_some());
    }

    #[test]
    fn received_batch_keeps_each_packet_state_in_spawn_move_move_remove_order() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(4);
        let identity = EntityIdentity::new(4, 7);
        assert_eq!(identity.key(), "4:7");

        let mut add_snapshot = snapshot(4, 7, 10.0);
        add_snapshot.velocity = [0.25, -0.5, 0.75];
        add_snapshot.yaw = compact_rotation_radians(64);
        add_snapshot.pitch = compact_pitch_radians(-32);
        add_snapshot.head_yaw = Some(compact_rotation_radians(32));

        let events = [
            cache.apply(
                4,
                EntityProducerInput::Spawn {
                    token: token(4, "admission:10:add"),
                    snapshot: add_snapshot,
                },
            ),
            cache.apply(
                4,
                EntityProducerInput::Move {
                    token: token(4, "admission:11:move-pos"),
                    patch: move_patch(4, 7, Some([4096, 0, 0]), None, false),
                },
            ),
            cache.apply(
                4,
                EntityProducerInput::Move {
                    token: token(4, "admission:12:move-pos-rot"),
                    patch: move_patch(4, 7, Some([2048, 0, 0]), Some([32, -16]), true),
                },
            ),
            cache.apply(
                4,
                EntityProducerInput::Remove {
                    token: token(4, "admission:13:remove:7:0"),
                    entity: identity,
                },
            ),
        ];

        let Some(NormalizedEntityEvent::Spawned { entity: spawned }) = &events[0] else {
            panic!("Add must produce Spawned");
        };
        assert_eq!(spawned.entity_key(), "4:7");
        assert_eq!(spawned.position, [10.0, 64.0, 2.0]);
        assert_eq!(spawned.velocity, [0.25, -0.5, 0.75]);
        assert_eq!(spawned.yaw, compact_rotation_radians(64));
        assert_eq!(spawned.pitch, compact_pitch_radians(-32));
        assert_eq!(spawned.head_yaw, Some(compact_rotation_radians(32)));

        let Some(NormalizedEntityEvent::Moved { entity: first_move }) = &events[1] else {
            panic!("MovePos must produce Moved");
        };
        assert_eq!(first_move.position, [11.0, 64.0, 2.0]);
        assert_eq!(
            first_move.yaw,
            compact_rotation_radians(64),
            "pos-only must retain yaw"
        );
        assert_eq!(
            first_move.pitch,
            compact_pitch_radians(-32),
            "pos-only must retain pitch"
        );

        let Some(NormalizedEntityEvent::Moved {
            entity: second_move,
        }) = &events[2]
        else {
            panic!("MovePosRot must produce Moved");
        };
        assert_eq!(second_move.position, [11.5, 64.0, 2.0]);
        assert_eq!(second_move.yaw, compact_rotation_radians(32));
        assert_eq!(second_move.pitch, compact_pitch_radians(-16));
        assert!(second_move.on_ground);

        assert!(matches!(
            &events[3],
            Some(NormalizedEntityEvent::Removed { entity, last })
                if entity.key() == "4:7" && last.position == [11.5, 64.0, 2.0]
        ));

        cache.reset_scope(5);
        assert!(cache
            .apply(
                4,
                EntityProducerInput::Remove {
                    token: token(4, "late:remove:7"),
                    entity: identity,
                },
            )
            .is_none());
        assert_eq!(cache.scope_epoch(), Some(5));
    }

    #[test]
    fn add_then_remove_same_batch_uses_packet_shadow_without_ecs_entity() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(3);
        let identity = EntityIdentity::new(3, 21);
        let add = snapshot(3, 21, 8.0);

        let spawned = cache.apply(
            3,
            EntityProducerInput::Spawn {
                token: token(3, "admission:20:add"),
                snapshot: add.clone(),
            },
        );
        let removed = cache.apply(
            3,
            EntityProducerInput::Remove {
                token: token(3, "admission:21:remove:21:0"),
                entity: identity,
            },
        );

        assert!(matches!(
            spawned,
            Some(NormalizedEntityEvent::Spawned { .. })
        ));
        assert!(matches!(
            removed,
            Some(NormalizedEntityEvent::Removed { last, .. }) if last == add
        ));
    }

    #[test]
    fn rot_only_preserves_position_pos_only_preserves_look_and_unknown_move_is_ignored() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(6);
        let mut add = snapshot(6, 7, 3.0);
        add.yaw = 12.0_f64.to_radians();
        add.pitch = -6.0_f64.to_radians();
        assert!(cache
            .apply(
                6,
                EntityProducerInput::Spawn {
                    token: token(6, "admission:30:add"),
                    snapshot: add,
                },
            )
            .is_some());

        let rotation = cache.apply(
            6,
            EntityProducerInput::Move {
                token: token(6, "admission:31:move-rot"),
                patch: move_patch(6, 7, None, Some([16, -8]), true),
            },
        );
        let Some(NormalizedEntityEvent::Moved { entity: rotated }) = rotation else {
            panic!("known rot-only move must emit");
        };
        assert_eq!(rotated.position, [3.0, 64.0, 2.0]);
        assert_eq!(rotated.yaw, compact_rotation_radians(16));
        assert_eq!(rotated.pitch, compact_pitch_radians(-8));

        let position = cache.apply(
            6,
            EntityProducerInput::Move {
                token: token(6, "admission:32:move-pos"),
                patch: move_patch(6, 7, Some([4096, 0, 0]), None, false),
            },
        );
        let Some(NormalizedEntityEvent::Moved { entity: moved }) = position else {
            panic!("known pos-only move must emit");
        };
        assert_eq!(moved.position, [4.0, 64.0, 2.0]);
        assert_eq!(moved.yaw, compact_rotation_radians(16));
        assert_eq!(moved.pitch, compact_pitch_radians(-8));

        let unknown_token = token(6, "admission:33:unknown-move");
        assert!(cache
            .apply(
                6,
                EntityProducerInput::Move {
                    token: unknown_token.clone(),
                    patch: move_patch(6, 99, Some([4096, 0, 0]), None, false),
                },
            )
            .is_none());
        assert!(cache
            .apply(
                6,
                EntityProducerInput::Spawn {
                    token: token(6, "admission:34:add-unknown"),
                    snapshot: snapshot(6, 99, 20.0),
                },
            )
            .is_some());
        assert!(cache
            .apply(
                6,
                EntityProducerInput::Move {
                    token: unknown_token,
                    patch: move_patch(6, 99, Some([4096, 0, 0]), None, false),
                },
            )
            .is_some());
    }

    #[test]
    fn world_boundary_preserves_old_remove_and_starts_a_new_shadow() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(1);

        assert!(cache
            .apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "before:add:7"),
                    snapshot: snapshot(1, 7, 10.0),
                },
            )
            .is_some());
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "before:move:7"),
                    patch: move_patch(1, 7, Some([4096, 0, 0]), None, true),
                },
            ),
            Some(NormalizedEntityEvent::Moved { entity })
                if entity.position[0] == 11.0
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Remove {
                    token: token(1, "before:remove:7"),
                    entity: EntityIdentity::new(1, 7),
                },
            ),
            Some(NormalizedEntityEvent::Removed { last, .. })
                if last.position[0] == 11.0
        ));

        // A second old shadow is intentionally left live so a boundary reset
        // proves that no pre-Login/Respawn entity crosses into the new world.
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "before:add:8"),
                    snapshot: snapshot(1, 8, 30.0),
                },
            )
            .is_some());
        // The runtime performs this reset at the Login/Respawn packet's exact
        // position in the ordered ReceiveGamePacketEvent stream.
        cache.reset_scope(1);
        assert!(cache
            .apply(
                1,
                EntityProducerInput::Remove {
                    token: token(1, "after:remove:8"),
                    entity: EntityIdentity::new(1, 8),
                },
            )
            .is_none());

        assert!(cache
            .apply(
                1,
                EntityProducerInput::Spawn {
                    token: token(1, "after:add:7"),
                    snapshot: snapshot(1, 7, 20.0),
                },
            )
            .is_some());
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Move {
                    token: token(1, "after:move:7"),
                    patch: move_patch(1, 7, Some([4096, 0, 0]), None, false),
                },
            ),
            Some(NormalizedEntityEvent::Moved { entity })
                if entity.position[0] == 21.0
        ));
        assert!(matches!(
            cache.apply(
                1,
                EntityProducerInput::Remove {
                    token: token(1, "after:remove:7"),
                    entity: EntityIdentity::new(1, 7),
                },
            ),
            Some(NormalizedEntityEvent::Removed { last, .. })
                if last.position[0] == 21.0
        ));
    }

    #[test]
    fn teleport_mixed_relative_applies_old_velocity_rotation_before_delta() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(8);
        let identity = EntityIdentity::new(8, 7);
        let mut add = snapshot(8, 7, 10.0);
        add.yaw = 30.0_f64.to_radians();
        add.pitch = 20.0_f64.to_radians();
        add.velocity = [1.0, 2.0, 3.0];
        assert!(cache
            .apply(
                8,
                EntityProducerInput::Spawn {
                    token: token(8, "teleport:add"),
                    snapshot: add,
                },
            )
            .is_some());

        let moved = cache.apply(
            8,
            EntityProducerInput::Move {
                token: token(8, "teleport:1"),
                patch: EntityMovePatch::teleport(
                    identity,
                    [1.0, 70.0, -2.0],
                    [50.0_f64.to_radians(), 35.0_f64.to_radians()],
                    [true, false, true],
                    [false, false],
                    [0.5, 4.0, -0.25],
                    [true, false, true],
                    true,
                    true,
                ),
            },
        );
        let Some(NormalizedEntityEvent::Moved { entity }) = moved else {
            panic!("teleport must produce Moved");
        };
        assert_eq!(entity.position, [11.0, 70.0, 0.0]);
        assert_eq!(entity.yaw, 50.0_f64.to_radians());
        assert_eq!(entity.pitch, 35.0_f64.to_radians());
        // Use the vendor Vec3 implementation directly: it preserves the
        // f32 angle conversion and the locked x_rot(...).y_rot(...) order.
        let rotated = azalea::Vec3::new(1.0, 2.0, 3.0)
            .x_rot((20.0_f64.to_radians() - 35.0_f64.to_radians()) as f32)
            .y_rot((30.0_f64.to_radians() - 50.0_f64.to_radians()) as f32);
        assert!((entity.velocity[0] - (rotated.x + 0.5)).abs() < 1e-12);
        assert!((entity.velocity[1] - 4.0).abs() < 1e-12);
        assert!((entity.velocity[2] - (rotated.z - 0.25)).abs() < 1e-12);
        assert!(entity.on_ground);
    }

    #[test]
    fn position_sync_is_absolute_and_rotate_head_is_a_shadow_move() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(9);
        let identity = EntityIdentity::new(9, 12);
        assert!(cache
            .apply(
                9,
                EntityProducerInput::Spawn {
                    token: token(9, "sync:add"),
                    snapshot: snapshot(9, 12, 2.0),
                },
            )
            .is_some());

        let synced = cache.apply(
            9,
            EntityProducerInput::Move {
                token: token(9, "sync:1"),
                patch: EntityMovePatch::position_sync(
                    identity,
                    [20.0, 65.0, -4.0],
                    [180.0_f64.to_radians(), -30.0_f64.to_radians()],
                    [0.0, 1.25, 0.0],
                    true,
                ),
            },
        );
        let Some(NormalizedEntityEvent::Moved { entity }) = synced else {
            panic!("position sync must produce Moved");
        };
        assert_eq!(entity.position, [20.0, 65.0, -4.0]);
        assert_eq!(entity.yaw, 180.0_f64.to_radians());
        assert_eq!(entity.pitch, -30.0_f64.to_radians());
        assert_eq!(entity.velocity, [0.0, 1.25, 0.0]);
        assert!(entity.on_ground);

        let rotated = cache.apply(
            9,
            EntityProducerInput::Move {
                token: token(9, "rotate-head:1"),
                patch: EntityMovePatch::rotate_head(identity, 64),
            },
        );
        let Some(NormalizedEntityEvent::Moved { entity }) = rotated else {
            panic!("rotate head must produce Moved");
        };
        assert_eq!(entity.head_yaw, Some(compact_rotation_radians(64)));
        assert_eq!(entity.position, [20.0, 65.0, -4.0]);
        assert_eq!(entity.yaw, 180.0_f64.to_radians());
        assert!(entity.on_ground, "head-only movement must retain on_ground");
    }

    #[test]
    fn velocity_residual_has_no_event_and_unknown_or_invalid_inputs_can_retry() {
        let mut cache = EntityProducerCache::default();
        cache.reset_scope(10);
        let identity = EntityIdentity::new(10, 4);
        let residual_token = token(10, "motion:1");
        assert!(cache
            .apply_velocity_residual(10, residual_token.clone(), identity, [1.0, 2.0, 3.0])
            .is_none());
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: token(10, "motion:add"),
                    snapshot: snapshot(10, 4, 4.0),
                },
            )
            .is_some());
        let updated = cache
            .apply_velocity_residual(10, residual_token.clone(), identity, [1.0, 2.0, 3.0])
            .expect("known SetEntityMotion should update the shadow");
        assert_eq!(updated.velocity, [1.0, 2.0, 3.0]);
        assert!(cache
            .apply_velocity_residual(10, residual_token, identity, [9.0, 9.0, 9.0])
            .is_none());
        let retry_motion_token = token(10, "motion:retry");
        assert!(cache
            .apply_velocity_residual(
                10,
                retry_motion_token.clone(),
                identity,
                [f64::NAN, f64::INFINITY, 0.0],
            )
            .is_none());
        assert_eq!(
            cache
                .apply_velocity_residual(10, retry_motion_token, identity, [4.0, 5.0, 6.0])
                .expect("finite SetEntityMotion retry")
                .velocity,
            [4.0, 5.0, 6.0]
        );

        let teleport_identity = EntityIdentity::new(10, 8);
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: token(10, "teleport:finite:add"),
                    snapshot: snapshot(10, 8, 8.0),
                },
            )
            .is_some());
        let teleport_token = token(10, "teleport:finite:retry");
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Move {
                    token: teleport_token.clone(),
                    patch: EntityMovePatch::teleport(
                        teleport_identity,
                        [f64::INFINITY, 70.0, 0.0],
                        [0.0, 0.0],
                        [false; 3],
                        [false; 2],
                        [0.0, 0.0, 0.0],
                        [false; 3],
                        false,
                        false,
                    ),
                },
            )
            .is_none());
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Move {
                    token: teleport_token,
                    patch: EntityMovePatch::teleport(
                        teleport_identity,
                        [18.0, 70.0, 0.0],
                        [0.0, 0.0],
                        [false; 3],
                        [false; 2],
                        [0.0, 0.0, 0.0],
                        [false; 3],
                        false,
                        false,
                    ),
                },
            )
            .is_some());

        let rotate_identity = EntityIdentity::new(10, 9);
        let rotate_token = token(10, "rotate:unknown:retry");
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Move {
                    token: rotate_token.clone(),
                    patch: EntityMovePatch::rotate_head(rotate_identity, 32),
                },
            )
            .is_none());
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: token(10, "rotate:unknown:add"),
                    snapshot: snapshot(10, 9, 9.0),
                },
            )
            .is_some());
        let Some(NormalizedEntityEvent::Moved { entity }) = cache.apply(
            10,
            EntityProducerInput::Move {
                token: rotate_token,
                patch: EntityMovePatch::rotate_head(rotate_identity, 32),
            },
        ) else {
            panic!("unknown RotateHead should retry after Spawn");
        };
        assert_eq!(entity.head_yaw, Some(compact_rotation_radians(32)));

        let invalid_token = token(10, "finite:retry");
        let mut invalid = snapshot(10, 5, 5.0);
        invalid.position[0] = f64::NAN;
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: invalid_token.clone(),
                    snapshot: invalid,
                },
            )
            .is_none());
        let mut retry = snapshot(10, 5, f64::MAX);
        retry.yaw = 12.0;
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: invalid_token,
                    snapshot: retry,
                },
            )
            .is_some());
        let scalar_token = token(10, "finite:scalar");
        let mut invalid_scalar = snapshot(10, 6, 6.0);
        invalid_scalar.yaw = f64::NAN;
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: scalar_token.clone(),
                    snapshot: invalid_scalar,
                },
            )
            .is_none());
        assert!(cache
            .apply(
                10,
                EntityProducerInput::Spawn {
                    token: scalar_token,
                    snapshot: snapshot(10, 6, 6.0),
                },
            )
            .is_some());
    }
}
