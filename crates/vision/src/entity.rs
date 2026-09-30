//! 实体成像：按 26.1.2 客户端的模型定义转录几何，套原版贴图。
//!
//! 几何口径逐项对齐原版：
//! - 方块体与 UV：`ModelPart.Cube`（`texOffs` 展开、`CubeDeformation` 外扩、`mirror` 交换 X 两端）。
//! - 部件变换：`ModelPart.translateAndRotate`——先平移 `offset/16`，再按 Z·Y·X 旋转。
//! - 实体变换：`LivingEntityRenderer.submit`——绕 Y 转 `180 - bodyYaw`，`scale(-1,-1,1)`，
//!   再下移 1.501 格（模型原点在脚下 24 像素处，y 轴朝下）。
//! - 头部：`setupAnim` 的 `head.xRot = pitch`、`head.yRot = 头相对身体的偏转`。
//!
//! 不做的（诚实列在报告里）：行走摆臂等动画、幼年模型、变种贴图（冷/暖）、
//! 装备与手持物、受伤变红、光照。认不出的实体画成与碰撞箱等大的占位块。

use std::f64::consts::FRAC_PI_2;
use std::sync::Arc;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::assets::missing_texture;
use crate::geometry::{add, Triangle, V3};
use crate::{Report, Resources};

/// 场景里的一个实体。朝向用原版角度制（yaw 0 = 朝南，90 = 朝西；pitch 向下为正）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entity {
    /// 注册名，带不带 `minecraft:` 前缀都行。
    pub kind: String,
    /// 脚底中心。
    pub position: [f64; 3],
    pub body_yaw: f64,
    #[serde(default)]
    pub head_yaw: Option<f64>,
    #[serde(default)]
    pub pitch: f64,
    /// 碰撞箱，只用于认不出的实体的占位块。
    pub width: f64,
    pub height: f64,
    /// 玩家的 UUID（按原版 `DefaultPlayerSkin` 选默认皮肤）。
    #[serde(default)]
    pub uuid: Option<String>,
    /// 掉落物（`item`）是什么物品。
    #[serde(default)]
    pub item: Option<String>,
}

struct Cube {
    tex: [f64; 2],
    min: V3,
    size: V3,
    grow: f64,
    mirror: bool,
}

struct Part {
    name: &'static str,
    offset: V3,
    rotation: V3,
    cubes: Vec<Cube>,
    children: Vec<Part>,
}

/// 一层模型：一张贴图、一棵部件树。一个实体可以有多层（羊毛）。
struct Layer {
    texture: String,
    texture_size: [f64; 2],
    parts: Vec<Part>,
}

fn cube(tex: [u32; 2], min: V3, size: V3) -> Cube {
    Cube {
        tex: tex.map(f64::from),
        min,
        size,
        grow: 0.0,
        mirror: false,
    }
}

impl Cube {
    fn grow(mut self, grow: f64) -> Self {
        self.grow = grow;
        self
    }
    fn mirrored(mut self) -> Self {
        self.mirror = true;
        self
    }
}

fn part(name: &'static str, offset: V3, cubes: Vec<Cube>) -> Part {
    Part {
        name,
        offset,
        rotation: [0.0; 3],
        cubes,
        children: Vec::new(),
    }
}

impl Part {
    fn rotated(mut self, rotation: V3) -> Self {
        self.rotation = rotation;
        self
    }
    fn with(mut self, child: Part) -> Self {
        self.children.push(child);
        self
    }
}

fn replace(parts: &mut Vec<Part>, new: Part) {
    match parts.iter_mut().find(|part| part.name == new.name) {
        Some(slot) => *slot = new,
        None => parts.push(new),
    }
}

fn child_of<'a>(parts: &'a mut [Part], name: &str) -> &'a mut Part {
    parts
        .iter_mut()
        .find(|part| part.name == name)
        .expect("转录的模型里有这个部件")
}

// ---- 模型定义（逐个对照 26.1.2 客户端同名类的 createBodyLayer/createMesh） ----

/// `QuadrupedModel.createBodyMesh`
fn quadruped(leg: f64, mirror_left: bool, mirror_right: bool) -> Vec<Part> {
    let leg_cube = |mirror: bool| {
        let cube = cube([0, 16], [-2.0, 0.0, -2.0], [4.0, leg, 4.0]);
        if mirror {
            cube.mirrored()
        } else {
            cube
        }
    };
    vec![
        part(
            "head",
            [0.0, 18.0 - leg, -6.0],
            vec![cube([0, 0], [-4.0, -4.0, -8.0], [8.0, 8.0, 8.0])],
        ),
        part(
            "body",
            [0.0, 17.0 - leg, 2.0],
            vec![cube([28, 8], [-5.0, -10.0, -7.0], [10.0, 16.0, 8.0])],
        )
        .rotated([FRAC_PI_2, 0.0, 0.0]),
        part(
            "right_hind_leg",
            [-3.0, 24.0 - leg, 7.0],
            vec![leg_cube(mirror_right)],
        ),
        part(
            "left_hind_leg",
            [3.0, 24.0 - leg, 7.0],
            vec![leg_cube(mirror_left)],
        ),
        part(
            "right_front_leg",
            [-3.0, 24.0 - leg, -5.0],
            vec![leg_cube(mirror_right)],
        ),
        part(
            "left_front_leg",
            [3.0, 24.0 - leg, -5.0],
            vec![leg_cube(mirror_left)],
        ),
    ]
}

/// `PigModel.createBasePigModel`
fn pig() -> Vec<Part> {
    let mut parts = quadruped(6.0, true, false);
    replace(
        &mut parts,
        part(
            "head",
            [0.0, 12.0, -6.0],
            vec![
                cube([0, 0], [-4.0, -4.0, -8.0], [8.0, 8.0, 8.0]),
                cube([16, 16], [-2.0, 0.0, -9.0], [4.0, 3.0, 1.0]),
            ],
        ),
    );
    parts
}

/// `CowModel.createBaseCowModel`
fn cow() -> Vec<Part> {
    let leg = |mirror: bool| {
        let cube = cube([0, 16], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]);
        if mirror {
            cube.mirrored()
        } else {
            cube
        }
    };
    vec![
        part(
            "head",
            [0.0, 4.0, -8.0],
            vec![
                cube([0, 0], [-4.0, -4.0, -6.0], [8.0, 8.0, 6.0]),
                cube([1, 33], [-3.0, 1.0, -7.0], [6.0, 3.0, 1.0]),
                cube([22, 0], [-5.0, -5.0, -5.0], [1.0, 3.0, 1.0]),
                cube([22, 0], [4.0, -5.0, -5.0], [1.0, 3.0, 1.0]),
            ],
        ),
        part(
            "body",
            [0.0, 5.0, 2.0],
            vec![
                cube([18, 4], [-6.0, -10.0, -7.0], [12.0, 18.0, 10.0]),
                cube([52, 0], [-2.0, 2.0, -8.0], [4.0, 6.0, 1.0]),
            ],
        )
        .rotated([FRAC_PI_2, 0.0, 0.0]),
        part("right_hind_leg", [-4.0, 12.0, 7.0], vec![leg(false)]),
        part("left_hind_leg", [4.0, 12.0, 7.0], vec![leg(true)]),
        part("right_front_leg", [-4.0, 12.0, -5.0], vec![leg(false)]),
        part("left_front_leg", [4.0, 12.0, -5.0], vec![leg(true)]),
    ]
}

/// `SheepModel.createBodyLayer`
fn sheep() -> Vec<Part> {
    let mut parts = quadruped(12.0, false, true);
    replace(
        &mut parts,
        part(
            "head",
            [0.0, 6.0, -8.0],
            vec![cube([0, 0], [-3.0, -4.0, -6.0], [6.0, 6.0, 8.0])],
        ),
    );
    replace(
        &mut parts,
        part(
            "body",
            [0.0, 5.0, 2.0],
            vec![cube([28, 8], [-4.0, -10.0, -7.0], [8.0, 16.0, 6.0])],
        )
        .rotated([FRAC_PI_2, 0.0, 0.0]),
    );
    parts
}

/// `SheepFurModel.createFurLayer`
fn sheep_wool() -> Vec<Part> {
    let leg = || cube([0, 16], [-2.0, 0.0, -2.0], [4.0, 6.0, 4.0]).grow(0.5);
    vec![
        part(
            "head",
            [0.0, 6.0, -8.0],
            vec![cube([0, 0], [-3.0, -4.0, -4.0], [6.0, 6.0, 6.0]).grow(0.6)],
        ),
        part(
            "body",
            [0.0, 5.0, 2.0],
            vec![cube([28, 8], [-4.0, -10.0, -7.0], [8.0, 16.0, 6.0]).grow(1.75)],
        )
        .rotated([FRAC_PI_2, 0.0, 0.0]),
        part("right_hind_leg", [-3.0, 12.0, 7.0], vec![leg()]),
        part("left_hind_leg", [3.0, 12.0, 7.0], vec![leg()]),
        part("right_front_leg", [-3.0, 12.0, -5.0], vec![leg()]),
        part("left_front_leg", [3.0, 12.0, -5.0], vec![leg()]),
    ]
}

/// `AdultChickenModel.createBaseChickenModel`
fn chicken() -> Vec<Part> {
    let leg = || cube([26, 0], [-1.0, 0.0, -3.0], [3.0, 5.0, 3.0]);
    vec![
        part(
            "head",
            [0.0, 15.0, -4.0],
            vec![cube([0, 0], [-2.0, -6.0, -2.0], [4.0, 6.0, 3.0])],
        )
        .with(part(
            "beak",
            [0.0; 3],
            vec![cube([14, 0], [-2.0, -4.0, -4.0], [4.0, 2.0, 2.0])],
        ))
        .with(part(
            "red_thing",
            [0.0; 3],
            vec![cube([14, 4], [-1.0, -2.0, -3.0], [2.0, 2.0, 2.0])],
        )),
        part(
            "body",
            [0.0, 16.0, 0.0],
            vec![cube([0, 9], [-3.0, -4.0, -3.0], [6.0, 8.0, 6.0])],
        )
        .rotated([FRAC_PI_2, 0.0, 0.0]),
        part("right_leg", [-2.0, 19.0, 1.0], vec![leg()]),
        part("left_leg", [1.0, 19.0, 1.0], vec![leg()]),
        part(
            "right_wing",
            [-4.0, 13.0, 0.0],
            vec![cube([24, 13], [0.0, 0.0, -3.0], [1.0, 4.0, 6.0])],
        ),
        part(
            "left_wing",
            [4.0, 13.0, 0.0],
            vec![cube([24, 13], [-1.0, 0.0, -3.0], [1.0, 4.0, 6.0])],
        ),
    ]
}

/// `CreeperModel.createBodyLayer`
fn creeper() -> Vec<Part> {
    let leg = || cube([0, 16], [-2.0, 0.0, -2.0], [4.0, 6.0, 4.0]);
    vec![
        part(
            "head",
            [0.0, 6.0, 0.0],
            vec![cube([0, 0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0])],
        ),
        part(
            "body",
            [0.0, 6.0, 0.0],
            vec![cube([16, 16], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0])],
        ),
        part("right_hind_leg", [-2.0, 18.0, 4.0], vec![leg()]),
        part("left_hind_leg", [2.0, 18.0, 4.0], vec![leg()]),
        part("right_front_leg", [-2.0, 18.0, -4.0], vec![leg()]),
        part("left_front_leg", [2.0, 18.0, -4.0], vec![leg()]),
    ]
}

/// `SpiderModel.createSpiderBodyLayer`
fn spider() -> Vec<Part> {
    use std::f64::consts::{FRAC_PI_4, FRAC_PI_8};
    let right = || cube([18, 0], [-15.0, -1.0, -1.0], [16.0, 2.0, 2.0]);
    let left = || cube([18, 0], [-1.0, -1.0, -1.0], [16.0, 2.0, 2.0]).mirrored();
    let middle = 0.581_194_64;
    vec![
        part(
            "head",
            [0.0, 15.0, -3.0],
            vec![cube([32, 4], [-4.0, -4.0, -8.0], [8.0, 8.0, 8.0])],
        ),
        part(
            "body0",
            [0.0, 15.0, 0.0],
            vec![cube([0, 0], [-3.0, -3.0, -3.0], [6.0, 6.0, 6.0])],
        ),
        part(
            "body1",
            [0.0, 15.0, 9.0],
            vec![cube([0, 12], [-5.0, -4.0, -6.0], [10.0, 8.0, 12.0])],
        ),
        part("right_hind_leg", [-4.0, 15.0, 2.0], vec![right()])
            .rotated([0.0, FRAC_PI_4, -FRAC_PI_4]),
        part("left_hind_leg", [4.0, 15.0, 2.0], vec![left()]).rotated([0.0, -FRAC_PI_4, FRAC_PI_4]),
        part("right_middle_hind_leg", [-4.0, 15.0, 1.0], vec![right()])
            .rotated([0.0, FRAC_PI_8, -middle]),
        part("left_middle_hind_leg", [4.0, 15.0, 1.0], vec![left()])
            .rotated([0.0, -FRAC_PI_8, middle]),
        part("right_middle_front_leg", [-4.0, 15.0, 0.0], vec![right()])
            .rotated([0.0, -FRAC_PI_8, -middle]),
        part("left_middle_front_leg", [4.0, 15.0, 0.0], vec![left()])
            .rotated([0.0, FRAC_PI_8, middle]),
        part("right_front_leg", [-4.0, 15.0, -1.0], vec![right()])
            .rotated([0.0, -FRAC_PI_4, -FRAC_PI_4]),
        part("left_front_leg", [4.0, 15.0, -1.0], vec![left()])
            .rotated([0.0, FRAC_PI_4, FRAC_PI_4]),
    ]
}

/// `HumanoidModel.createMesh(NONE, 0)`
fn humanoid() -> Vec<Part> {
    vec![
        part(
            "head",
            [0.0; 3],
            vec![cube([0, 0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0])],
        )
        .with(part(
            "hat",
            [0.0; 3],
            vec![cube([32, 0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0]).grow(0.5)],
        )),
        part(
            "body",
            [0.0; 3],
            vec![cube([16, 16], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0])],
        ),
        part(
            "right_arm",
            [-5.0, 2.0, 0.0],
            vec![cube([40, 16], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0])],
        ),
        part(
            "left_arm",
            [5.0, 2.0, 0.0],
            vec![cube([40, 16], [-1.0, -2.0, -2.0], [4.0, 12.0, 4.0]).mirrored()],
        ),
        part(
            "right_leg",
            [-1.9, 12.0, 0.0],
            vec![cube([0, 16], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0])],
        ),
        part(
            "left_leg",
            [1.9, 12.0, 0.0],
            vec![cube([0, 16], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]).mirrored()],
        ),
    ]
}

/// 僵尸：人形网格 + `AnimationUtils.animateZombieArms` 的静止姿态（非攻击：
/// 两臂前伸 `xRot = -π/2.25`，`yRot` 各向外 0.1）。原版僵尸永远这样举着手，认它靠这个。
fn zombie() -> Vec<Part> {
    let mut parts = humanoid();
    let arm_drop = -std::f64::consts::PI / 2.25;
    child_of(&mut parts, "right_arm").rotation = [arm_drop, -0.1, 0.0];
    child_of(&mut parts, "left_arm").rotation = [arm_drop, 0.1, 0.0];
    parts
}

/// `SkeletonModel.createBodyLayer`（细胳膊细腿）
fn skeleton() -> Vec<Part> {
    let mut parts = humanoid();
    let thin = |tex: [u32; 2], min: V3| cube(tex, min, [2.0, 12.0, 2.0]);
    replace(
        &mut parts,
        part(
            "right_arm",
            [-5.0, 2.0, 0.0],
            vec![thin([40, 16], [-1.0, -2.0, -1.0])],
        ),
    );
    replace(
        &mut parts,
        part(
            "left_arm",
            [5.0, 2.0, 0.0],
            vec![thin([40, 16], [-1.0, -2.0, -1.0]).mirrored()],
        ),
    );
    replace(
        &mut parts,
        part(
            "right_leg",
            [-2.0, 12.0, 0.0],
            vec![thin([0, 16], [-1.0, 0.0, -1.0])],
        ),
    );
    replace(
        &mut parts,
        part(
            "left_leg",
            [2.0, 12.0, 0.0],
            vec![thin([0, 16], [-1.0, 0.0, -1.0]).mirrored()],
        ),
    );
    parts
}

/// `PlayerModel.createMesh(NONE, slim)`：外层（帽子、袖子、裤腿、外套）一并画。
fn player(slim: bool) -> Vec<Part> {
    let mut parts = humanoid();
    let arm = if slim { 3.0 } else { 4.0 };
    replace(
        &mut parts,
        part(
            "left_arm",
            [5.0, 2.0, 0.0],
            vec![cube([32, 48], [-1.0, -2.0, -2.0], [arm, 12.0, 4.0])],
        )
        .with(part(
            "left_sleeve",
            [0.0; 3],
            vec![cube([48, 48], [-1.0, -2.0, -2.0], [arm, 12.0, 4.0]).grow(0.25)],
        )),
    );
    let right_min = if slim { -2.0 } else { -3.0 };
    replace(
        &mut parts,
        part(
            "right_arm",
            [-5.0, 2.0, 0.0],
            vec![cube([40, 16], [right_min, -2.0, -2.0], [arm, 12.0, 4.0])],
        )
        .with(part(
            "right_sleeve",
            [0.0; 3],
            vec![cube([40, 32], [right_min, -2.0, -2.0], [arm, 12.0, 4.0]).grow(0.25)],
        )),
    );
    replace(
        &mut parts,
        part(
            "left_leg",
            [1.9, 12.0, 0.0],
            vec![cube([16, 48], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0])],
        )
        .with(part(
            "left_pants",
            [0.0; 3],
            vec![cube([0, 48], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]).grow(0.25)],
        )),
    );
    child_of(&mut parts, "right_leg").children.push(part(
        "right_pants",
        [0.0; 3],
        vec![cube([0, 32], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]).grow(0.25)],
    ));
    child_of(&mut parts, "body").children.push(part(
        "jacket",
        [0.0; 3],
        vec![cube([16, 32], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]).grow(0.25)],
    ));
    parts
}

/// 原版 `DefaultPlayerSkin.DEFAULT_SKINS`，顺序即下标。
const DEFAULT_SKINS: [(&str, bool); 18] = [
    ("slim/alex", true),
    ("slim/ari", true),
    ("slim/efe", true),
    ("slim/kai", true),
    ("slim/makena", true),
    ("slim/noor", true),
    ("slim/steve", true),
    ("slim/sunny", true),
    ("slim/zuri", true),
    ("wide/alex", false),
    ("wide/ari", false),
    ("wide/efe", false),
    ("wide/kai", false),
    ("wide/makena", false),
    ("wide/noor", false),
    ("wide/steve", false),
    ("wide/sunny", false),
    ("wide/zuri", false),
];

/// `DefaultPlayerSkin.get(uuid)`：`floorMod(uuid.hashCode(), 18)`；
/// Java `UUID.hashCode` = 高低 64 位异或后再把两半 32 位异或。没有 UUID 用 `getDefaultSkin`（下标 6）。
fn default_skin(uuid: Option<&str>) -> (&'static str, bool) {
    let index = uuid
        .and_then(|uuid| u128::from_str_radix(&uuid.replace('-', ""), 16).ok())
        .map(|bits| {
            let xor = ((bits >> 64) as u64) ^ (bits as u64);
            let hash = ((xor >> 32) as u32 ^ xor as u32) as i32;
            hash.rem_euclid(DEFAULT_SKINS.len() as i32) as usize
        })
        .unwrap_or(6);
    DEFAULT_SKINS[index]
}

fn layers(entity: &Entity) -> Option<Vec<Layer>> {
    let kind = entity
        .kind
        .strip_prefix("minecraft:")
        .unwrap_or(&entity.kind);
    let layer = |texture: &str, size: [f64; 2], parts: Vec<Part>| Layer {
        texture: format!("minecraft:entity/{texture}"),
        texture_size: size,
        parts,
    };
    Some(match kind {
        "pig" => vec![layer("pig/pig_temperate", [64.0, 64.0], pig())],
        "cow" => vec![layer("cow/cow_temperate", [64.0, 64.0], cow())],
        "sheep" => vec![
            layer("sheep/sheep", [64.0, 32.0], sheep()),
            layer("sheep/sheep_wool", [64.0, 32.0], sheep_wool()),
        ],
        "chicken" => vec![layer("chicken/chicken_temperate", [64.0, 32.0], chicken())],
        "creeper" => vec![layer("creeper/creeper", [64.0, 32.0], creeper())],
        "spider" => vec![layer("spider/spider", [64.0, 32.0], spider())],
        "zombie" => vec![layer("zombie/zombie", [64.0, 64.0], zombie())],
        "skeleton" => vec![layer("skeleton/skeleton", [64.0, 32.0], skeleton())],
        "player" => {
            let (skin, slim) = default_skin(entity.uuid.as_deref());
            vec![layer(&format!("player/{skin}"), [64.0, 64.0], player(slim))]
        }
        _ => return None,
    })
}

// ---- 变换 ----

type M3 = [[f64; 3]; 3];

fn mat_mul(a: M3, b: M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

fn mat_apply(m: M3, v: V3) -> V3 {
    std::array::from_fn(|r| (0..3).map(|k| m[r][k] * v[k]).sum())
}

fn rot_x(a: f64) -> M3 {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}
fn rot_y(a: f64) -> M3 {
    let (s, c) = a.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}
fn rot_z(a: f64) -> M3 {
    let (s, c) = a.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// 仿射变换：`p ↦ linear·p + translation`。
#[derive(Clone, Copy)]
struct Pose {
    linear: M3,
    translation: V3,
}

impl Pose {
    fn apply(&self, p: V3) -> V3 {
        add(mat_apply(self.linear, p), self.translation)
    }
    /// 等价于 PoseStack 上先 `translate(t)` 再 `mulPose(m)`：新点先过 m 再过 t 再过旧姿态。
    fn then(&self, t: V3, m: M3) -> Pose {
        Pose {
            linear: mat_mul(self.linear, m),
            translation: self.apply(t),
        }
    }
}

const SHADE: [(V3, f64); 6] = [
    ([0.0, 1.0, 0.0], 1.0),
    ([0.0, -1.0, 0.0], 0.5),
    ([0.0, 0.0, -1.0], 0.8),
    ([0.0, 0.0, 1.0], 0.8),
    ([1.0, 0.0, 0.0], 0.6),
    ([-1.0, 0.0, 0.0], 0.6),
];

/// 按世界朝向给面上固定明暗（与方块同一套近似，不是原版实体光照）。
fn shade(normal: V3) -> f64 {
    SHADE
        .iter()
        .map(|(axis, value)| {
            let d = (0..3).map(|i| axis[i] * normal[i]).sum::<f64>().max(0.0);
            d * d * value
        })
        .sum::<f64>()
        .clamp(0.4, 1.0)
}

fn emit_cube(
    cube: &Cube,
    pose: &Pose,
    texture: &Arc<RgbaImage>,
    size: [f64; 2],
    triangles: &mut Vec<Triangle>,
) {
    let [w, h, d] = cube.size;
    let g = cube.grow;
    let (mut x0, y0, z0) = (cube.min[0] - g, cube.min[1] - g, cube.min[2] - g);
    let (mut x1, y1, z1) = (
        cube.min[0] + w + g,
        cube.min[1] + h + g,
        cube.min[2] + d + g,
    );
    if cube.mirror {
        std::mem::swap(&mut x0, &mut x1);
    }
    let t0 = [x0, y0, z0];
    let t1 = [x1, y0, z0];
    let t2 = [x1, y1, z0];
    let t3 = [x0, y1, z0];
    let l0 = [x0, y0, z1];
    let l1 = [x1, y0, z1];
    let l2 = [x1, y1, z1];
    let l3 = [x0, y1, z1];
    let [u, v] = cube.tex;
    let (u0, u1, u2, u22, u3, u4) = (
        u,
        u + d,
        u + d + w,
        u + d + w + w,
        u + d + w + d,
        u + d + w + d + w,
    );
    let (v0, v1, v2) = (v, v + d, v + d + h);
    // (顶点, u0, v0, u1, v1, 模型空间法线)，顺序与 ModelPart.Cube 相同。
    let faces: [([V3; 4], f64, f64, f64, f64, V3); 6] = [
        ([l1, l0, t0, t1], u1, v0, u2, v1, [0.0, -1.0, 0.0]),
        ([t2, t3, l3, l2], u2, v1, u22, v0, [0.0, 1.0, 0.0]),
        ([t0, l0, l3, t3], u0, v1, u1, v2, [-1.0, 0.0, 0.0]),
        ([t1, t0, t3, t2], u1, v1, u2, v2, [0.0, 0.0, -1.0]),
        ([l1, t1, t2, l2], u2, v1, u3, v2, [1.0, 0.0, 0.0]),
        ([l0, l1, l2, l3], u3, v1, u4, v2, [0.0, 0.0, 1.0]),
    ];
    for (vertices, fu0, fv0, fu1, fv1, normal) in faces {
        // Polygon 构造：v0←(u1,v0) v1←(u0,v0) v2←(u0,v1) v3←(u1,v1)，归一到贴图尺寸。
        // 光栅器按「宽 16 单位」采样，所以换算成 0..16。
        let scale = |pu: f64, pv: f64| [pu / size[0] * 16.0, pv / size[1] * 16.0];
        let uv = [
            scale(fu1, fv0),
            scale(fu0, fv0),
            scale(fu0, fv1),
            scale(fu1, fv1),
        ];
        let world = vertices.map(|p| pose.apply(p.map(|c| c / 16.0)));
        let world_normal = mat_apply(pose.linear, normal);
        let length = (0..3)
            .map(|i| world_normal[i] * world_normal[i])
            .sum::<f64>()
            .sqrt();
        let light = shade(world_normal.map(|n| n / length.max(1e-9)));
        for indices in [[0, 1, 2], [0, 2, 3]] {
            triangles.push(Triangle {
                vertices: indices.map(|i| world[i]),
                uv: indices.map(|i| uv[i]),
                texture: texture.clone(),
                color: [light; 3],
                alpha: 1.0,
            });
        }
    }
}

fn emit_part(
    part: &Part,
    parent: &Pose,
    head: (f64, f64),
    texture: &Arc<RgbaImage>,
    size: [f64; 2],
    triangles: &mut Vec<Triangle>,
) {
    let mut rotation = part.rotation;
    if part.name == "head" {
        rotation[0] += head.0;
        rotation[1] += head.1;
    }
    let pose = parent.then(
        part.offset.map(|c| c / 16.0),
        mat_mul(
            mat_mul(rot_z(rotation[2]), rot_y(rotation[1])),
            rot_x(rotation[0]),
        ),
    );
    for cube in &part.cubes {
        emit_cube(cube, &pose, texture, size, triangles);
    }
    for child in &part.children {
        emit_part(child, &pose, (0.0, 0.0), texture, size, triangles);
    }
}

fn wrap_degrees(angle: f64) -> f64 {
    let wrapped = angle.rem_euclid(360.0);
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// 把场景里的实体变成三角形。认不出的实体画成碰撞箱大小的占位块并记入报告。
pub(crate) fn build(
    entities: &[Entity],
    eye: V3,
    resources: &mut Resources,
    report: &mut Report,
) -> Vec<Triangle> {
    let mut triangles = Vec::new();
    for entity in entities {
        let valid = entity.position.iter().all(|c| c.is_finite())
            && entity.body_yaw.is_finite()
            && entity.pitch.is_finite();
        if !valid {
            report
                .warnings
                .insert(format!("{}: non-finite pose, not drawn", entity.kind));
            continue;
        }
        let kind = entity
            .kind
            .strip_prefix("minecraft:")
            .unwrap_or(&entity.kind);
        if kind == "item" {
            if !dropped_item(entity, eye, resources, report, &mut triangles) {
                placeholder(entity, &mut triangles);
            }
            continue;
        }
        let Some(layers) = layers(entity) else {
            report.warnings.insert(format!(
                "{}: no entity model yet, drawn as hitbox placeholder",
                entity.kind
            ));
            placeholder(entity, &mut triangles);
            continue;
        };
        // LivingEntityRenderer：平移到脚底 → 绕 Y 转 180-bodyYaw → scale(-1,-1,1) → 下移 1.501。
        let root = Pose {
            linear: rot_y((180.0 - entity.body_yaw).to_radians()),
            translation: entity.position,
        };
        let flip = [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]];
        let root = root
            .then([0.0; 3], flip)
            .then([0.0, -1.501, 0.0], rot_x(0.0));
        let head_yaw = wrap_degrees(entity.head_yaw.unwrap_or(entity.body_yaw) - entity.body_yaw);
        let head = (entity.pitch.to_radians(), head_yaw.to_radians());
        for layer in layers {
            let texture = resources.texture(&layer.texture).unwrap_or_else(|error| {
                report
                    .warnings
                    .insert(format!("{}: texture unavailable ({error})", entity.kind));
                missing_texture()
            });
            for part in &layer.parts {
                emit_part(
                    part,
                    &root,
                    head,
                    &texture,
                    layer.texture_size,
                    &mut triangles,
                );
            }
        }
    }
    report.warnings.insert(
        "entities: static pose (no walk/limb animation), adult model, default variant texture, no equipment or held items".to_owned(),
    );
    triangles
}

/// 掉落物。方块物品按方块自己的模型缩到 1/4（原版 ground 变换的尺度），平面物品画成
/// 半格高、正对镜头的物品贴图。原版掉落物会上下浮动、原地旋转、按数量叠几份，
/// 静态图里都不画；正对镜头是为了单张图里认得出，不是原版姿态。
fn dropped_item(
    entity: &Entity,
    eye: V3,
    resources: &mut Resources,
    report: &mut Report,
    triangles: &mut Vec<Triangle>,
) -> bool {
    let Some(name) = entity.item.as_deref() else {
        report
            .warnings
            .insert("item: item kind unknown, drawn as hitbox placeholder".to_owned());
        return false;
    };
    report.warnings.insert(
        "dropped items: static, single copy; flat items face the camera, block items are 1/4-scale blocks".to_owned(),
    );
    let [x, y, z] = entity.position;
    let lift = 0.1;
    let block = crate::geometry::block_item_triangles(name, resources, report, |p| {
        [
            x + (p[0] - 0.5) * 0.25,
            y + lift + p[1] * 0.25,
            z + (p[2] - 0.5) * 0.25,
        ]
    });
    if let Ok(block) = block {
        triangles.extend(block);
        return true;
    }
    let Ok(texture) = resources.texture(&format!("minecraft:item/{name}")) else {
        report.warnings.insert(format!(
            "item {name}: no block model or item texture, drawn as placeholder"
        ));
        return false;
    };
    // 水平方向朝向镜头的竖直方片。
    let to_eye = [eye[0] - x, 0.0, eye[2] - z];
    let length = (to_eye[0] * to_eye[0] + to_eye[2] * to_eye[2])
        .sqrt()
        .max(1e-9);
    let right = [to_eye[2] / length * 0.25, 0.0, -to_eye[0] / length * 0.25];
    let bottom = y + lift;
    let top = bottom + 0.5;
    let corner = |side: f64, height: f64| [x + right[0] * side, height, z + right[2] * side];
    let vertices = [
        corner(-1.0, top),
        corner(1.0, top),
        corner(1.0, bottom),
        corner(-1.0, bottom),
    ];
    let uv = [[0.0, 0.0], [16.0, 0.0], [16.0, 16.0], [0.0, 16.0]];
    for indices in [[0, 1, 2], [0, 2, 3]] {
        triangles.push(Triangle {
            vertices: indices.map(|i| vertices[i]),
            uv: indices.map(|i| uv[i]),
            texture: texture.clone(),
            color: [1.0; 3],
            alpha: 1.0,
        });
    }
    true
}

/// 认不出的实体：碰撞箱大小的紫黑块，和认不出的方块一样显眼，不假装知道它长什么样。
fn placeholder(entity: &Entity, triangles: &mut Vec<Triangle>) {
    let half = entity.width.max(0.1) / 2.0;
    let height = entity.height.max(0.1);
    let min = [-half, 0.0, -half];
    let size = [half * 2.0, height, half * 2.0];
    let pose = Pose {
        linear: rot_y(0.0),
        translation: entity.position,
    };
    let texture = missing_texture();
    // 复用方块体展开，贴图坐标铺满 0..16。
    let cube = Cube {
        tex: [0.0, 0.0],
        min: min.map(|c| c * 16.0),
        size: size.map(|c| c * 16.0),
        grow: 0.0,
        mirror: false,
    };
    let span = size.map(|c| c * 16.0);
    let texture_size = [span[2] * 2.0 + span[0] * 2.0, span[2] + span[1]];
    emit_cube(&cube, &pose, &texture, texture_size, triangles);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skin_matches_java_uuid_hash() {
        // Java: new UUID(0L, 0L).hashCode() == 0 → 下标 0（slim/alex）。
        assert_eq!(
            default_skin(Some("00000000-0000-0000-0000-000000000000")),
            ("slim/alex", true)
        );
        // hi=0, lo=7 → hash 7 → wide? 下标 7 = slim/sunny。
        assert_eq!(
            default_skin(Some("00000000-0000-0000-0000-000000000007")),
            ("slim/sunny", true)
        );
        // hi=0, lo=0x0000_0001_0000_0000 → 高低 32 位异或 = 1 → 下标 1。
        assert_eq!(
            default_skin(Some("00000000-0000-0000-0000-000100000000")),
            ("slim/ari", true)
        );
        // 负数取 floorMod：-1 mod 18 = 17 → wide/zuri。
        assert_eq!(
            default_skin(Some("00000000-0000-0000-0000-0000ffffffff")),
            ("wide/zuri", false)
        );
        assert_eq!(default_skin(None), ("slim/steve", true));
    }

    #[test]
    fn a_pig_standing_at_the_origin_occupies_its_vanilla_footprint() {
        let entity = Entity {
            kind: "pig".to_owned(),
            position: [0.0; 3],
            body_yaw: 0.0,
            head_yaw: None,
            pitch: 0.0,
            width: 0.9,
            height: 0.9,
            uuid: None,
            item: None,
        };
        let mut triangles = Vec::new();
        let layers = layers(&entity).unwrap();
        let root = Pose {
            linear: rot_y(180.0_f64.to_radians()),
            translation: entity.position,
        }
        .then(
            [0.0; 3],
            [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .then([0.0, -1.501, 0.0], rot_x(0.0));
        let texture = missing_texture();
        for part in &layers[0].parts {
            emit_part(
                part,
                &root,
                (0.0, 0.0),
                &texture,
                [64.0, 64.0],
                &mut triangles,
            );
        }
        let points: Vec<V3> = triangles.iter().flat_map(|t| t.vertices).collect();
        let min_y = points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let max_y = points
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let max_z = points
            .iter()
            .map(|p| p[2])
            .fold(f64::NEG_INFINITY, f64::max);
        let min_z = points.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
        // 腿底比脚底高 0.001（原版 1.501 与 24 像素之差），头顶在 16 像素（1 格）处。
        assert!((min_y - 0.001).abs() < 1e-6, "{min_y}");
        assert!((max_y - 1.001).abs() < 1e-6, "{max_y}");
        // yaw 0 朝南（+z）：鼻子（z=-9 像素）在 +z 侧 9/16 + 6/16 处。
        assert!((max_z - 15.0 / 16.0).abs() < 1e-6, "{max_z}");
        assert!(min_z < 0.0 && min_z > -0.7, "{min_z}");
    }
}
