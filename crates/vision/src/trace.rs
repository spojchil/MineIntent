//! Test-only ray-tracing reference, independent of the rasterizer.
use image::RgbaImage;

use crate::geometry::{add, build, cross, dot, mul, sub, Triangle, V3};
use crate::{Camera, Frame, Options, Report, Resources, Scene};

struct Node {
    min: V3,
    max: V3,
    start: usize,
    end: usize,
    children: Option<[usize; 2]>,
}

struct Mesh {
    triangles: Vec<Triangle>,
    nodes: Vec<Node>,
}

impl Mesh {
    fn new(triangles: Vec<Triangle>) -> Self {
        let mut mesh = Self {
            triangles,
            nodes: Vec::new(),
        };
        mesh.partition(0, mesh.triangles.len());
        mesh
    }

    fn partition(&mut self, start: usize, end: usize) -> usize {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for triangle in &self.triangles[start..end] {
            for vertex in triangle.vertices {
                for axis in 0..3 {
                    min[axis] = min[axis].min(vertex[axis]);
                    max[axis] = max[axis].max(vertex[axis]);
                }
            }
        }
        let index = self.nodes.len();
        self.nodes.push(Node {
            min,
            max,
            start,
            end,
            children: None,
        });
        if end - start > 8 {
            let axis = (0..3)
                .max_by(|&a, &b| (max[a] - min[a]).total_cmp(&(max[b] - min[b])))
                .unwrap();
            let middle = (start + end) / 2;
            self.triangles[start..end].select_nth_unstable_by(middle - start, |a, b| {
                let center = |t: &Triangle| t.vertices.iter().map(|v| v[axis]).sum::<f64>();
                center(a).total_cmp(&center(b))
            });
            let children = [self.partition(start, middle), self.partition(middle, end)];
            self.nodes[index].children = Some(children);
        }
        index
    }

    fn nearest(
        &self,
        origin: V3,
        direction: V3,
        near: f64,
        far: f64,
        stack: &mut Vec<usize>,
    ) -> Option<(f64, [f64; 4])> {
        if self.triangles.is_empty() {
            return None;
        }
        let mut distance = far;
        let mut hit = None;
        stack.clear();
        stack.push(0);
        while let Some(index) = stack.pop() {
            let node = &self.nodes[index];
            if !intersects_box(node, origin, direction, near, distance) {
                continue;
            }
            if let Some(children) = node.children {
                stack.extend(children);
                continue;
            }
            for triangle in &self.triangles[node.start..node.end] {
                if let Some((t, u, v)) = intersect_triangle(triangle, origin, direction) {
                    if t < near || t >= distance {
                        continue;
                    }
                    let color = sample(triangle, u, v);
                    if color[3] < 0.01 {
                        continue;
                    }
                    distance = t;
                    hit = Some((t, color));
                }
            }
        }
        hit
    }
}

fn intersects_box(node: &Node, origin: V3, direction: V3, mut near: f64, mut far: f64) -> bool {
    for axis in 0..3 {
        if direction[axis].abs() < 1e-12 {
            if origin[axis] < node.min[axis] - 1e-8 || origin[axis] > node.max[axis] + 1e-8 {
                return false;
            }
        } else {
            let a = (node.min[axis] - origin[axis]) / direction[axis];
            let b = (node.max[axis] - origin[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
            if near > far + 1e-8 {
                return false;
            }
        }
    }
    true
}

fn intersect_triangle(triangle: &Triangle, origin: V3, direction: V3) -> Option<(f64, f64, f64)> {
    let a = sub(triangle.vertices[1], triangle.vertices[0]);
    let b = sub(triangle.vertices[2], triangle.vertices[0]);
    let p = cross(direction, b);
    let determinant = dot(a, p);
    if determinant.abs() < 1e-10 {
        return None;
    }
    let offset = sub(origin, triangle.vertices[0]);
    let u = dot(offset, p) / determinant;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(offset, a);
    let v = dot(direction, q) / determinant;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some((dot(b, q) / determinant, u, v))
}

fn sample(triangle: &Triangle, u: f64, v: f64) -> [f64; 4] {
    let uv: [f64; 2] = std::array::from_fn(|i| {
        triangle.uv[0][i] * (1.0 - u - v) + triangle.uv[1][i] * u + triangle.uv[2][i] * v
    });
    let width = triangle.texture.width();
    // Vanilla animated block sheets are vertical; freeze the first square tile.
    let height = triangle.texture.height().min(width);
    let x = ((uv[0] / 16.0).clamp(0.0, 1.0) * f64::from(width)) as u32;
    let y = ((uv[1] / 16.0).clamp(0.0, 1.0) * f64::from(height)) as u32;
    let color = triangle
        .texture
        .get_pixel(x.min(width - 1), y.min(height - 1))
        .0;
    [
        f64::from(color[0]) * triangle.color[0],
        f64::from(color[1]) * triangle.color[1],
        f64::from(color[2]) * triangle.color[2],
        f64::from(color[3]) / 255.0 * triangle.alpha,
    ]
}

pub(crate) fn ray(camera: &Camera, x: f64, y: f64, options: Options) -> V3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let forward = [-sy * cp, -sp, cy * cp];
    let right = [-cy, 0.0, -sy];
    let up = [-sy * sp, cp, cy * sp];
    let scale = (camera.vertical_fov.to_radians() / 2.0).tan();
    let horizontal = (2.0 * x / f64::from(options.width) - 1.0) * scale * f64::from(options.width)
        / f64::from(options.height);
    let vertical = (1.0 - 2.0 * y / f64::from(options.height)) * scale;
    let direction = add(add(forward, mul(right, horizontal)), mul(up, vertical));
    mul(direction, 1.0 / dot(direction, direction).sqrt())
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
        image: draw(triangles, &scene.camera, options),
        report,
    })
}

pub(crate) fn draw(triangles: Vec<Triangle>, camera: &Camera, options: Options) -> RgbaImage {
    let mesh = Mesh::new(triangles);
    let mut pixels = vec![0u8; options.width as usize * options.height as usize * 4];
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let rows = (options.height as usize).div_ceil(workers);
    std::thread::scope(|scope| {
        for (chunk_index, chunk) in pixels
            .chunks_mut(rows * options.width as usize * 4)
            .enumerate()
        {
            let mesh = &mesh;
            scope.spawn(move || {
                let mut stack = Vec::with_capacity(64);
                for (index, pixel) in chunk.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = index % options.width as usize;
                    let y = chunk_index * rows + index / options.width as usize;
                    let direction = ray(camera, x as f64 + 0.5, y as f64 + 0.5, options);
                    let mut near = 1e-4;
                    let mut remaining = 1.0;
                    let mut color = [0.0; 3];
                    for _ in 0..16 {
                        let Some((distance, hit)) =
                            mesh.nearest(camera.eye, direction, near, options.far, &mut stack)
                        else {
                            break;
                        };
                        for i in 0..3 {
                            color[i] += remaining * hit[3] * hit[i];
                        }
                        remaining *= 1.0 - hit[3];
                        if remaining < 0.005 {
                            break;
                        }
                        near = distance + 1e-7;
                    }
                    let background = [150.0, 185.0, 215.0];
                    for i in 0..3 {
                        pixel[i] = (color[i] + remaining * background[i]).clamp(0.0, 255.0) as u8;
                    }
                    pixel[3] = 255;
                }
            });
        }
    });
    RgbaImage::from_raw(options.width, options.height, pixels).expect("validated pixel dimensions")
}
