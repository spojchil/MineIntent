use serde::{Deserialize, Serialize};

pub use mineintent_contracts::information::RelativeDirection;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Point3 {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub x: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub y: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub z: f64,
}

pub fn distance_between(a: Point3, b: Point3) -> f64 {
    (a.x - b.x).hypot(a.y - b.y).hypot(a.z - b.z)
}

/// 原版的 yaw/pitch → 单位朝向向量。**角度制**——与契约里的
/// `SelfPose::yaw`、azalea 的 `LookDirection::y_rot()` 同单位。
///
/// 这里原来写的是弧度，而传进来的一直是角度：声音的「前/后/左/右」因此是
/// 按一个错误朝向算出来的。三角函数不会报错，只会安静地给出一个像样的错答案。
pub fn look_direction(yaw_degrees: f64, pitch_degrees: f64) -> Point3 {
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.to_radians();
    Point3 {
        x: -yaw.sin() * pitch.cos(),
        y: pitch.sin(),
        z: -yaw.cos() * pitch.cos(),
    }
}

pub fn relative_bearing(
    self_yaw: f64,
    self_position: Point3,
    target_position: Point3,
) -> RelativeDirection {
    let dx = target_position.x - self_position.x;
    let dz = target_position.z - self_position.z;
    if dx == 0.0 && dz == 0.0 {
        return RelativeDirection::Ahead;
    }

    let look = look_direction(self_yaw, 0.0);
    let dot = look.x * dx + look.z * dz;
    let cross = look.x * dz - look.z * dx;
    let angle = cross.atan2(dot);
    let quarter = std::f64::consts::FRAC_PI_2;
    if angle >= -quarter / 2.0 && angle < quarter / 2.0 {
        RelativeDirection::Ahead
    } else if angle >= quarter / 2.0 && angle < quarter * 3.0 / 2.0 {
        RelativeDirection::Right
    } else if angle >= -quarter * 3.0 / 2.0 && angle < -quarter / 2.0 {
        RelativeDirection::Left
    } else {
        RelativeDirection::Behind
    }
}

pub(crate) fn serialize_finite<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error;

    if value.is_finite() {
        serializer.serialize_f64(*value)
    } else {
        Err(S::Error::custom("number must be finite"))
    }
}

pub(crate) fn deserialize_finite<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value = f64::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(D::Error::custom("number must be finite"))
    }
}

pub(crate) fn serialize_optional_finite<S>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error;

    match value {
        Some(value) if value.is_finite() => serializer.serialize_some(value),
        Some(_) => Err(S::Error::custom("number must be finite")),
        None => serializer.serialize_none(),
    }
}

pub(crate) fn deserialize_optional_finite<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(D::Error::custom(
            "explicit null is not an optional field value",
        ));
    }
    let value = serde_json::from_value::<f64>(value).map_err(D::Error::custom)?;
    if value.is_finite() {
        Ok(Some(value))
    } else {
        Err(D::Error::custom("number must be finite"))
    }
}

pub(crate) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    use serde::de::Error;

    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(D::Error::custom(
            "explicit null is not an optional field value",
        ));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(D::Error::custom)
}
