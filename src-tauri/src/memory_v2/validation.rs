use super::error::{MemoryError, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const EMBEDDING_DIMENSIONS: usize = 768;

/// A wire timestamp is kept as its canonical spelling so serialization cannot
/// silently change an offset or precision supplied by a caller.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalUtc(String);

impl CanonicalUtc {
    pub fn parse(value: &str) -> Result<Self> {
        let parsed =
            DateTime::parse_from_rfc3339(value).map_err(|_| MemoryError::InvalidContent)?;
        let utc = parsed.with_timezone(&Utc);
        let canonical = utc.to_rfc3339_opts(SecondsFormat::Secs, true);
        if canonical != value {
            return Err(MemoryError::InvalidContent);
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CanonicalUtc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for CanonicalUtc {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for CanonicalUtc {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CanonicalUtc {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Embedding(Vec<f32>);

impl Embedding {
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
}

impl Serialize for Embedding {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Embedding {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = Vec::<f32>::deserialize(deserializer)?;
        Self::try_from(values).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<Vec<f32>> for Embedding {
    type Error = MemoryError;
    fn try_from(values: Vec<f32>) -> Result<Self> {
        if values.len() != EMBEDDING_DIMENSIONS {
            return Err(MemoryError::InvalidEmbeddingLength {
                expected: EMBEDDING_DIMENSIONS,
                actual: values.len(),
            });
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(MemoryError::NonFiniteEmbedding);
        }
        Ok(Self(values))
    }
}

pub fn validate_embedding(values: Option<Vec<f32>>) -> Result<Option<Embedding>> {
    values.map(Embedding::try_from).transpose()
}
