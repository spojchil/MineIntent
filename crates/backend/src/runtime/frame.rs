//! Atomic observation frame state, light/armor reducers, and snapshot capture.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LightSectionGeometry {
    pub(super) min_light_section: i32,
    pub(super) light_section_count: usize,
}

impl LightSectionGeometry {
    pub(super) fn from_world(world: &azalea::world::World) -> Option<Self> {
        let min_y = world.chunks.min_y();
        let height = world.chunks.height();
        if height == 0 || height % 16 != 0 || min_y % 16 != 0 {
            return None;
        }
        let min_light_section = (min_y >> 4).checked_sub(1)?;
        let light_section_count = usize::try_from(height / 16 + 2).ok()?;
        (light_section_count > 0).then_some(Self {
            min_light_section,
            light_section_count,
        })
    }

    pub(super) fn index_for_section_y(self, section_y: i32) -> Option<usize> {
        let index = section_y.checked_sub(self.min_light_section)?;
        let index = usize::try_from(index).ok()?;
        (index < self.light_section_count).then_some(index)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct CachedLightChunk {
    pub(super) sky: Vec<Option<Box<[u8; 4096]>>>,
    pub(super) block: Vec<Option<Box<[u8; 4096]>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LightCacheContext {
    pub(super) epoch: u64,
    pub(super) scope_generation: u64,
    pub(super) dimension: String,
    pub(super) has_skylight: Option<bool>,
    pub(super) geometry: Option<LightSectionGeometry>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct LightCache {
    pub(super) context: Option<LightCacheContext>,
    pub(super) chunks: HashMap<(i32, i32), CachedLightChunk>,
}

impl LightCache {
    pub(super) fn clear(&mut self) {
        self.context = None;
        self.chunks.clear();
    }

    pub(super) fn reset_scope(
        &mut self,
        epoch: u64,
        scope_generation: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) {
        self.chunks.clear();
        self.context = dimension.map(|dimension| LightCacheContext {
            epoch,
            scope_generation,
            dimension,
            has_skylight,
            geometry: None,
        });
    }

    pub(super) fn context_matches(
        context: &LightCacheContext,
        source: CanonicalSourceAdmission,
        dimension: &str,
    ) -> bool {
        context.epoch == source.epoch
            && context.scope_generation == source.scope_generation
            && context.dimension == dimension
    }

    pub(super) fn ensure_context(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: String,
        has_skylight: Option<bool>,
        geometry: LightSectionGeometry,
    ) -> bool {
        let Some(context) = self.context.as_mut() else {
            self.context = Some(LightCacheContext {
                epoch: source.epoch,
                scope_generation: source.scope_generation,
                dimension,
                has_skylight,
                geometry: Some(geometry),
            });
            return true;
        };
        if !Self::context_matches(context, source, &dimension) {
            return false;
        }
        if context.has_skylight != has_skylight {
            // A dimension's skylight property is part of the same scope.  If
            // the registry proof changes underneath a packet, refuse the
            // packet instead of silently reinterpreting old layers.
            return false;
        }
        match context.geometry {
            Some(current) if current != geometry => false,
            None => {
                context.geometry = Some(geometry);
                true
            }
            Some(_) => true,
        }
    }

    pub(super) fn apply_packet(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: String,
        has_skylight: Option<bool>,
        geometry: LightSectionGeometry,
        chunk_x: i32,
        chunk_z: i32,
        data: &azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
        replace_chunk: bool,
    ) -> bool {
        if !self.ensure_context(source, dimension, has_skylight, geometry) {
            return false;
        }
        let chunk = self.chunks.entry((chunk_x, chunk_z)).or_default();
        if replace_chunk
            || chunk.sky.len() != geometry.light_section_count
            || chunk.block.len() != geometry.light_section_count
        {
            chunk.sky = vec![None; geometry.light_section_count];
            chunk.block = vec![None; geometry.light_section_count];
        }

        apply_light_layer_mask(
            &mut chunk.sky,
            &data.sky_y_mask,
            &data.empty_sky_y_mask,
            data.sky_updates.as_ref(),
            geometry.light_section_count,
            has_skylight == Some(false),
        );
        apply_light_layer_mask(
            &mut chunk.block,
            &data.block_y_mask,
            &data.empty_block_y_mask,
            data.block_updates.as_ref(),
            geometry.light_section_count,
            false,
        );
        true
    }

    pub(super) fn remove_chunk(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: &str,
        chunk_x: i32,
        chunk_z: i32,
    ) -> bool {
        let Some(context) = self.context.as_ref() else {
            return false;
        };
        if !Self::context_matches(context, source, dimension) {
            return false;
        }
        self.chunks.remove(&(chunk_x, chunk_z));
        true
    }

    pub(super) fn value_at(
        &self,
        position: &Vec3Value,
        epoch: u64,
        scope_generation: u64,
        dimension: &str,
    ) -> Option<u8> {
        let context = self.context.as_ref()?;
        if context.epoch != epoch
            || context.scope_generation != scope_generation
            || context.dimension != dimension
        {
            return None;
        }
        let x = floor_block_coordinate(position.x)?;
        let y = floor_block_coordinate(position.y)?;
        let z = floor_block_coordinate(position.z)?;
        let section_y = y.div_euclid(16);
        let section_index = context.geometry?.index_for_section_y(section_y)?;
        let chunk_x = x.div_euclid(16);
        let chunk_z = z.div_euclid(16);
        let local_x = usize::try_from(x.rem_euclid(16)).ok()?;
        let local_y = usize::try_from(y.rem_euclid(16)).ok()?;
        let local_z = usize::try_from(z.rem_euclid(16)).ok()?;
        let layer_index = (local_y << 8) | (local_z << 4) | local_x;
        let chunk = self.chunks.get(&(chunk_x, chunk_z));
        let sky = if context.has_skylight == Some(false) {
            Some(0)
        } else {
            chunk
                .and_then(|chunk| chunk.sky.get(section_index))
                .and_then(|layer| layer.as_ref())
                .and_then(|layer| layer.get(layer_index).copied())
        };
        let block = chunk
            .and_then(|chunk| chunk.block.get(section_index))
            .and_then(|layer| layer.as_ref())
            .and_then(|layer| layer.get(layer_index).copied());

        match (sky, block) {
            (Some(sky), Some(block)) => Some(sky.max(block)),
            (Some(15), None) | (None, Some(15)) => Some(15),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn layer(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        section_index: usize,
        sky: bool,
    ) -> Option<&Box<[u8; 4096]>> {
        self.chunks
            .get(&(chunk_x, chunk_z))
            .and_then(|chunk| {
                if sky {
                    chunk.sky.get(section_index)
                } else {
                    chunk.block.get(section_index)
                }
            })
            .and_then(Option::as_ref)
    }
}

fn apply_light_layer_mask(
    layers: &mut [Option<Box<[u8; 4096]>>],
    data_mask: &azalea::core::bitset::BitSet,
    empty_mask: &azalea::core::bitset::BitSet,
    updates: &[Box<[u8]>],
    light_section_count: usize,
    force_zero: bool,
) {
    let mut update_index = 0;
    for section_index in data_mask.iter_ones() {
        let update = updates.get(update_index);
        update_index += 1;
        if section_index >= light_section_count {
            continue;
        }
        layers[section_index] = if force_zero {
            Some(zero_light_layer())
        } else {
            update.and_then(|update| decode_light_layer(update))
        };
    }

    for section_index in empty_mask.iter_ones() {
        if section_index >= light_section_count || data_mask.get(section_index) == Some(true) {
            continue;
        }
        layers[section_index] = Some(zero_light_layer());
    }
}

fn zero_light_layer() -> Box<[u8; 4096]> {
    Box::new([0; 4096])
}

fn decode_light_layer(bytes: &[u8]) -> Option<Box<[u8; 4096]>> {
    if bytes.len() != 2048 {
        return None;
    }
    let mut layer = Box::new([0; 4096]);
    for local_y in 0..16 {
        for local_z in 0..16 {
            for local_x in 0..16 {
                let index = (local_y << 8) | (local_z << 4) | local_x;
                let packed = bytes[index >> 1];
                layer[index] = if index & 1 == 0 {
                    packed & 0x0f
                } else {
                    packed >> 4
                };
            }
        }
    }
    Some(layer)
}

pub(super) fn floor_block_coordinate(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let value = value.floor();
    (value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX)).then_some(value as i32)
}

/// Reproduce `AttributeInstance`'s grouped operation order.  The packet's
/// modifier vector is first reduced to a Java-map-shaped last-write-wins map;
/// operation iteration is never used to determine the three group order.
fn calculate_armor_snapshot(
    snapshot: &azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot,
) -> Option<u8> {
    calculate_armor_values(
        snapshot.base,
        &snapshot.modifiers,
        |modifier| modifier.id.clone(),
        |modifier| modifier.amount,
        |modifier| modifier.operation,
    )
}

pub(super) fn calculate_armor_values<T, K, Id, Amount, Operation>(
    base: f64,
    modifiers: &[T],
    mut id: Id,
    mut amount: Amount,
    mut operation: Operation,
) -> Option<u8>
where
    K: PartialEq,
    Id: FnMut(&T) -> K,
    Amount: FnMut(&T) -> f64,
    Operation: FnMut(&T) -> azalea::core::attribute_modifier_operation::AttributeModifierOperation,
{
    use azalea::core::attribute_modifier_operation::AttributeModifierOperation;

    if !base.is_finite() {
        return None;
    }
    let mut modifier_indices = Vec::<usize>::new();
    for index in 0..modifiers.len() {
        if let Some(existing) = modifier_indices
            .iter()
            .position(|existing| id(&modifiers[*existing]) == id(&modifiers[index]))
        {
            modifier_indices[existing] = index;
        } else {
            modifier_indices.push(index);
        }
    }

    let mut add_value = base;
    let mut multiplied_base_sum: f64 = 0.0;
    let mut multiplied_total = Vec::new();
    for index in modifier_indices {
        let modifier_amount = amount(&modifiers[index]);
        if !modifier_amount.is_finite() {
            return None;
        }
        match operation(&modifiers[index]) {
            AttributeModifierOperation::AddValue => {
                add_value += modifier_amount;
                if !add_value.is_finite() {
                    return None;
                }
            }
            AttributeModifierOperation::AddMultipliedBase => {
                multiplied_base_sum += modifier_amount;
                if !multiplied_base_sum.is_finite() {
                    return None;
                }
            }
            AttributeModifierOperation::AddMultipliedTotal => {
                multiplied_total.push(modifier_amount);
            }
        }
    }

    let mut value = add_value + add_value * multiplied_base_sum;
    if !value.is_finite() {
        return None;
    }
    for amount in multiplied_total {
        let factor = 1.0 + amount;
        if !factor.is_finite() {
            return None;
        }
        value *= factor;
        if !value.is_finite() {
            return None;
        }
    }

    // The vanilla attribute sanitizer's lower bound is zero for armor.  The
    // public frame fact additionally follows the frozen 0..20 wire range.
    let sanitized = value.max(0.0);
    if !sanitized.is_finite() {
        return None;
    }
    Some(sanitized.floor().clamp(0.0, 20.0) as u8)
}

/// The observation values used by one viewport capture share one short-lived
/// generation lock. The world itself remains behind its own read/write lock;
/// this lock only binds the world handle, snapshot, source and entities to one
/// published capture.

impl SharedRuntime {
    /// Apply one self-armor packet at its immutable source position. The
    /// admission lock is reacquired before mutating the observation
    /// generation, so a packet admitted for A cannot be relabelled as B.
    pub(super) fn apply_armor_packet(
        &self,
        source: CanonicalSourceAdmission,
        values: &[azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot],
    ) -> bool {
        let mut armor = None;
        let mut saw_armor = false;
        for value in values {
            if !matches!(value.attribute, azalea::registry::builtin::Attribute::Armor) {
                continue;
            }
            saw_armor = true;
            armor = calculate_armor_snapshot(value);
        }
        if !saw_armor {
            return false;
        }

        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let mut observation = self.observation.write();
        observation.armor = armor;
        observation.armor_epoch = Some(source.epoch);
        observation.bump_generation();
        true
    }

    /// Apply light data without consulting a later Bevy scope.  `source` is
    /// the immutable stamp captured while the raw packet was at the reducer's
    /// cursor; a scope reset between the two checks therefore rejects the
    /// packet instead of relabeling it with the final scope.
    pub(super) fn apply_light_packet(
        &self,
        source: CanonicalSourceAdmission,
        geometry: LightSectionGeometry,
        chunk_x: i32,
        chunk_z: i32,
        data: &azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
        replace_chunk: bool,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let Some(dimension) = self.writer.lock().dimension.clone() else {
            return false;
        };
        let has_skylight = self
            .observation
            .read()
            .light_cache
            .context
            .as_ref()
            .and_then(|context| context.has_skylight);
        let mut observation = self.observation.write();
        if !observation.light_cache.apply_packet(
            source,
            dimension,
            has_skylight,
            geometry,
            chunk_x,
            chunk_z,
            data,
            replace_chunk,
        ) {
            return false;
        }
        observation.bump_generation();
        true
    }

    pub(super) fn remove_light_chunk(
        &self,
        source: CanonicalSourceAdmission,
        chunk_x: i32,
        chunk_z: i32,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let Some(dimension) = self.writer.lock().dimension.clone() else {
            return false;
        };
        let mut observation = self.observation.write();
        if !observation
            .light_cache
            .remove_chunk(source, &dimension, chunk_x, chunk_z)
        {
            return false;
        }
        observation.bump_generation();
        true
    }

    pub(super) fn set_world_if_running(&self, world: SharedWorld) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        let (current_epoch, current_dimension) = {
            let writer = self.writer.lock();
            (writer.connection_epoch, writer.dimension.clone())
        };
        let (current_scope_generation, current_owner_matches_epoch) = {
            let producer = self.entity_producer.lock();
            (
                producer.scope_generation,
                producer
                    .owner
                    .is_some_and(|(_, epoch)| epoch == current_epoch),
            )
        };
        let mut observation = self.observation.write();
        let replaced = observation
            .world
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, &world));
        observation.world = Some(world);
        if replaced {
            observation.snapshot = None;
            observation.snapshot_scope_generation = 0;
            observation.source = None;
            observation.tracked_entities.clear();
            observation.entity_residuals.clear();
            let preserve_light = current_owner_matches_epoch
                && observation
                    .light_cache
                    .context
                    .as_ref()
                    .is_some_and(|context| {
                        context.epoch == current_epoch
                            && context.scope_generation == current_scope_generation
                            && current_dimension.as_deref() == Some(context.dimension.as_str())
                    });
            if !preserve_light {
                observation.light_cache.clear();
            }
        }
        observation.bump_generation();
        true
    }

    pub(super) fn clear_observations(&self) {
        *self.reported_dimension.lock() = None;
        #[cfg(test)]
        self.invoke_observation_write_boundary_hook();
        let mut observation = self.observation.write();
        observation.world = None;
        observation.snapshot = None;
        observation.snapshot_scope_generation = 0;
        observation.source = None;
        observation.tracked_entities.clear();
        observation.entity_residuals.clear();
        observation.clear_all_frame_facts();
        observation.bump_generation();
    }

    pub(super) fn refresh_snapshot(
        &self,
        bot: &Client,
        force: bool,
        source: FactSource,
    ) -> Option<MinecraftSnapshotV1> {
        let capture_generation = self.observation.read().generation;
        let (process_session_id, connection_epoch, connection_attempt_id) = self.context();
        let next_revision = self.snapshot_revision.load(Ordering::Acquire) + 1;
        let Some(candidate) = capture(
            bot,
            &self.config.world_id,
            &process_session_id,
            connection_epoch,
            &connection_attempt_id,
            next_revision,
            self.lifecycle_revision.load(Ordering::Acquire),
            now_utc(),
        ) else {
            // 断线/重连时 Azalea 会先移除本地玩家实体；此刻不能把“读不到”
            // 伪造成坐标，也不能调用 query_self 触发 panic。
            return None;
        };
        let entities = crate::snapshot::capture_tracked_entities_for_epoch(bot, connection_epoch);
        if self.connection_epoch() != connection_epoch {
            return None;
        }
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        let scope_generation = self.entity_producer.lock().scope_generation;
        let mut observation = self.observation.write();
        if observation.generation != capture_generation
            || self.connection_epoch() != connection_epoch
        {
            return None;
        }
        let entities = merge_refreshed_tracked_entities(
            entities,
            &mut observation.entity_residuals,
            connection_epoch,
        );
        let changed = observation
            .snapshot
            .as_ref()
            .is_none_or(|previous| !previous.same_state_as(&candidate));
        observation.tracked_entities = entities;
        if force || changed {
            self.snapshot_revision
                .store(next_revision, Ordering::Release);
            observation.snapshot = Some(candidate.clone());
            observation.snapshot_scope_generation = scope_generation;
            observation.source = Some(source);
            observation.bump_generation();
            Some(candidate)
        } else {
            observation.bump_generation();
            None
        }
    }

    pub(super) fn stored_snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.observation.read().snapshot.clone()
    }

    pub(super) fn capture_frame_facts(&self) -> Option<RuntimeFrameFacts> {
        self.capture_frame_facts_locked(|| {}, || {})
    }

    pub(super) fn capture_frame_facts_locked<F, G>(
        &self,
        before_values: F,
        after_boundary: G,
    ) -> Option<RuntimeFrameFacts>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        let observation = self.observation.read();
        let snapshot = observation.snapshot.clone()?;
        before_values();
        after_boundary();
        let armor = (observation.armor_epoch == Some(snapshot.connection_epoch))
            .then_some(observation.armor)
            .flatten();
        let light = observation.light_cache.value_at(
            &snapshot.self_snapshot.position,
            snapshot.connection_epoch,
            observation.snapshot_scope_generation,
            &snapshot.world.dimension,
        );
        Some(RuntimeFrameFacts {
            snapshot,
            armor,
            light,
        })
    }

    #[cfg(test)]
    pub(super) fn capture_frame_facts_with_test_hooks<F, G>(
        &self,
        before_values: F,
        after_boundary: G,
    ) -> Option<RuntimeFrameFacts>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        self.capture_frame_facts_locked(before_values, after_boundary)
    }

    pub(super) fn emit_snapshot(&self, snapshot: MinecraftSnapshotV1, source: FactSource) {
        self.emit_if_running(
            source,
            BackendEventPayload::SnapshotChanged(ContractProtocolSnapshotChangedEvent {
                group: "world".to_owned(),
                snapshot_revision: snapshot.snapshot_revision,
            }),
        );
    }
}

pub(super) fn read_block_from_world(
    world: &azalea::world::World,
    position: BlockPosition,
) -> BlockReadResult {
    let block_position = azalea::BlockPos {
        x: position.x,
        y: position.y,
        z: position.z,
    };
    let y = i64::from(block_position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return BlockReadResult::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return BlockReadResult::Unloaded;
    };
    BlockReadResult::Loaded {
        block: block_snapshot(position, state),
    }
}
