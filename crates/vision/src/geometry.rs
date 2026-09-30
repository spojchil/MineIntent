use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use serde_json::{json, Value};

use crate::assets::{missing_texture, texture_id};
use crate::{Block, Report, Resources, Scene};

pub(crate) type V3 = [f64; 3];
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
#[cfg(test)]
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[derive(Clone)]
struct Face {
    vertices: [V3; 4],
    uv: [[f64; 2]; 4],
    texture: Arc<RgbaImage>,
    color: V3,
    alpha: f64,
    cull: Option<[i32; 3]>,
}

pub(crate) struct Triangle {
    pub vertices: [V3; 3],
    pub uv: [[f64; 2]; 3],
    pub texture: Arc<RgbaImage>,
    pub color: V3,
    pub alpha: f64,
}

pub(crate) fn build(
    scene: &Scene,
    resources: &mut Resources,
    report: &mut Report,
) -> Vec<Triangle> {
    let neighbours: HashMap<_, _> = scene.blocks.iter().map(|b| (b.position, b)).collect();
    let mut cache: HashMap<String, Vec<Face>> = HashMap::new();
    let mut triangles = Vec::new();
    for block in &scene.blocks {
        if matches!(
            block.name.as_str(),
            "air"
                | "cave_air"
                | "void_air"
                | "minecraft:air"
                | "minecraft:cave_air"
                | "minecraft:void_air"
        ) {
            continue;
        }
        let key = format!("{}{:?}", block.name, block.properties);
        let faces =
            cache
                .entry(key)
                .or_insert_with(|| match block_faces(block, resources, report) {
                    Ok(faces) if !faces.is_empty() => faces,
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
        for face in faces.iter() {
            if let Some(direction) = face.cull {
                let pos = std::array::from_fn(|i| block.position[i].saturating_add(direction[i]));
                if neighbours.get(&pos).is_some_and(|b| b.opaque) {
                    continue;
                }
            }
            let offset = block.position.map(f64::from);
            for indices in [[0, 1, 2], [0, 2, 3]] {
                triangles.push(Triangle {
                    vertices: indices.map(|i| add(face.vertices[i], offset)),
                    uv: indices.map(|i| face.uv[i]),
                    texture: face.texture.clone(),
                    color: face.color,
                    alpha: face.alpha,
                });
            }
        }
    }
    triangles
}

fn block_faces(
    block: &Block,
    resources: &mut Resources,
    report: &mut Report,
) -> Result<Vec<Face>, String> {
    let name = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
    if name == "water" || name == "lava" {
        report.warnings.insert(
            "fluids use flat level surfaces; flow, biome tint and waterlogging are not reproduced"
                .to_owned(),
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
        return Ok(cube_faces(
            texture,
            if name == "water" {
                [0.25, 0.45, 0.95]
            } else {
                [1.0; 3]
            },
            if name == "water" { 0.65 } else { 1.0 },
            height,
        ));
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
                let shade = if element["shade"].as_bool() == Some(false) {
                    1.0
                } else {
                    0.6 + 0.25 * normal[1].max(0.0) + 0.1 * normal[2].abs()
                };
                let color = if definition["tintindex"].as_i64().is_some_and(|i| i >= 0) {
                    report.warnings.insert("tinted models use a fixed vegetation color; biome/redstone tints and server light are not yet used".to_owned());
                    mul([0.55, 0.8, 0.32], shade)
                } else {
                    [shade; 3]
                };
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
                    color,
                    alpha: 1.0,
                    cull,
                });
            }
        }
    }
    Ok(result)
}

fn cube_faces(texture: Arc<RgbaImage>, color: V3, alpha: f64, height: f64) -> Vec<Face> {
    ["down", "up", "north", "south", "west", "east"]
        .into_iter()
        .map(|direction| {
            let (vertices, _, _) =
                face_geometry(direction, [0.0; 3], [1.0, height, 1.0]).expect("known direction");
            Face {
                vertices,
                uv: [[0.0, 0.0], [16.0, 0.0], [16.0, 16.0], [0.0, 16.0]],
                texture: texture.clone(),
                color,
                alpha,
                cull: None,
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
    }
}
