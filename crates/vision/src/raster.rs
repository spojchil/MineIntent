//! CPU rasterization into an image: no surface, window or graphics device.
use image::RgbaImage;

use crate::geometry::{build, dot, sub, Triangle, V3};
use crate::{Camera, Frame, Options, Report, Resources, Scene};

const NEAR: f64 = 1e-4;
const SKY: [f64; 3] = [150.0, 185.0, 215.0];
const MAX_LAYERS: usize = 16;

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
        let background = if count < MAX_LAYERS && remaining >= 0.005 {
            self.color
        } else {
            SKY
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
    y0: usize,
    output: &mut [u8],
) {
    let width = options.width as usize;
    let rows = output.len() / (width * 4);
    let mut pixels: Vec<_> = (0..width * rows)
        .map(|_| Pixel {
            z: f64::INFINITY,
            color: SKY,
            layers: Vec::new(),
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
                let color = sample(t.triangle, uv);
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

pub(crate) fn draw(triangles: &[Triangle], camera: &Camera, options: Options) -> RgbaImage {
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
                    draw_band(projected, projection, options, i * rows + band * 16, output);
                }
            });
        }
    });
    RgbaImage::from_raw(options.width, options.height, pixels).expect("validated pixel dimensions")
}

pub fn render(scene: &Scene, resources: &mut Resources, options: Options) -> Result<Frame, String> {
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
        || !(1.0..=256.0).contains(&options.far)
        || !scene.camera.eye.iter().all(|n| n.is_finite())
        || !scene.camera.yaw.is_finite()
        || !scene.camera.pitch.is_finite()
        || !(-90.0..=90.0).contains(&scene.camera.pitch)
        || !scene.camera.vertical_fov.is_finite()
        || !(10.0..=150.0).contains(&scene.camera.vertical_fov)
        || scene.blocks.len() > 300_000
    {
        return Err(
            "invalid camera, image size (1..2048), distance (1..256), or block count (>300000)"
                .to_owned(),
        );
    }
    let mut report = Report {
        blocks: scene.blocks.len(),
        ..Report::default()
    };
    report.warnings.insert("prototype: fixed daylight/background, no server lighting, entities, block-entity contents, weather or HUD".to_owned());
    report
        .warnings
        .insert("transparency is composited through at most 16 surfaces".to_owned());
    let triangles = build(scene, resources, &mut report);
    report.triangles = triangles.len();
    Ok(Frame {
        image: draw(&triangles, &scene.camera, options),
        report,
    })
}
