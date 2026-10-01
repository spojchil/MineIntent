//! 昼夜：原版环境属性（`EnvironmentAttributes`）在主世界的取值。
//!
//! 原版 `EnvironmentAttributeSystem` 的层序：底值取维度属性（`dimension_type/overworld.json`
//! 的 `attributes`），没有的取属性默认值；再叠相机处的生物群系层（各生物群系 json 的
//! `attributes`，按高斯权重插值）；最后按主世界的时间轴（`tags/timeline/in_overworld` 里的
//! `timeline/day`、`timeline/moon`）逐轨道叠加。数据全部从客户端 jar 读，不在代码里抄数值。

use serde_json::Value;

use crate::geometry::V3;
use crate::Resources;

/// 一帧所需的环境属性取值。颜色分量 0..1。
#[derive(Clone, Debug)]
pub(crate) struct Daylight {
    pub sky_color: V3,
    pub fog_color: V3,
    pub sky_light_color: V3,
    pub sky_light_factor: f64,
    pub ambient_light_color: V3,
    pub block_light_tint: V3,
    /// ARGB 的 RGB 与 alpha。
    pub sunrise_color: V3,
    pub sunrise_alpha: f64,
    /// 度。
    pub sun_angle: f64,
    pub moon_angle: f64,
    pub star_angle: f64,
    pub star_brightness: f64,
    pub moon_phase: String,
}

const TIMELINES: [&str; 2] = ["day", "moon"];

impl Daylight {
    /// `biomes` 是相机处各生物群系的属性表与权重（首次出现顺序），见
    /// [`crate::biome::Biomes::camera_weights`]；空表即没有生物群系层。
    pub(crate) fn evaluate(
        resources: &mut Resources,
        clock_ticks: u64,
        biomes: &[(&Value, f64)],
    ) -> Result<Self, String> {
        let dimension = resources.json("data/minecraft/dimension_type/overworld.json")?;
        let base = &dimension["attributes"];
        let color = |key: &str, default: u32| {
            base.get(format!("minecraft:visual/{key}"))
                .and_then(parse_color)
                .unwrap_or_else(|| argb(default))
        };
        let mut daylight = Self {
            sky_color: rgb(color("sky_color", 0)),
            fog_color: rgb(color("fog_color", 0)),
            sky_light_color: rgb(color("sky_light_color", 0xFFFF_FFFF)),
            sky_light_factor: 1.0,
            ambient_light_color: rgb(color("ambient_light_color", 0xFF00_0000)),
            block_light_tint: rgb(color("block_light_tint", 0xFFFF_D88C)),
            sunrise_color: [0.0; 3],
            sunrise_alpha: 0.0,
            sun_angle: 0.0,
            moon_angle: 0.0,
            star_angle: 0.0,
            star_brightness: 0.0,
            moon_phase: "full_moon".to_owned(),
        };
        for (key, target) in [
            ("sky_color", &mut daylight.sky_color),
            ("fog_color", &mut daylight.fog_color),
            ("sky_light_color", &mut daylight.sky_light_color),
            ("ambient_light_color", &mut daylight.ambient_light_color),
        ] {
            *target = biome_layer(biomes, key, *target);
        }
        for name in TIMELINES {
            let timeline = resources.json(&format!("data/minecraft/timeline/{name}.json"))?;
            let period = timeline["period_ticks"].as_u64().unwrap_or(24_000).max(1);
            let t = (clock_ticks % period) as f64;
            let Some(tracks) = timeline["tracks"].as_object() else {
                continue;
            };
            for (key, track) in tracks {
                let Some(attribute) = key.strip_prefix("minecraft:visual/") else {
                    continue;
                };
                daylight.apply(attribute, track, t, period as f64);
            }
        }
        Ok(daylight)
    }

    fn apply(&mut self, attribute: &str, track: &Value, t: f64, period: f64) {
        let multiply = track["modifier"].as_str() == Some("multiply");
        let kind = match attribute {
            "moon_phase" => Kind::Name,
            a if a.ends_with("color") || a == "block_light_tint" => Kind::Color,
            _ => Kind::Float,
        };
        let Some(value) = sample(track, t, period, kind) else {
            return;
        };
        let color = |target: &mut V3, value: &Sampled| {
            if let Sampled::Color(c) = value {
                let c = rgb(*c);
                *target = if multiply {
                    std::array::from_fn(|i| target[i] * c[i])
                } else {
                    c
                };
            }
        };
        let float = |target: &mut f64, value: &Sampled| {
            if let Sampled::Float(v) = value {
                *target = if multiply { *target * v } else { *v };
            }
        };
        match attribute {
            "sky_color" => color(&mut self.sky_color, &value),
            "fog_color" => color(&mut self.fog_color, &value),
            "sky_light_color" => color(&mut self.sky_light_color, &value),
            "ambient_light_color" => color(&mut self.ambient_light_color, &value),
            "block_light_tint" => color(&mut self.block_light_tint, &value),
            "sky_light_factor" => float(&mut self.sky_light_factor, &value),
            "sun_angle" => float(&mut self.sun_angle, &value),
            "moon_angle" => float(&mut self.moon_angle, &value),
            "star_angle" => float(&mut self.star_angle, &value),
            "star_brightness" => float(&mut self.star_brightness, &value),
            "sunrise_sunset_color" => {
                if let Sampled::Color(c) = value {
                    self.sunrise_color = rgb(c);
                    self.sunrise_alpha = c[0];
                }
            }
            "moon_phase" => {
                if let Sampled::Name(name) = value {
                    self.moon_phase = name;
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Float,
    Color,
    Name,
}

enum Sampled {
    Float(f64),
    /// ARGB，0..1。
    Color([f64; 4]),
    Name(String),
}

/// 时间轴轨道在 `t` 处的值：关键帧按周期首尾相接，前后两帧之间按缓动插值。
fn sample(track: &Value, t: f64, period: f64, kind: Kind) -> Option<Sampled> {
    let parse = |value: &Value| parse(value, kind);
    let keyframes = track["keyframes"].as_array()?;
    let frames: Vec<(f64, &Value)> = keyframes
        .iter()
        .filter_map(|k| Some((k["ticks"].as_f64()?, &k["value"])))
        .collect();
    let first = frames.first()?;
    let last = frames.last()?;
    let (previous, next) = match frames.iter().rposition(|(ticks, _)| *ticks <= t) {
        Some(i) => (
            frames[i],
            frames
                .get(i + 1)
                .copied()
                .unwrap_or((first.0 + period, first.1)),
        ),
        None => ((last.0 - period, last.1), *first),
    };
    if next.0 <= previous.0 || track["ease"].as_str() == Some("constant") {
        return parse(previous.1);
    }
    let mut progress = ((t - previous.0) / (next.0 - previous.0)).clamp(0.0, 1.0);
    if let Some(bezier) = track["ease"]["cubic_bezier"].as_array() {
        let p: Vec<f64> = bezier.iter().filter_map(Value::as_f64).collect();
        if p.len() == 4 {
            progress = cubic_bezier(p[0], p[1], p[2], p[3], progress);
        }
    }
    Some(match (parse(previous.1)?, parse(next.1)?) {
        (Sampled::Float(a), Sampled::Float(b)) => Sampled::Float(a + (b - a) * progress),
        (Sampled::Color(a), Sampled::Color(b)) => {
            Sampled::Color(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * progress))
        }
        (value, _) => value,
    })
}

fn parse(value: &Value, kind: Kind) -> Option<Sampled> {
    match kind {
        Kind::Float => value.as_f64().map(Sampled::Float),
        Kind::Color => parse_color(value).map(Sampled::Color),
        Kind::Name => value.as_str().map(|s| Sampled::Name(s.to_owned())),
    }
}

/// 生物群系层（`SpatialAttributeInterpolator.applyAttributeLayer`）：只有一个来源时直接取它；
/// 多个来源时按首次出现顺序累积，每次以「本来源权重 / 累计权重」做 `ARGB.srgbLerp`。
/// 生物群系没给这个属性就用底值。
fn biome_layer(biomes: &[(&Value, f64)], key: &str, base: V3) -> V3 {
    let value_of = |attributes: &Value| {
        attributes
            .get(format!("minecraft:visual/{key}"))
            .and_then(parse_color)
            .map_or(base, rgb)
    };
    let to_int = |c: V3| c.map(|v| (v * 255.0).round());
    match biomes {
        [] => base,
        [(attributes, _)] => value_of(attributes),
        _ => {
            let mut result: Option<V3> = None;
            let mut accumulated = 0.0;
            for (attributes, weight) in biomes {
                let value = to_int(value_of(attributes));
                accumulated += weight;
                result = Some(match result {
                    None => value,
                    Some(previous) => {
                        let t = (weight / accumulated) as f32;
                        std::array::from_fn(|i| {
                            previous[i] + (f64::from(t) * (value[i] - previous[i])).floor()
                        })
                    }
                });
            }
            result.map_or(base, |c| c.map(|v| v / 255.0))
        }
    }
}

/// `"#rrggbb"`（不透明）、`"#aarrggbb"` 或 ARGB 整数。
fn parse_color(value: &Value) -> Option<[f64; 4]> {
    if let Some(i) = value.as_i64() {
        return Some(argb(i as u32));
    }
    let hex = value.as_str()?.strip_prefix('#')?;
    let packed = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(argb(0xFF00_0000 | packed)),
        8 => Some(argb(packed)),
        _ => None,
    }
}

fn argb(packed: u32) -> [f64; 4] {
    [24, 16, 8, 0].map(|shift| f64::from((packed >> shift) & 0xFF) / 255.0)
}

fn rgb(argb: [f64; 4]) -> V3 {
    [argb[1], argb[2], argb[3]]
}

/// CSS 式三次贝塞尔缓动：两端固定在 (0,0)、(1,1)，按 x 反解参数后取 y。
fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let curve = |a: f64, b: f64, s: f64| {
        3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3)
    };
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..50 {
        let mid = (low + high) / 2.0;
        if curve(x1, x2, mid) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    curve(y1, y2, (low + high) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keyframes_wrap_around_the_period() {
        let track = json!({"keyframes":[{"ticks":100,"value":1.0},{"ticks":300,"value":3.0}]});
        let at = |t| match sample(&track, t, 1000.0, Kind::Float) {
            Some(Sampled::Float(v)) => v,
            _ => panic!(),
        };
        assert_eq!(at(200.0), 2.0);
        assert_eq!(at(300.0), 3.0);
        // 300 → 1100（下一周期的 100）：从 3 回落到 1。
        assert_eq!(at(700.0), 2.0);
        // 0 落在 -700（上一周期的 300）与 100 之间，走了 7/8。
        assert_eq!(at(0.0), 1.25);
    }

    #[test]
    fn colors_parse_rgb_and_argb_strings() {
        assert_eq!(parse_color(&json!("#ff0000")), Some([1.0, 1.0, 0.0, 0.0]));
        assert_eq!(parse_color(&json!("#00ffe533")).unwrap()[0], 0.0);
        assert_eq!(parse_color(&json!(-1)), Some([1.0; 4]));
    }
}
