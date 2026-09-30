use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::json;

use crate::{assets, geometry, render, trace, Block, Camera, Options, Resources, Scene};

struct TestJar(PathBuf);
impl Drop for TestJar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// All resources here are test-authored, with solid synthetic textures.
fn resources() -> (TestJar, Resources) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "mineintent-vision-{}-{}.jar",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let mut write = |name: &str, bytes: &[u8]| {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    };
    write("version.json", br#"{"id":"test"}"#);
    let mut faces = serde_json::Map::new();
    for face in ["north", "south", "east", "west", "up", "down"] {
        faces.insert(face.to_owned(), json!({"texture":"#all","cullface":face}));
    }
    write(
        "assets/minecraft/models/block/base.json",
        json!({"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":faces}]})
            .to_string()
            .as_bytes(),
    );
    for (name, color) in [
        ("red", [255, 0, 0, 255]),
        ("green", [0, 255, 0, 255]),
        ("clear", [0, 0, 255, 0]),
    ] {
        write(
            &format!("assets/minecraft/blockstates/{name}.json"),
            json!({"variants":{"":{"model":format!("block/{name}")}}})
                .to_string()
                .as_bytes(),
        );
        write(&format!("assets/minecraft/models/block/{name}.json"),json!({"parent":"block/base","textures":{"all":"#alias","alias":format!("block/{name}")}}).to_string().as_bytes());
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba(color)))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        write(
            &format!("assets/minecraft/textures/block/{name}.png"),
            png.get_ref(),
        );
    }
    write("assets/minecraft/models/block/panel.json",json!({"textures":{"all":"block/red"},"elements":[{"from":[0,0,0],"to":[16,16,2],"faces":{"north":{"texture":"#all"}}}]}).to_string().as_bytes());
    write("assets/minecraft/blockstates/panel.json",json!({"variants":{"facing=east":{"model":"block/panel","y":90},"facing=north":{"model":"block/panel"}}}).to_string().as_bytes());
    write("assets/minecraft/blockstates/multi.json",json!({"multipart":[{"when":{"east":"true"},"apply":{"model":"block/panel","y":90}},{"when":{"north":"true"},"apply":{"model":"block/panel"}}]}).to_string().as_bytes());
    zip.finish().unwrap();
    let resources = Resources::open(&path).unwrap();
    (TestJar(path), resources)
}

fn block(name: &str, position: [i32; 3]) -> Block {
    Block {
        position,
        name: name.to_owned(),
        properties: BTreeMap::new(),
        opaque: false,
    }
}
fn scene(blocks: Vec<Block>) -> Scene {
    Scene {
        game_version: "test".to_owned(),
        camera: Camera {
            eye: [0.5, 0.5, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            vertical_fov: 60.0,
        },
        blocks,
    }
}
fn small() -> Options {
    Options {
        width: 9,
        height: 9,
        far: 12.0,
    }
}

#[test]
fn camera_uses_minecraft_cardinal_directions_and_pitch() {
    let mut camera = scene(vec![]).camera;
    for (yaw, expected) in [
        (0.0, [0., 0., 1.]),
        (90.0, [-1., 0., 0.]),
        (180.0, [0., 0., -1.]),
        (-90.0, [1., 0., 0.]),
    ] {
        camera.yaw = yaw;
        let ray = trace::ray(&camera, 4.5, 4.5, small());
        assert!(ray.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-9));
    }
    camera.pitch = 90.0;
    assert!(trace::ray(&camera, 4.5, 4.5, small())[1] < -0.999);
}

#[test]
fn depth_and_parent_texture_aliases_survive_input_order() {
    let (_jar, mut resources) = resources();
    for blocks in [
        vec![block("green", [0, 0, 5]), block("red", [0, 0, 2])],
        vec![block("red", [0, 0, 2]), block("green", [0, 0, 5])],
    ] {
        let frame = render(&scene(blocks), &mut resources, small()).unwrap();
        let p = frame.image.get_pixel(4, 4).0;
        assert!(p[0] > 150 && p[1] == 0 && p[2] == 0, "{p:?}");
        assert_eq!(frame.report.triangles, 24);
        assert!(frame.png().unwrap().starts_with(b"\x89PNG"));
    }
}

#[test]
fn alpha_holes_do_not_hide_geometry_behind_them() {
    let (_jar, mut resources) = resources();
    let frame = render(
        &scene(vec![block("clear", [0, 0, 2]), block("green", [0, 0, 5])]),
        &mut resources,
        small(),
    )
    .unwrap();
    let p = frame.image.get_pixel(4, 4).0;
    assert!(p[1] > 150 && p[0] == 0, "{p:?}");
}

#[test]
fn rejects_wrong_resource_version_and_nonfinite_camera() {
    let (_jar, mut resources) = resources();
    let mut scene = scene(vec![]);
    scene.game_version = "wrong".to_owned();
    assert!(render(&scene, &mut resources, small()).is_err());
    scene.game_version = "test".to_owned();
    scene.camera.eye[0] = f64::NAN;
    assert!(render(&scene, &mut resources, small()).is_err());
}

#[test]
fn variants_rotate_geometry_and_multipart_selects_connections() {
    let (_jar, mut resources) = resources();
    let mut panel = block("panel", [0, 0, 0]);
    panel
        .properties
        .insert("facing".to_owned(), "east".to_owned());
    let mut report = crate::Report::default();
    let triangles = geometry::build(&scene(vec![panel]), &mut resources, &mut report);
    assert_eq!(triangles.len(), 2);
    assert!(triangles
        .iter()
        .flat_map(|t| t.vertices)
        .all(|v| (v[0] - 1.0).abs() < 1e-9));
    let mut multi = block("multi", [0, 0, 0]);
    multi
        .properties
        .insert("east".to_owned(), "true".to_owned());
    multi
        .properties
        .insert("north".to_owned(), "false".to_owned());
    assert_eq!(
        geometry::build(&scene(vec![multi.clone()]), &mut resources, &mut report).len(),
        2
    );
    multi
        .properties
        .insert("north".to_owned(), "true".to_owned());
    assert_eq!(
        geometry::build(&scene(vec![multi]), &mut resources, &mut report).len(),
        4
    );
}

#[test]
fn multipart_boolean_conditions_and_missing_properties() {
    let mut b = block("test", [0, 0, 0]);
    b.properties.insert("facing".to_owned(), "east".to_owned());
    assert!(assets::matches_condition(
        &json!({"OR":[{"facing":"north|east"},{"missing":"true"}]}),
        &b
    ));
    assert!(!assets::matches_condition(
        &json!({"AND":[{"facing":"east"},{"missing":"true"}]}),
        &b
    ));
    assert!(!assets::matches_condition(&json!({"missing":"!true"}), &b));
    assert!(assets::texture_id(&json!({"textures":{"a":"#b","b":"#a"}}), "#a").is_err());
    assert_eq!(
        assets::texture_id(
            &json!({"textures":{"a":"#b","b":{"sprite":"block/glass","force_translucent":true}}}),
            "#a"
        )
        .unwrap(),
        "block/glass"
    );
}

#[test]
fn unknown_models_are_visible_and_reported() {
    let (_jar, mut resources) = resources();
    let frame = render(
        &scene(vec![block("unimplemented", [0, 0, 2])]),
        &mut resources,
        small(),
    )
    .unwrap();
    assert_eq!(frame.report.triangles, 12);
    assert!(frame
        .report
        .warnings
        .iter()
        .any(|w| w.contains("unimplemented")));
}

#[test]
fn only_opaque_neighbours_remove_shared_faces() {
    let (_jar, mut resources) = resources();
    let mut blocks = vec![block("red", [0, 0, 0]), block("green", [1, 0, 0])];
    for b in &mut blocks {
        b.opaque = true;
    }
    let mut report = crate::Report::default();
    assert_eq!(
        geometry::build(&scene(blocks.clone()), &mut resources, &mut report).len(),
        20
    );
    blocks[1].opaque = false;
    assert_eq!(
        geometry::build(&scene(blocks), &mut resources, &mut report).len(),
        22
    );
}

#[test]
fn resource_scene_matches_ray_reference_at_several_camera_angles() {
    let (_jar, mut resources) = resources();
    let mut scene = scene(vec![
        block("red", [0, 0, 2]),
        block("clear", [1, 0, 2]),
        block("green", [1, 0, 4]),
        block("green", [-1, -1, 3]),
    ]);
    // Avoid pixel centers exactly on a silhouette: raster top-left coverage and
    // inclusive ray intersections intentionally have different boundary rules.
    scene.camera.eye = [0.537, 0.493, 0.017];
    let options = Options {
        width: 73,
        height: 47,
        ..small()
    };
    for (yaw, pitch) in [(0.0, 0.0), (21.7, -12.3), (-40.1, 32.9), (178.1, 0.0)] {
        scene.camera.yaw = yaw;
        scene.camera.pitch = pitch;
        let actual = render(&scene, &mut resources, options).unwrap().image;
        let expected = trace::render(&scene, &mut resources, options)
            .unwrap()
            .image;
        assert_images_close(&actual, &expected, 0);
    }
}

fn assert_images_close(actual: &RgbaImage, expected: &RgbaImage, allowed_pixels: usize) {
    let differing = actual
        .pixels()
        .zip(expected.pixels())
        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > 1))
        .count();
    assert!(
        differing <= allowed_pixels,
        "{differing} pixels differ by more than one channel level (allowed {allowed_pixels})"
    );
}

fn triangle(vertices: [[f64; 3]; 3], texture: Arc<RgbaImage>) -> geometry::Triangle {
    geometry::Triangle {
        vertices,
        uv: [[0.0, 0.0], [16.0, 0.0], [0.0, 16.0]],
        texture,
        color: [1.0; 3],
        alpha: 1.0,
    }
}

fn quad(z: f64, texture: Arc<RgbaImage>) -> Vec<geometry::Triangle> {
    let a = [-3.0, -3.0, z];
    let b = [3.0, -3.0, z];
    let c = [3.0, 3.0, z];
    let d = [-3.0, 3.0, z];
    vec![
        triangle([a, b, c], texture.clone()),
        triangle([a, c, d], texture),
    ]
}

#[test]
fn transparency_is_depth_ordered_and_shared_diagonal_is_not_blended_twice() {
    let camera = Camera {
        eye: [0.0; 3],
        ..scene(vec![]).camera
    };
    let make_mesh = || {
        let mut mesh = quad(
            4.0,
            Arc::new(RgbaImage::from_pixel(1, 1, Rgba([0, 255, 0, 255]))),
        );
        mesh.extend(quad(
            2.0,
            Arc::new(RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]))),
        ));
        mesh.extend(quad(
            3.0,
            Arc::new(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 255, 128]))),
        ));
        // Fully hidden transparency must not affect the image.
        mesh.extend(quad(
            5.0,
            Arc::new(RgbaImage::from_pixel(1, 1, Rgba([255, 255, 255, 128]))),
        ));
        mesh
    };
    let expected = trace::draw(make_mesh(), &camera, small());
    let mut mesh = make_mesh();
    for _ in 0..2 {
        let image = crate::raster::draw(&mesh, &camera, small());
        assert_images_close(&image, &expected, 0);
        assert_eq!(image.get_pixel(4, 4).0, [128, 63, 63, 255]);
        mesh.reverse();
    }
}

#[test]
fn raster_clipping_perspective_uv_cutouts_and_far_distance_match_ray_reference() {
    let options = Options {
        width: 87,
        height: 61,
        far: 4.2,
    };
    let camera = Camera {
        eye: [0.0; 3],
        ..scene(vec![]).camera
    };
    let mut texture = RgbaImage::new(8, 8);
    for (x, y, pixel) in texture.enumerate_pixels_mut() {
        *pixel = Rgba([
            (x * 31) as u8,
            (y * 31) as u8,
            127,
            if (x + y) % 3 == 0 { 0 } else { 255 },
        ]);
    }
    let texture = Arc::new(texture);
    // Triangle crossing the camera plane, an oblique textured surface, one crossing
    // the radial far limit, one behind the camera, and a degenerate triangle.
    let positions = [
        [[-0.73, -0.61, -0.3], [1.33, -0.47, 2.7], [-0.62, 1.57, 2.1]],
        [[-2.01, -1.13, 1.8], [2.31, -0.93, 3.9], [0.27, 2.13, 3.2]],
        [[-3.01, -1.1, 3.6], [3.41, -1.21, 4.1], [0.23, 3.47, 4.9]],
        [[-1.0, -1.0, -2.0], [1.0, -1.0, -2.0], [0.0, 1.0, -2.0]],
        [[0.0, 0.0, 2.0]; 3],
    ];
    let mesh = || {
        positions
            .map(|p| triangle(p, texture.clone()))
            .into_iter()
            .collect::<Vec<_>>()
    };
    let expected = trace::draw(mesh(), &camera, options);
    let actual = crate::raster::draw(&mesh(), &camera, options);
    assert_images_close(&actual, &expected, 0);
}

#[test]
fn many_translucent_layers_are_bounded_and_keep_the_nearest_surfaces() {
    let camera = Camera {
        eye: [0.0; 3],
        ..scene(vec![]).camera
    };
    let make_mesh = || {
        (0..25)
            .rev()
            .flat_map(|i| {
                quad(
                    1.0 + f64::from(i) * 0.1,
                    Arc::new(RgbaImage::from_pixel(
                        1,
                        1,
                        Rgba([(i * 10) as u8, 20, 230, 20]),
                    )),
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = trace::draw(make_mesh(), &camera, small());
    let actual = crate::raster::draw(&make_mesh(), &camera, small());
    assert_images_close(&actual, &expected, 0);
}

/// Manual performance comparison, never downloads or embeds game resources.
#[test]
#[ignore = "requires MINEINTENT_CLIENT_JAR pointing to local 26.1.2 resources"]
fn benchmark_ray_and_raster_with_local_resources() {
    use std::time::Instant;
    let path = std::env::var("MINEINTENT_CLIENT_JAR").expect("set MINEINTENT_CLIENT_JAR");
    let fixture = crate::fixture();
    let mut extended = fixture.clone();
    extended.blocks.clear();
    for x in -1..=1 {
        for z in -1..=1 {
            extended
                .blocks
                .extend(fixture.blocks.iter().cloned().map(|mut b| {
                    b.position[0] += x * 17;
                    b.position[2] += z * 18;
                    b
                }));
        }
    }
    for (name, scene) in [("fixture", fixture), ("extended", extended)] {
        let mut resources = Resources::open(&path).unwrap();
        let options = Options::default();
        // Warm both paths. Timed pairs share the same cached resource set, and
        // alternate execution order to reduce machine-load/order bias.
        trace::render(&scene, &mut resources, options).unwrap();
        render(&scene, &mut resources, options).unwrap();
        let mut times = [Vec::new(), Vec::new()];
        for i in 0..10 {
            for which in [i % 2, 1 - i % 2] {
                let start = Instant::now();
                let frame = if which == 0 {
                    trace::render(&scene, &mut resources, options).unwrap()
                } else {
                    render(&scene, &mut resources, options).unwrap()
                };
                times[which].push(start.elapsed().as_secs_f64() * 1000.0);
                std::hint::black_box(frame);
            }
        }
        println!(
            "{}",
            json!({"scene":name,"blocks":scene.blocks.len(),
            "ray_ms":times[0],"raster_ms":times[1], "width":640,"height":360})
        );
    }
}
