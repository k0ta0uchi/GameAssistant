use super::canonical::canonical_json;
use super::error::{MemoryError, Result};
use super::validation::CanonicalUtc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Changing this prefix is an intentional operation-ID version migration.
pub const OPERATION_DOMAIN_PREFIX: &str = "gameassistant.memory_v2.operation.v1";
pub const OPERATION_SCHEMA_VERSION: &str = OPERATION_DOMAIN_PREFIX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    RawEvent,
    Fact,
    Redaction,
    Embedding,
    SummaryStatus,
}

impl OperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RawEvent => "raw_event",
            Self::Fact => "fact",
            Self::Redaction => "redaction",
            Self::Embedding => "embedding",
            Self::SummaryStatus => "summary_status",
        }
    }
}

pub fn operation_id(kind: OperationKind, payload: &Value) -> Result<String> {
    let canonical = canonical_json(payload)?;
    let mut hasher = Sha256::new();
    hasher.update(OPERATION_DOMAIN_PREFIX.as_bytes());
    hasher.update([0]);
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(canonical.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OperationEnvelope {
    schema_version: String,
    operation_id: String,
    operation_kind: OperationKind,
    entity_id: String,
    issued_at: CanonicalUtc,
    expected_revision: Option<u64>,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationWire {
    schema_version: String,
    operation_id: String,
    operation_kind: OperationKind,
    entity_id: String,
    issued_at: CanonicalUtc,
    // The field itself is required, but its value may be null for operations
    // that do not use revision CAS (for example embeddings). A nested Option
    // without a custom deserializer cannot distinguish JSON null from a
    // missing field, so deserialize the required nullable value directly.
    #[serde(deserialize_with = "deserialize_required_optional")]
    expected_revision: Option<u64>,
    payload: Value,
}

impl<'de> Deserialize<'de> for OperationEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = OperationWire::deserialize(deserializer)?;
        Self::try_new(
            wire.operation_id,
            wire.operation_kind,
            wire.entity_id,
            wire.issued_at.as_str(),
            wire.expected_revision,
            wire.payload,
        )
        .and_then(|envelope| {
            if envelope.schema_version == wire.schema_version {
                Ok(envelope)
            } else {
                Err(MemoryError::InvalidOperationKind)
            }
        })
        .map_err(serde::de::Error::custom)
    }
}

fn deserialize_required_optional<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)
}

impl OperationEnvelope {
    pub fn new(
        kind: OperationKind,
        entity_id: impl Into<String>,
        issued_at: impl AsRef<str>,
        expected_revision: Option<u64>,
        payload: Value,
    ) -> Result<Self> {
        let entity_id = entity_id.into();
        if entity_id.trim().is_empty() {
            return Err(MemoryError::EmptyValue("entity_id"));
        }
        let issued_at = CanonicalUtc::parse(issued_at.as_ref())?;
        let canonical_payload = canonical_payload(payload)?;
        let operation_id = operation_id_for_fields(
            kind,
            &entity_id,
            issued_at.as_str(),
            expected_revision,
            &canonical_payload,
        )?;
        Ok(Self {
            schema_version: OPERATION_SCHEMA_VERSION.to_string(),
            operation_id,
            operation_kind: kind,
            entity_id,
            issued_at,
            expected_revision,
            payload: canonical_payload,
        })
    }

    pub fn try_new(
        operation_id: impl Into<String>,
        kind: OperationKind,
        entity_id: impl Into<String>,
        issued_at: impl AsRef<str>,
        expected_revision: Option<u64>,
        payload: Value,
    ) -> Result<Self> {
        let operation_id = operation_id.into();
        let entity_id = entity_id.into();
        if operation_id.trim().is_empty() || entity_id.trim().is_empty() {
            return Err(MemoryError::EmptyValue("operation_id/entity_id"));
        }
        let issued_at = CanonicalUtc::parse(issued_at.as_ref())?;
        let payload = canonical_payload(payload)?;
        if operation_id
            != operation_id_for_fields(
                kind,
                &entity_id,
                issued_at.as_str(),
                expected_revision,
                &payload,
            )?
        {
            return Err(MemoryError::InvalidOperationKind);
        }
        Ok(Self {
            schema_version: OPERATION_SCHEMA_VERSION.to_string(),
            operation_id,
            operation_kind: kind,
            entity_id,
            issued_at,
            expected_revision,
            payload,
        })
    }

    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn kind(&self) -> OperationKind {
        self.operation_kind
    }
    pub fn entity_id(&self) -> &str {
        &self.entity_id
    }
    pub fn issued_at(&self) -> &str {
        self.issued_at.as_str()
    }
    pub fn expected_revision(&self) -> Option<u64> {
        self.expected_revision
    }
    pub fn payload(&self) -> &Value {
        &self.payload
    }
    pub fn verify_operation_id(&self) -> Result<()> {
        if self.operation_id
            == operation_id_for_fields(
                self.operation_kind,
                &self.entity_id,
                self.issued_at.as_str(),
                self.expected_revision,
                &self.payload,
            )?
        {
            Ok(())
        } else {
            Err(MemoryError::InvalidOperationKind)
        }
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let value = serde_json::to_value(self)
            .map_err(|error| MemoryError::CanonicalJson(error.to_string()))?;
        Ok(canonical_json(&value)?.into_bytes())
    }
}

fn canonical_payload(payload: Value) -> Result<Value> {
    let text = canonical_json(&payload)?;
    serde_json::from_str(&text).map_err(|error| MemoryError::CanonicalJson(error.to_string()))
}

pub fn operation_id_for_envelope(envelope: &OperationEnvelope) -> Result<String> {
    operation_id_for_fields(
        envelope.operation_kind,
        &envelope.entity_id,
        envelope.issued_at.as_str(),
        envelope.expected_revision,
        &envelope.payload,
    )
}

fn operation_id_for_fields(
    kind: OperationKind,
    entity_id: &str,
    issued_at: &str,
    expected_revision: Option<u64>,
    payload: &Value,
) -> Result<String> {
    // `prior` is undo metadata, not the identity of a mutation.  Excluding it
    // keeps retries idempotent when the materialized row has already applied
    // the operation and the caller reconstructs the envelope from that row.
    let payload_for_id = if kind == OperationKind::Fact
        && matches!(
            payload.get("action").and_then(Value::as_str),
            Some("upsert") | Some("retract")
        ) {
        let mut payload = payload.clone();
        if let Some(object) = payload.as_object_mut() {
            object.remove("prior");
        }
        payload
    } else {
        payload.clone()
    };
    let unsigned_envelope = json!({
        "entity_id": entity_id,
        "expected_revision": expected_revision,
        "issued_at": issued_at,
        "operation_kind": kind,
        "payload": payload_for_id,
        "schema_version": OPERATION_SCHEMA_VERSION,
    });
    let canonical = canonical_json(&unsigned_envelope)?;
    let mut hasher = Sha256::new();
    hasher.update(OPERATION_DOMAIN_PREFIX.as_bytes());
    hasher.update([0]);
    hasher.update(canonical.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationDisposition {
    Accepted,
    NoOpRetry,
}

#[derive(Default, Debug)]
pub struct OperationRegistry {
    operations: BTreeMap<String, Vec<u8>>,
}

impl OperationRegistry {
    pub fn admit(&mut self, envelope: &OperationEnvelope) -> Result<OperationDisposition> {
        let bytes = envelope.canonical_bytes()?;
        if let Some(existing) = self.operations.get(envelope.operation_id()) {
            if existing == &bytes {
                return Ok(OperationDisposition::NoOpRetry);
            }
            return Err(MemoryError::InvalidOperationKind);
        }
        self.operations
            .insert(envelope.operation_id().to_string(), bytes);
        Ok(OperationDisposition::Accepted)
    }
}
