//! Canonical lossless JSON encoding for public query values.
//!
//! Native session frames and the browser facade share this implementation so
//! a portable `.graph` file cannot acquire a platform-dependent result shape.

use crate::graph::types::Value;
use serde_json::{Value as JsonValue, json};

/// Encode one database value without flattening refs or keywords into strings.
///
/// Finite scalar values retain their ordinary JSON representation. Values
/// without an unambiguous native JSON representation use a single-key tag.
pub(crate) fn to_tagged_json(value: &Value) -> JsonValue {
    match value {
        Value::String(string) => JsonValue::String(string.clone()),
        Value::Integer(integer) => json!(integer),
        Value::Float(float) => {
            if float.is_nan() {
                json!({"$float": "nan"})
            } else if float.is_infinite() {
                json!({"$float": if *float > 0.0 { "inf" } else { "-inf" }})
            } else {
                serde_json::Number::from_f64(*float)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            }
        }
        Value::Boolean(boolean) => JsonValue::Bool(*boolean),
        Value::Ref(uuid) => json!({"$ref": uuid.to_string()}),
        Value::Keyword(keyword) => json!({"$kw": keyword}),
        Value::Null => JsonValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn ambiguous_values_are_tagged() {
        let uuid =
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("valid fixture UUID");
        assert_eq!(
            to_tagged_json(&Value::Ref(uuid)),
            json!({"$ref": "00000000-0000-0000-0000-000000000001"})
        );
        assert_eq!(
            to_tagged_json(&Value::Keyword(":status/active".to_string())),
            json!({"$kw": ":status/active"})
        );
        assert_eq!(
            to_tagged_json(&Value::String(":status/active".to_string())),
            json!(":status/active")
        );
    }

    #[test]
    fn non_finite_floats_have_lossless_tags() {
        assert_eq!(
            to_tagged_json(&Value::Float(f64::NAN)),
            json!({"$float": "nan"})
        );
        assert_eq!(
            to_tagged_json(&Value::Float(f64::INFINITY)),
            json!({"$float": "inf"})
        );
        assert_eq!(
            to_tagged_json(&Value::Float(f64::NEG_INFINITY)),
            json!({"$float": "-inf"})
        );
        assert_eq!(to_tagged_json(&Value::Float(1.5)), json!(1.5));
    }
}
