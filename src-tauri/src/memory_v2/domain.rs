use super::canonical::canonical_json;
use super::error::{MemoryError, Result};
use super::policy::{FactDecision, Policy, PrivacyAdmission};
use super::redaction::{RedactedText, Redactor};
use super::subject::Subject;
use super::validation::CanonicalUtc;
use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;
use uuid::Uuid;

#[cfg(test)]
const DEFAULT_OCCURRED_AT: &str = "1970-01-01T00:00:00Z";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Human,
    AiResponse,
    AutoCommentary,
    Manual,
    System,
}

impl EventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::AiResponse => "ai_response",
            Self::AutoCommentary => "auto_commentary",
            Self::Manual => "manual",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RawEvent {
    event_id: Uuid,
    subject: Subject,
    event_type: EventType,
    source: SourceKind,
    occurred_at: CanonicalUtc,
    content: RedactedText,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEventWire {
    event_id: String,
    subject: Subject,
    event_type: EventType,
    source: SourceKind,
    occurred_at: CanonicalUtc,
    content: RedactedText,
}

impl<'de> Deserialize<'de> for RawEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RawEventWire::deserialize(deserializer)?;
        let event_id = parse_wire_uuid(&wire.event_id).map_err(serde::de::Error::custom)?;
        Self::try_new_with_metadata(
            event_id,
            wire.subject,
            wire.event_type,
            wire.source,
            wire.occurred_at.as_str(),
            wire.content,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn parse_wire_uuid(value: &str) -> Result<Uuid> {
    if value.len() != 36 || value != value.to_ascii_lowercase() {
        return Err(MemoryError::InvalidContent);
    }
    let uuid = Uuid::parse_str(value).map_err(|_| MemoryError::InvalidContent)?;
    if uuid.is_nil() {
        return Err(MemoryError::InvalidContent);
    }
    Ok(uuid)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Microphone,
    Discord,
    Twitch,
    Manual,
    System,
}

impl SourceKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "microphone" => Ok(Self::Microphone),
            "discord" => Ok(Self::Discord),
            "twitch" => Ok(Self::Twitch),
            "manual" => Ok(Self::Manual),
            "system" => Ok(Self::System),
            _ => Err(MemoryError::InvalidSource),
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Discord => "discord",
            Self::Twitch => "twitch",
            Self::Manual => "manual",
            Self::System => "system",
        }
    }
}

impl From<SourceKind> for String {
    fn from(source: SourceKind) -> Self {
        source.as_str().to_string()
    }
}

impl RawEvent {
    /// Legacy constructor retained with deterministic metadata defaults.
    #[cfg(test)]
    pub(crate) fn try_new(
        event_id: Uuid,
        subject: Subject,
        source: impl Into<String>,
        content: RedactedText,
    ) -> Result<Self> {
        Self::try_new_with_metadata(
            event_id,
            subject,
            EventType::Manual,
            source,
            DEFAULT_OCCURRED_AT,
            content,
        )
    }

    pub(crate) fn try_new_with_metadata(
        event_id: Uuid,
        subject: Subject,
        event_type: EventType,
        source: impl Into<String>,
        occurred_at: impl AsRef<str>,
        content: RedactedText,
    ) -> Result<Self> {
        if event_id.is_nil() {
            return Err(MemoryError::InvalidContent);
        }
        let source = source.into();
        let source = SourceKind::parse(&source)?;
        if content.as_str().trim().is_empty()
            || Redactor::default().redact(content.as_str()).text() != content.as_str()
        {
            return Err(MemoryError::InvalidContent);
        }
        Ok(Self {
            event_id,
            subject,
            event_type,
            source,
            occurred_at: CanonicalUtc::parse(occurred_at.as_ref())?,
            content,
        })
    }

    /// Persistence-facing constructor. Raw content must cross the policy
    /// boundary here before a RawEvent can be created.
    pub fn try_new_admitted(
        event_id: Uuid,
        subject: Subject,
        event_type: EventType,
        source: impl Into<String>,
        occurred_at: impl AsRef<str>,
        raw_content: &str,
        policy: &Policy,
        admission: PrivacyAdmission,
    ) -> Result<Self> {
        let content = policy.admit_raw_text(raw_content, admission)?;
        Self::try_new_with_metadata(event_id, subject, event_type, source, occurred_at, content)
    }

    pub fn try_admit(
        event_id: Uuid,
        subject: Subject,
        event_type: EventType,
        source: impl Into<String>,
        occurred_at: impl AsRef<str>,
        raw_content: &str,
        policy: &Policy,
        admission: PrivacyAdmission,
    ) -> Result<Self> {
        Self::try_new_admitted(
            event_id,
            subject,
            event_type,
            source,
            occurred_at,
            raw_content,
            policy,
            admission,
        )
    }
    pub fn event_id(&self) -> Uuid {
        self.event_id
    }
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
    pub fn event_type(&self) -> EventType {
        self.event_type
    }
    pub fn source_kind(&self) -> SourceKind {
        self.source
    }
    pub fn source(&self) -> &str {
        self.source.as_str()
    }
    pub fn occurred_at(&self) -> &str {
        self.occurred_at.as_str()
    }
    pub fn content(&self) -> &RedactedText {
        &self.content
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FactStatus {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "confirmed")]
    Confirmed,
    #[serde(rename = "edited")]
    Edited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactPredicate {
    Fact,
    Identity,
    Preference,
    Profile,
    Has,
    Prefers,
    KnownAs,
}

impl FactPredicate {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Identity => "identity",
            Self::Preference => "preference",
            Self::Profile => "profile",
            Self::Has => "has",
            Self::Prefers => "prefers",
            Self::KnownAs => "known_as",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fact {
    fact_id: String,
    subject: Subject,
    predicate: FactPredicate,
    key: String,
    value: String,
    status: FactStatus,
    source_event_id: Uuid,
    revision: u64,
    operation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactWire {
    fact_id: String,
    subject: Subject,
    predicate: FactPredicate,
    key: String,
    value: String,
    status: FactStatus,
    source_event_id: String,
    revision: u64,
    operation_id: String,
}

impl<'de> Deserialize<'de> for Fact {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FactWire::deserialize(deserializer)?;
        if wire.fact_id.trim().is_empty() {
            return Err(serde::de::Error::custom(MemoryError::EmptyValue("fact_id")));
        }
        let source_event_id =
            parse_wire_uuid(&wire.source_event_id).map_err(serde::de::Error::custom)?;
        let fact = Self::try_derive_with_metadata(
            wire.subject,
            wire.predicate,
            wire.key,
            &wire.value,
            wire.status,
            source_event_id,
            wire.revision,
            wire.operation_id,
            &Policy::default(),
        )
        .map_err(serde::de::Error::custom)?;
        if fact.fact_id != wire.fact_id {
            return Err(serde::de::Error::custom(MemoryError::InvalidContent));
        }
        Ok(fact)
    }
}

pub fn fact_id_for(subject: &Subject, key: &str) -> Result<String> {
    validate_fact_key(key)?;
    Ok(format!("fact:{}:{}", subject, key))
}

fn validate_fact_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 64
        || !key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        })
    {
        return Err(MemoryError::InvalidContent);
    }
    Ok(())
}

impl Fact {
    pub fn try_derive(
        subject: Subject,
        key: impl Into<String>,
        value: &str,
        status: FactStatus,
        source_event_id: Uuid,
        policy: &Policy,
    ) -> Result<Self> {
        let key = key.into();
        let operation_id = fact_id_for(&subject, &key)?;
        Self::try_derive_with_metadata(
            subject,
            FactPredicate::Fact,
            key,
            value,
            status,
            source_event_id,
            0,
            operation_id,
            policy,
        )
    }

    pub fn try_derive_with_metadata(
        subject: Subject,
        predicate: FactPredicate,
        key: impl Into<String>,
        value: &str,
        status: FactStatus,
        source_event_id: Uuid,
        revision: u64,
        operation_id: impl Into<String>,
        policy: &Policy,
    ) -> Result<Self> {
        let value = Redactor::default().redact_text(value)?;
        Self::try_derive_redacted_with_metadata(
            subject,
            predicate,
            key,
            value,
            status,
            source_event_id,
            revision,
            operation_id,
            policy,
        )
    }

    pub fn try_derive_redacted(
        subject: Subject,
        key: impl Into<String>,
        value: RedactedText,
        status: FactStatus,
        source_event_id: Uuid,
        policy: &Policy,
    ) -> Result<Self> {
        let key = key.into();
        let operation_id = fact_id_for(&subject, &key)?;
        Self::try_derive_redacted_with_metadata(
            subject,
            FactPredicate::Fact,
            key,
            value,
            status,
            source_event_id,
            0,
            operation_id,
            policy,
        )
    }

    pub fn try_derive_redacted_with_metadata(
        subject: Subject,
        predicate: FactPredicate,
        key: impl Into<String>,
        value: RedactedText,
        status: FactStatus,
        source_event_id: Uuid,
        revision: u64,
        operation_id: impl Into<String>,
        policy: &Policy,
    ) -> Result<Self> {
        let key = key.into();
        let operation_id = operation_id.into();
        validate_fact_key(&key)?;
        if source_event_id.is_nil() {
            return Err(MemoryError::InvalidContent);
        }
        if operation_id.trim().is_empty() {
            return Err(MemoryError::EmptyValue("operation_id"));
        }
        if value.as_str().trim().is_empty()
            || Redactor::default().redact(value.as_str()).text() != value.as_str()
        {
            return Err(MemoryError::InvalidContent);
        }
        if policy.fact_decision(value.as_str()) == FactDecision::Prohibited {
            return Err(MemoryError::ProhibitedCategory(
                "configured sensitive category".to_string(),
            ));
        }
        Ok(Self {
            fact_id: fact_id_for(&subject, &key)?,
            subject,
            predicate,
            key,
            value: value.as_str().to_string(),
            status,
            source_event_id,
            revision,
            operation_id,
        })
    }

    pub fn fact_id(&self) -> &str {
        &self.fact_id
    }
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
    pub fn predicate(&self) -> FactPredicate {
        self.predicate
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn status(&self) -> FactStatus {
        self.status
    }
    pub fn source_event_id(&self) -> Uuid {
        self.source_event_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Derive a replacement for a summary Fact produced by the automatic
    /// summarizer.  This is intentionally separate from `apply_transition`:
    /// user-confirmed and user-edited Facts may not be rewritten by a
    /// backfill, while an eligible old automatic Fact keeps its identity and
    /// advances through an explicit revision CAS.
    pub fn try_rederive_auto(
        &self,
        value: &str,
        expected_revision: u64,
        policy: &Policy,
    ) -> Result<Self> {
        if self.status != FactStatus::Auto || self.revision != expected_revision {
            return Err(MemoryError::InvalidContent);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(MemoryError::InvalidContent)?;
        Self::try_derive_with_metadata(
            self.subject.clone(),
            self.predicate,
            self.key.clone(),
            value,
            FactStatus::Auto,
            self.source_event_id,
            revision,
            self.operation_id.clone(),
            policy,
        )
    }

    pub fn is_summary_fact(&self) -> bool {
        self.key.starts_with("summary-")
    }

    pub fn is_auto_summary(&self) -> bool {
        self.status == FactStatus::Auto && self.is_summary_fact()
    }

    pub fn apply_transition(
        &self,
        proposed: &Fact,
        expected_revision: Option<u64>,
        expected_status: Option<FactStatus>,
    ) -> Result<Self> {
        if self.fact_id != proposed.fact_id
            || self.subject != proposed.subject
            || self.key != proposed.key
        {
            return Err(MemoryError::InvalidContent);
        }
        let expected = expected_revision.ok_or(MemoryError::InvalidContent)?;
        if expected != self.revision {
            return Err(MemoryError::InvalidContent);
        }
        if expected_status.ok_or(MemoryError::InvalidContent)? != self.status {
            return Err(MemoryError::InvalidContent);
        }
        if self.status == proposed.status {
            if self.value == proposed.value {
                return Ok(self.clone());
            }
            return Err(MemoryError::InvalidContent);
        }
        let allowed = matches!(
            (self.status, proposed.status),
            (FactStatus::Auto, FactStatus::Confirmed)
                | (FactStatus::Auto, FactStatus::Edited)
                | (FactStatus::Confirmed, FactStatus::Edited)
        );
        if !allowed {
            return Err(MemoryError::InvalidContent);
        }
        let mut next = proposed.clone();
        next.revision = self.revision + 1;
        Ok(next)
    }
}

pub fn resolve_fact_conflict_checked(left: &Fact, right: &Fact) -> Result<Fact> {
    if left.subject != right.subject || left.key != right.key {
        return Err(MemoryError::InvalidContent);
    }
    match left.status.cmp(&right.status) {
        Ordering::Less => Ok(right.clone()),
        Ordering::Greater => Ok(left.clone()),
        Ordering::Equal => Ok(match left.revision.cmp(&right.revision) {
            Ordering::Less => right.clone(),
            Ordering::Greater => left.clone(),
            Ordering::Equal => match right.operation_id.cmp(&left.operation_id) {
                Ordering::Less => right.clone(),
                Ordering::Greater => left.clone(),
                Ordering::Equal => {
                    let left_bytes = canonical_json(
                        &serde_json::to_value(left).map_err(|_| MemoryError::InvalidContent)?,
                    )?;
                    let right_bytes = canonical_json(
                        &serde_json::to_value(right).map_err(|_| MemoryError::InvalidContent)?,
                    )?;
                    if left_bytes <= right_bytes {
                        left.clone()
                    } else {
                        right.clone()
                    }
                }
            },
        }),
    }
}

pub fn resolve_fact_conflict(left: &Fact, right: &Fact) -> Fact {
    resolve_fact_conflict_checked(left, right).unwrap_or_else(|_| left.clone())
}
