//! 原版的方块光照：平滑光照与环境光遮蔽（`BlockModelLighter`）、平面光照、
//! 光照坐标（`LevelRenderer.getLightCoords`、`LightCoordsUtil`）与光照贴图（`lightmap.fsh`）。
//! 全部按 26.1.2 客户端转录；光照坐标沿用原版单位：每级 16，0..=240。

use std::collections::HashMap;

use crate::geometry::V3;
use crate::Cell;

/// 光照坐标 `[方块光, 天空光]`，原版单位（每级 16）。
pub(crate) type Coords = [f64; 2];

/// 自发光渲染（岩浆块等）与无光照场景用的满亮度。
pub(crate) const FULL_BRIGHT: Coords = [240.0, 240.0];

/// 场景里的光照格。查不到的格按露天空气（天空光 15、方块光 0）。
pub(crate) struct Cells(HashMap<[i32; 3], Cell>);

const OPEN_AIR: Cell = Cell {
    position: [0; 3],
    sky_light: 15,
    block_light: 0,
    emission: 0,
    dampening: 0,
    view_blocking: false,
    solid_render: false,
    emissive: false,
    full_collision: false,
};

impl Cells {
    pub(crate) fn new(cells: &[Cell]) -> Self {
        Self(cells.iter().map(|cell| (cell.position, *cell)).collect())
    }

    pub(crate) fn get(&self, position: [i32; 3]) -> &Cell {
        self.0.get(&position).unwrap_or(&OPEN_AIR)
    }

    /// `LevelRenderer.getLightCoords(state, pos)`：`state` 决定自发光，`pos` 决定存储的光照。
    pub(crate) fn coords(&self, state: &Cell, position: [i32; 3]) -> Coords {
        if state.emissive {
            return FULL_BRIGHT;
        }
        let stored = self.get(position);
        [
            f64::from(stored.block_light.max(state.emission)) * 16.0,
            f64::from(stored.sky_light) * 16.0,
        ]
    }

    /// `getShadeBrightness`：碰撞箱是完整方块的格 0.2，其余 1.0。
    fn shade(&self, position: [i32; 3]) -> f64 {
        if self.get(position).full_collision {
            0.2
        } else {
            1.0
        }
    }
}

/// `LightCoordsUtil.smoothBlend`：四格取平均；中心格够亮时，某格某通道为 0 就用中心值顶替。
/// 原版在打包整数上运算，结果按通道向下取整到原版单位。
fn smooth_blend(mut a: Coords, mut b: Coords, mut c: Coords, center: Coords) -> Coords {
    if center[1] > 32.0 || center[0] > 32.0 {
        for neighbour in [&mut a, &mut b, &mut c] {
            for channel in 0..2 {
                if neighbour[channel] < 16.0 {
                    neighbour[channel] = center[channel];
                }
            }
        }
    }
    std::array::from_fn(|i| ((a[i] + b[i] + c[i] + center[i]) / 4.0).floor())
}

/// 面的朝向（原版 `Direction` 序：下、上、北、南、西、东）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    pub(crate) fn vector(self) -> [i32; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    /// 法线最接近的轴向（原版烘焙时按法线定四边形朝向）。
    pub(crate) fn nearest(normal: V3) -> Self {
        let axis = (0..3)
            .max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))
            .unwrap_or(1);
        match (axis, normal[axis] >= 0.0) {
            (0, false) => Self::West,
            (0, true) => Self::East,
            (1, false) => Self::Down,
            (1, true) => Self::Up,
            (_, false) => Self::North,
            (_, true) => Self::South,
        }
    }

    /// 主世界的面朝向明暗（`CardinalLighting.DEFAULT`）。
    pub(crate) fn cardinal_shade(self) -> f64 {
        match self {
            Self::Down => 0.5,
            Self::Up => 1.0,
            Self::North | Self::South => 0.8,
            Self::West | Self::East => 0.6,
        }
    }

    /// `BlockModelLighter.AdjacencyInfo.corners`：面平面内的四个侧向，0/1 相对、2/3 相对。
    fn corners(self) -> [Self; 4] {
        use Direction::*;
        match self {
            Down => [West, East, North, South],
            Up => [East, West, North, South],
            North => [Up, Down, East, West],
            South => [West, East, Down, Up],
            West => [Up, Down, North, South],
            East => [Down, Up, North, South],
        }
    }
}

fn offset(position: [i32; 3], direction: Direction) -> [i32; 3] {
    let d = direction.vector();
    std::array::from_fn(|i| position[i] + d[i])
}

/// 顶点在方块内（0..1）朝某个侧向的权重：朝正向取坐标本身，朝负向取 1 减它。
fn toward(local: V3, direction: Direction) -> f64 {
    let d = direction.vector();
    let axis = (0..3).find(|&i| d[i] != 0).unwrap_or(0);
    let v = local[axis].clamp(0.0, 1.0);
    if d[axis] > 0 {
        v
    } else {
        1.0 - v
    }
}

/// 一个面四个顶点的（环境光遮蔽亮度, 光照坐标）。
///
/// `BlockModelLighter.prepareQuadAmbientOcclusion` 的转录。原版按固定的顶点重排表取四个角，
/// 部分面再按面范围做权重；这里统一按顶点在面内的位置对四个角值做双线性插值——
/// 顶点恰在角上时就是取那个角，部分面时与原版 `SizeInfo` 权重相同。
pub(crate) fn ambient_occlusion(
    cells: &Cells,
    position: [i32; 3],
    direction: Direction,
    local: &[V3],
) -> Vec<(f64, Coords)> {
    let own = cells.get(position);
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for v in local {
        for i in 0..3 {
            min[i] = min[i].min(v[i]);
            max[i] = max[i].max(v[i]);
        }
    }
    const LOW: f64 = 1.0e-4;
    const HIGH: f64 = 0.9999;
    let full = own.full_collision;
    let cubic = match direction {
        Direction::Down => min[1] == max[1] && (min[1] < LOW || full),
        Direction::Up => min[1] == max[1] && (max[1] > HIGH || full),
        Direction::North => min[2] == max[2] && (min[2] < LOW || full),
        Direction::South => min[2] == max[2] && (max[2] > HIGH || full),
        Direction::West => min[0] == max[0] && (min[0] < LOW || full),
        Direction::East => min[0] == max[0] && (max[0] > HIGH || full),
    };
    let base = if cubic {
        offset(position, direction)
    } else {
        position
    };
    let corners = direction.corners();
    let side = corners.map(|c| offset(base, c));
    let light = side.map(|p| cells.coords(cells.get(p), p));
    let shade = side.map(|p| cells.shade(p));
    let translucent = side.map(|p| {
        let beyond = cells.get(offset(p, direction));
        !beyond.view_blocking || beyond.dampening == 0
    });
    // 角格：两侧都不透光时，原版 26.1.2 一律退回侧格 0 的值（不是各自那一侧）。
    let corner = |a: usize, b: usize| -> (f64, Coords) {
        if !translucent[a] && !translucent[b] {
            (shade[0], light[0])
        } else {
            let p = offset(side[a], corners[b]);
            (cells.shade(p), cells.coords(cells.get(p), p))
        }
    };
    let mut center_light = cells.coords(own, position);
    let next = offset(position, direction);
    if cubic || !cells.get(next).solid_render {
        center_light = cells.coords(cells.get(next), next);
    }
    let center_shade = if cubic {
        cells.shade(base)
    } else {
        cells.shade(position)
    };
    // 四个角值，按 (侧 a ∈ {0,1}, 侧 b ∈ {2,3})。
    let corner_value = |a: usize, b: usize| -> (f64, Coords) {
        let (diagonal_shade, diagonal_light) = corner(a, b);
        (
            (shade[a] + shade[b] + diagonal_shade + center_shade) * 0.25,
            smooth_blend(light[b], light[a], diagonal_light, center_light),
        )
    };
    let values = [
        [corner_value(0, 2), corner_value(0, 3)],
        [corner_value(1, 2), corner_value(1, 3)],
    ];
    local
        .iter()
        .map(|&v| {
            let wa = [toward(v, corners[0]), toward(v, corners[1])];
            let wb = [toward(v, corners[2]), toward(v, corners[3])];
            let mut ao = 0.0;
            let mut coords = [0.0; 2];
            for a in 0..2 {
                for b in 0..2 {
                    let w = wa[a] * wb[b];
                    let (s, l) = values[a][b];
                    ao += s * w;
                    coords[0] += l[0] * w;
                    coords[1] += l[1] * w;
                }
            }
            (ao.clamp(0.0, 1.0), coords.map(f64::floor))
        })
        .collect()
}

/// 平面光照（`tesselateFlat` / `prepareQuadFlat`）：带剔面方向的面取那一侧邻格的光，
/// 其余取本格（立方面取面前那格）。光照里的方块光不低于本方块自身的发光值。
pub(crate) fn flat(
    cells: &Cells,
    position: [i32; 3],
    direction: Direction,
    toward_neighbour: bool,
) -> Coords {
    let own = cells.get(position);
    let at = if toward_neighbour {
        offset(position, direction)
    } else {
        position
    };
    cells.coords(own, at)
}

/// 原版 `FluidRenderer.getLightCoords`：本格与上方一格逐通道取大。
pub(crate) fn fluid(cells: &Cells, position: [i32; 3]) -> Coords {
    let here = cells.coords(cells.get(position), position);
    let above_position = offset(position, Direction::Up);
    let above = cells.coords(cells.get(above_position), above_position);
    [here[0].max(above[0]), here[1].max(above[1])]
}

/// 光照贴图（16×16，`lightmap.fsh`），按顶点光照坐标双线性取样（原版贴图线性过滤）。
pub(crate) struct Lightmap {
    texels: [[V3; 16]; 16],
}

/// 光照贴图的输入，对应 `LightmapRenderState`。
pub(crate) struct LightmapInputs {
    pub sky_factor: f64,
    pub block_factor: f64,
    pub block_light_tint: V3,
    pub sky_light_color: V3,
    pub ambient_color: V3,
    /// 亮度选项（原版默认 0.5）。
    pub brightness: f64,
}

impl Lightmap {
    pub(crate) fn new(inputs: &LightmapInputs) -> Self {
        let brightness_curve = |level: f64| level / (4.0 - 3.0 * level);
        let mut texels = [[[0.0; 3]; 16]; 16];
        for (block_index, row) in texels.iter_mut().enumerate() {
            for (sky_index, texel) in row.iter_mut().enumerate() {
                let block_level = block_index as f64 / 15.0;
                let sky_level = sky_index as f64 / 15.0;
                let block_brightness = brightness_curve(block_level) * inputs.block_factor;
                let sky_brightness = brightness_curve(sky_level) * inputs.sky_factor;
                let parabolic = (2.0 * block_level - 1.0).powi(2);
                let mut color: V3 = std::array::from_fn(|i| {
                    let block_color = inputs.block_light_tint[i]
                        + (1.0 - inputs.block_light_tint[i]) * 0.9 * parabolic;
                    inputs.ambient_color[i]
                        + inputs.sky_light_color[i] * sky_brightness
                        + block_color * block_brightness
                });
                color = color.map(|c| c.clamp(0.0, 1.0));
                let max = color.iter().copied().fold(0.0, f64::max);
                if max > 0.0 {
                    let inverted = 1.0 - max;
                    let scaled = 1.0 - inverted.powi(4);
                    color = color.map(|c| c + (c * (scaled / max) - c) * inputs.brightness);
                }
                *texel = color;
            }
        }
        Self { texels }
    }

    /// 原版 `sample_lightmap`：坐标 /256 + 半格，夹在首末格中心之间，线性过滤。
    pub(crate) fn sample(&self, coords: Coords) -> V3 {
        let level = coords.map(|c| (c / 16.0).clamp(0.0, 15.0));
        let low = level.map(|l| l.floor() as usize);
        let high = low.map(|l| (l + 1).min(15));
        let t = [level[0] - low[0] as f64, level[1] - low[1] as f64];
        std::array::from_fn(|i| {
            let row = |b: usize| {
                self.texels[b][low[1]][i] * (1.0 - t[1]) + self.texels[b][high[1]][i] * t[1]
            };
            row(low[0]) * (1.0 - t[0]) + row(high[0]) * t[0]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(position: [i32; 3], sky: u8, block: u8, solid: bool) -> Cell {
        Cell {
            position,
            sky_light: sky,
            block_light: block,
            emission: 0,
            dampening: if solid { 15 } else { 0 },
            view_blocking: solid,
            solid_render: solid,
            emissive: false,
            full_collision: solid,
        }
    }

    #[test]
    fn open_top_face_is_fully_lit_and_unoccluded() {
        // 一块孤立的实心方块，上面全是露天空气。
        let cells = Cells::new(&[cell([0, 0, 0], 0, 0, true)]);
        let corners = [
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        for (ao, coords) in ambient_occlusion(&cells, [0, 0, 0], Direction::Up, &corners) {
            assert_eq!(ao, 1.0);
            assert_eq!(coords, [0.0, 240.0]);
        }
    }

    #[test]
    fn a_wall_beside_the_top_face_darkens_only_its_edge() {
        // 东边上方一格立着实心方块：顶面东侧两个顶点被遮挡。
        let cells = Cells::new(&[cell([0, 0, 0], 0, 0, true), cell([1, 1, 0], 0, 0, true)]);
        let corners = [[0.0, 1.0, 0.5], [1.0, 1.0, 0.5]];
        let values = ambient_occlusion(&cells, [0, 0, 0], Direction::Up, &corners);
        assert_eq!(values[0].0, 1.0);
        // 东侧：侧格遮挡 0.2，另三格 1.0，平均 0.8；南北两角对半。
        assert!((values[1].0 - 0.8).abs() < 1e-9, "{}", values[1].0);
    }

    #[test]
    fn lightmap_is_dark_without_light_and_bright_in_daylight() {
        let map = Lightmap::new(&LightmapInputs {
            sky_factor: 1.0,
            block_factor: 1.4,
            block_light_tint: [1.0, 216.0 / 255.0, 140.0 / 255.0],
            sky_light_color: [1.0; 3],
            ambient_color: [10.0 / 255.0; 3],
            brightness: 0.5,
        });
        let dark = map.sample([0.0, 0.0]);
        let day = map.sample([0.0, 240.0]);
        assert!(dark.iter().all(|&c| c < 0.1), "{dark:?}");
        assert!(day.iter().all(|&c| c > 0.99), "{day:?}");
    }
}
