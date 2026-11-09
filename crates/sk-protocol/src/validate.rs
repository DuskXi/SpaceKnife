//! Validation of inbound params (the pydantic `Field(...)` constraints of the Python models).
//!
//! Deserialising a params type checks shapes and unknown fields; [`Validate::validate`] then checks
//! ranges, lengths and patterns. [`parse_params`] does both and returns a [`ParamsError`] whose
//! [`data`](ParamsError::data) is the `error.data` of an INVALID_PARAMS response.

use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::errors::ErrorCode;

/// One failed constraint: where (`loc`, field path) and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamsError {
    /// Field path, outermost first (`["settings", "viewport", "width"]`); empty for the whole value.
    pub loc: Vec<String>,
    /// Human-readable message.
    pub msg: String,
    /// Machine-readable kind, pydantic style (`string_too_long`, `greater_than_equal`, `json_invalid`…).
    pub kind: &'static str,
}

impl ParamsError {
    pub fn new(field: &str, kind: &'static str, msg: impl Into<String>) -> Self {
        Self {
            loc: vec![field.to_owned()],
            msg: msg.into(),
            kind,
        }
    }

    /// Prefix the location with an outer field (used when validating nested models).
    #[must_use]
    pub fn at(mut self, outer: impl Into<String>) -> Self {
        self.loc.insert(0, outer.into());
        self
    }

    /// The JSON-RPC error code this maps to.
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::InvalidParams
    }

    /// `error.data` of the INVALID_PARAMS response: `[{"loc": [...], "msg": ..., "type": ...}]`.
    pub fn data(&self) -> Value {
        json!([{ "loc": self.loc, "msg": self.msg, "type": self.kind }])
    }
}

impl std::fmt::Display for ParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.loc.is_empty() {
            f.write_str(&self.msg)
        } else {
            write!(f, "{} at {}", self.msg, self.loc.join("."))
        }
    }
}

impl std::error::Error for ParamsError {}

impl From<serde_json::Error> for ParamsError {
    fn from(e: serde_json::Error) -> Self {
        let kind = match e.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                "json_invalid"
            }
            _ => "value_error",
        };
        Self {
            loc: Vec::new(),
            msg: e.to_string(),
            kind,
        }
    }
}

pub type ValidationResult = Result<(), ParamsError>;

/// Constraints beyond the shape checked by deserialisation.
pub trait Validate {
    fn validate(&self) -> ValidationResult {
        Ok(())
    }
}

impl<T: Validate> Validate for Option<T> {
    fn validate(&self) -> ValidationResult {
        match self {
            Some(v) => v.validate(),
            None => Ok(()),
        }
    }
}

/// Deserialise and validate request params. Absent (or `null`) params validate as `{}`, as the
/// Python server did (`model_validate(msg.params or {})`).
pub fn parse_params<P: DeserializeOwned + Validate>(
    raw: Option<&RawValue>,
) -> Result<P, ParamsError> {
    let text = match raw {
        Some(r) if r.get() != "null" => r.get(),
        _ => "{}",
    };
    let params: P = serde_json::from_str(text)?;
    params.validate()?;
    Ok(params)
}

/// Same as [`parse_params`] from a string.
pub fn parse_params_str<P: DeserializeOwned + Validate>(text: &str) -> Result<P, ParamsError> {
    let params: P = serde_json::from_str(text)?;
    params.validate()?;
    Ok(params)
}

// ------------------------------------------------------------------------ checks

/// Length in characters (pydantic counts code points, not bytes).
pub(crate) fn str_len(field: &str, s: &str, min: usize, max: usize) -> ValidationResult {
    // Fast path: the byte length bounds the char count from above.
    if s.len() <= max && s.len() >= min && (min == 0 || s.is_ascii()) {
        return Ok(());
    }
    let n = s.chars().count();
    if n < min {
        return Err(ParamsError::new(
            field,
            "string_too_short",
            format!(
                "String should have at least {min} character{}",
                if min == 1 { "" } else { "s" }
            ),
        ));
    }
    if n > max {
        return Err(ParamsError::new(
            field,
            "string_too_long",
            format!("String should have at most {max} characters"),
        ));
    }
    Ok(())
}

pub(crate) fn opt_str_len(
    field: &str,
    s: &Option<String>,
    min: usize,
    max: usize,
) -> ValidationResult {
    match s {
        Some(s) => str_len(field, s, min, max),
        None => Ok(()),
    }
}

pub(crate) fn list_len<T>(field: &str, v: &[T], min: usize, max: usize) -> ValidationResult {
    if v.len() < min {
        return Err(ParamsError::new(
            field,
            "too_short",
            format!(
                "List should have at least {min} item{} after validation",
                if min == 1 { "" } else { "s" }
            ),
        ));
    }
    if v.len() > max {
        return Err(ParamsError::new(
            field,
            "too_long",
            format!(
                "List should have at most {max} items after validation, not {}",
                v.len()
            ),
        ));
    }
    Ok(())
}

/// Inclusive range `ge..=le` for integers.
pub(crate) fn int_range<T>(field: &str, v: T, ge: T, le: T) -> ValidationResult
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    if v < ge {
        return Err(ParamsError::new(
            field,
            "greater_than_equal",
            format!("Input should be greater than or equal to {ge}"),
        ));
    }
    if v > le {
        return Err(ParamsError::new(
            field,
            "less_than_equal",
            format!("Input should be less than or equal to {le}"),
        ));
    }
    Ok(())
}

pub(crate) fn opt_int_range<T>(field: &str, v: Option<T>, ge: T, le: T) -> ValidationResult
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    match v {
        Some(v) => int_range(field, v, ge, le),
        None => Ok(()),
    }
}

/// `gt < v <= le` for floats (pydantic `Field(gt=..., le=...)`); NaN fails.
pub(crate) fn float_gt_le(field: &str, v: f64, gt: f64, le: f64) -> ValidationResult {
    if v.is_nan() || v <= gt {
        return Err(ParamsError::new(
            field,
            "greater_than",
            format!("Input should be greater than {gt}"),
        ));
    }
    if v > le {
        return Err(ParamsError::new(
            field,
            "less_than_equal",
            format!("Input should be less than or equal to {le}"),
        ));
    }
    Ok(())
}

/// `ge <= v <= le` for floats; NaN fails.
pub(crate) fn float_ge_le(field: &str, v: f64, ge: f64, le: f64) -> ValidationResult {
    if v.is_nan() || v < ge {
        return Err(ParamsError::new(
            field,
            "greater_than_equal",
            format!("Input should be greater than or equal to {ge}"),
        ));
    }
    if v > le {
        return Err(ParamsError::new(
            field,
            "less_than_equal",
            format!("Input should be less than or equal to {le}"),
        ));
    }
    Ok(())
}

pub(crate) fn pattern(field: &str, ok: bool, pat: &str) -> ValidationResult {
    if ok {
        Ok(())
    } else {
        Err(ParamsError::new(
            field,
            "string_pattern_mismatch",
            format!("String should match pattern '{pat}'"),
        ))
    }
}

/// `^\d+\.\d+$` (ASCII digits).
pub(crate) fn is_version(s: &str) -> bool {
    match s.split_once('.') {
        Some((a, b)) => {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// `^(#[0-9A-Fa-f]{6})?$`
pub(crate) fn is_color_or_empty(s: &str) -> bool {
    s.is_empty()
        || (s.len() == 7
            && s.as_bytes()[0] == b'#'
            && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit))
}

/// `^\d{1,6}(\.\d{1,6})?$`
pub(crate) fn is_series(s: &str) -> bool {
    let digits = |p: &str| (1..=6).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_digit());
    match s.split_once('.') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(s),
    }
}

/// Visual ids: `^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$`, 1–48 characters.
pub fn is_visual_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 48 || !b[0].is_ascii_lowercase() {
        return false;
    }
    let mut prev_sep = false;
    for &c in &b[1..] {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            prev_sep = false;
        } else if matches!(c, b'.' | b'_' | b'-') {
            if prev_sep {
                return false;
            }
            prev_sep = true;
        } else {
            return false;
        }
    }
    !prev_sep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(
            is_version("1.0")
                && is_version("12.34")
                && !is_version("1")
                && !is_version("1.")
                && !is_version("a.1")
        );
        assert!(
            is_color_or_empty("")
                && is_color_or_empty("#A0b1C2")
                && !is_color_or_empty("#12345")
                && !is_color_or_empty("123456 ")
        );
        assert!(
            is_series("0.4")
                && is_series("1")
                && !is_series("1.2.3")
                && !is_series("1234567")
                && !is_series("")
        );
    }

    #[test]
    fn lengths_count_characters() {
        assert!(str_len("x", "视频视频", 1, 4).is_ok());
        assert!(str_len("x", "视频视频视", 1, 4).is_err());
        assert!(str_len("x", "", 1, 4).is_err());
        assert_eq!(
            str_len("x", "abcde", 0, 4).unwrap_err().kind,
            "string_too_long"
        );
    }
}
