use crate::information::contracts::{InformationValueSchema, InformationValueSchemaError};
use serde_json::Value;

const JS_MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub(super) struct NumberSchema {
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: bool,
}

impl NumberSchema {
    pub(super) const fn new(minimum: Option<f64>, maximum: Option<f64>, integer: bool) -> Self {
        Self {
            minimum,
            maximum,
            integer,
        }
    }
}

impl InformationValueSchema for NumberSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        parse_number(&value, self.minimum, self.maximum, self.integer)?;
        Ok(value)
    }
}

pub(super) fn parse_number(
    value: &Value,
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: bool,
) -> Result<f64, InformationValueSchemaError> {
    let Value::Number(number) = value else {
        return Err(error("expected number"));
    };
    let Some(number) = number.as_f64() else {
        return Err(error("expected finite JavaScript number"));
    };
    if integer && (number.fract() != 0.0 || number.abs() > JS_MAX_SAFE_INTEGER) {
        return Err(error("expected JavaScript safe integer"));
    }
    if minimum.is_some_and(|minimum| number < minimum) {
        return Err(error("number is below its minimum"));
    }
    if maximum.is_some_and(|maximum| number > maximum) {
        return Err(error("number is above its maximum"));
    }
    Ok(number)
}

pub(super) fn parse_non_empty_string(value: &Value) -> Result<&str, InformationValueSchemaError> {
    let Value::String(value) = value else {
        return Err(error("expected string"));
    };
    if value.is_empty() {
        return Err(error("string must contain at least one UTF-16 code unit"));
    }
    Ok(value)
}

pub(super) fn error(message: &str) -> InformationValueSchemaError {
    InformationValueSchemaError {
        message: message.to_owned(),
    }
}
