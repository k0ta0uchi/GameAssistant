use super::error::{MemoryError, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    SelfSubject,
    Twitch(String),
    Discord(String),
    ManualPerson(Uuid),
}

impl Subject {
    pub fn parse(value: &str) -> Result<Self> {
        if value == "self" {
            return Ok(Self::SelfSubject);
        }
        if let Some(id) = value.strip_prefix("twitch:") {
            return Self::checked_id(id).map(Self::Twitch);
        }
        if let Some(id) = value.strip_prefix("discord:") {
            return Self::checked_id(id).map(Self::Discord);
        }
        if let Some(id) = value.strip_prefix("manual-person:") {
            if id.len() != 36
                || !id.is_ascii()
                || id != id.to_ascii_lowercase()
                || !id
                    .chars()
                    .enumerate()
                    .all(|(index, ch)| matches!(index, 8 | 13 | 18 | 23) == (ch == '-'))
            {
                return Err(MemoryError::InvalidSubject(value.to_string()));
            }
            return Uuid::parse_str(id)
                .map_err(|_| MemoryError::InvalidSubject(value.to_string()))
                .and_then(|id| {
                    if id.is_nil() {
                        Err(MemoryError::InvalidSubject(value.to_string()))
                    } else {
                        Ok(Self::ManualPerson(id))
                    }
                });
        }
        Err(MemoryError::InvalidSubject(value.to_string()))
    }

    fn checked_id(value: &str) -> Result<String> {
        if value.is_empty()
            || value == "nil"
            || value.len() > 128
            || value != value.to_ascii_lowercase()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(MemoryError::InvalidSubject(value.to_string()));
        }
        Ok(value.to_string())
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfSubject => f.write_str("self"),
            Self::Twitch(id) => write!(f, "twitch:{}", id),
            Self::Discord(id) => write!(f, "discord:{}", id),
            Self::ManualPerson(id) => write!(f, "manual-person:{}", id),
        }
    }
}

impl Serialize for Subject {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Subject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}
