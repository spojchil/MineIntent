use super::*;

#[derive(Clone, Copy)]
struct TestArmorModifier {
    id: u8,
    amount: f64,
    operation: azalea::core::attribute_modifier_operation::AttributeModifierOperation,
}

fn test_armor_value(base: f64, modifiers: &[TestArmorModifier]) -> Option<u8> {
    calculate_armor_values(
        base,
        modifiers,
        |modifier| modifier.id,
        |modifier| modifier.amount,
        |modifier| modifier.operation,
    )
}

#[test]
fn armor_formula_groups_operations_and_fails_closed_for_bad_values() {
    use azalea::core::attribute_modifier_operation::AttributeModifierOperation as Op;

    let modifiers = [
        TestArmorModifier {
            id: 3,
            amount: 0.25,
            operation: Op::AddMultipliedTotal,
        },
        TestArmorModifier {
            id: 2,
            amount: 0.5,
            operation: Op::AddMultipliedBase,
        },
        TestArmorModifier {
            id: 1,
            amount: 2.0,
            operation: Op::AddValue,
        },
    ];
    // d1 = 4 + 2 = 6; d3 = 6 + 6*0.5 = 9; d3 *= 1.25 = 11.25.
    assert_eq!(test_armor_value(4.0, &modifiers), Some(11));

    let duplicate_id = [
        TestArmorModifier {
            id: 1,
            amount: 1.0,
            operation: Op::AddValue,
        },
        TestArmorModifier {
            id: 1,
            amount: 3.0,
            operation: Op::AddValue,
        },
    ];
    assert_eq!(test_armor_value(10.0, &duplicate_id), Some(13));

    assert_eq!(test_armor_value(-5.0, &[]), Some(0));
    assert_eq!(test_armor_value(30.0, &[]), Some(20));
    assert_eq!(test_armor_value(0.0, &[]), Some(0));
    assert_eq!(test_armor_value(f64::NAN, &[]), None);
    assert_eq!(
        test_armor_value(
            1.0,
            &[TestArmorModifier {
                id: 1,
                amount: f64::INFINITY,
                operation: Op::AddValue,
            }],
        ),
        None
    );
    assert_eq!(
        test_armor_value(
            f64::MAX,
            &[TestArmorModifier {
                id: 1,
                amount: f64::MAX,
                operation: Op::AddValue,
            }],
        ),
        None
    );
    assert_eq!(
        test_armor_value(
            f64::MAX,
            &[TestArmorModifier {
                id: 1,
                amount: f64::MAX,
                operation: Op::AddMultipliedTotal,
            }],
        ),
        None
    );
}
