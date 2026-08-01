use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    InformationCatalogRequest, InformationHelpRequest, InformationQueryRequest,
    InformationReadRequest, InformationSelectorRef,
};

const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum InformationRequestParseError {
    #[error("invalid information request JSON: {message}")]
    InvalidJson { message: String },
    #[error("information request does not match its strict schema: {message}")]
    InvalidShape { message: String },
    #[error("invalid information request field {field}: {message}")]
    InvalidField { field: String, message: String },
}

pub fn parse_information_catalog_request(
    json: &str,
) -> Result<InformationCatalogRequest, InformationRequestParseError> {
    let request: InformationCatalogRequest = parse_json(json)?;
    if let Some(revision) = &request.known_catalog_revision {
        validate_string("knownCatalogRevision", revision, 1, 160)?;
    }
    Ok(request)
}

pub fn parse_information_query_request(
    json: &str,
) -> Result<InformationQueryRequest, InformationRequestParseError> {
    let request: InformationQueryRequest = parse_json(json)?;
    match &request {
        InformationQueryRequest::Help(help) => validate_help(help)?,
        InformationQueryRequest::Read(read) => validate_read(read)?,
    }
    Ok(request)
}

pub fn parse_information_selector_ref(
    json: &str,
) -> Result<InformationSelectorRef, InformationRequestParseError> {
    let selector: InformationSelectorRef = parse_json(json)?;
    validate_selector(&selector)?;
    Ok(selector)
}

fn parse_json<T>(json: &str) -> Result<T, InformationRequestParseError>
where
    T: for<'de> Deserialize<'de>,
{
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| InformationRequestParseError::InvalidJson {
            message: error.to_string(),
        })?;
    serde_json::from_value(value).map_err(|error| InformationRequestParseError::InvalidShape {
        message: error.to_string(),
    })
}

fn validate_help(request: &InformationHelpRequest) -> Result<(), InformationRequestParseError> {
    if let Some(search) = &request.search {
        validate_string("search", search, 1, 160)?;
    }
    if let Some(fields) = &request.fields {
        if fields.len() > 128 {
            return invalid("fields", "must contain at most 128 entries");
        }
        validate_fields(fields)?;
    }
    Ok(())
}

fn validate_read(request: &InformationReadRequest) -> Result<(), InformationRequestParseError> {
    validate_string("schemaRevision", &request.schema_revision, 1, 160)?;
    if request.fields.is_empty() || request.fields.len() > 128 {
        return invalid("fields", "must contain between 1 and 128 entries");
    }
    validate_fields(&request.fields)?;
    if let Some(selector) = &request.selector {
        validate_selector(selector)?;
    }
    if let Some(page) = &request.page {
        if let Some(cursor) = &page.cursor {
            validate_string("page.cursor", cursor, 16, 160)?;
        }
        if let Some(limit) = page.limit {
            if !(1..=10_000).contains(&limit) {
                return invalid("page.limit", "must be an integer between 1 and 10000");
            }
        }
    }
    Ok(())
}

fn validate_fields(fields: &[String]) -> Result<(), InformationRequestParseError> {
    for field in fields {
        validate_string("fields[]", field, 1, 160)?;
    }
    Ok(())
}

fn validate_selector(
    selector: &InformationSelectorRef,
) -> Result<(), InformationRequestParseError> {
    validate_string("selector.id", &selector.id, 16, 160)?;
    validate_safe_integer("selector.connectionEpoch", selector.connection_epoch)?;
    validate_safe_integer(
        "selector.basedOnInformationRevision",
        selector.based_on_information_revision,
    )?;
    if let Some(world_id) = &selector.world_id {
        validate_string("selector.worldId", world_id, 1, 256)?;
    }
    if let Some(screen_id) = &selector.screen_instance_id {
        validate_string("selector.screenInstanceId", screen_id, 1, 256)?;
    }
    if let Some(valid_until) = &selector.valid_until {
        if !is_zod_iso_datetime(valid_until) {
            return invalid("selector.validUntil", "must be an ISO datetime");
        }
    }
    Ok(())
}

fn validate_safe_integer(field: &str, value: u64) -> Result<(), InformationRequestParseError> {
    if value > JS_MAX_SAFE_INTEGER {
        return invalid(field, "must be a JavaScript safe integer");
    }
    Ok(())
}

fn validate_string(
    field: &str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), InformationRequestParseError> {
    let length = value.encode_utf16().count();
    if length < minimum || length > maximum {
        return invalid(
            field,
            &format!("must contain between {minimum} and {maximum} UTF-16 code units"),
        );
    }
    Ok(())
}

// `z.iso.datetime()` defaults to UTC (`Z`) and accepts optional fractional seconds.
fn is_zod_iso_datetime(value: &str) -> bool {
    let Some(core) = value.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = core.split_once('T') else {
        return false;
    };
    let date_parts: Vec<_> = date.split('-').collect();
    let (hour_minute_second, fraction) = match time.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (time, None),
    };
    let time_parts: Vec<_> = hour_minute_second.split(':').collect();
    if date_parts.len() != 3
        || time_parts.len() != 3
        || date_parts[0].len() != 4
        || date_parts[1].len() != 2
        || date_parts[2].len() != 2
        || time_parts.iter().any(|part| part.len() != 2)
        || fraction.is_some_and(|part| part.is_empty() || !all_ascii_digits(part))
    {
        return false;
    }
    let Some(year) = parse_digits(date_parts[0]) else {
        return false;
    };
    let Some(month) = parse_digits(date_parts[1]) else {
        return false;
    };
    let Some(day) = parse_digits(date_parts[2]) else {
        return false;
    };
    let Some(hour) = parse_digits(time_parts[0]) else {
        return false;
    };
    let Some(minute) = parse_digits(time_parts[1]) else {
        return false;
    };
    let Some(second) = parse_digits(time_parts[2]) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day >= 1 && day <= days_in_month && hour <= 23 && minute <= 59 && second <= 59
}

fn parse_digits(value: &str) -> Option<u32> {
    if !all_ascii_digits(value) {
        return None;
    }
    value.parse().ok()
}

fn all_ascii_digits(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_digit())
}

fn invalid<T>(field: &str, message: &str) -> Result<T, InformationRequestParseError> {
    Err(InformationRequestParseError::InvalidField {
        field: field.to_owned(),
        message: message.to_owned(),
    })
}
