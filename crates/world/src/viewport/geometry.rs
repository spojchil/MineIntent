//! 视口的纯几何原语：向量、视锥、轴对齐盒。
//!
//! 这一层**不认识读取器、不认识方块**——只有数与形。视口内核里所有带
//! `<P, F, C>` 的扫描函数都依赖它，它不依赖任何扫描函数，所以能单独读、
//! 单独测、单独改。
//!
//! 坐标约定沿用原版：yaw 0 = 南(+z)、90 = 西(−x)；视锥是矩形而非各向同性
//! 的圆锥（与原版视野一致），`inside_frustum` 因此按 right/up 两个半角
//! 分别判定。

use std::cmp::Ordering;

use super::{ViewportOptions, SECTION_SIZE};
use crate::BlockPosition;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Point3 {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) z: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ViewAxes {
    pub(super) right: Point3,
    pub(super) up: Point3,
    pub(super) forward: Point3,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AxisAlignedBox {
    pub(super) min: Point3,
    pub(super) max: Point3,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CameraPoint {
    pub(super) depth: f64,
    pub(super) right: f64,
    pub(super) up: f64,
}

pub(super) const FACE_NORMALS: [Point3; 6] = [
    Point3 {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    },
    Point3 {
        x: -1.0,
        y: 0.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: -1.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    },
    Point3 {
        x: 0.0,
        y: 0.0,
        z: -1.0,
    },
];

pub(super) fn view_axes(yaw_degrees: f64, pitch_degrees: f64) -> ViewAxes {
    // 角度制输入（azalea LookDirection 同单位），内部转弧度。
    // 号向=原版视向量（Entity.calculateViewVector；azalea view_vector、
    // TS geometry.ts 同式）：yaw 0=南(+z)、90=西(−x)；pitch 正=向下。
    // 注意 y、z 两分量的符号：取反即垂直反转+南北镜像。
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.to_radians();
    let forward = Point3 {
        x: -yaw.sin() * pitch.cos(),
        y: -pitch.sin(),
        z: yaw.cos() * pitch.cos(),
    };
    let level = Point3 {
        x: -yaw.sin(),
        y: 0.0,
        z: yaw.cos(),
    };
    let right = Point3 {
        x: -level.z,
        y: 0.0,
        z: level.x,
    };
    ViewAxes {
        right,
        up: cross(right, forward),
        forward,
    }
}

pub(super) fn inside_frustum(axes: ViewAxes, delta: Point3, options: &ViewportOptions) -> bool {
    let depth = dot(delta, axes.forward);
    depth > 0.0
        && dot(delta, axes.right).abs() <= depth * options.horizontal_half_angle.tan()
        && dot(delta, axes.up).abs() <= depth * options.vertical_half_angle.tan()
}

pub(super) fn box_intersects_frustum(
    axes: ViewAxes,
    eye: Point3,
    bounds: AxisAlignedBox,
    options: &ViewportOptions,
) -> bool {
    let tan_horizontal = options.horizontal_half_angle.tan();
    let tan_vertical = options.vertical_half_angle.tan();
    let corners = box_corners(bounds).map(|corner| {
        let delta = subtract(corner, eye);
        CameraPoint {
            depth: dot(delta, axes.forward),
            right: dot(delta, axes.right),
            up: dot(delta, axes.up),
        }
    });
    let outside_depth = corners.iter().all(|point| point.depth <= 0.0);
    let outside_left = corners
        .iter()
        .all(|point| point.right < -point.depth * tan_horizontal);
    let outside_right = corners
        .iter()
        .all(|point| point.right > point.depth * tan_horizontal);
    let outside_bottom = corners
        .iter()
        .all(|point| point.up < -point.depth * tan_vertical);
    let outside_top = corners
        .iter()
        .all(|point| point.up > point.depth * tan_vertical);
    !(outside_depth || outside_left || outside_right || outside_bottom || outside_top)
}

pub(super) fn box_corners(bounds: AxisAlignedBox) -> [Point3; 8] {
    let AxisAlignedBox { min, max } = bounds;
    [
        Point3 {
            x: min.x,
            y: min.y,
            z: min.z,
        },
        Point3 {
            x: min.x,
            y: min.y,
            z: max.z,
        },
        Point3 {
            x: min.x,
            y: max.y,
            z: min.z,
        },
        Point3 {
            x: min.x,
            y: max.y,
            z: max.z,
        },
        Point3 {
            x: max.x,
            y: min.y,
            z: min.z,
        },
        Point3 {
            x: max.x,
            y: min.y,
            z: max.z,
        },
        Point3 {
            x: max.x,
            y: max.y,
            z: min.z,
        },
        Point3 {
            x: max.x,
            y: max.y,
            z: max.z,
        },
    ]
}

pub(super) fn box_visibility_samples(bounds: AxisAlignedBox) -> Vec<Point3> {
    let xs = axis_samples(bounds.min.x, bounds.max.x, [0.05, 0.5, 0.95]);
    let ys = axis_samples(bounds.min.y, bounds.max.y, [0.15, 0.5, 0.85]);
    let zs = axis_samples(bounds.min.z, bounds.max.z, [0.05, 0.5, 0.95]);
    let mut points = Vec::with_capacity(27);
    for x in xs {
        for y in ys {
            for z in zs {
                points.push(Point3 { x, y, z });
            }
        }
    }
    points
}

pub(super) fn axis_samples(minimum: f64, maximum: f64, fractions: [f64; 3]) -> [f64; 3] {
    fractions.map(|fraction| minimum + (maximum - minimum) * fraction)
}

pub(super) fn point_inside_box(point: Point3, bounds: AxisAlignedBox) -> bool {
    point.x >= bounds.min.x
        && point.x <= bounds.max.x
        && point.y >= bounds.min.y
        && point.y <= bounds.max.y
        && point.z >= bounds.min.z
        && point.z <= bounds.max.z
}

pub(super) fn distance_to_box(point: Point3, bounds: AxisAlignedBox) -> f64 {
    let dx = (bounds.min.x - point.x)
        .max(0.0)
        .max(point.x - bounds.max.x);
    let dy = (bounds.min.y - point.y)
        .max(0.0)
        .max(point.y - bounds.max.y);
    let dz = (bounds.min.z - point.z)
        .max(0.0)
        .max(point.z - bounds.max.z);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

pub(super) fn section_of(value: i32) -> i32 {
    value.div_euclid(SECTION_SIZE)
}

pub(super) fn compare_candidate(
    left: &(f64, BlockPosition),
    right: &(f64, BlockPosition),
) -> Ordering {
    left.0
        .partial_cmp(&right.0)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.1.x.cmp(&right.1.x))
        .then_with(|| left.1.y.cmp(&right.1.y))
        .then_with(|| left.1.z.cmp(&right.1.z))
}

pub(super) fn cross(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.y * right.z - left.z * right.y,
        y: left.z * right.x - left.x * right.z,
        z: left.x * right.y - left.y * right.x,
    }
}

pub(super) fn add(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.x + right.x,
        y: left.y + right.y,
        z: left.z + right.z,
    }
}

pub(super) fn scale(value: Point3, factor: f64) -> Point3 {
    Point3 {
        x: value.x * factor,
        y: value.y * factor,
        z: value.z * factor,
    }
}

pub(super) fn subtract(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.x - right.x,
        y: left.y - right.y,
        z: left.z - right.z,
    }
}

pub(super) fn dot(left: Point3, right: Point3) -> f64 {
    left.x * right.x + left.y * right.y + left.z * right.z
}

pub(super) fn length(value: Point3) -> f64 {
    dot(value, value).sqrt()
}

pub(super) fn normalize(value: Point3, magnitude: f64) -> Point3 {
    Point3 {
        x: value.x / magnitude,
        y: value.y / magnitude,
        z: value.z / magnitude,
    }
}

pub(super) fn same_voxel(left: &BlockPosition, right: &BlockPosition) -> bool {
    left.x == right.x && left.y == right.y && left.z == right.z
}

pub(super) fn round_position(position: Point3) -> [f64; 3] {
    [
        round_one(position.x),
        round_one(position.y),
        round_one(position.z),
    ]
}

pub(super) fn round_one(value: f64) -> f64 {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}
