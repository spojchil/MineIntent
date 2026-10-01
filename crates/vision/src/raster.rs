//! CPU rasterization into an image: no surface, window or graphics device.
use image::RgbaImage;

use crate::geometry::{build, dot, sub, Triangle, V3};
use crate::{Camera, Environment, Frame, Options, Report, Resources, Scene};

const NEAR: f64 = 1e-4;
const SKY: [f64; 3] = [150.0, 185.0, 215.0];
const MAX_LAYERS: usize = 16;

/// 原版的视距雾与天空：`FogRenderer.setupFog`、`AtmosphericFogEnvironment`、`SkyRenderer`
/// 与 `fog.glsl`（26.1.2 客户端）。颜色暂取主世界白天的值，见 [`Environment`]。
pub(crate) struct Sky {
    fog_color: [f64; 3],
    sky_color: [f64; 3],
    render_start: f64,
    render_end: f64,
    sky_end: f64,
    /// 眼睛低于地平线：天空下半是黑盘（`SkyRenderer.shouldRenderDarkDisc`）。
    dark_disc: bool,
}

/// `dimension_type/overworld` 的 `visual/sky_color` 与 `visual/fog_color`。
const OVERWORLD_SKY: [f64; 3] = [120.0, 167.0, 255.0];
const OVERWORLD_FOG: [f64; 3] = [192.0, 216.0, 255.0];
/// `EnvironmentAttributes` 默认值：环境雾 0–1024 格、天空雾止于 512 格。
const ENVIRONMENTAL_FOG_END: f64 = 1024.0;
const SKY_FOG_END: f64 = 512.0;
/// 天空盘半径（`SkyRenderer.SKY_DISC_RADIUS`）与高度；黑盘在眼下 16-12 格。
const SKY_DISC_RADIUS: f64 = 512.0;
const SKY_DISC_HEIGHT: f64 = 16.0;
const DARK_DISC_DEPTH: f64 = 4.0;

impl Sky {
    pub(crate) fn new(environment: &Environment, eye_y: f64) -> Self {
        let render_distance = f64::from(environment.view_distance) * 16.0;
        // AtmosphericFogEnvironment.getBaseColor：雾色按视距朝天空色偏。
        let sky_fog_end_chunks = (SKY_FOG_END / 16.0).min(f64::from(environment.view_distance));
        let factor = 0.25 + 0.75 * (sky_fog_end_chunks / 32.0).clamp(0.0, 1.0);
        let mix = 1.0 - factor.powf(0.25);
        let fog_color = std::array::from_fn(|i| {
            (OVERWORLD_FOG[i] + mix * (OVERWORLD_SKY[i] - OVERWORLD_FOG[i])).trunc()
        });
        let span = (render_distance / 10.0).clamp(4.0, 64.0);
        Self {
            fog_color,
            sky_color: OVERWORLD_SKY,
            render_start: render_distance - span,
            render_end: render_distance,
            sky_end: render_distance.min(SKY_FOG_END),
            dark_disc: eye_y < environment.horizon_height,
        }
    }

    /// `fog.glsl` 的 `total_fog_value`：球面距离走环境雾，柱面距离走视距雾，取大。
    fn fog(&self, relative: V3) -> f64 {
        let spherical = dot(relative, relative).sqrt();
        let cylindrical = relative[0].hypot(relative[2]).max(relative[1].abs());
        linear_fog(spherical, 0.0, ENVIRONMENTAL_FOG_END).max(linear_fog(
            cylindrical,
            self.render_start,
            self.render_end,
        ))
    }

    fn apply(&self, color: [f64; 4], relative: V3) -> [f64; 4] {
        let fog = self.fog(relative);
        if fog <= 0.0 {
            return color;
        }
        [
            color[0] + fog * (self.fog_color[0] - color[0]),
            color[1] + fog * (self.fog_color[1] - color[1]),
            color[2] + fog * (self.fog_color[2] - color[2]),
            color[3],
        ]
    }

    /// 没有地形挡住时看到的颜色：清屏色是雾色，上面画天空盘，眼低于地平线时下面画黑盘；
    /// 两张盘都按 `sky.fsh` 的天空雾渐变到雾色。
    fn background(&self, direction: V3) -> [f64; 3] {
        let (plane, base) = if direction[1] > 0.0 {
            (SKY_DISC_HEIGHT, self.sky_color)
        } else if direction[1] < 0.0 && self.dark_disc {
            (-DARK_DISC_DEPTH, [0.0; 3])
        } else {
            return self.fog_color;
        };
        let hit = direction.map(|v| v * plane / direction[1]);
        let radius = hit[0].hypot(hit[2]);
        if radius > SKY_DISC_RADIUS {
            return self.fog_color;
        }
        let spherical = dot(hit, hit).sqrt();
        let cylindrical = radius.max(plane.abs());
        let fog = linear_fog(spherical, 0.0, self.sky_end).max(linear_fog(
            cylindrical,
            self.sky_end,
            self.sky_end,
        ));
        std::array::from_fn(|i| base[i] + fog * (self.fog_color[i] - base[i]))
    }
}

fn linear_fog(distance: f64, start: f64, end: f64) -> f64 {
    if distance <= start {
        0.0
    } else if distance >= end {
        1.0
    } else {
        (distance - start) / (end - start)
    }
}

struct Projection {
    axes: [V3; 3], // right, up, forward
    scale: [f64; 2],
    size: [f64; 2],
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

    fn vertex(&self, position: V3, uv: [f64; 2], eye: V3) -> Vertex {
        let relative = sub(position, eye);
        Vertex {
            position: self.axes.map(|axis| dot(relative, axis)),
            uv,
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
        }
    }
}

#[derive(Clone, Copy)]
struct Vertex {
    position: V3,
    uv: [f64; 2],
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
}

struct Projected<'a> {
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

fn project<'a>(
    triangles: &'a [Triangle],
    camera: &Camera,
    projection: &Projection,
    far: f64,
) -> Vec<Projected<'a>> {
    let mut result = Vec::new();
    let mut polygon = Vec::with_capacity(12);
    let mut scratch = Vec::with_capacity(12);
    let [sx, sy] = projection.scale;
    // Side clipping avoids huge projected coordinates for triangles crossing the eye.
    let planes = [
        ([0.0, 0.0, 1.0], -NEAR),
        ([0.0, 0.0, -1.0], far),
        ([1.0, 0.0, sx], 0.0),
        ([-1.0, 0.0, sx], 0.0),
        ([0.0, 1.0, sy], 0.0),
        ([0.0, -1.0, sy], 0.0),
    ];
    for triangle in triangles {
        polygon.clear();
        polygon.extend(
            (0..3).map(|i| projection.vertex(triangle.vertices[i], triangle.uv[i], camera.eye)),
        );
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
        for i in 1..polygon.len().saturating_sub(1) {
            let mut vertices =
                [polygon[0], polygon[i], polygon[i + 1]].map(|v| projection.project(v));
            let mut area = edge(vertices[0].xy, vertices[1].xy, vertices[2].xy);
            if area.abs() < 1e-12 {
                continue;
            }
            // Both sides are visible, matching the prototype's model semantics.
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
                    vertices,
                    area,
                    bounds,
                    triangle,
                });
            }
        }
    }
    // Front-to-back improves early depth rejection; correctness does not require sorting.
    result.sort_unstable_by(|a, b| {
        let nearest = |t: &Projected<'_>| t.vertices.iter().map(|v| v.inv_z).fold(0.0, f64::max);
        nearest(b).total_cmp(&nearest(a))
    });
    result
}

#[derive(Clone, Copy)]
struct Fragment {
    z: f64,
    color: [f64; 4],
}

struct Pixel {
    z: f64,
    color: [f64; 3],
    background: [f64; 3],
    layers: Vec<Fragment>,
}

impl Pixel {
    fn insert(&mut self, fragment: Fragment) {
        if fragment.color[3] >= 1.0 {
            self.z = fragment.z;
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

    fn finish(&self) -> [u8; 4] {
        let mut color = [0.0; 3];
        let mut remaining = 1.0;
        let mut count = 0;
        for fragment in self.layers.iter().take_while(|f| f.z < self.z) {
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
            self.background
        };
        [
            (color[0] + remaining * background[0]).clamp(0.0, 255.0) as u8,
            (color[1] + remaining * background[1]).clamp(0.0, 255.0) as u8,
            (color[2] + remaining * background[2]).clamp(0.0, 255.0) as u8,
            255,
        ]
    }
}

fn sample(triangle: &Triangle, uv: [f64; 2]) -> [f64; 4] {
    let width = triangle.texture.width();
    let height = triangle.texture.height().min(width);
    let x = ((uv[0] / 16.0).clamp(0.0, 1.0) * f64::from(width)) as u32;
    let y = ((uv[1] / 16.0).clamp(0.0, 1.0) * f64::from(height)) as u32;
    let c = triangle
        .texture
        .get_pixel(x.min(width - 1), y.min(height - 1))
        .0;
    [
        f64::from(c[0]) * triangle.color[0],
        f64::from(c[1]) * triangle.color[1],
        f64::from(c[2]) * triangle.color[2],
        f64::from(c[3]) / 255.0 * triangle.alpha,
    ]
}

fn draw_band(
    projected: &[Projected<'_>],
    projection: &Projection,
    options: Options,
    sky: Option<&Sky>,
    y0: usize,
    output: &mut [u8],
) {
    let width = options.width as usize;
    let rows = output.len() / (width * 4);
    // 每个像素的视线方向（世界坐标，前向分量为 1）：片元位置 = 深度 × 方向。
    let directions: Vec<V3> = (0..width * rows)
        .map(|i| {
            let (x, y) = (i % width, y0 + i / width);
            let right = (2.0 * (x as f64 + 0.5) / projection.size[0] - 1.0) * projection.scale[0];
            let up = (1.0 - 2.0 * (y as f64 + 0.5) / projection.size[1]) * projection.scale[1];
            std::array::from_fn(|k| {
                right * projection.axes[0][k] + up * projection.axes[1][k] + projection.axes[2][k]
            })
        })
        .collect();
    let mut pixels: Vec<_> = directions
        .iter()
        .map(|&direction| {
            let background = sky.map_or(SKY, |sky| sky.background(direction));
            Pixel {
                z: f64::INFINITY,
                color: background,
                background,
                layers: Vec::new(),
            }
        })
        .collect();
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
    for t in projected {
        let [x0, ty0, x1, ty1] = t.bounds;
        let first_y = ty0.max(y0);
        let last_y = ty1.min(y0 + rows);
        if first_y >= last_y {
            continue;
        }
        let v = t.vertices;
        let pairs = [(v[1].xy, v[2].xy), (v[2].xy, v[0].xy), (v[0].xy, v[1].xy)];
        let inclusive = pairs.map(|(a, b)| top_left(a, b));
        let inverse_area = 1.0 / t.area;
        for y in first_y..last_y {
            for x in x0..x1 {
                let p = [x as f64 + 0.5, y as f64 + 0.5];
                let e = pairs.map(|(a, b)| edge(a, b, p));
                if (0..3).any(|i| e[i] < 0.0 || (e[i] == 0.0 && !inclusive[i])) {
                    continue;
                }
                let weights = e.map(|value| value * inverse_area);
                let inv_z =
                    weights[0] * v[0].inv_z + weights[1] * v[1].inv_z + weights[2] * v[2].inv_z;
                let z = 1.0 / inv_z;
                let pixel = &mut pixels[(y - y0) * width + x];
                if z >= pixel.z
                    || z * z * (1.0 + x_squared[x] + y_squared[y - y0]) >= options.far * options.far
                {
                    continue;
                }
                let uv = std::array::from_fn(|i| {
                    (weights[0] * v[0].uv_over_z[i]
                        + weights[1] * v[1].uv_over_z[i]
                        + weights[2] * v[2].uv_over_z[i])
                        * z
                });
                let mut color = sample(t.triangle, uv);
                if let Some(sky) = sky {
                    let direction = directions[(y - y0) * width + x];
                    color = sky.apply(color, direction.map(|v| v * z));
                }
                if color[3] >= 0.01 {
                    pixel.insert(Fragment { z, color });
                }
            }
        }
    }
    for (pixel, out) in pixels.iter().zip(output.as_chunks_mut::<4>().0) {
        *out = pixel.finish();
    }
}

pub(crate) fn draw(
    triangles: &[Triangle],
    camera: &Camera,
    options: Options,
    sky: Option<&Sky>,
) -> RgbaImage {
    let projection = Projection::new(camera, options);
    let projected = project(triangles, camera, &projection, options.far);
    let mut pixels = vec![0; options.width as usize * options.height as usize * 4];
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let rows = (options.height as usize).div_ceil(workers);
    std::thread::scope(|scope| {
        for (i, output) in pixels
            .chunks_mut(rows * options.width as usize * 4)
            .enumerate()
        {
            let projection = &projection;
            let projected = &projected;
            scope.spawn(move || {
                // Limit transparency scratch memory even for large images.
                for (band, output) in output
                    .chunks_mut(16 * options.width as usize * 4)
                    .enumerate()
                {
                    draw_band(
                        projected,
                        projection,
                        options,
                        sky,
                        i * rows + band * 16,
                        output,
                    );
                }
            });
        }
    });
    RgbaImage::from_raw(options.width, options.height, pixels).expect("validated pixel dimensions")
}

pub fn render(scene: &Scene, resources: &mut Resources, options: Options) -> Result<Frame, String> {
    let mut options = options;
    let sky = scene
        .environment
        .as_ref()
        .map(|environment| Sky::new(environment, scene.camera.eye[1]));
    if let Some(environment) = &scene.environment {
        // 视距雾按柱面距离算，最远的角落在斜上方；放宽到能覆盖整个视距立方体。
        options.far = (f64::from(environment.view_distance) * 16.0 * 1.8).max(1.0);
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
    let mut report = Report {
        blocks: scene.blocks.len(),
        ..Report::default()
    };
    report.warnings.insert(if scene.environment.is_some() {
        "prototype: view-distance fog and sky use overworld daytime colours; no server lighting, sun/moon/stars/clouds, block-entity contents, weather; HUD is only the crosshair"
    } else {
        "prototype: fixed daylight/background, no fog, no server lighting, block-entity contents, weather; HUD is only the crosshair"
    }.to_owned());
    report
        .warnings
        .insert("transparency is composited through at most 16 surfaces".to_owned());
    let mut triangles = build(scene, resources, &mut report);
    if !scene.entities.is_empty() {
        triangles.extend(crate::entity::build(
            &scene.entities,
            scene.camera.eye,
            resources,
            &mut report,
        ));
    }
    report.triangles = triangles.len();
    let mut image = draw(&triangles, &scene.camera, options, sky.as_ref());
    if options.crosshair {
        draw_crosshair(&mut image, resources, &mut report);
    }
    Ok(Frame { image, report })
}

/// 准星：原版 `hud/crosshair` 精灵，按原版混合（`ONE_MINUS_DST_COLOR`，即对底色取反）
/// 画在画面正中。640×360 下原版自动 GUI 缩放为 1（高 360 < 2×240），精灵 1:1。
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
    // 原版：(屏宽 - 15) / 2，整数除。
    let left = (image.width().saturating_sub(width) / 2) as i64;
    let top = (image.height().saturating_sub(height) / 2) as i64;
    for y in 0..height {
        for x in 0..width {
            let coverage = match &sprite {
                Some(sprite) => f64::from(sprite.get_pixel(x, y).0[3]) / 255.0,
                None => f64::from(u8::from(x == width / 2 || y == height / 2)),
            };
            if coverage <= 0.0 {
                continue;
            }
            let (px, py) = (left + i64::from(x), top + i64::from(y));
            if px < 0 || py < 0 || px >= i64::from(image.width()) || py >= i64::from(image.height())
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
