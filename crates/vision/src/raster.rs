//! CPU rasterization into an image: no surface, window or graphics device.
use image::RgbaImage;

use crate::biome::Biomes;
use crate::daylight::Daylight;
use crate::geometry::{build_lit, dot, sub, Triangle, V3};
use crate::light::{Cells, Lightmap, LightmapInputs};
use crate::sky::Sky;
use crate::{Camera, Frame, Options, Report, Resources, Scene};

const NEAR: f64 = 1e-4;
const SKY: [f64; 3] = [150.0, 185.0, 215.0];
const MAX_LAYERS: usize = 16;
/// 光栅化的条带高度（行）：限制透明层的临时内存，也是分箱的单位。
const BAND: usize = 16;

pub(crate) struct Frustum {
    eye: V3,
    /// 世界坐标法线与偏移：点 p 在内侧当且仅当 `normal · (p - eye) + offset ≥ 0`。
    planes: [(V3, f64); 6],
}

impl Frustum {
    /// 这些点是否全在同一个视锥平面外侧。是的话，它们围成的凸多边形里每一点也都在
    /// 那一侧，裁剪后为空、画不出任何像素。留一点余量：这里在世界坐标里算，裁剪在
    /// 相机坐标里算，贴着平面的点两边舍入可能不同，宁可留下交给裁剪。
    pub(crate) fn excludes(&self, points: &[V3]) -> bool {
        const MARGIN: f64 = 1e-6;
        self.planes.iter().any(|(normal, offset)| {
            points
                .iter()
                .all(|point| dot(*normal, sub(*point, self.eye)) + offset < -MARGIN)
        })
    }

    /// 区块段（16³）是否可能落在视锥里。外扩 1 格，容纳伸出格子的模型元素；
    /// 对每个平面取最靠内的角（原版 `FrustumIntersection.testAab` 的做法），保守不漏。
    pub(crate) fn section_visible(&self, section: [i32; 3]) -> bool {
        let min = section.map(|c| f64::from(c * 16 - 1));
        let max = section.map(|c| f64::from(c * 16 + 17));
        self.planes.iter().all(|(normal, offset)| {
            let corner: V3 =
                std::array::from_fn(|k| if normal[k] >= 0.0 { max[k] } else { min[k] });
            dot(*normal, sub(corner, self.eye)) + offset >= 0.0
        })
    }
}

pub(crate) struct Projection {
    axes: [V3; 3], // right, up, forward
    scale: [f64; 2],
    pub(crate) size: [f64; 2],
}

impl Projection {
    fn new(camera: &Camera, options: Options) -> Self {
        let (sy, cy) = camera.yaw.to_radians().sin_cos();
        let (sp, cp) = camera.pitch.to_radians().sin_cos();
        let vertical = (camera.vertical_fov.to_radians() * 0.5).tan();
        Self {
            axes: [
                [-cy, 0.0, -sy],
                [-sy * sp, cp, cy * sp],
                [-sy * cp, -sp, cy * cp],
            ],
            scale: [
                vertical * f64::from(options.width) / f64::from(options.height),
                vertical,
            ],
            size: [f64::from(options.width), f64::from(options.height)],
        }
    }

    fn vertex(&self, position: V3, uv: [f64; 2], color: V3, eye: V3) -> Vertex {
        let relative = sub(position, eye);
        Vertex {
            position: self.axes.map(|axis| dot(relative, axis)),
            uv,
            color,
        }
    }

    /// 像素中心的视线方向（世界坐标，前向分量为 1）：片元位置 = 深度 × 方向。
    pub(crate) fn direction(&self, x: usize, y: usize) -> V3 {
        let right = (2.0 * (x as f64 + 0.5) / self.size[0] - 1.0) * self.scale[0];
        let up = (1.0 - 2.0 * (y as f64 + 0.5) / self.size[1]) * self.scale[1];
        std::array::from_fn(|k| right * self.axes[0][k] + up * self.axes[1][k] + self.axes[2][k])
    }

    /// 近平面、远平面与四个侧面（相机坐标）。侧面裁剪避免穿过眼睛的三角形投影出巨大坐标。
    fn planes(&self, far: f64) -> [(V3, f64); 6] {
        let [sx, sy] = self.scale;
        [
            ([0.0, 0.0, 1.0], -NEAR),
            ([0.0, 0.0, -1.0], far),
            ([1.0, 0.0, sx], 0.0),
            ([-1.0, 0.0, sx], 0.0),
            ([0.0, 1.0, sy], 0.0),
            ([0.0, -1.0, sy], 0.0),
        ]
    }

    /// 世界坐标里的视锥（与 [`Self::planes`] 同一组平面，原版 `Frustum`）。
    pub(crate) fn frustum(&self, eye: V3, far: f64) -> Frustum {
        Frustum {
            eye,
            planes: self.planes(far).map(|(normal, offset)| {
                let world =
                    std::array::from_fn(|k| (0..3).map(|i| normal[i] * self.axes[i][k]).sum());
                (world, offset)
            }),
        }
    }

    fn project(&self, vertex: Vertex) -> ScreenVertex {
        let inv_z = 1.0 / vertex.position[2];
        ScreenVertex {
            xy: [
                (1.0 + vertex.position[0] * inv_z / self.scale[0]) * self.size[0] * 0.5,
                (1.0 - vertex.position[1] * inv_z / self.scale[1]) * self.size[1] * 0.5,
            ],
            inv_z,
            uv_over_z: vertex.uv.map(|v| v * inv_z),
            color_over_z: vertex.color.map(|v| v * inv_z),
        }
    }
}

#[derive(Clone, Copy)]
struct Vertex {
    position: V3,
    uv: [f64; 2],
    /// 逐顶点颜色（明暗、遮蔽与光照贴图已乘入），透视正确地插值。
    color: V3,
}

/// Sutherland-Hodgman in camera space, interpolating UV before projection.
fn clip(input: &[Vertex], output: &mut Vec<Vertex>, normal: V3, offset: f64) {
    output.clear();
    let Some(&last) = input.last() else { return };
    let mut previous = last;
    let mut a = dot(previous.position, normal) + offset;
    for &current in input {
        let b = dot(current.position, normal) + offset;
        if (a >= 0.0) != (b >= 0.0) {
            let t = a / (a - b);
            output.push(Vertex {
                position: std::array::from_fn(|i| {
                    previous.position[i] + t * (current.position[i] - previous.position[i])
                }),
                uv: std::array::from_fn(|i| previous.uv[i] + t * (current.uv[i] - previous.uv[i])),
                color: std::array::from_fn(|i| {
                    previous.color[i] + t * (current.color[i] - previous.color[i])
                }),
            });
        }
        if b >= 0.0 {
            output.push(current);
        }
        previous = current;
        a = b;
    }
}

#[derive(Clone, Copy)]
struct ScreenVertex {
    xy: [f64; 2],
    inv_z: f64,
    uv_over_z: [f64; 2],
    color_over_z: V3,
}

struct Projected<'a> {
    /// 最近顶点的 1/z，排序键（大的在前）。
    nearest: f64,
    vertices: [ScreenVertex; 3],
    area: f64,
    bounds: [usize; 4], // x0, y0, x1, y1 (exclusive)
    triangle: &'a Triangle,
}

fn edge(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    // Reversing an edge produces exactly opposite coefficients, so adjacent
    // triangles agree on coverage even when the pixel lies on their diagonal.
    (a[1] - b[1]) * p[0] + (b[0] - a[0]) * p[1] + (a[0] * b[1] - a[1] * b[0])
}

fn top_left(a: [f64; 2], b: [f64; 2]) -> bool {
    b[1] < a[1] || (b[1] == a[1] && b[0] > a[0])
}

/// 第 `y` 行里被三角形覆盖的像素 `[from, to)`（在 `x0..x1` 之内）。
///
/// 同一行上 [`edge`] 是「常数 × 横坐标 + 常数」，浮点乘、加的舍入都是单调的，所以每条边
/// 的内侧判定沿 x 至多翻转一次，三条边的交是一个区间。先按解析解估出端点，再用与逐像素
/// 测试完全相同的判定校正，得到的像素集合与逐个测试相同，只是不再测区间外的像素。
fn row_span(
    pairs: &[([f64; 2], [f64; 2]); 3],
    inclusive: [bool; 3],
    y: usize,
    [x0, x1]: [usize; 2],
    probes: &mut u64,
) -> [usize; 2] {
    let py = y as f64 + 0.5;
    let mut inside = |k: usize, x: usize| {
        *probes += 1;
        let (a, b) = pairs[k];
        let e = edge(a, b, [x as f64 + 0.5, py]);
        e > 0.0 || (e == 0.0 && inclusive[k])
    };
    let (mut from, mut to) = (x0, x1);
    for (k, (a, b)) in pairs.iter().enumerate() {
        if from >= to {
            break;
        }
        let slope = a[1] - b[1];
        if slope == 0.0 {
            // 水平边：整行同一个判定。
            if !inside(k, from) {
                return [from, from];
            }
            continue;
        }
        // 像素中心正好落在边上的横坐标，只作起点；端点以下面的逐像素判定为准。
        let constant = (b[0] - a[0]) * py + (a[0] * b[1] - a[1] * b[0]);
        let crossing = -constant / slope - 0.5;
        let mut x = if crossing.is_finite() {
            crossing.ceil().clamp(from as f64, to as f64) as usize
        } else {
            from
        };
        if slope > 0.0 {
            // 内侧在右：找第一个在内侧的像素。
            while x > from && inside(k, x - 1) {
                x -= 1;
            }
            while x < to && !inside(k, x) {
                x += 1;
            }
            from = x;
        } else {
            // 内侧在左：找第一个不在内侧的像素。
            while x > from && !inside(k, x - 1) {
                x -= 1;
            }
            while x < to && inside(k, x) {
                x += 1;
            }
            to = x;
        }
    }
    [from, to.max(from)]
}

/// 投影、裁剪所有三角形，每个分段一个线程；结果按段留着，不拼成一个大数组。
fn project<'a>(
    parts: &'a [Vec<Triangle>],
    camera: &Camera,
    projection: &Projection,
    far: f64,
) -> Vec<Vec<Projected<'a>>> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .iter()
            .map(|part| {
                scope.spawn(move || {
                    let projected = project_part(part, camera, projection, far);
                    crate::counters::flush();
                    projected
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("投影线程不 panic"))
            .collect()
    })
}

/// 从近到远的绘制顺序：近的先画，深度测试能早早挡掉后面的片元（正确性不依赖顺序）。
/// 只排「深度键 + 序号」的小对，不搬 200 多字节的投影记录；同键按原顺序。
fn front_to_back<'p, 'a>(parts: &'p [Vec<Projected<'a>>]) -> Vec<&'p Projected<'a>> {
    let all: Vec<&Projected<'a>> = parts.iter().flatten().collect();
    let mut keys: Vec<(f64, u32)> = all
        .iter()
        .enumerate()
        .map(|(i, t)| (t.nearest, i as u32))
        .collect();
    keys.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    keys.into_iter().map(|(_, i)| all[i as usize]).collect()
}

fn project_part<'a>(
    triangles: &'a [Triangle],
    camera: &Camera,
    projection: &Projection,
    far: f64,
) -> Vec<Projected<'a>> {
    // 多数三角形投影出一个，被裁的出零个；先留够。
    let mut result = Vec::with_capacity(triangles.len());
    let mut polygon = Vec::with_capacity(12);
    let mut scratch = Vec::with_capacity(12);
    let planes = projection.planes(far);
    let mut dropped = 0;
    for triangle in triangles {
        polygon.clear();
        polygon.extend((0..3).map(|i| {
            projection.vertex(
                triangle.vertices[i],
                triangle.uv[i],
                triangle.color[i],
                camera.eye,
            )
        }));
        if polygon
            .iter()
            .any(|v| !v.position.iter().all(|n| n.is_finite()))
        {
            continue;
        }
        for (normal, offset) in planes {
            clip(&polygon, &mut scratch, normal, offset);
            std::mem::swap(&mut polygon, &mut scratch);
            if polygon.is_empty() {
                break;
            }
        }
        if polygon.is_empty() {
            dropped += 1;
        }
        for i in 1..polygon.len().saturating_sub(1) {
            let mut vertices =
                [polygon[0], polygon[i], polygon[i + 1]].map(|v| projection.project(v));
            let mut area = edge(vertices[0].xy, vertices[1].xy, vertices[2].xy);
            if area.abs() < 1e-12 {
                continue;
            }
            // 光栅器两种绕序都画：方块与方块实体的背面已在建几何时按原版剔掉，
            // 留下的生物（entityCutoutNoCull）与流体本就双面。
            if area < 0.0 {
                vertices.swap(1, 2);
                area = -area;
            }
            let min: [f64; 2] = std::array::from_fn(|axis| {
                vertices
                    .iter()
                    .map(|v| v.xy[axis])
                    .fold(f64::INFINITY, f64::min)
            });
            let max: [f64; 2] = std::array::from_fn(|axis| {
                vertices
                    .iter()
                    .map(|v| v.xy[axis])
                    .fold(f64::NEG_INFINITY, f64::max)
            });
            let bounds = [
                (min[0] - 0.5).ceil().max(0.0) as usize,
                (min[1] - 0.5).ceil().max(0.0) as usize,
                ((max[0] - 0.5).floor() + 1.0).clamp(0.0, projection.size[0]) as usize,
                ((max[1] - 0.5).floor() + 1.0).clamp(0.0, projection.size[1]) as usize,
            ];
            if bounds[0] < bounds[2] && bounds[1] < bounds[3] {
                result.push(Projected {
                    nearest: vertices.iter().map(|v| v.inv_z).fold(0.0, f64::max),
                    vertices,
                    area,
                    bounds,
                    triangle,
                });
            }
        }
    }
    use crate::counters::{add, Counter};
    add(Counter::TrianglesProjected, triangles.len() as u64);
    add(Counter::TrianglesDropped, dropped);
    add(Counter::TrianglesOut, result.len() as u64);
    result
}

#[derive(Clone, Copy)]
struct Fragment {
    z: f64,
    color: [f64; 4],
}

/// 一个像素除深度外的状态。最近不透明面的深度单独放在一个连续数组里：深度测试是
/// 内层循环里最频繁的访存，只碰 8 字节比拉一整个 `Pixel` 省缓存。
/// 背景（天空或雾色）不另存，收尾时从天空底图读。
struct Pixel {
    color: [f64; 3],
    layers: Vec<Fragment>,
}

/// 条带的工作区：每个线程一份，在条带之间复用（透明层的 `Vec` 也保留容量），
/// 不必每条带重新分配。
#[derive(Default)]
struct BandScratch {
    pixels: Vec<Pixel>,
    depths: Vec<f64>,
}

impl Pixel {
    /// `depth` 是这个像素最近不透明面的深度。
    fn insert(&mut self, depth: &mut f64, fragment: Fragment) {
        if fragment.color[3] >= 1.0 {
            *depth = fragment.z;
            self.color = [fragment.color[0], fragment.color[1], fragment.color[2]];
            return;
        }
        let index = self.layers.partition_point(|f| f.z < fragment.z);
        // A shared edge or coplanar surface must not blend twice.
        if self
            .layers
            .get(index)
            .is_some_and(|f| (f.z - fragment.z).abs() < 1e-9)
            || index
                .checked_sub(1)
                .is_some_and(|i| (self.layers[i].z - fragment.z).abs() < 1e-9)
        {
            return;
        }
        if index < MAX_LAYERS {
            if self.layers.len() == MAX_LAYERS {
                self.layers.pop();
            }
            self.layers.insert(index, fragment);
        }
    }

    fn finish(&self, depth: f64, background: [f64; 3]) -> [u8; 4] {
        let mut color = [0.0; 3];
        let mut remaining = 1.0;
        let mut count = 0;
        for fragment in self.layers.iter().take_while(|f| f.z < depth) {
            for (i, channel) in color.iter_mut().enumerate() {
                *channel += remaining * fragment.color[3] * fragment.color[i];
            }
            remaining *= 1.0 - fragment.color[3];
            count += 1;
            if remaining < 0.005 {
                break;
            }
        }
        // 透明层超过上限时深处已丢，按看不穿处理成背景（天空/雾）。
        let background = if count < MAX_LAYERS && remaining >= 0.005 {
            self.color
        } else {
            background
        };
        [
            (color[0] + remaining * background[0]).clamp(0.0, 255.0) as u8,
            (color[1] + remaining * background[1]).clamp(0.0, 255.0) as u8,
            (color[2] + remaining * background[2]).clamp(0.0, 255.0) as u8,
            255,
        ]
    }
}

/// 天空用的整幅光栅化：一个凸多边形（相对眼睛的世界坐标、uv、颜色），逐个覆盖到的像素
/// 回调（行优先像素下标, 透视校正的 uv, 颜色）。不做深度测试，画的先后就是遮挡关系。
pub(crate) fn rasterize(
    projection: &Projection,
    corners: &[(V3, [f64; 2], V3)],
    mut paint: impl FnMut(usize, [f64; 2], V3),
) {
    let mut polygon: Vec<Vertex> = corners
        .iter()
        .map(|&(position, uv, color)| projection.vertex(position, uv, color, [0.0; 3]))
        .collect();
    let mut scratch = Vec::with_capacity(12);
    for (normal, offset) in projection.planes(f64::MAX) {
        clip(&polygon, &mut scratch, normal, offset);
        std::mem::swap(&mut polygon, &mut scratch);
        if polygon.is_empty() {
            return;
        }
    }
    let width = projection.size[0] as usize;
    for i in 1..polygon.len().saturating_sub(1) {
        let mut v = [polygon[0], polygon[i], polygon[i + 1]].map(|v| projection.project(v));
        let mut area = edge(v[0].xy, v[1].xy, v[2].xy);
        if area.abs() < 1e-12 {
            continue;
        }
        if area < 0.0 {
            v.swap(1, 2);
            area = -area;
        }
        let min = [0, 1].map(|a| v.iter().map(|p| p.xy[a]).fold(f64::INFINITY, f64::min));
        let max = [0, 1].map(|a| v.iter().map(|p| p.xy[a]).fold(f64::NEG_INFINITY, f64::max));
        let x0 = (min[0] - 0.5).ceil().max(0.0) as usize;
        let y0 = (min[1] - 0.5).ceil().max(0.0) as usize;
        let x1 = ((max[0] - 0.5).floor() + 1.0).clamp(0.0, projection.size[0]) as usize;
        let y1 = ((max[1] - 0.5).floor() + 1.0).clamp(0.0, projection.size[1]) as usize;
        let pairs = [(v[1].xy, v[2].xy), (v[2].xy, v[0].xy), (v[0].xy, v[1].xy)];
        let inclusive = pairs.map(|(a, b)| top_left(a, b));
        for y in y0..y1 {
            for x in x0..x1 {
                let p = [x as f64 + 0.5, y as f64 + 0.5];
                let e = pairs.map(|(a, b)| edge(a, b, p));
                if (0..3).any(|k| e[k] < 0.0 || (e[k] == 0.0 && !inclusive[k])) {
                    continue;
                }
                let w = e.map(|value| value / area);
                let z = 1.0 / (w[0] * v[0].inv_z + w[1] * v[1].inv_z + w[2] * v[2].inv_z);
                let uv = std::array::from_fn(|k| {
                    (w[0] * v[0].uv_over_z[k] + w[1] * v[1].uv_over_z[k] + w[2] * v[2].uv_over_z[k])
                        * z
                });
                let color = std::array::from_fn(|k| {
                    (w[0] * v[0].color_over_z[k]
                        + w[1] * v[1].color_over_z[k]
                        + w[2] * v[2].color_over_z[k])
                        * z
                });
                paint(y * width + x, uv, color);
            }
        }
    }
}

fn sample(triangle: &Triangle, uv: [f64; 2], tint: V3) -> [f64; 4] {
    let width = triangle.texture.width();
    let height = triangle.texture.height().min(width);
    let x = ((uv[0] / 16.0).clamp(0.0, 1.0) * f64::from(width)) as u32;
    let y = ((uv[1] / 16.0).clamp(0.0, 1.0) * f64::from(height)) as u32;
    let c = triangle
        .texture
        .get_pixel(x.min(width - 1), y.min(height - 1))
        .0;
    [
        f64::from(c[0]) * tint[0],
        f64::from(c[1]) * tint[1],
        f64::from(c[2]) * tint[2],
        f64::from(c[3]) / 255.0 * triangle.alpha,
    ]
}

// `x` 同时是像素横坐标与下标，迭代器写法反而难读。
#[allow(clippy::needless_range_loop)]
#[allow(clippy::too_many_arguments)]
fn draw_band(
    scratch: &mut BandScratch,
    bin: &[&Projected<'_>],
    projection: &Projection,
    options: Options,
    sky: Option<&Sky>,
    background: Option<&[V3]>,
    y0: usize,
    output: &mut [u8],
) {
    let width = options.width as usize;
    let rows = output.len() / (width * 4);
    let background_at = |i: usize| background.map_or(SKY, |image| image[y0 * width + i]);
    let BandScratch { pixels, depths } = scratch;
    pixels.truncate(width * rows);
    for (i, pixel) in pixels.iter_mut().enumerate() {
        pixel.color = background_at(i);
        pixel.layers.clear();
    }
    for i in pixels.len()..width * rows {
        pixels.push(Pixel {
            color: background_at(i),
            layers: Vec::new(),
        });
    }
    depths.clear();
    depths.resize(width * rows, f64::INFINITY);
    // Preserve radial far distance, not a camera-Z-only far plane.
    let x_squared: Vec<_> = (0..width)
        .map(|x| {
            ((2.0 * (x as f64 + 0.5) / projection.size[0] - 1.0) * projection.scale[0]).powi(2)
        })
        .collect();
    let y_squared: Vec<_> = (y0..y0 + rows)
        .map(|y| {
            ((1.0 - 2.0 * (y as f64 + 0.5) / projection.size[1]) * projection.scale[1]).powi(2)
        })
        .collect();
    let [mut tests, mut probes, mut covered, mut passed, mut transparent, mut fogged] = [0u64; 6];
    for t in bin {
        let [x0, ty0, x1, ty1] = t.bounds;
        let first_y = ty0.max(y0);
        let last_y = ty1.min(y0 + rows);
        if first_y >= last_y {
            continue;
        }
        tests += ((last_y - first_y) * (x1 - x0)) as u64;
        let v = t.vertices;
        let pairs = [(v[1].xy, v[2].xy), (v[2].xy, v[0].xy), (v[0].xy, v[1].xy)];
        let inclusive = pairs.map(|(a, b)| top_left(a, b));
        let inverse_area = 1.0 / t.area;
        for y in first_y..last_y {
            let [from, to] = row_span(&pairs, inclusive, y, [x0, x1], &mut probes);
            for x in from..to {
                let p = [x as f64 + 0.5, y as f64 + 0.5];
                let e = pairs.map(|(a, b)| edge(a, b, p));
                debug_assert!((0..3).all(|i| e[i] > 0.0 || (e[i] == 0.0 && inclusive[i])));
                covered += 1;
                let weights = e.map(|value| value * inverse_area);
                let inv_z =
                    weights[0] * v[0].inv_z + weights[1] * v[1].inv_z + weights[2] * v[2].inv_z;
                let z = 1.0 / inv_z;
                let index = (y - y0) * width + x;
                if z >= depths[index]
                    || z * z * (1.0 + x_squared[x] + y_squared[y - y0]) >= options.far * options.far
                {
                    continue;
                }
                passed += 1;
                let uv = std::array::from_fn(|i| {
                    (weights[0] * v[0].uv_over_z[i]
                        + weights[1] * v[1].uv_over_z[i]
                        + weights[2] * v[2].uv_over_z[i])
                        * z
                });
                let tint = std::array::from_fn(|i| {
                    (weights[0] * v[0].color_over_z[i]
                        + weights[1] * v[1].color_over_z[i]
                        + weights[2] * v[2].color_over_z[i])
                        * z
                });
                let mut color = sample(t.triangle, uv, tint);
                // 镂空纹素先丢：雾不改 alpha，算了也白算。
                if color[3] < 0.01 {
                    transparent += 1;
                    continue;
                }
                if let Some(sky) = sky {
                    fogged += 1;
                    let direction = projection.direction(x, y);
                    color = sky.apply(color, direction.map(|v| v * z));
                }
                pixels[index].insert(&mut depths[index], Fragment { z, color });
            }
        }
    }
    use crate::counters::{add, Counter};
    add(Counter::CoverageTests, tests);
    add(Counter::SpanProbes, probes);
    add(Counter::Covered, covered);
    add(Counter::DepthPassed, passed);
    add(Counter::Transparent, transparent);
    add(Counter::FogApplied, fogged);
    for (i, ((pixel, depth), out)) in pixels
        .iter()
        .zip(depths.iter())
        .zip(output.as_chunks_mut::<4>().0)
        .enumerate()
    {
        *out = pixel.finish(*depth, background_at(i));
    }
}

/// `parts` 按顺序接起来是全部三角形（几何各线程的分段与实体）。
pub(crate) fn draw(
    parts: &[Vec<Triangle>],
    camera: &Camera,
    options: Options,
    sky: Option<&Sky>,
    report: &mut Report,
) -> RgbaImage {
    let mut clock = std::time::Instant::now();
    let projection = Projection::new(camera, options);
    let projected = project(parts, camera, &projection, options.far);
    report.stage("project, clip", &mut clock);
    let ordered = front_to_back(&projected);
    report.stage("sort", &mut clock);
    // 分箱：每个三角形按（已排好的）顺序登记到它覆盖的每个 16 行条带，条带只扫自己的箱。
    let band_count = (options.height as usize).div_ceil(BAND);
    let mut bins: Vec<Vec<&Projected<'_>>> = vec![Vec::new(); band_count];
    for t in ordered {
        for bin in &mut bins[t.bounds[1] / BAND..t.bounds[3].div_ceil(BAND)] {
            bin.push(t);
        }
    }
    report.stage("bin", &mut clock);
    let background = sky.map(|sky| sky.paint(&projection));
    report.stage("sky", &mut clock);
    let mut pixels = vec![0; options.width as usize * options.height as usize * 4];
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    // 条带按需领取：天空条带轻、地面条带重，固定分段会让最慢的线程决定墙钟时间。
    // 箱与条带一一对应；条带也限制了透明层的临时内存。
    let bands = std::sync::Mutex::new(
        pixels
            .chunks_mut(BAND * options.width as usize * 4)
            .enumerate(),
    );
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (projection, bins, bands) = (&projection, &bins, &bands);
            let background = background.as_deref();
            scope.spawn(move || {
                let mut scratch = BandScratch::default();
                loop {
                    let next = bands.lock().expect("条带领取不 panic").next();
                    let Some((band, output)) = next else {
                        break;
                    };
                    draw_band(
                        &mut scratch,
                        &bins[band],
                        projection,
                        options,
                        sky,
                        background,
                        band * BAND,
                        output,
                    );
                }
                crate::counters::flush();
            });
        }
    });
    report.stage("bands", &mut clock);
    RgbaImage::from_raw(options.width, options.height, pixels).expect("validated pixel dimensions")
}

/// 有环境时的远平面：视距雾按柱面距离算，最远的角落在斜上方；放宽到能覆盖整个视距立方体。
fn view_far(view_distance: u32) -> f64 {
    (f64::from(view_distance) * 16.0 * 1.8).max(1.0)
}

/// 成像会画的区块段（区块段坐标 = 方块坐标 >> 4）：与 [`render`] 剔除区块段是同一个视锥
/// （带环境、视距 `view_distance` 的场景）。采集方据此只拷贝视野里的方块，判据外的区块段交了也不画。
pub fn section_filter(
    camera: &Camera,
    options: Options,
    view_distance: u32,
) -> impl Fn([i32; 3]) -> bool + Send + Sync {
    let frustum = Projection::new(camera, options).frustum(camera.eye, view_far(view_distance));
    move |section| frustum.section_visible(section)
}

pub fn render(scene: &Scene, resources: &mut Resources, options: Options) -> Result<Frame, String> {
    let mut options = options;
    if let Some(environment) = &scene.environment {
        options.far = view_far(environment.view_distance);
    }
    if scene.game_version != resources.version() {
        return Err(format!(
            "scene version {} does not match client resources {}",
            scene.game_version,
            resources.version()
        ));
    }
    if !(1..=2048).contains(&options.width)
        || !(1..=2048).contains(&options.height)
        || !options.far.is_finite()
        || !(1.0..=1024.0).contains(&options.far)
        || !scene.camera.eye.iter().all(|n| n.is_finite())
        || !scene.camera.yaw.is_finite()
        || !scene.camera.pitch.is_finite()
        || !(-90.0..=90.0).contains(&scene.camera.pitch)
        || !scene.camera.vertical_fov.is_finite()
        || !(10.0..=150.0).contains(&scene.camera.vertical_fov)
        || scene.blocks.len() > 8_000_000
    {
        return Err(
            "invalid camera, image size (1..2048), distance (1..1024), or block count (>8000000)"
                .to_owned(),
        );
    }
    if let Some([x, y, width, height]) = options.crop {
        if width == 0
            || height == 0
            || u64::from(x) + u64::from(width) > u64::from(options.width)
            || u64::from(y) + u64::from(height) > u64::from(options.height)
        {
            return Err("crop must be a non-empty rectangle inside the image".to_owned());
        }
    }
    let mut report = Report {
        blocks: scene.blocks.len(),
        ..Report::default()
    };
    crate::counters::take();
    report.warnings.insert(if scene.environment.is_some() {
        "prototype: overworld only, no clouds, weather, block-entity contents or block-light flicker; HUD is only the crosshair"
    } else {
        "prototype: fixed daylight/background, no fog, no server lighting, block-entity contents, weather; HUD is only the crosshair"
    }.to_owned());
    report
        .warnings
        .insert("transparency is composited through at most 16 surfaces".to_owned());
    let mut clock = std::time::Instant::now();
    // 有环境才有光照与昼夜；没有时（模型夹具、测试）满亮度、固定背景。
    let biomes = match Biomes::new(scene, resources, &mut report) {
        Ok(biomes) => Some(biomes),
        Err(error) => {
            report.warnings.insert(format!(
                "biome tint unavailable ({error}); tinted faces drawn white"
            ));
            None
        }
    };
    let lighting = match &scene.environment {
        Some(environment) => {
            let layer = biomes
                .as_ref()
                .map(|biomes| biomes.camera_weights(scene.camera.eye))
                .unwrap_or_default();
            let daylight = Daylight::evaluate(resources, environment.clock_ticks, &layer)?;
            let lightmap = Lightmap::new(&LightmapInputs {
                sky_factor: daylight.sky_light_factor,
                // LightmapRenderStateExtractor：1.4 加方块光闪烁（这里不模拟闪烁）。
                block_factor: 1.4,
                block_light_tint: daylight.block_light_tint,
                sky_light_color: daylight.sky_light_color,
                ambient_color: daylight.ambient_light_color,
                // 亮度选项默认值（「亮度：中」）。
                brightness: 0.5,
            });
            let sky = Sky::new(environment, &scene.camera, daylight, resources, &mut report);
            Some((Cells::new(&scene.cells), lightmap, sky))
        }
        None => None,
    };
    report.stage("biomes, daylight, sky, light cells", &mut clock);
    let cells = lighting.as_ref().map(|(cells, _, _)| cells);
    let frustum = Projection::new(&scene.camera, options).frustum(scene.camera.eye, options.far);
    let mut parts = build_lit(
        scene,
        cells,
        biomes.as_ref(),
        Some(&frustum),
        Some(scene.camera.eye),
        resources,
        &mut report,
    );
    clock = std::time::Instant::now();
    if !scene.entities.is_empty() {
        parts.push(crate::entity::build(
            &scene.entities,
            scene.camera.eye,
            cells,
            resources,
            &mut report,
        ));
    }
    // 原版逐顶点乘光照贴图（`vertexColor = Color * sample_lightmap(UV2)`）；各分段并行。
    if let Some((_, lightmap, _)) = &lighting {
        std::thread::scope(|scope| {
            for part in &mut parts {
                scope.spawn(move || {
                    for triangle in part {
                        for i in 0..3 {
                            let light = lightmap.sample(triangle.light[i]);
                            triangle.color[i] =
                                std::array::from_fn(|k| triangle.color[i][k] * light[k]);
                        }
                    }
                });
            }
        });
    }
    report.stage("entities, lightmap", &mut clock);
    report.triangles = parts.iter().map(Vec::len).sum();
    let sky = lighting.as_ref().map(|(_, _, sky)| sky);
    let mut image = draw(&parts, &scene.camera, options, sky, &mut report);
    clock = std::time::Instant::now();
    if options.crosshair {
        draw_crosshair(&mut image, resources, &mut report);
    }
    if let Some([x, y, width, height]) = options.crop {
        image = image::imageops::crop_imm(&image, x, y, width, height).to_image();
    }
    report.stage("crosshair, crop", &mut clock);
    report.counters = crate::counters::take();
    Ok(Frame { image, report })
}

/// 原版自动 GUI 缩放（`Window.calculateScale(0, false)`）：最大的整数倍，使屏幕除以它后
/// 仍不小于 320×240。
pub(crate) fn gui_scale(width: u32, height: u32) -> u32 {
    let mut scale = 1;
    while scale < width
        && scale < height
        && width / (scale + 1) >= 320
        && height / (scale + 1) >= 240
    {
        scale += 1;
    }
    scale
}

/// 准星：原版 `hud/crosshair` 精灵，按原版混合（`ONE_MINUS_DST_COLOR`，即对底色取反）
/// 画在画面正中：GUI 坐标 `((guiWidth - 15) / 2, (guiHeight - 15) / 2)`，每个纹素放大成
/// GUI 缩放倍数的方块（`Gui.extractCrosshair`）。
/// 资源里没有这张精灵时退回一个 9 像素的取反十字，并在报告里说明。
fn draw_crosshair(image: &mut RgbaImage, resources: &mut Resources, report: &mut Report) {
    let sprite = resources
        .texture("minecraft:gui/sprites/hud/crosshair")
        .ok();
    if sprite.is_none() {
        report
            .warnings
            .insert("crosshair sprite missing; drew a plain inverted cross".to_owned());
    }
    let (width, height) = sprite
        .as_ref()
        .map_or((9, 9), |sprite| (sprite.width(), sprite.height()));
    let scale = gui_scale(image.width(), image.height());
    // GUI 尺寸向上取整（`Window.setGuiScale`），位置是 GUI 坐标里的整数除再乘回像素。
    let gui = |pixels: u32| pixels.div_ceil(scale);
    let left = i64::from(gui(image.width()).saturating_sub(width) / 2 * scale);
    let top = i64::from(gui(image.height()).saturating_sub(height) / 2 * scale);
    for y in 0..height {
        for x in 0..width {
            let coverage = match &sprite {
                Some(sprite) => f64::from(sprite.get_pixel(x, y).0[3]) / 255.0,
                None => f64::from(u8::from(x == width / 2 || y == height / 2)),
            };
            if coverage <= 0.0 {
                continue;
            }
            for (dx, dy) in (0..scale).flat_map(|dx| (0..scale).map(move |dy| (dx, dy))) {
                let px = left + i64::from(x * scale + dx);
                let py = top + i64::from(y * scale + dy);
                if px < 0
                    || py < 0
                    || px >= i64::from(image.width())
                    || py >= i64::from(image.height())
                {
                    continue;
                }
                let pixel = image.get_pixel_mut(px as u32, py as u32);
                for channel in 0..3 {
                    let dst = f64::from(pixel.0[channel]);
                    pixel.0[channel] = (dst + coverage * (255.0 - 2.0 * dst)).round() as u8;
                }
            }
        }
    }
}
