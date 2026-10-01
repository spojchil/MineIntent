//! 服务端下发的天空光与方块光。azalea 解析了光照包但不存，这里自己存一份供成像取用。
//!
//! 每个区块柱的光照段比方块段多两段：下标 0 是世界底下面那一段，末尾是世界顶上面那一段
//! （原版 `LightEngine` 的段范围）。每段 2048 字节，一格 4 位，下标 `y<<8 | z<<4 | x`，
//! 偶数格在低 4 位（原版 `DataLayer.get`）。

use std::collections::HashMap;
use std::sync::Arc;

use azalea::core::bitset::BitSet;
use azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData;

pub(crate) const LAYER_BYTES: usize = 2048;
type Layer = Arc<[u8; LAYER_BYTES]>;

#[derive(Clone, Debug, Default)]
pub(crate) struct ColumnLight {
    sky: Vec<Option<Layer>>,
    block: Vec<Option<Layer>>,
}

#[derive(Debug, Default)]
pub(super) struct LightStore {
    columns: HashMap<(i32, i32), ColumnLight>,
}

impl LightStore {
    /// 区块包（`fresh`，整柱重来）或光照更新包。原版 `ClientPacketListener.applyLightData`：
    /// 掩码里有的段用下发数据，空掩码里有的段置全 0，两者都没有的段保持原样。
    pub(super) fn apply(
        &mut self,
        x: i32,
        z: i32,
        data: &ClientboundLightUpdatePacketData,
        fresh: bool,
    ) {
        let column = self.columns.entry((x, z)).or_default();
        if fresh {
            *column = ColumnLight::default();
        }
        apply_layers(
            &mut column.sky,
            &data.sky_y_mask,
            &data.empty_sky_y_mask,
            &data.sky_updates,
        );
        apply_layers(
            &mut column.block,
            &data.block_y_mask,
            &data.empty_block_y_mask,
            &data.block_updates,
        );
    }

    pub(super) fn forget(&mut self, x: i32, z: i32) {
        self.columns.remove(&(x, z));
    }

    /// 换维度或重生：旧维度的光照全部作废。
    pub(super) fn clear(&mut self) {
        self.columns.clear();
    }

    /// 成像冻结用：段是 `Arc`，克隆只加引用计数。
    pub(super) fn column(&self, x: i32, z: i32) -> Option<ColumnLight> {
        self.columns.get(&(x, z)).cloned()
    }
}

fn apply_layers(
    layers: &mut Vec<Option<Layer>>,
    mask: &BitSet,
    empty: &BitSet,
    updates: &[Box<[u8]>],
) {
    let mut data = updates.iter();
    for index in mask.iter_ones() {
        let Some(bytes) = data.next() else {
            break;
        };
        let Ok(bytes) = <[u8; LAYER_BYTES]>::try_from(&bytes[..]) else {
            continue;
        };
        set(layers, index, Some(Arc::new(bytes)));
    }
    for index in empty.iter_ones() {
        set(layers, index, Some(Arc::new([0; LAYER_BYTES])));
    }
}

fn set(layers: &mut Vec<Option<Layer>>, index: usize, layer: Option<Layer>) {
    if layers.len() <= index {
        layers.resize(index + 1, None);
    }
    layers[index] = layer;
}

fn nibble(layer: &[u8; LAYER_BYTES], x: usize, y: usize, z: usize) -> u8 {
    let index = y << 8 | z << 4 | x;
    (layer[index >> 1] >> ((index & 1) << 2)) & 0xF
}

impl ColumnLight {
    /// 一格的 (天空光, 方块光)。`local` 是区块内 x/z（0..16），`y` 是世界坐标。
    ///
    /// 方块光段缺失按 0。天空光段缺失按原版 `SkyLightSectionStorage.getLightValue`：
    /// 往上找第一段有数据的，取它最底层同一 x/z 的值；上面都没有就是露天 15。
    pub(crate) fn get(&self, local: [usize; 2], y: i32, min_y: i32) -> (u8, u8) {
        let offset = y - min_y + 16;
        if offset < 0 {
            return (0, 0);
        }
        let section = (offset >> 4) as usize;
        let inner = (offset & 15) as usize;
        let block = self
            .block
            .get(section)
            .and_then(Option::as_ref)
            .map_or(0, |layer| nibble(layer, local[0], inner, local[1]));
        let sky = match self.sky.get(section).and_then(Option::as_ref) {
            Some(layer) => nibble(layer, local[0], inner, local[1]),
            None => self
                .sky
                .iter()
                .skip(section + 1)
                .flatten()
                .next()
                .map_or(15, |layer| nibble(layer, local[0], 0, local[1])),
        };
        (sky, block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(bits: &[usize]) -> BitSet {
        let mut set = BitSet::new(64);
        for &bit in bits {
            set.set(bit);
        }
        set
    }

    #[test]
    fn missing_sky_sections_inherit_from_the_next_section_above() {
        let mut layer = [0u8; LAYER_BYTES];
        // 段 3 的最底层 (x=1, z=0)：下标 1，奇数在高 4 位。
        layer[0] = 0x70;
        let data = ClientboundLightUpdatePacketData {
            sky_y_mask: mask(&[3]),
            empty_sky_y_mask: mask(&[1]),
            sky_updates: Arc::new(vec![layer.to_vec().into_boxed_slice()].into_boxed_slice()),
            ..Default::default()
        };
        let mut store = LightStore::default();
        store.apply(0, 0, &data, true);
        let column = store.column(0, 0).unwrap();
        let min_y = -64;
        // 段 2 没数据：往上取段 3 最底层。
        assert_eq!(column.get([1, 0], min_y + 16, min_y).0, 7);
        // 段 1 是空掩码：全 0，不往上找。
        assert_eq!(column.get([1, 0], min_y, min_y).0, 0);
        // 段 4 及以上没有数据：露天 15。
        assert_eq!(column.get([1, 0], min_y + 48, min_y).0, 15);
        // 方块光没数据：0。
        assert_eq!(column.get([1, 0], min_y + 32, min_y).1, 0);
    }
}
