use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use serde_json::{json, Value};

use crate::assets::{missing_texture, texture_id};
use crate::biome::Biomes;
use crate::light::{self, Cells, Coords, Direction, FULL_BRIGHT};
use crate::{Block, Report, Resources, Scene};

pub(crate) type V3 = [f64; 3];

/// 以方块坐标为键的哈希表。哈希用 FxHash（rustc 的 `FxHasher` 同一算法）：每帧要建、查上百万次，
/// 默认的 SipHash 慢一个量级；键是场景里的坐标，不需要抗碰撞攻击。
pub(crate) type PositionMap<V> =
    HashMap<[i32; 3], V, std::hash::BuildHasherDefault<PositionHasher>>;

#[derive(Default, Clone, Copy)]
pub(crate) struct PositionHasher(u64);

impl PositionHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for PositionHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
}
pub(crate) fn add(a: V3, b: V3) -> V3 {
    std::array::from_fn(|i| a[i] + b[i])
}
pub(crate) fn sub(a: V3, b: V3) -> V3 {
    std::array::from_fn(|i| a[i] - b[i])
}
pub(crate) fn mul(a: V3, b: f64) -> V3 {
    a.map(|v| v * b)
}
pub(crate) fn dot(a: V3, b: V3) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[derive(Clone)]
struct Face {
    /// 方块内坐标（0..1）。
    vertices: [V3; 4],
    uv: [[f64; 2]; 4],
    texture: Arc<RgbaImage>,
    /// 不随方块变的底色；不染色为白。
    tint: V3,
    /// 模型的 `tintindex`：有就按方块与位置取原版染色（[`crate::biome`]），乘在底色上。
    tint_index: Option<usize>,
    alpha: f64,
    cull: Option<[i32; 3]>,
    /// 原版烘焙出的四边形朝向（法线最接近的轴）。
    direction: Direction,
    /// 模型元素的 `shade`：false 时不按朝向压暗。
    shade: bool,
    /// 模型的 `ambientocclusion`。
    ambient_occlusion: bool,
    fluid: bool,
}

pub(crate) struct Triangle {
    pub vertices: [V3; 3],
    pub uv: [[f64; 2]; 3],
    pub texture: Arc<RgbaImage>,
    /// 逐顶点颜色：染色 × 朝向明暗 × 环境光遮蔽。光照贴图另由 `light` 取样后乘上。
    pub color: [V3; 3],
    /// 逐顶点光照坐标 `[方块光, 天空光]`，原版单位。
    pub light: [Coords; 3],
    pub alpha: f64,
}

impl Face {
    /// 从方块内坐标 `eye` 看过去是不是正面：顶点算出的法线按面的朝向取外侧，
    /// 眼睛在面所在平面的外侧才是正面（与 GPU 按屏幕绕序剔除等价）。
    fn faces(&self, eye: V3) -> bool {
        let [a, b, c, _] = self.vertices;
        let mut normal = cross(sub(b, a), sub(c, a));
        let outward = self.direction.vector().map(f64::from);
        if dot(normal, outward) < 0.0 {
            normal = normal.map(|n| -n);
        }
        dot(normal, sub(eye, a)) > 0.0
    }

    fn directional_shade(&self) -> f64 {
        if self.shade {
            self.direction.cardinal_shade()
        } else {
            Direction::Up.cardinal_shade()
        }
    }

    /// 本面四个顶点的（颜色, 光照坐标）。`cells` 为 `None` 时不算光照：满亮度、无遮蔽。
    fn lighting(&self, position: [i32; 3], tint: V3, cells: Option<&Cells>) -> [(V3, Coords); 4] {
        let shade = self.directional_shade();
        let plain = mul(tint, shade);
        let Some(cells) = cells else {
            return [(plain, FULL_BRIGHT); 4];
        };
        if self.fluid {
            return [(plain, light::fluid(cells, position)); 4];
        }
        if self.ambient_occlusion && cells.get(position).emission == 0 {
            crate::counters::add(crate::counters::Counter::AoFaces, 1);
            let values = light::ambient_occlusion(cells, position, self.direction, &self.vertices);
            return std::array::from_fn(|i| (mul(tint, shade * values[i].0), values[i].1));
        }
        let coords = light::flat(cells, position, self.direction, self.cull.is_some());
        [(plain, coords); 4]
    }
}

/// 不带光照的方块三角形（测试）：满亮度、只按朝向明暗。
#[cfg(test)]
pub(crate) fn build(
    scene: &Scene,
    resources: &mut Resources,
    report: &mut Report,
) -> Vec<Triangle> {
    let biomes = Biomes::new(scene, resources, report).ok();
    build_lit(scene, None, biomes.as_ref(), None, None, resources, report)
}

/// `cells` 为 `Some` 时按原版平滑光照与光照坐标逐顶点算光；`biomes` 给模型面染色
/// （没有时染色面按白）；`frustum` 为 `Some` 时整段跳过视锥外的区块段；`eye` 为 `Some`
/// 时剔除背向它的面（只看面结构的测试不传）。
///
/// 分三步（参照原版 `SectionRenderDispatcher` 与 Sodium 的分段编译）：视锥按段筛方块；
/// 串行解析每种方块状态的面与方块实体（要读资源）；再按方块并行出三角形，每个线程一份
/// 染色缓存（原版 `ClientLevel` 的 `tintCaches` 也是每线程一份），按方块原顺序拼回。
pub(crate) fn build_lit(
    scene: &Scene,
    cells: Option<&Cells>,
    biomes: Option<&Biomes>,
    frustum: Option<&crate::raster::Frustum>,
    eye: Option<V3>,
    resources: &mut Resources,
    report: &mut Report,
) -> Vec<Triangle> {
    let mut clock = std::time::Instant::now();
    let mut sections: PositionMap<bool> = PositionMap::default();
    let visible: Vec<&Block> = scene
        .blocks
        .iter()
        .filter(|block| !is_air(&block.name))
        .filter(|block| {
            frustum.is_none_or(|frustum| {
                *sections
                    .entry(block.position.map(|c| c >> 4))
                    .or_insert_with_key(|section| frustum.section_visible(*section))
            })
        })
        .collect();
    report.stage("geometry: frustum select", &mut clock);

    let mut states: HashMap<(&str, &std::collections::BTreeMap<String, String>), usize> =
        HashMap::new();
    let mut faces_by_state: Vec<Vec<Face>> = Vec::new();
    let mut prepared: Vec<(usize, Vec<Triangle>)> = Vec::with_capacity(visible.len());
    for block in &visible {
        let entity = crate::block_entity::triangles(block, eye, cells, resources, report);
        let state = *states
            .entry((block.name.as_str(), &block.properties))
            .or_insert_with(|| {
                faces_by_state.push(match block_faces(block, resources, report) {
                    Ok(faces) if !faces.is_empty() => faces,
                    // 箱子、潜影盒等没有资源定义的几何，整个外形由方块实体画。
                    _ if crate::block_entity::has(&block.name) => Vec::new(),
                    result => {
                        let reason = result
                            .err()
                            .unwrap_or_else(|| "empty/special model".to_owned());
                        report.warnings.insert(format!(
                            "{}: checkerboard placeholder ({reason})",
                            block.name
                        ));
                        cube_faces(missing_texture(), [1.0; 3], 1.0, 1.0)
                    }
                });
                faces_by_state.len() - 1
            });
        prepared.push((state, entity));
    }
    report.stage("geometry: state faces, block entities", &mut clock);

    // 只有缺遮挡位的方块（测试夹具）才要查邻居。
    let neighbours: PositionMap<&Block> = if visible.iter().any(|b| b.covered.is_none()) {
        scene.blocks.iter().map(|b| (b.position, b)).collect()
    } else {
        PositionMap::default()
    };
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let chunk = visible.len().div_ceil(workers).max(1);
    let faces_by_state = &faces_by_state;
    let neighbours = &neighbours;
    let parts: Vec<Vec<Triangle>> = std::thread::scope(|scope| {
        let handles: Vec<_> = visible
            .chunks(chunk)
            .zip(prepared.chunks_mut(chunk))
            .map(|(blocks, prepared)| {
                scope.spawn(move || {
                    let mut tints = crate::biome::TintCache::new();
                    let mut triangles = Vec::new();
                    for (block, (state, entity)) in blocks.iter().zip(prepared) {
                        triangles.append(entity);
                        emit_block(
                            block,
                            eye,
                            frustum,
                            &faces_by_state[*state],
                            neighbours,
                            cells,
                            biomes,
                            &mut tints,
                            &mut triangles,
                        );
                    }
                    crate::counters::flush();
                    triangles
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("几何线程不 panic"))
            .collect()
    });
    let mut triangles = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for mut part in parts {
        triangles.append(&mut part);
    }
    report.stage("geometry: emit", &mut clock);
    triangles
}

fn is_air(name: &str) -> bool {
    matches!(
        name.strip_prefix("minecraft:").unwrap_or(name),
        "air" | "cave_air" | "void_air"
    )
}

/// 一个方块的模型面：剔掉被遮挡的面、背向眼睛的面与视锥外的面，染色、算光，写成三角形。
#[allow(clippy::too_many_arguments)]
fn emit_block(
    block: &Block,
    eye: Option<V3>,
    frustum: Option<&crate::raster::Frustum>,
    faces: &[Face],
    neighbours: &PositionMap<&Block>,
    cells: Option<&Cells>,
    biomes: Option<&Biomes>,
    tints: &mut crate::biome::TintCache,
    triangles: &mut Vec<Triangle>,
) {
    let fluid = is_fluid(&block.name);
    let [mut covered, mut backfacing, mut outside, mut emitted] = [0u64; 4];
    for face in faces {
        if let Some(direction) = face.cull {
            let hidden = match block.covered {
                Some(mask) => direction_bit(direction).is_some_and(|bit| mask & (1 << bit) != 0),
                None => {
                    let pos =
                        std::array::from_fn(|i| block.position[i].saturating_add(direction[i]));
                    // 原版 FluidRenderer：同种流体相邻的面不画。
                    neighbours
                        .get(&pos)
                        .is_some_and(|b| b.opaque || (fluid && b.name == block.name))
                }
            };
            if hidden {
                covered += 1;
                continue;
            }
        }
        let offset = block.position.map(f64::from);
        // 背面剔除：原版方块的渲染管线都开着面剔除（流体除外，FluidRenderer 自己补反面）。
        if !face.fluid && eye.is_some_and(|eye| !face.faces(sub(eye, offset))) {
            backfacing += 1;
            continue;
        }
        // 整个落在视锥外的面会被裁剪整个丢掉：不必再染色、算光、出三角形。
        if frustum.is_some_and(|frustum| frustum.excludes(&face.vertices.map(|v| add(v, offset)))) {
            outside += 1;
            continue;
        }
        emitted += 1;
        let tint = match (face.tint_index, biomes) {
            (Some(index), Some(biomes)) => {
                let color =
                    biomes.block_tint(&block.name, &block.properties, index, block.position, tints);
                std::array::from_fn(|i| face.tint[i] * color[i])
            }
            _ => face.tint,
        };
        let lit = face.lighting(block.position, tint, cells);
        for indices in [[0, 1, 2], [0, 2, 3]] {
            triangles.push(Triangle {
                vertices: indices.map(|i| add(face.vertices[i], offset)),
                uv: indices.map(|i| face.uv[i]),
                texture: face.texture.clone(),
                color: indices.map(|i| lit[i].0),
                light: indices.map(|i| lit[i].1),
                alpha: face.alpha,
            });
        }
    }
    use crate::counters::Counter;
    crate::counters::add(Counter::FacesConsidered, faces.len() as u64);
    crate::counters::add(Counter::FacesCovered, covered);
    crate::counters::add(Counter::FacesBackfacing, backfacing);
    crate::counters::add(Counter::FacesOutside, outside);
    crate::counters::add(Counter::FacesEmitted, emitted);
}

fn is_fluid(name: &str) -> bool {
    matches!(
        name.strip_prefix("minecraft:").unwrap_or(name),
        "water" | "lava"
    )
}

/// 剔面方向在 [`crate::Block::covered`] 里的位：下、上、北、南、西、东。
fn direction_bit(direction: [i32; 3]) -> Option<usize> {
    match direction {
        [0, -1, 0] => Some(0),
        [0, 1, 0] => Some(1),
        [0, 0, -1] => Some(2),
        [0, 0, 1] => Some(3),
        [-1, 0, 0] => Some(4),
        [1, 0, 0] => Some(5),
        _ => None,
    }
}

/// 用方块自己的模型画一个缩小的方块（掉落在地上的方块物品）。`place` 把 0..1 的方块
/// 空间换到世界坐标。方块状态取默认属性；需要属性才能选出模型的方块会返回 Err。
pub(crate) fn block_item_triangles(
    name: &str,
    resources: &mut Resources,
    report: &mut Report,
    light: Coords,
    place: impl Fn(V3) -> V3,
) -> Result<Vec<Triangle>, String> {
    let block = Block {
        position: [0; 3],
        name: name.to_owned(),
        properties: Default::default(),
        opaque: false,
        covered: None,
    };
    let faces = block_faces(&block, resources, report)?;
    if faces.is_empty() {
        return Err(format!("{name}: empty block model"));
    }
    let mut triangles = Vec::new();
    for face in faces {
        let tint = match face.tint_index {
            Some(index) => crate::biome::item_tint(name, &block.properties, index, resources),
            None => [1.0; 3],
        };
        let color = mul(
            std::array::from_fn(|i| face.tint[i] * tint[i]),
            face.directional_shade(),
        );
        for indices in [[0, 1, 2], [0, 2, 3]] {
            triangles.push(Triangle {
                vertices: indices.map(|i| place(face.vertices[i])),
                uv: indices.map(|i| face.uv[i]),
                texture: face.texture.clone(),
                color: [color; 3],
                light: [light; 3],
                alpha: face.alpha,
            });
        }
    }
    Ok(triangles)
}

fn block_faces(
    block: &Block,
    resources: &mut Resources,
    report: &mut Report,
) -> Result<Vec<Face>, String> {
    let name = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
    if name == "water" || name == "lava" {
        report.warnings.insert(
            "fluids use flat level surfaces; flow and waterlogging are not reproduced".to_owned(),
        );
        let level = block
            .properties
            .get("level")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let height = if level >= 8 {
            1.0
        } else {
            (8 - level) as f64 / 9.0
        };
        let texture = resources.texture(&format!("block/{name}_still"))?;
        // 水的透明度在贴图自身的 alpha 里；颜色是生物群系水色（流体模型的 tint 源）。
        let mut faces = cube_faces(texture, [1.0; 3], 1.0, height);
        for face in &mut faces {
            face.fluid = true;
            face.tint_index = (name == "water").then_some(0);
        }
        return Ok(faces);
    }
    if block
        .properties
        .get("waterlogged")
        .is_some_and(|s| s == "true")
    {
        report
            .warnings
            .insert("waterlogged fluid geometry is not yet drawn".to_owned());
    }
    let mut result = Vec::new();
    for selection in resources.selections(block, report)? {
        let id = selection["model"]
            .as_str()
            .ok_or("model selection has no model identifier")?;
        let model = resources.model(id, &mut Vec::new())?;
        let x = selection["x"].as_f64().unwrap_or(0.0);
        let y = selection["y"].as_f64().unwrap_or(0.0);
        if selection["uvlock"].as_bool() == Some(true) {
            report
                .warnings
                .insert("uvlock is approximated: textures rotate with the model".to_owned());
        }
        let ambient_occlusion = model["ambientocclusion"].as_bool() != Some(false);
        let elements = model["elements"]
            .as_array()
            .ok_or_else(|| format!("{id}: no resource-defined geometry"))?;
        for (element_index, element) in elements.iter().enumerate() {
            let from = vector(&element["from"])?;
            let to = vector(&element["to"])?;
            let faces = element["faces"].as_object().ok_or("element has no faces")?;
            for (direction, definition) in faces {
                let (mut vertices, normal, default_uv) = face_geometry(direction, from, to)?;
                let texture_name = texture_id(
                    &model,
                    definition["texture"]
                        .as_str()
                        .ok_or("face has no texture")?,
                )?;
                let texture = resources.texture(&texture_name)?;
                if texture.height() > texture.width() {
                    report
                        .warnings
                        .insert("animated textures use the first tile".to_owned());
                }
                let rect = if let Some(uv) = definition["uv"].as_array() {
                    if uv.len() != 4 {
                        return Err("face UV needs four numbers".to_owned());
                    }
                    let mut rect = [0.0; 4];
                    for i in 0..4 {
                        rect[i] = uv[i].as_f64().ok_or("invalid UV")?;
                    }
                    rect
                } else {
                    default_uv
                };
                let mut uv = [
                    [rect[0], rect[1]],
                    [rect[2], rect[1]],
                    [rect[2], rect[3]],
                    [rect[0], rect[3]],
                ];
                let turns = definition["rotation"].as_u64().unwrap_or(0) / 90;
                uv.rotate_right((turns % 4) as usize);
                for vertex in &mut vertices {
                    // Separate coplanar overlay elements enough for alpha compositing.
                    *vertex = add(*vertex, mul(normal, element_index as f64 * 0.00001));
                    if let Some(rotation) = element.get("rotation") {
                        let origin = vector(&rotation["origin"])?;
                        let axis =
                            axis_index(rotation["axis"].as_str().ok_or("missing rotation axis")?)?;
                        let angle = rotation["angle"].as_f64().ok_or("missing rotation angle")?;
                        let mut relative = rotate(sub(*vertex, origin), axis, angle);
                        if rotation["rescale"].as_bool() == Some(true) {
                            let scale = 1.0 / angle.to_radians().cos();
                            for (i, value) in relative.iter_mut().enumerate() {
                                if i != axis {
                                    *value *= scale;
                                }
                            }
                        }
                        *vertex = add(relative, origin);
                    }
                    *vertex = add(
                        rotate(rotate(sub(*vertex, [8.0; 3]), 0, -x), 1, -y),
                        [8.0; 3],
                    );
                    *vertex = mul(*vertex, 1.0 / 16.0);
                }
                let normal = rotate(rotate(normal, 0, -x), 1, -y);
                let tint_index = definition["tintindex"]
                    .as_i64()
                    .and_then(|i| usize::try_from(i).ok());
                let cull = definition["cullface"]
                    .as_str()
                    .map(|face| {
                        let (_, normal, _) = face_geometry(face, [0.0; 3], [16.0; 3])?;
                        Ok::<_, String>(
                            rotate(rotate(normal, 0, -x), 1, -y).map(|v| v.round() as i32),
                        )
                    })
                    .transpose()?;
                result.push(Face {
                    vertices,
                    uv,
                    texture,
                    tint: [1.0; 3],
                    tint_index,
                    alpha: 1.0,
                    cull,
                    direction: Direction::nearest(normal),
                    shade: element["shade"].as_bool() != Some(false),
                    ambient_occlusion,
                    fluid: false,
                });
            }
        }
    }
    Ok(result)
}

fn cube_faces(texture: Arc<RgbaImage>, tint: V3, alpha: f64, height: f64) -> Vec<Face> {
    ["down", "up", "north", "south", "west", "east"]
        .into_iter()
        .map(|direction| {
            let (vertices, normal, _) =
                face_geometry(direction, [0.0; 3], [1.0, height, 1.0]).expect("known direction");
            Face {
                vertices,
                uv: [[0.0, 0.0], [16.0, 0.0], [16.0, 16.0], [0.0, 16.0]],
                texture: texture.clone(),
                tint,
                tint_index: None,
                alpha,
                cull: Some(normal.map(|v| v.round() as i32)),
                direction: Direction::nearest(normal),
                shade: true,
                ambient_occlusion: true,
                fluid: false,
            }
        })
        .collect()
}

fn vector(v: &Value) -> Result<V3, String> {
    let values = v.as_array().ok_or("expected model vector")?;
    if values.len() != 3 {
        return Err("model vector needs three numbers".to_owned());
    }
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = values[i]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or("invalid model coordinate")?;
    }
    Ok(out)
}

fn axis_index(axis: &str) -> Result<usize, String> {
    match axis {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        _ => Err("invalid model rotation axis".to_owned()),
    }
}

pub(crate) fn rotate(v: V3, axis: usize, degrees: f64) -> V3 {
    let (s, c) = degrees.to_radians().sin_cos();
    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut out = v;
    out[a] = c * v[a] - s * v[b];
    out[b] = s * v[a] + c * v[b];
    out
}

type FaceGeometry = ([V3; 4], V3, [f64; 4]);
fn face_geometry(face: &str, a: V3, b: V3) -> Result<FaceGeometry, String> {
    let ([x0, y0, z0], [x1, y1, z1]) = (a, b);
    Ok(match face {
        "north" => (
            [[x1, y1, z0], [x0, y1, z0], [x0, y0, z0], [x1, y0, z0]],
            [0., 0., -1.],
            [16. - x1, 16. - y1, 16. - x0, 16. - y0],
        ),
        "south" => (
            [[x0, y1, z1], [x1, y1, z1], [x1, y0, z1], [x0, y0, z1]],
            [0., 0., 1.],
            [x0, 16. - y1, x1, 16. - y0],
        ),
        "west" => (
            [[x0, y1, z0], [x0, y1, z1], [x0, y0, z1], [x0, y0, z0]],
            [-1., 0., 0.],
            [z0, 16. - y1, z1, 16. - y0],
        ),
        "east" => (
            [[x1, y1, z1], [x1, y1, z0], [x1, y0, z0], [x1, y0, z1]],
            [1., 0., 0.],
            [16. - z1, 16. - y1, 16. - z0, 16. - y0],
        ),
        "up" => (
            [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
            [0., 1., 0.],
            [x0, z0, x1, z1],
        ),
        "down" => (
            [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
            [0., -1., 0.],
            [x0, 16. - z1, x1, 16. - z0],
        ),
        _ => return Err(format!("unknown model face {face}")),
    })
}

/// A small deterministic resource-driven scene for inspecting model support.
pub(crate) fn fixture_block(
    position: [i32; 3],
    name: &str,
    properties: Value,
    opaque: bool,
) -> Block {
    Block {
        position,
        name: name.to_owned(),
        properties: serde_json::from_value(properties).unwrap_or_default(),
        opaque,
        covered: None,
    }
}

/// Does not contain game assets or simulated game behaviour.
pub fn fixture() -> Scene {
    let mut blocks = Vec::new();
    for x in -8..=8 {
        for z in -5..=12 {
            blocks.push(fixture_block(
                [x, 0, z],
                "grass_block",
                json!({"snowy":"false"}),
                true,
            ));
        }
    }
    for (x, name, properties) in [
        (-4, "stone", json!({})),
        (
            -2,
            "oak_stairs",
            json!({"facing":"south","half":"bottom","shape":"straight","waterlogged":"false"}),
        ),
        (
            0,
            "oak_slab",
            json!({"type":"bottom","waterlogged":"false"}),
        ),
        (2, "glass", json!({})),
        (
            4,
            "oak_leaves",
            json!({"distance":"7","persistent":"true","waterlogged":"false"}),
        ),
    ] {
        blocks.push(fixture_block([x, 1, 3], name, properties, name == "stone"));
    }
    for x in -3..=3 {
        blocks.push(fixture_block([x,1,7],"oak_fence",json!({"north":"false","south":"false","west":if x > -3 {"true"} else {"false"},"east":if x < 3 {"true"} else {"false"},"waterlogged":"false"}),false));
    }
    for x in -1..=1 {
        blocks.push(fixture_block(
            [x, 1, 10],
            "water",
            json!({"level":"0"}),
            false,
        ));
    }
    Scene {
        game_version: "26.1.2".to_owned(),
        camera: crate::Camera {
            eye: [6.0, 5.0, -7.0],
            yaw: 24.0,
            pitch: 18.0,
            vertical_fov: 60.0,
        },
        blocks,
        entities: Vec::new(),
        environment: None,
        cells: Vec::new(),
        biome_names: Vec::new(),
        biomes: Vec::new(),
    }
}
