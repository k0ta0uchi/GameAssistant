//! Compact canonical JSON normalization used by operation IDs.
//!
//! Objects are recursively rebuilt with keys sorted by their UTF-8 byte
//! ordering. Arrays retain order, strings and numbers retain their serde_json
//! values (no locale, whitespace, or environment-dependent normalization), and
//! the final representation is compact JSON.

use super::error::{MemoryError, Result};
use serde_json::{Map, Value};

pub const CANONICAL_JSON_VERSION: &str = "canonical-json-v1";

pub fn canonical_json(value: &Value) -> Result<String> {
    let normalized = normalize(value);
    serde_json::to_string(&normalized)
        .map_err(|error| MemoryError::CanonicalJson(error.to_string()))
}

fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            let mut normalized = Map::with_capacity(entries.len());
            for (key, value) in entries {
                normalized.insert(key.clone(), normalize(value));
            }
            Value::Object(normalized)
        }
        Value::Array(values) => Value::Array(values.iter().map(normalize).collect()),
        other => other.clone(),
    }
}
