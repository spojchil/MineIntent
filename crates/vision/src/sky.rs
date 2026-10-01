//! 原版的视距雾与天空：`FogRenderer.computeFogColor`、`AtmosphericFogEnvironment`、
//! `SkyRenderer` 与 `fog.glsl`（26.1.2 客户端）。颜色与天体角度来自 [`Daylight`]。
//!
//! 天空按原版绘制顺序在整幅画面上先画好一张背景：清屏为雾色 → 天空盘 → 日出日落扇面
//! （半透明混合）→ 太阳、月亮、星星（叠加混合）→ 眼睛低于地平线时的黑盘。这些天空管线
//! 都不做深度测试，后画的盖住先画的。天气（雨、雷）与云尚未接入。

use std::sync::Arc;

use image::RgbaImage;

use crate::daylight::Daylight;
use crate::geometry::{dot, V3};
use crate::random::JavaRandom;
use crate::raster::{rasterize, Projection};
use crate::{Camera, Environment, Report, Resources};

/// `EnvironmentAttributes` 默认值：环境雾 0–1024 格、天空雾止于 512 格。
const ENVIRONMENTAL_FOG_END: f64 = 1024.0;
const SKY_FOG_END: f64 = 512.0;
/// 天空盘半径（`SkyRenderer.SKY_DISC_RADIUS`）与高度；黑盘在眼下 16-12 格。
const SKY_DISC_RADIUS: f64 = 512.0;
const SKY_DISC_HEIGHT: f64 = 16.0;
const DARK_DISC_DEPTH: f64 = 4.0;

pub(crate) struct Sky {
    /// 0..255。
    fog_color: V3,
    sky_color: V3,
    render_start: f64,
    render_end: f64,
    sky_end: f64,
    /// 眼睛低于地平线：天空下半是黑盘（`SkyRenderer.shouldRenderDarkDisc`）。
    dark_disc: bool,
    daylight: Daylight,
    sun: Option<Arc<RgbaImage>>,
    moon: Option<Arc<RgbaImage>>,
}

impl Sky {
    pub(crate) fn new(
        environment: &Environment,
        camera: &Camera,
        daylight: Daylight,
        resources: &mut Resources,
        report: &mut Report,
    ) -> Self {
        let view_distance = f64::from(environment.view_distance);
        let render_distance = view_distance * 16.0;
        let to_255 = |c: V3| c.map(|v| (v * 255.0).round());
        let sky_color = to_255(daylight.sky_color);
        // AtmosphericFogEnvironment.getBaseColor：看向太阳一侧时雾色朝日出日落色偏……
        let mut fog_color = to_255(daylight.fog_color);
        if environment.view_distance >= 4 {
            let sun_x = if daylight.sun_angle.to_radians().sin() > 0.0 {
                -1.0
            } else {
                1.0
            };
            // 相机前向的 x 分量（与 `Projection` 的前向轴一致）点乘 (sun_x, 0, 0)。
            let forward_x = -camera.yaw.to_radians().sin() * camera.pitch.to_radians().cos();
            let looking_at_sun = forward_x * sun_x;
            if looking_at_sun > 0.0 && daylight.sunrise_alpha > 0.0 {
                let sunrise = to_255(daylight.sunrise_color);
                fog_color = srgb_lerp(looking_at_sun * daylight.sunrise_alpha, fog_color, sunrise);
            }
        }
        // ……再按视距朝天空色偏。
        let sky_fog_end_chunks = (SKY_FOG_END / 16.0).min(view_distance);
        let factor = 0.25 + 0.75 * (sky_fog_end_chunks / 32.0).clamp(0.0, 1.0);
        fog_color = srgb_lerp(1.0 - factor.powf(0.25), fog_color, sky_color);
        let span = (render_distance / 10.0).clamp(4.0, 64.0);
        let mut texture = |id: &str| match resources.texture(id) {
            Ok(texture) => Some(texture),
            Err(error) => {
                report.warnings.insert(format!("sky texture {id}: {error}"));
                None
            }
        };
        let sun = texture("minecraft:environment/celestial/sun");
        let moon = texture(&format!(
            "minecraft:environment/celestial/moon/{}",
            daylight.moon_phase.trim_start_matches("minecraft:")
        ));
        Self {
            fog_color,
            sky_color,
            render_start: render_distance - span,
            render_end: render_distance,
            sky_end: render_distance.min(SKY_FOG_END),
            dark_disc: camera.eye[1] < environment.horizon_height,
            daylight,
            sun,
            moon,
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

    pub(crate) fn apply(&self, color: [f64; 4], relative: V3) -> [f64; 4] {
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

    /// 一张水平圆盘（天空盘或黑盘）在视线方向上的颜色，按 `sky.fsh` 的天空雾渐变到雾色；
    /// 视线碰不到圆盘时 `None`。
    fn disc(&self, direction: V3, plane: f64, base: V3) -> Option<V3> {
        if direction[1] == 0.0 || (direction[1] > 0.0) != (plane > 0.0) {
            return None;
        }
        let hit = direction.map(|v| v * plane / direction[1]);
        let radius = hit[0].hypot(hit[2]);
        if radius > SKY_DISC_RADIUS {
            return None;
        }
        let spherical = dot(hit, hit).sqrt();
        let cylindrical = radius.max(plane.abs());
        let fog = linear_fog(spherical, 0.0, self.sky_end).max(linear_fog(
            cylindrical,
            self.sky_end,
            self.sky_end,
        ));
        Some(std::array::from_fn(|i| {
            base[i] + fog * (self.fog_color[i] - base[i])
        }))
    }

    /// 没有地形挡住时整幅画面的颜色（0..255），行优先。
    pub(crate) fn paint(&self, projection: &Projection) -> Vec<V3> {
        let [width, height] = projection.size.map(|v| v as usize);
        let directions: Vec<V3> = (0..width * height)
            .map(|i| projection.direction(i % width, i / width))
            .collect();
        let mut image: Vec<V3> = directions
            .iter()
            .map(|&d| {
                self.disc(d, SKY_DISC_HEIGHT, self.sky_color)
                    .unwrap_or(self.fog_color)
            })
            .collect();
        let daylight = &self.daylight;
        self.sunrise(projection, &mut image);
        // renderSunMoonAndStars：整体先绕 Y 转 -90°，各自再绕 X 转自己的角度。
        let celestial = |angle: f64, v: V3| rotate_y(-90.0, rotate_x(angle, v));
        for (texture, angle, half_size) in [
            (&self.sun, daylight.sun_angle, 30.0),
            (&self.moon, daylight.moon_angle, 20.0),
        ] {
            let Some(texture) = texture else { continue };
            let corners = [
                ([-1.0, 0.0, -1.0], [0.0, 0.0]),
                ([1.0, 0.0, -1.0], [16.0, 0.0]),
                ([1.0, 0.0, 1.0], [16.0, 16.0]),
                ([-1.0, 0.0, 1.0], [0.0, 16.0]),
            ]
            .map(|(v, uv)| {
                let local = [v[0] * half_size, 100.0, v[2] * half_size];
                (celestial(angle, local), uv, [1.0; 3])
            });
            // CELESTIAL 管线：`position_tex` 采样，透明处丢弃，`OVERLAY` 混合（源 × 源 alpha 加到底色上）。
            rasterize(projection, &corners, |index, uv, _| {
                let c = nearest(texture, uv);
                if c[3] > 0.0 {
                    for channel in 0..3 {
                        image[index][channel] += c[channel] * c[3];
                    }
                }
            });
        }
        if daylight.star_brightness > 0.0 {
            // STARS 管线：颜色与 alpha 都是星光亮度，`OVERLAY` 混合。
            let added = daylight.star_brightness * daylight.star_brightness * 255.0;
            for star in stars() {
                let corners = star.map(|v| (celestial(daylight.star_angle, v), [0.0; 2], [1.0; 3]));
                rasterize(projection, &corners, |index, _, _| {
                    for channel in &mut image[index] {
                        *channel += added;
                    }
                });
            }
        }
        if self.dark_disc {
            for (pixel, &d) in image.iter_mut().zip(&directions) {
                if let Some(color) = self.disc(d, -DARK_DISC_DEPTH, [0.0; 3]) {
                    *pixel = color;
                }
            }
        }
        for pixel in &mut image {
            *pixel = pixel.map(|c| c.min(255.0));
        }
        image
    }

    /// `renderSunriseAndSunset`：太阳一侧的半透明扇面，中心不透明、边缘全透明。
    fn sunrise(&self, projection: &Projection, image: &mut [V3]) {
        let daylight = &self.daylight;
        let alpha = daylight.sunrise_alpha;
        if alpha <= 0.001 {
            return;
        }
        let turn = if daylight.sun_angle.to_radians().sin() < 0.0 {
            180.0
        } else {
            0.0
        };
        let place = |v: V3| rotate_x(90.0, rotate_z(turn + 90.0, [v[0], v[1], v[2] * alpha]));
        let color = daylight.sunrise_color.map(|c| c * 255.0);
        let center = (place([0.0, 100.0, 0.0]), [0.0; 2], [1.0; 3]);
        let rim = |i: usize| {
            let (s, c) = (i as f64 * std::f64::consts::TAU / 16.0).sin_cos();
            (place([s * 120.0, c * 120.0, -c * 40.0]), [0.0; 2], [0.0; 3])
        };
        // 顶点颜色是白色乘调制色：rgb 为日出日落色，alpha 为顶点 alpha × 其 alpha；`TRANSLUCENT` 混合。
        for i in 0..16 {
            rasterize(
                projection,
                &[center, rim(i), rim(i + 1)],
                |index, _, vertex| {
                    let a = vertex[0] * alpha;
                    for channel in 0..3 {
                        image[index][channel] += a * (color[channel] - image[index][channel]);
                    }
                },
            );
        }
    }
}

/// 原版 `ARGB.srgbLerp`：逐通道整数插值（`Mth.lerpInt` 向下取整）。
fn srgb_lerp(t: f64, from: V3, to: V3) -> V3 {
    std::array::from_fn(|i| from[i] + (t * (to[i] - from[i])).floor())
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

/// 最近邻采样，`uv` 按 0..16，返回 rgb 0..255 与 alpha 0..1。
fn nearest(texture: &RgbaImage, uv: [f64; 2]) -> [f64; 4] {
    let (width, height) = texture.dimensions();
    let x = ((uv[0] / 16.0).clamp(0.0, 1.0) * f64::from(width)) as u32;
    let y = ((uv[1] / 16.0).clamp(0.0, 1.0) * f64::from(height)) as u32;
    let c = texture.get_pixel(x.min(width - 1), y.min(height - 1)).0;
    [
        f64::from(c[0]),
        f64::from(c[1]),
        f64::from(c[2]),
        f64::from(c[3]) / 255.0,
    ]
}

/// JOML 右手系旋转（角度为度）。
fn rotate_x(degrees: f64, v: V3) -> V3 {
    let (s, c) = degrees.to_radians().sin_cos();
    [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
}

fn rotate_y(degrees: f64, v: V3) -> V3 {
    let (s, c) = degrees.to_radians().sin_cos();
    [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c]
}

fn rotate_z(degrees: f64, v: V3) -> V3 {
    let (s, c) = degrees.to_radians().sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c, v[2]]
}

/// `SkyRenderer.buildStars`：种子 10842 的线性同余随机数，1500 次取样里落在单位球壳内的
/// 那些成为星星，放到 100 格远处，各自随机转一个角度。
fn stars() -> &'static [[V3; 4]] {
    static STARS: std::sync::OnceLock<Vec<[V3; 4]>> = std::sync::OnceLock::new();
    STARS.get_or_init(|| {
        let mut random = JavaRandom::new(10842);
        let mut stars = Vec::new();
        for _ in 0..1500 {
            let x = random.next_float() * 2.0 - 1.0;
            let y = random.next_float() * 2.0 - 1.0;
            let z = random.next_float() * 2.0 - 1.0;
            let size = 0.15 + random.next_float() * 0.1;
            let length_squared = x * x + y * y + z * z;
            if length_squared <= 0.010_000_001 || length_squared >= 1.0 {
                continue;
            }
            let length = length_squared.sqrt();
            let center = [x / length * 100.0, y / length * 100.0, z / length * 100.0];
            let z_rotation = random.next_double() * std::f64::consts::TAU;
            // Matrix3f.rotateTowards(-center, +Y)：局部 +Z 对准 -center。
            let forward = [-x / length, -y / length, -z / length];
            let left = normalize(cross([0.0, 1.0, 0.0], forward));
            let up = cross(forward, left);
            let place = |u: f64, v: f64| {
                let (s, c) = (-z_rotation).sin_cos();
                let (a, b) = (u * c - v * s, u * s + v * c);
                std::array::from_fn(|i| left[i] * a + up[i] * b + center[i])
            };
            stars.push([
                place(size, -size),
                place(size, size),
                place(-size, size),
                place(-size, -size),
            ]);
        }
        stars
    })
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: V3) -> V3 {
    let length = dot(v, v).sqrt().max(1e-12);
    v.map(|c| c / length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_samples_become_stars() {
        // 立方体内落进单位球（去掉中心小球）的比例约 π/6。
        let count = stars().len();
        assert!((700..850).contains(&count), "{count}");
    }

    #[test]
    fn the_sun_rises_in_the_east() {
        // 原版太阳方向 (-sin θ, cos θ, 0)：θ = -90° 时在东边（+X）地平线上。
        let sun = rotate_y(-90.0, rotate_x(-90.0, [0.0, 100.0, 0.0]));
        assert!(
            (sun[0] - 100.0).abs() < 1e-9 && sun[1].abs() < 1e-9,
            "{sun:?}"
        );
    }
}
