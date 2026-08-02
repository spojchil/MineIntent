use std::{
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{ser::Formatter, Value};

pub trait InformationClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemInformationClock;

impl InformationClock for SystemInformationClock {
    fn now_millis(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => {
                let millis = i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX);
                millis.saturating_neg()
            }
        }
    }
}

pub(crate) fn clone_bounded_json<E>(
    value: &Value,
    maximum: usize,
    not_serializable: impl Fn() -> E,
    too_large: impl Fn(usize, usize) -> E,
) -> Result<Value, E> {
    let serialized = javascript_json_bytes(value).map_err(|_| not_serializable())?;
    if serialized.len() > maximum {
        return Err(too_large(serialized.len(), maximum));
    }
    serde_json::from_slice(&serialized).map_err(|_| not_serializable())
}

/// Returns the UTF-8 bytes of the JSON representation used by JavaScript `JSON.stringify` for
/// the JSON-compatible values crossing the information boundary.  In particular this keeps
/// integer/float rendering aligned with the ref/cursor byte guards.
pub(crate) fn javascript_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ()> {
    let mut serializer =
        serde_json::Serializer::with_formatter(Vec::new(), JavaScriptJsonFormatter);
    value.serialize(&mut serializer).map_err(|_| ())?;
    Ok(serializer.into_inner())
}

#[derive(Clone, Copy, Debug, Default)]
struct JavaScriptJsonFormatter;

impl Formatter for JavaScriptJsonFormatter {
    fn write_i64<W>(&mut self, writer: &mut W, value: i64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        write_javascript_number(writer, value as f64)
    }

    fn write_u64<W>(&mut self, writer: &mut W, value: u64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        write_javascript_number(writer, value as f64)
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        write_javascript_number(writer, value)
    }
}

fn write_javascript_number(writer: &mut (impl Write + ?Sized), value: f64) -> io::Result<()> {
    writer.write_all(javascript_number_to_string(value).as_bytes())
}

fn javascript_number_to_string(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }

    let negative = value.is_sign_negative();
    let mut buffer = ryu::Buffer::new();
    let rendered = buffer.format_finite(value.abs());
    let (mantissa, exponent) = rendered
        .split_once(['e', 'E'])
        .map_or((rendered, 0_i32), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap_or(0))
        });
    let mut decimal_position = mantissa
        .find('.')
        .map_or(mantissa.len(), |position| position) as i32
        + exponent;
    let mut digits = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .collect::<Vec<_>>();
    let leading_zeroes = digits.iter().take_while(|byte| **byte == b'0').count();
    digits.drain(..leading_zeroes);
    decimal_position -= leading_zeroes as i32;
    while digits.last() == Some(&b'0') {
        digits.pop();
    }

    let digit_count = digits.len() as i32;
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if digit_count <= decimal_position && decimal_position <= 21 {
        output.extend(digits.iter().map(|byte| char::from(*byte)));
        output.extend(std::iter::repeat_n(
            '0',
            (decimal_position - digit_count) as usize,
        ));
    } else if 0 < decimal_position && decimal_position <= 21 {
        let split = decimal_position as usize;
        output.extend(digits[..split].iter().map(|byte| char::from(*byte)));
        output.push('.');
        output.extend(digits[split..].iter().map(|byte| char::from(*byte)));
    } else if -6 < decimal_position && decimal_position <= 0 {
        output.push_str("0.");
        output.extend(std::iter::repeat_n('0', (-decimal_position) as usize));
        output.extend(digits.iter().map(|byte| char::from(*byte)));
    } else {
        output.push(char::from(digits[0]));
        if digits.len() > 1 {
            output.push('.');
            output.extend(digits[1..].iter().map(|byte| char::from(*byte)));
        }
        let scientific_exponent = decimal_position - 1;
        output.push('e');
        if scientific_exponent >= 0 {
            output.push('+');
        }
        output.push_str(&scientific_exponent.to_string());
    }
    output
}

pub(crate) fn is_expired(valid_until: Option<&str>, now: i64) -> bool {
    valid_until
        .and_then(parse_javascript_date_millis)
        .is_some_and(|valid_until| valid_until <= now)
}

pub(crate) fn parse_javascript_date_millis(timestamp: &str) -> Option<i64> {
    if !timestamp.contains('T') {
        let (year, month, day) = parse_date(timestamp)?;
        return days_from_civil(year, month, day).checked_mul(86_400_000);
    }

    let (date, time_and_zone) = timestamp.split_once('T')?;
    let (year, month, day) = parse_date(date)?;
    let (time, offset_minutes) = if let Some(time) = time_and_zone.strip_suffix('Z') {
        (time, 0_i64)
    } else {
        let zone_index = time_and_zone
            .char_indices()
            .skip(1)
            .filter(|(_, character)| *character == '+' || *character == '-')
            .map(|(index, _)| index)
            .last()?;
        let (time, zone) = time_and_zone.split_at(zone_index);
        if zone.len() != 6 || zone.as_bytes().get(3) != Some(&b':') {
            return None;
        }
        let hours = parse_digits(&zone[1..3])? as i64;
        let minutes = parse_digits(&zone[4..6])? as i64;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let magnitude = hours.checked_mul(60)?.checked_add(minutes)?;
        let offset = if zone.starts_with('+') {
            magnitude
        } else {
            -magnitude
        };
        (time, offset)
    };

    let (whole_time, fraction) = match time.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() => (whole, Some(fraction)),
        Some(_) => return None,
        None => (time, None),
    };
    let time_parts: Vec<_> = whole_time.split(':').collect();
    if time_parts.len() != 3 || time_parts.iter().any(|part| part.len() != 2) {
        return None;
    }
    let hour = parse_digits(time_parts[0])? as i64;
    let minute = parse_digits(time_parts[1])? as i64;
    let second = parse_digits(time_parts[2])? as i64;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let millis = match fraction {
        Some(fraction) if fraction.bytes().all(|byte| byte.is_ascii_digit()) => {
            let mut digits = fraction.bytes().take(3).collect::<Vec<_>>();
            while digits.len() < 3 {
                digits.push(b'0');
            }
            i64::from(digits[0] - b'0') * 100
                + i64::from(digits[1] - b'0') * 10
                + i64::from(digits[2] - b'0')
        }
        Some(_) => return None,
        None => 0,
    };
    let days = days_from_civil(year, month, day);
    let local_millis = i128::from(days)
        .checked_mul(86_400_000)?
        .checked_add(i128::from(
            hour * 3_600_000 + minute * 60_000 + second * 1_000 + millis,
        ))?;
    i64::try_from(local_millis - i128::from(offset_minutes * 60_000)).ok()
}

fn parse_date(date: &str) -> Option<(i64, u32, u32)> {
    let date_parts: Vec<_> = date.split('-').collect();
    if date_parts.len() != 3
        || date_parts[0].len() != 4
        || date_parts[1].len() != 2
        || date_parts[2].len() != 2
    {
        return None;
    }
    let year = parse_digits(date_parts[0])? as i64;
    let month = parse_digits(date_parts[1])? as u32;
    let day = parse_digits(date_parts[2])? as u32;
    if day < 1 || day > days_in_month(year, month)? {
        return None;
    }
    Some((year, month, day))
}

pub(crate) fn format_utc_millis<E>(
    timestamp: i64,
    out_of_range: impl Fn() -> E,
) -> Result<String, E> {
    let days = timestamp.div_euclid(86_400_000);
    let time = timestamp.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9_999).contains(&year) {
        return Err(out_of_range());
    }
    let hour = time / 3_600_000;
    let minute = time % 3_600_000 / 60_000;
    let second = time % 60_000 / 1_000;
    let millis = time % 1_000;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

fn parse_digits(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{javascript_number_to_string, parse_javascript_date_millis};

    #[test]
    fn javascript_number_format_matches_node_characterization() {
        assert_eq!(javascript_number_to_string(1.0), "1");
        assert_eq!(javascript_number_to_string(-0.0), "0");
        assert_eq!(javascript_number_to_string(1e-6), "0.000001");
        assert_eq!(javascript_number_to_string(1e-7), "1e-7");
        assert_eq!(javascript_number_to_string(1e20), "100000000000000000000");
        assert_eq!(javascript_number_to_string(1e21), "1e+21");
        assert_eq!(
            javascript_number_to_string(f64::from_bits(0x4317_9085_685d_83c9)),
            "1658206780088562.2"
        );
        assert_eq!(
            javascript_number_to_string(9_007_199_254_740_993_u64 as f64),
            "9007199254740992"
        );
    }

    #[test]
    fn javascript_date_parser_accepts_standard_utc_date_only_and_rejects_invalid_text() {
        assert_eq!(
            parse_javascript_date_millis("2026-07-13"),
            Some(1_783_900_800_000)
        );
        assert_eq!(parse_javascript_date_millis("not-a-date"), None);
    }
}
