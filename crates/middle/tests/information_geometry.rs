use std::f64::consts::{FRAC_PI_2, PI};

use mineintent_contracts::information::RelativeDirection;
use mineintent_middle::information::geometry::{
    distance_between, look_direction, relative_bearing, Point3,
};

fn point(x: f64, y: f64, z: f64) -> Point3 {
    Point3 { x, y, z }
}

#[test]
fn distance_between_computes_3d_euclidean_distance() {
    assert_eq!(
        distance_between(point(0.0, 0.0, 0.0), point(3.0, 0.0, 4.0)),
        5.0
    );
    assert_eq!(
        distance_between(point(1.0, 1.0, 1.0), point(1.0, 1.0, 1.0)),
        0.0
    );
}

#[test]
fn look_direction_matches_mineflayers_own_yaw_pitch_to_direction_formula() {
    let cases = [
        (0.0, 0.0, point(0.0, 0.0, -1.0)),
        (FRAC_PI_2, 0.0, point(-1.0, 0.0, 0.0)),
        (0.0, FRAC_PI_2, point(0.0, 1.0, 0.0)),
    ];
    for (yaw, pitch, expected) in cases {
        let direction = look_direction(yaw, pitch);
        assert!(
            (direction.x - expected.x).abs() < 1e-9,
            "x for yaw={yaw} pitch={pitch}"
        );
        assert!(
            (direction.y - expected.y).abs() < 1e-9,
            "y for yaw={yaw} pitch={pitch}"
        );
        assert!(
            (direction.z - expected.z).abs() < 1e-9,
            "z for yaw={yaw} pitch={pitch}"
        );
    }
}

#[test]
fn relative_bearing_classifies_target_position_relative_to_self_facing() {
    let self_position = point(0.0, 64.0, 0.0);
    assert_eq!(
        relative_bearing(0.0, self_position, point(0.0, 64.0, -5.0)),
        RelativeDirection::Ahead
    );
    assert_eq!(
        relative_bearing(0.0, self_position, point(0.0, 64.0, 5.0)),
        RelativeDirection::Behind
    );
    assert_eq!(
        relative_bearing(0.0, self_position, point(-5.0, 64.0, 0.0)),
        RelativeDirection::Left
    );
    assert_eq!(
        relative_bearing(0.0, self_position, point(5.0, 64.0, 0.0)),
        RelativeDirection::Right
    );
}

#[test]
fn relative_bearing_agrees_with_the_rightward_axis_of_look_direction_at_every_yaw() {
    let self_position = point(0.0, 64.0, 0.0);
    for yaw in [0.0, FRAC_PI_2, PI, -FRAC_PI_2, 0.7] {
        let look = look_direction(yaw, 0.0);
        let rightward_x = -look.z;
        let rightward_z = look.x;
        let to_right = point(rightward_x * 5.0, 64.0, rightward_z * 5.0);
        let to_left = point(-rightward_x * 5.0, 64.0, -rightward_z * 5.0);
        assert_eq!(
            relative_bearing(yaw, self_position, to_right),
            RelativeDirection::Right,
            "yaw {yaw} rightward"
        );
        assert_eq!(
            relative_bearing(yaw, self_position, to_left),
            RelativeDirection::Left,
            "yaw {yaw} leftward"
        );
    }
}

#[test]
fn relative_bearing_rotates_with_self_yaw() {
    let self_position = point(0.0, 64.0, 0.0);
    let facing_positive_x = -FRAC_PI_2;
    assert_eq!(
        relative_bearing(facing_positive_x, self_position, point(5.0, 64.0, 0.0)),
        RelativeDirection::Ahead
    );
}

#[test]
fn relative_bearing_defaults_to_ahead_when_target_is_exactly_at_self_position() {
    let self_position = point(0.0, 64.0, 0.0);
    assert_eq!(
        relative_bearing(0.0, self_position, self_position),
        RelativeDirection::Ahead
    );
}
