//! Production persistence boundary for memory-v2.
//!
//! The older `lance_memory` module is kept as a compatibility projection while
//! this repository owns the durable memory-v2 stores.  Every mutation is
//! represented by a validated operation envelope, written to the journal, and
//! then applied idempotently to one of the three LanceDB stores.  The manifest
//! is advanced only after the target stores exist and the journal hash matches.

use super::domain::{EventType, Fact, RawEvent, SourceKind};
use super::error::MemoryError;
use super::journal::{raw_event_fingerprint, Journal, JournalRecord, JournalState};
use super::manifest::ManifestSelector;
use super::operation::{OperationEnvelope, OperationKind};
use super::paths::MemoryPaths;
use super::policy::{Policy, PrivacyAdmission};
use super::subject::Subject;
use super::validation::{CanonicalUtc, EMBEDDING_DIMENSIONS};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Connection, Table};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

const RAW_TABLE: &str = "raw_events";
const FACTS_TABLE: &str = "facts";
const EMBEDDINGS_TABLE: &str = "embeddings";
const VECTOR_DIM: i32 = EMBEDDING_DIMENSIONS as i32;

/// Contract version for all newly-created summary attempts.  The version
/// covers the source fields, prompt serialization, output schema, and
/// validator together; it is not merely a display label.  Keep this as the
/// single storage-facing source of truth; the compatibility adapter and local
/// model service re-export it so a prompt bump cannot leave terminal rows
/// permanently stale.
pub const SUMMARY_PROMPT_VERSION: &str = "v3";
const SUMMARY_MAX_CHARS: usize = 240;
pub const SUMMARY_MODEL_ID: &str = "gemma-3-1b-it-Q4_K_S.gguf";

const SUMMARY_STATUSES: [&str; 7] = [
    "pending",
    "completed",
    "skipped",
    "fallback",
    "error",
    "legacy",
    "deleted",
];

/// Stable reason codes accepted from the backfill boundary.  A terminal
/// fallback/error may carry a diagnostic string from an older caller, but the
/// journal must never turn that diagnostic into an unbounded public reason.
/// Unknown values therefore retain the historical compatibility code.
const SUMMARY_REASON_CODES: [&str; 34] = [
    "invalid_model_output",
    "metadata_echo",
    "ungrounded_summary",
    "empty_source",
    "source_too_long",
    "summary_runtime_timeout",
    "summary_runtime_failed",
    "summary_queue_failed",
    "embedding_failed",
    "invalid_embedding",
    "policy_excluded",
    "fact_validation_failed",
    "subject_unresolved",
    "model_declined",
    "should_store_false",
    "inference_failed",
    // Repository-generated terminal outcomes and fatal persistence errors are
    // part of the stable reason vocabulary too.  Keeping them in this one
    // allowlist prevents a future backfill caller from accidentally collapsing
    // an already-machine-readable code to `inference_failed`.
    "fact_protected",
    "source_provenance_missing",
    "not_storable",
    "non_persistent",
    "not_candidate",
    "candidate_not_eligible",
    "source_not_allowed",
    "journal_commit_failed",
    "scan_failed",
    "repository_open_failed",
    "queue_failed",
    "backfill_fatal",
    "projection_resync_failed",
    "summary_raw_event_read_failed",
    "summary_fact_read_failed",
    "summary_status_read_failed",
    "candidate_admission_changed",
    "candidate_admission_invalid",
];

// Journal writes are cross-process serialized by the journal lock. LanceDB
// materialization happens after that commit, so also serialize materializer
// reads/adds/updates within this process. This closes the read-then-add race
// that could otherwise create duplicate Fact or embedding rows on replay.
static MATERIALIZE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static SUMMARY_BATCH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn materialize_lock() -> &'static tokio::sync::Mutex<()> {
    MATERIALIZE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn summary_batch_lock() -> &'static tokio::sync::Mutex<()> {
    SUMMARY_BATCH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub type StoreResult<T> = Result<T, String>;

/// Input accepted from the legacy SessionEvent/LanceDB adapter.  The adapter
/// deliberately contains no LanceDB types so the domain remains testable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompatMemory {
    pub id: String,
    pub memory_type: String,
    pub source: String,
    pub timestamp: String,
    pub document: String,
}

/// One completed summary produced by the local Gemma worker.  Backfill keeps
/// inference separate from persistence so many results can be committed with
/// one journal recovery instead of reopening the 500+ MB journal per row.
#[derive(Clone, Debug)]
pub struct SummaryBatchInput {
    pub entity_id: String,
    pub summary: String,
    pub embedding: Option<Vec<f32>>,
    /// Explicit attempt metadata supplied by the worker. Older callers may
    /// leave these unset; new writes receive deterministic v2 defaults.
    pub attempt_id: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
}

/// A terminal status produced by a backfill pass when no summary fact is
/// created (for example, a transient worker failure or a non-persistent turn).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryStatusBatchInput {
    pub entity_id: String,
    pub status: String,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
    /// Explicit attempt identity for queue/retry boundaries.  Older callers
    /// may leave this unset; the repository supplies a deterministic identity
    /// for first writes and a fresh one when recovering stale pending work.
    pub attempt_id: Option<String>,
    /// A stable, non-sensitive explanation for a terminal status.  Older
    /// journal records omit this field and remain readable as `None`.
    pub reason: Option<String>,
}

/// Latest durable summary attempt for one source event.  This is kept in the
/// repository layer so the legacy projection and the Tauri API use the same
/// journal decoding rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryStatusRecord {
    pub entity_id: String,
    /// Canonical event identity. `entity_id` remains for API compatibility;
    /// both fields are populated from new journal rows.
    pub event_id: String,
    pub status: String,
    pub reason: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
    pub attempt_id: Option<String>,
    pub lease_expires_at: Option<String>,
    pub sequence: u64,
}

impl SummaryStatusRecord {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "skipped" | "fallback" | "error" | "legacy" | "deleted"
        )
    }

    pub fn is_v2(&self) -> bool {
        self.prompt_version.as_deref() == Some(SUMMARY_PROMPT_VERSION)
    }

    pub fn is_retryable_failure(&self) -> bool {
        matches!(self.status.as_str(), "fallback" | "error")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryProcessMode {
    Automatic,
    ProcessAll,
    Retry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryProcessDecision {
    Process,
    SkipCompleted,
    SkipSkipped,
    RetryRequired,
    LeaseActive,
    ResumeStalePending,
    SkipLegacy,
    ReprocessLegacy,
    Deleted,
}

/// Decide whether an event may enter inference. Status is authoritative even
/// when a Fact exists; callers pass the Fact bit only to distinguish a
/// legacy/versionless row from a brand-new event.
pub fn summary_processing_decision(
    status: Option<&SummaryStatusRecord>,
    has_summary_fact: bool,
    mode: SummaryProcessMode,
    now: Option<&str>,
) -> SummaryProcessDecision {
    let Some(status) = status else {
        return if has_summary_fact && mode != SummaryProcessMode::ProcessAll {
            SummaryProcessDecision::SkipLegacy
        } else {
            SummaryProcessDecision::Process
        };
    };
    match status.status.as_str() {
        "deleted" => SummaryProcessDecision::Deleted,
        // A completed attempt without its Fact is a projection-only partial
        // commit.  Process all must recover it; only a durable Fact makes a
        // same-version completion eligible for the fast skip.
        "completed" if status.is_v2() && has_summary_fact => SummaryProcessDecision::SkipCompleted,
        "completed" if status.is_v2() => SummaryProcessDecision::Process,
        "skipped" if status.is_v2() => SummaryProcessDecision::SkipSkipped,
        "fallback" | "error" if status.is_v2() => {
            if mode == SummaryProcessMode::Retry {
                SummaryProcessDecision::Process
            } else {
                SummaryProcessDecision::RetryRequired
            }
        }
        "pending" => {
            if lease_is_active(status.lease_expires_at.as_deref(), now) {
                SummaryProcessDecision::LeaseActive
            } else {
                SummaryProcessDecision::ResumeStalePending
            }
        }
        _ if mode == SummaryProcessMode::ProcessAll => SummaryProcessDecision::ReprocessLegacy,
        _ => SummaryProcessDecision::SkipLegacy,
    }
}

fn lease_is_active(lease_expires_at: Option<&str>, now: Option<&str>) -> bool {
    let (Some(lease), Some(now)) = (lease_expires_at, now) else {
        return false;
    };
    let Ok(lease) = DateTime::parse_from_rfc3339(lease) else {
        return false;
    };
    let Ok(now) = DateTime::parse_from_rfc3339(now) else {
        return false;
    };
    lease > now
}

/// Explicit metadata carried by each newly created summary attempt.  Journal
/// readers use `SummaryStatusRecord` so old/null fields remain compatible;
/// this type is for new writes and therefore requires the complete identity
/// and v2 contract metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryAttempt {
    pub attempt_id: String,
    pub event_id: String,
    pub model_id: String,
    pub prompt_version: String,
    pub status: String,
    pub reason: Option<String>,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
}

impl SummaryAttempt {
    pub fn new(
        attempt_id: impl Into<String>,
        event_id: impl Into<String>,
        model_id: impl Into<String>,
        status: impl Into<String>,
        reason: Option<String>,
    ) -> StoreResult<Self> {
        let attempt_id = attempt_id.into();
        let event_id = event_id.into();
        let model_id = model_id.into();
        let status = status.into();
        if attempt_id.trim().is_empty() {
            return Err("summary attempt_id must not be empty".to_string());
        }
        if event_id.trim().is_empty() {
            return Err("summary event_id must not be empty".to_string());
        }
        if model_id.trim().is_empty() {
            return Err("summary model_id must not be empty".to_string());
        }
        if !SUMMARY_STATUSES.contains(&status.as_str()) {
            return Err("invalid summary status".to_string());
        }
        if reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err("summary reason must not be empty".to_string());
        }
        Ok(Self {
            attempt_id,
            event_id,
            model_id,
            prompt_version: SUMMARY_PROMPT_VERSION.to_string(),
            status,
            reason,
            lease_expires_at: None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryBackfillBatchResult {
    /// Entity IDs whose Fact (and optional summary embedding) was accepted.
    pub completed_ids: Vec<String>,
    /// All terminal status writes, including statuses supplied by the caller
    /// and row-local validation/policy outcomes produced here.
    pub terminal_statuses: Vec<SummaryStatusBatchInput>,
}

#[derive(Clone, Debug)]
pub struct MemoryRepository {
    paths: MemoryPaths,
    journal: Journal,
}

impl MemoryRepository {
    /// Open the EXE-adjacent memory-v2 stores and reconcile the journal before
    /// accepting new writes.  No user-profile or current-directory fallback is
    /// used; callers must provide the already resolved runtime root.
    pub async fn open(root_dir: impl AsRef<Path>) -> StoreResult<Self> {
        let root_dir = root_dir.as_ref().to_path_buf();
        let paths = MemoryPaths::from_runtime_root(&root_dir).map_err(memory_error)?;
        for path in [
            paths.memory_root(),
            paths.raw_events(),
            paths.facts(),
            paths.operations(),
            paths.embeddings(),
            paths.staging(),
            paths.legacy(),
        ] {
            std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
            paths.validate_contained_path(path).map_err(memory_error)?;
        }

        let journal = Journal::from_paths(&paths).map_err(|error| error.to_string())?;
        let repository = Self { paths, journal };
        repository.ensure_tables().await?;

        let journal_size = std::fs::metadata(repository.paths.journal())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if journal_size >= 64 * 1024 * 1024 {
            // Large compatibility journals are validated through the bounded
            // index.  When the manifest already points at the journal tail,
            // all materialized tables are current and replaying every raw
            // payload would only waste multiple gigabytes of RAM.
            let index = repository
                .journal
                .recover_index()
                .map_err(|error| error.to_string())?;
            let committed_sequence = index.next_sequence.saturating_sub(1);
            let manifest = ManifestSelector::from_paths(&repository.paths)
                .map_err(|error| error.to_string())?
                .read()
                .map_err(|error| error.to_string())?;
            if manifest
                .as_ref()
                .is_some_and(|manifest| manifest.committed_sequence() == committed_sequence)
            {
                repository.publish_manifest_for_sequence(committed_sequence)?;
            } else {
                return Err(
                    "large journal requires materialization before opening (manifest is stale)"
                        .to_string(),
                );
            }
        } else {
            let report = repository
                .journal
                .recover()
                .map_err(|error| error.to_string())?;
            // Crash recovery can contain the entire legacy import. Apply the
            // replayable operations in grouped LanceDB writes instead of opening
            // and mutating each table once per journal row.
            let replayable = report.replayable_record_refs();
            repository
                .apply_records_batched_refs(replayable.into_iter())
                .await?;
            repository.publish_manifest_for_report(&report)?;
        }
        Ok(repository)
    }

    pub fn paths(&self) -> &MemoryPaths {
        &self.paths
    }

    /// Return the canonical event identity used by memory-v2 for a legacy or
    /// compatibility ID.  The compatibility projection may retain the
    /// original string while raw events/facts use this stable UUID.
    pub fn canonical_event_id(entity_id: &str) -> String {
        stable_uuid(entity_id).to_string()
    }

    /// Persist a compatibility event into the immutable raw-events store.
    /// Repeating the same event ID is a no-op after verifying the existing
    /// operation; a malformed event is rejected before journaling.
    pub async fn append_compat_event(&self, input: CompatMemory) -> StoreResult<bool> {
        let event_id = stable_uuid(&input.id);
        let event_type = map_event_type(&input.memory_type);
        let source = map_source(&input.memory_type);
        let timestamp = canonical_timestamp(&input.timestamp);
        let event = RawEvent::try_new_admitted(
            event_id,
            Subject::SelfSubject,
            event_type,
            source,
            timestamp,
            &input.document,
            &Policy::default(),
            PrivacyAdmission::public(),
        )
        .map_err(memory_error)?;
        let payload = serde_json::to_value(&event).map_err(|error| error.to_string())?;
        let envelope = OperationEnvelope::new(
            OperationKind::RawEvent,
            event_id.to_string(),
            event.occurred_at(),
            Some(0),
            payload,
        )
        .map_err(memory_error)?;
        self.commit_operation(envelope).await
    }

    /// Append a bounded compatibility batch in one journal transaction. This
    /// is used by the legacy importer to avoid one LanceDB commit per row.
    pub async fn append_compat_events_batch(
        &self,
        inputs: Vec<CompatMemory>,
    ) -> StoreResult<usize> {
        let mut envelopes = Vec::with_capacity(inputs.len());
        for input in inputs {
            let event_id = stable_uuid(&input.id);
            let event_type = map_event_type(&input.memory_type);
            let source = map_source(&input.memory_type);
            let timestamp = canonical_timestamp(&input.timestamp);
            let event = RawEvent::try_new_admitted(
                event_id,
                Subject::SelfSubject,
                event_type,
                source,
                timestamp,
                &input.document,
                &Policy::default(),
                PrivacyAdmission::public(),
            )
            .map_err(memory_error)?;
            let payload = serde_json::to_value(&event).map_err(|error| error.to_string())?;
            envelopes.push(
                OperationEnvelope::new(
                    OperationKind::RawEvent,
                    event_id.to_string(),
                    event.occurred_at(),
                    Some(0),
                    payload,
                )
                .map_err(memory_error)?,
            );
        }
        self.commit_operations(envelopes).await
    }

    /// Persist a compatibility batch and its document embeddings in one
    /// journal transaction. This is used by the legacy importer so the large
    /// journal is parsed once per chunk rather than once per column.
    pub async fn append_compat_events_with_embeddings_batch(
        &self,
        inputs: Vec<CompatMemory>,
        embeddings: Vec<(String, Vec<f32>)>,
    ) -> StoreResult<usize> {
        let mut envelopes = Vec::with_capacity(inputs.len() + embeddings.len());
        for input in inputs {
            let event_id = stable_uuid(&input.id);
            let event = RawEvent::try_new_admitted(
                event_id,
                Subject::SelfSubject,
                map_event_type(&input.memory_type),
                map_source(&input.memory_type),
                canonical_timestamp(&input.timestamp),
                &input.document,
                &Policy::default(),
                PrivacyAdmission::public(),
            )
            .map_err(memory_error)?;
            let payload = serde_json::to_value(&event).map_err(|error| error.to_string())?;
            envelopes.push(
                OperationEnvelope::new(
                    OperationKind::RawEvent,
                    event_id.to_string(),
                    event.occurred_at(),
                    Some(0),
                    payload,
                )
                .map_err(memory_error)?,
            );
        }
        for (entity_id, values) in embeddings {
            validate_embedding(&values)?;
            let entity_uuid = stable_uuid(&entity_id);
            let payload =
                embedding_operation_payload(&entity_uuid.to_string(), &values, "document")?;
            envelopes.push(
                OperationEnvelope::new(
                    OperationKind::Embedding,
                    entity_uuid.to_string(),
                    "1970-01-01T00:00:00Z",
                    None,
                    payload,
                )
                .map_err(memory_error)?,
            );
        }
        self.commit_operations(envelopes).await
    }

    /// Persist a validated 768-dimensional embedding in the separate
    /// embeddings store.  Embeddings are append-only; duplicate operation IDs
    /// are harmless and a later retry never rewrites the raw event.
    pub async fn put_embedding(
        &self,
        entity_id: &str,
        values: Vec<f32>,
        source: &str,
    ) -> StoreResult<bool> {
        validate_embedding(&values)?;
        let entity_uuid = stable_uuid(entity_id);
        let payload = embedding_operation_payload(&entity_uuid.to_string(), &values, source)?;
        // The event identity and payload determine retries; do not include the
        // wall clock in the operation ID for a background retry.
        let issued_at = "1970-01-01T00:00:00Z";
        let envelope = OperationEnvelope::new(
            OperationKind::Embedding,
            entity_uuid.to_string(),
            issued_at,
            None,
            payload,
        )
        .map_err(memory_error)?;
        self.commit_operation(envelope).await
    }

    /// Persist multiple document embeddings in one journal transaction.
    pub async fn put_embeddings_batch(
        &self,
        inputs: Vec<(String, Vec<f32>)>,
    ) -> StoreResult<usize> {
        let mut envelopes = Vec::with_capacity(inputs.len());
        for (entity_id, values) in inputs {
            validate_embedding(&values)?;
            let entity_uuid = stable_uuid(&entity_id);
            let payload =
                embedding_operation_payload(&entity_uuid.to_string(), &values, "document")?;
            envelopes.push(
                OperationEnvelope::new(
                    OperationKind::Embedding,
                    entity_uuid.to_string(),
                    "1970-01-01T00:00:00Z",
                    None,
                    payload,
                )
                .map_err(memory_error)?,
            );
        }
        self.commit_operations(envelopes).await
    }

    /// Store a Gemma summary as a structured Fact.  Operational summary
    /// status remains in the compatibility projection; only an accepted
    /// summary becomes a Fact and therefore participates in Fact CAS rules.
    pub async fn append_summary_fact(&self, entity_id: &str, summary: &str) -> StoreResult<bool> {
        if summary.trim().is_empty() {
            return Err("summary must not be empty".to_string());
        }
        let event_id = stable_uuid(entity_id);
        // Compatibility rows currently carry only a display name for Twitch
        // or Discord speakers.  Treating that text as `self` would attribute
        // a third party's statement to the user, so keep the raw event but do
        // not create a Fact until a stable actor subject is available.
        if self
            .read_raw_events()
            .await?
            .iter()
            .find(|event| event.event_id() == event_id)
            .is_some_and(|event| {
                matches!(
                    event.source_kind(),
                    SourceKind::Twitch | SourceKind::Discord
                )
            })
        {
            return Ok(false);
        }
        // A summary is a fact derived from one event.  Include that stable
        // event identity in the fact key so two events cannot collapse into
        // the single historical `fact:self:summary` row.
        let key = format!("summary-{}", event_id.simple());
        let fact = Fact::try_derive(
            Subject::SelfSubject,
            key,
            summary.trim(),
            super::domain::FactStatus::Auto,
            event_id,
            &Policy::default(),
        )
        .map_err(memory_error)?;
        let payload = serde_json::to_value(&fact).map_err(|error| error.to_string())?;
        let envelope = OperationEnvelope::new(
            OperationKind::Fact,
            fact.fact_id(),
            "1970-01-01T00:00:00Z",
            Some(fact.revision()),
            payload,
        )
        .map_err(memory_error)?;
        self.commit_operation(envelope).await
    }

    /// Record a compatibility-projection summary status change in the same
    /// journal as the durable fact.  The projection still owns model/status
    /// columns for old clients; this operation makes that write recoverable
    /// and visible in the manifest sequence.
    pub async fn append_summary_status(
        &self,
        entity_id: &str,
        status: &str,
        model_id: Option<&str>,
        _prompt_version: Option<&str>,
    ) -> StoreResult<bool> {
        self.append_summary_status_with_attempt(entity_id, status, model_id, _prompt_version, None)
            .await
    }

    /// Record a status transition with an explicit attempt identity.  A
    /// retry may legitimately return to `pending` after an earlier pending
    /// operation, so the attempt is part of the operation identity while the
    /// status projection remains keyed by entity ID.
    pub async fn append_summary_status_with_attempt(
        &self,
        entity_id: &str,
        status: &str,
        model_id: Option<&str>,
        _prompt_version: Option<&str>,
        attempt_id: Option<&str>,
    ) -> StoreResult<bool> {
        let explicit_attempt = attempt_id.is_some();
        let attempt_id = attempt_id
            .map(str::to_string)
            .unwrap_or_else(|| summary_attempt_id(entity_id, status, model_id, None));
        let canonical_id = Self::canonical_event_id(entity_id);
        if let Some(current) = self.read_summary_statuses()?.get(&canonical_id) {
            // Legacy status rows are intentionally reopenable by the explicit
            // Process-all compatibility path. New v2 rows, however, obey a
            // monotonic transition boundary: only a pending attempt may
            // advance to a terminal result, and only an explicitly identified
            // retry may reopen fallback/error/skipped.
            let same_attempt = current.attempt_id.as_deref() == Some(attempt_id.as_str());
            let allowed = if !current.is_v2() {
                true
            } else {
                match current.status.as_str() {
                    "deleted" => false,
                    "pending" => status != "pending" || same_attempt || explicit_attempt,
                    "fallback" | "error" | "skipped" => {
                        (status == "pending" && explicit_attempt)
                            || (status == current.status && same_attempt)
                    }
                    "completed" => status == "completed" && same_attempt,
                    _ => true,
                }
            };
            if !allowed {
                return Ok(false);
            }
        }
        let envelope = summary_status_envelope_with_attempt(
            entity_id,
            status,
            model_id,
            Some(SUMMARY_PROMPT_VERSION),
            None,
            Some(attempt_id.as_str()),
        )?;
        self.commit_operation(envelope).await
    }

    /// Persist a fully-specified v2 attempt. The old status helper remains
    /// available for callers that only know the compatibility fields, while
    /// this API makes the durable attempt contract explicit at the boundary.
    pub async fn append_summary_attempt(&self, attempt: SummaryAttempt) -> StoreResult<bool> {
        if attempt.prompt_version != SUMMARY_PROMPT_VERSION {
            return Err(format!(
                "summary attempts must use prompt_version {}",
                SUMMARY_PROMPT_VERSION
            ));
        }
        let envelope = summary_status_envelope_with_lease(
            &attempt.event_id,
            &attempt.status,
            Some(&attempt.model_id),
            Some(SUMMARY_PROMPT_VERSION),
            attempt.reason.as_deref(),
            Some(&attempt.attempt_id),
            attempt.lease_expires_at.as_deref(),
        )?;
        self.commit_operation(envelope).await
    }

    /// Persist the terminal results of a backfill in one journal transaction.
    /// This is deliberately separate from the live one-event APIs: a large
    /// import must not perform one full journal recovery for every memory.
    pub async fn append_summary_backfill_batch(
        &self,
        summaries: Vec<SummaryBatchInput>,
        statuses: Vec<SummaryStatusBatchInput>,
    ) -> StoreResult<SummaryBackfillBatchResult> {
        let _batch_guard = summary_batch_lock().lock().await;
        // Read the durable snapshot once. Repair decisions are made from the
        // journal-backed status and Fact provenance, never from Fact
        // existence alone. The snapshot also lets one chunk avoid a
        // read-after-every-row race with a concurrent replay.
        let existing_facts = self.read_facts().await?;
        let raw_events = self.read_raw_events().await?;
        let raw_events_by_id: HashMap<Uuid, RawEvent> = raw_events
            .into_iter()
            .map(|event| (event.event_id(), event))
            .collect();
        let raw_event_ids: HashSet<Uuid> = raw_events_by_id.keys().copied().collect();
        let durable_statuses = self.read_summary_statuses()?;
        let summary_embedding_ids = self.read_summary_embedding_ids()?;

        let mut statuses = statuses;
        for status in &mut statuses {
            status.reason = normalize_backfill_reason(&status.status, status.reason.take());
        }
        let mut envelopes = Vec::with_capacity(summaries.len() * 4 + statuses.len() * 2);
        let mut completed_ids = Vec::with_capacity(summaries.len());
        let mut terminal_statuses = statuses;
        let mut seen_summary_events = HashSet::new();

        for input in summaries {
            let entity_id = input.entity_id.clone();
            let event_id = stable_uuid(&entity_id);
            if !seen_summary_events.insert(event_id) {
                // A duplicate input row is not a new attempt. Keeping only
                // the first result prevents two auto-rederive operations from
                // racing the same revision in one chunk.
                continue;
            }

            // Status is authoritative for v2 terminal attempts. Check it
            // before validating the replayed payload so a malformed or stale
            // duplicate cannot downgrade an already completed/skipped/deleted
            // attempt to fallback. Explicit retry creates a fresh pending
            // attempt before a new result is accepted.
            let canonical_id = event_id.to_string();
            let status = durable_statuses.get(&canonical_id);
            if status.is_some_and(|status| status.is_v2() && status.is_retryable_failure()) {
                // A v2 fallback/error is intentionally not an implicit retry.
                // The explicit retry path first creates a fresh pending
                // attempt, after which this result may be committed.
                continue;
            }
            if status.is_some_and(|status| status.status == "deleted") {
                continue;
            }
            if status.is_some_and(|status| {
                status.is_v2() && matches!(status.status.as_str(), "completed" | "skipped")
            }) {
                // Same-version terminal attempts are idempotent even when a
                // stale compatibility projection no longer exposes a Fact.
                continue;
            }

            // Raw external chat is eligible for curation, but the current
            // compatibility source contains no stable actor ID.  Never turn
            // it into a self Fact; persist a durable skip and let the raw
            // event remain the evidence until subject mapping is introduced.
            if raw_events_by_id.get(&event_id).is_some_and(|event| {
                matches!(
                    event.source_kind(),
                    SourceKind::Twitch | SourceKind::Discord
                )
            }) {
                terminal_statuses.push(generated_summary_status_for_input(
                    &input,
                    "skipped",
                    "subject_unresolved",
                ));
                continue;
            }

            let raw_event = raw_events_by_id.get(&event_id);
            if input.summary.trim().is_empty() {
                terminal_statuses.push(generated_summary_status_for_input(
                    &input,
                    "fallback",
                    "empty_summary",
                ));
                continue;
            }
            if input.summary.chars().count() > SUMMARY_MAX_CHARS
                || input.summary.chars().any(char::is_control)
            {
                terminal_statuses.push(generated_summary_status_for_input(
                    &input,
                    "fallback",
                    "invalid_model_output",
                ));
                continue;
            }
            if is_metadata_echo_value(input.summary.trim(), raw_event) {
                terminal_statuses.push(generated_summary_status_for_input(
                    &input,
                    "fallback",
                    "metadata_echo",
                ));
                continue;
            }
            if let Some(ref embedding) = input.embedding {
                if validate_embedding(embedding).is_err() {
                    terminal_statuses.push(generated_summary_status_for_input(
                        &input,
                        "fallback",
                        "invalid_embedding",
                    ));
                    continue;
                }
            }

            let existing = existing_facts
                .iter()
                .filter(|fact| fact.source_event_id() == event_id && fact.is_summary_fact())
                .max_by(|left, right| {
                    left.revision()
                        .cmp(&right.revision())
                        .then(left.status().cmp(&right.status()))
                });

            if let Some(existing) = existing {
                if existing.status() != super::domain::FactStatus::Auto {
                    terminal_statuses.push(generated_summary_status_for_input(
                        &input,
                        "skipped",
                        "fact_protected",
                    ));
                    continue;
                }
                if !raw_event_ids.contains(&existing.source_event_id()) {
                    // An auto summary without raw provenance is not safe to
                    // repair. Preserve it for a human/explicit migration
                    // decision rather than guessing its source.
                    terminal_statuses.push(generated_summary_status_for_input(
                        &input,
                        "skipped",
                        "source_provenance_missing",
                    ));
                    continue;
                }
                if !summary_fact_is_repairable(
                    existing,
                    status,
                    raw_events_by_id.get(&existing.source_event_id()),
                ) {
                    continue;
                }
            }

            let key = format!("summary-{}", event_id.simple());
            let (fact, fact_payload, expected_revision) = match existing {
                Some(prior) => {
                    let fact = prior
                        .try_rederive_auto(
                            input.summary.trim(),
                            prior.revision(),
                            &Policy::default(),
                        )
                        .map_err(memory_error)?;
                    let payload = json!({
                        "action": "upsert",
                        "repair": true,
                        "fact": fact,
                        "prior": prior,
                    });
                    (fact, payload, prior.revision())
                }
                None => {
                    let fact = match Fact::try_derive(
                        Subject::SelfSubject,
                        key,
                        input.summary.trim(),
                        super::domain::FactStatus::Auto,
                        event_id,
                        &Policy::default(),
                    ) {
                        Ok(fact) => fact,
                        Err(MemoryError::ProhibitedCategory(_)) => {
                            terminal_statuses.push(generated_summary_status_for_input(
                                &input,
                                "skipped",
                                "policy_excluded",
                            ));
                            continue;
                        }
                        Err(_) => {
                            terminal_statuses.push(generated_summary_status_for_input(
                                &input,
                                "fallback",
                                "fact_validation_failed",
                            ));
                            continue;
                        }
                    };
                    let payload = serde_json::to_value(&fact).map_err(|error| error.to_string())?;
                    (fact, payload, 0)
                }
            };

            envelopes.push(
                OperationEnvelope::new(
                    OperationKind::Fact,
                    fact.fact_id(),
                    "1970-01-01T00:00:00Z",
                    Some(expected_revision),
                    fact_payload,
                )
                .map_err(memory_error)?,
            );
            if let Some(embedding) = input.embedding {
                envelopes.push(
                    OperationEnvelope::new(
                        OperationKind::Embedding,
                        event_id.to_string(),
                        "1970-01-01T00:00:00Z",
                        None,
                        embedding_operation_payload(&event_id.to_string(), &embedding, "summary")?,
                    )
                    .map_err(memory_error)?,
                );
            }
            let model_id = input
                .model_id
                .as_deref()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or(SUMMARY_MODEL_ID);
            let attempt_id = input
                .attempt_id
                .as_deref()
                .filter(|attempt| !attempt.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    summary_attempt_id(&entity_id, "completed", Some(model_id), None)
                });
            envelopes.push(summary_status_envelope_with_attempt(
                &entity_id,
                "completed",
                Some(model_id),
                Some(SUMMARY_PROMPT_VERSION),
                None,
                Some(attempt_id.as_str()),
            )?);
            completed_ids.push(entity_id);
        }

        // A v2 should_store=false result is represented by a skipped terminal
        // attempt. An eligible automatic Fact is retracted in this same
        // journal batch; the raw event is never touched. Confirmation/editing
        // and deleted tombstones are immutable and therefore do not enter
        // this branch.
        let mut retracted_fact_ids = HashSet::new();
        for input in &terminal_statuses {
            let event_id = stable_uuid(&input.entity_id);
            let current_status = durable_statuses.get(&event_id.to_string());
            let prior_summary_fact = existing_facts
                .iter()
                .filter(|fact| fact.source_event_id() == event_id && fact.is_summary_fact())
                .max_by(|left, right| {
                    left.revision()
                        .cmp(&right.revision())
                        .then(left.status().cmp(&right.status()))
                });
            let has_summary_fact = prior_summary_fact.is_some();
            let repairable_summary_fact = prior_summary_fact.is_some_and(|fact| {
                summary_fact_is_repairable(fact, current_status, raw_events_by_id.get(&event_id))
            });
            if !summary_batch_status_transition_allowed(
                current_status,
                &input.status,
                has_summary_fact,
                repairable_summary_fact,
            ) {
                // Keep the caller's terminal row in the returned accounting,
                // but do not append a stale status or retract a newer Fact.
                continue;
            }
            if is_should_store_false(input)
                && input.status == "skipped"
                && !durable_statuses
                    .get(&Self::canonical_event_id(&input.entity_id))
                    .is_some_and(|status| status.status == "deleted")
            {
                if let Some(prior) = existing_facts
                    .iter()
                    .filter(|fact| {
                        fact.source_event_id() == event_id
                            && fact.is_auto_summary()
                            && raw_event_ids.contains(&event_id)
                    })
                    .max_by(|left, right| left.revision().cmp(&right.revision()))
                {
                    if retracted_fact_ids.insert(prior.fact_id().to_string()) {
                        envelopes.push(summary_fact_retraction_envelope(
                            prior,
                            input.reason.as_deref().unwrap_or("model_declined"),
                        )?);
                    }
                }
                if summary_embedding_ids.contains(&event_id.to_string()) {
                    envelopes.push(summary_embedding_retraction_envelope(
                        &event_id,
                        input.reason.as_deref().unwrap_or("model_declined"),
                    )?);
                }
            }
            let attempt_id = input
                .attempt_id
                .as_deref()
                .filter(|attempt| !attempt.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    // A pending status already present in the durable journal
                    // represents a stale/incomplete attempt.  Requeueing it
                    // must create a fresh identity so a crash/restart cannot
                    // make two inferences look like one attempt.  First-time
                    // status writes retain the deterministic compatibility ID.
                    (input.status == "pending"
                        && durable_statuses
                            .get(&Self::canonical_event_id(&input.entity_id))
                            .is_some_and(|status| status.status == "pending"))
                    .then(|| format!("attempt-{}", Uuid::new_v4().simple()))
                });
            envelopes.push(summary_status_envelope_with_attempt(
                &input.entity_id,
                &input.status,
                input.model_id.as_deref(),
                input.prompt_version.as_deref(),
                input.reason.as_deref(),
                attempt_id.as_deref(),
            )?);
        }
        self.commit_operations(envelopes).await?;
        Ok(SummaryBackfillBatchResult {
            completed_ids,
            terminal_statuses,
        })
    }

    /// Journal and apply a redaction.  Redaction is append-only in the
    /// journal, while the three materialized stores remove the entity on
    /// replay; this keeps a deletion durable without mutating the journal.
    pub async fn append_redaction(&self, entity_id: &str) -> StoreResult<bool> {
        let entity_id = stable_uuid(entity_id).to_string();
        let envelope = OperationEnvelope::new(
            OperationKind::Redaction,
            entity_id.clone(),
            "1970-01-01T00:00:00Z",
            None,
            json!({"entity_id": entity_id}),
        )
        .map_err(memory_error)?;
        self.commit_operation(envelope).await
    }

    /// Read authoritative raw events.  Every row is decoded through the
    /// domain deserializer so malformed storage fails closed instead of being
    /// silently projected to the UI.
    pub async fn read_raw_events(&self) -> StoreResult<Vec<RawEvent>> {
        let table = open_table(&self.paths.raw_events().to_path_buf(), RAW_TABLE).await?;
        let mut stream = table.query().execute().await.map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(|e| e.to_string())? {
            let event_ids = required_strings(&batch, "event_id")?;
            let subjects = required_strings(&batch, "subject")?;
            let event_types = required_strings(&batch, "event_type")?;
            let sources = required_strings(&batch, "source")?;
            let occurred = required_strings(&batch, "occurred_at")?;
            let contents = required_strings(&batch, "content")?;
            for row in 0..batch.num_rows() {
                let value = json!({
                    "event_id": event_ids.value(row), "subject": subjects.value(row),
                    "event_type": event_types.value(row), "source": sources.value(row),
                    "occurred_at": occurred.value(row), "content": contents.value(row),
                });
                rows.push(
                    serde_json::from_value(value).map_err(|e| format!("invalid raw row: {e}"))?,
                );
            }
        }
        Ok(rows)
    }

    /// Read authoritative facts with strict domain validation.
    pub async fn read_facts(&self) -> StoreResult<Vec<Fact>> {
        let table = open_table(&self.paths.facts().to_path_buf(), FACTS_TABLE).await?;
        let mut stream = table.query().execute().await.map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(|e| e.to_string())? {
            let fact_ids = required_strings(&batch, "fact_id")?;
            let subjects = required_strings(&batch, "subject")?;
            let predicates = required_strings(&batch, "predicate")?;
            let keys = required_strings(&batch, "key")?;
            let values = required_strings(&batch, "value")?;
            let statuses = required_strings(&batch, "status")?;
            let source_ids = required_strings(&batch, "source_event_id")?;
            let revisions = batch
                .column_by_name("revision")
                .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
                .ok_or_else(|| "missing revision column".to_string())?;
            let operation_ids = required_strings(&batch, "operation_id")?;
            for row in 0..batch.num_rows() {
                let value = json!({
                    "fact_id": fact_ids.value(row), "subject": subjects.value(row),
                    "predicate": predicates.value(row), "key": keys.value(row),
                    "value": values.value(row), "status": statuses.value(row),
                    "source_event_id": source_ids.value(row), "revision": revisions.value(row),
                    "operation_id": operation_ids.value(row),
                });
                rows.push(
                    serde_json::from_value(value).map_err(|e| format!("invalid fact row: {e}"))?,
                );
            }
        }
        Ok(rows)
    }

    /// Return summary event IDs that have an explicit deletion tombstone in
    /// the journal. Fact deletion removes the materialized Fact row, so the
    /// journal is the only durable source that can prevent a later
    /// all-memory backfill from recreating a user's deletion.
    pub async fn read_deleted_summary_event_ids(&self) -> StoreResult<HashSet<String>> {
        Ok(self
            .read_summary_statuses()?
            .into_iter()
            .filter_map(|(entity_id, status)| (status.status == "deleted").then_some(entity_id))
            .collect())
    }

    /// Decode the latest summary status for each event from committed journal
    /// envelopes.  Fact-delete tombstones take precedence over older or
    /// malformed compatibility status rows so a later backfill cannot
    /// recreate a user deletion.
    pub fn read_summary_statuses(&self) -> StoreResult<HashMap<String, SummaryStatusRecord>> {
        let index = self
            .journal
            .recover_index()
            .map_err(|error| error.to_string())?;
        if let Some(error) = index.summary_status_error.as_deref() {
            return Err(error.to_string());
        }
        if let Some(error) = index.summary_fact_delete_error.as_deref() {
            return Err(error.to_string());
        }
        let mut statuses = HashMap::new();
        let mut deleted = HashSet::new();
        for indexed in &index.summary_statuses {
            let entity_id = Self::canonical_event_id(&indexed.entity_id);
            let event_id = indexed
                .event_id
                .as_deref()
                .map(Self::canonical_event_id)
                .unwrap_or_else(|| entity_id.clone());
            if event_id != entity_id {
                return Err("summary status entity_id/event_id mismatch".to_string());
            }
            if !SUMMARY_STATUSES.contains(&indexed.status.as_str()) {
                return Err(format!("invalid summary status: {}", indexed.status));
            }
            if let Some(lease) = indexed.lease_expires_at.as_deref() {
                CanonicalUtc::parse(lease).map_err(|_| {
                    "summary status lease_expires_at is not canonical UTC".to_string()
                })?;
            }
            let status = SummaryStatusRecord {
                entity_id: entity_id.clone(),
                event_id,
                status: indexed.status.clone(),
                reason: indexed.reason.clone(),
                model_id: indexed.model_id.clone(),
                prompt_version: indexed.prompt_version.clone(),
                attempt_id: indexed.attempt_id.clone(),
                lease_expires_at: indexed.lease_expires_at.clone(),
                sequence: indexed.sequence,
            };
            if status.status == "deleted" {
                deleted.insert(status.entity_id.clone());
            }
            statuses.insert(status.entity_id.clone(), status);
        }
        for indexed in &index.summary_fact_deletes {
            deleted.insert(Self::canonical_event_id(&indexed.entity_id));
        }
        for entity_id in deleted {
            statuses.insert(
                entity_id.clone(),
                SummaryStatusRecord {
                    entity_id: entity_id.clone(),
                    event_id: entity_id,
                    status: "deleted".into(),
                    ..SummaryStatusRecord::default()
                },
            );
        }
        Ok(statuses)
    }

    fn read_summary_embedding_ids(&self) -> StoreResult<HashSet<String>> {
        let index = self
            .journal
            .recover_index()
            .map_err(|error| error.to_string())?;
        if let Some(error) = index.summary_embedding_error.as_deref() {
            return Err(error.to_string());
        }
        Ok(index
            .summary_embedding_ids
            .iter()
            .map(|entity_id| Self::canonical_event_id(entity_id))
            .collect())
    }

    pub fn journal_sequence(&self) -> StoreResult<u64> {
        Ok(self
            .journal
            .recover_index()
            .map_err(|e| e.to_string())?
            .next_sequence
            .saturating_sub(1))
    }

    /// Resolve an event's inference disposition from the durable attempt
    /// first, then use Fact provenance only for legacy detection. A stale
    /// Fact therefore cannot make a fallback/error/pending attempt appear
    /// completed.
    pub async fn summary_processing_decision_for_event(
        &self,
        event_id: &str,
        mode: SummaryProcessMode,
        now: Option<&str>,
    ) -> StoreResult<SummaryProcessDecision> {
        let canonical = Self::canonical_event_id(event_id);
        let has_summary_fact =
            self.read_facts().await?.iter().any(|fact| {
                fact.is_summary_fact() && fact.source_event_id().to_string() == canonical
            });
        let statuses = self.read_summary_statuses()?;
        Ok(summary_processing_decision(
            statuses.get(&canonical),
            has_summary_fact,
            mode,
            now,
        ))
    }

    /// Journal a CAS-checked fact upsert.  The action wrapper is additive; old
    /// summary-derived Fact envelopes remain accepted by replay.
    pub async fn append_fact_upsert(
        &self,
        fact: &Fact,
        expected_revision: u64,
    ) -> StoreResult<(bool, String)> {
        let existing = self
            .read_facts()
            .await?
            .into_iter()
            .find(|current| current.fact_id() == fact.fact_id());
        let payload = match existing.as_ref() {
            Some(prior) => json!({ "action": "upsert", "fact": fact, "prior": prior }),
            None => json!({ "action": "upsert", "fact": fact }),
        };
        let envelope = OperationEnvelope::new(
            OperationKind::Fact,
            fact.fact_id(),
            "1970-01-01T00:00:00Z",
            Some(expected_revision),
            payload,
        )
        .map_err(memory_error)?;
        let operation_id = envelope.operation_id().to_string();
        // Idempotent retries must be allowed to replay the exact same
        // operation, but a different operation cannot overwrite a newer
        // revision merely because its caller read an older snapshot.
        let journal = self.journal.clone();
        let index = journal.recover_index().map_err(|error| error.to_string())?;
        if index.committed_operation_ids.contains(&operation_id) {
            return Ok((false, operation_id));
        }
        if index.operation_ids.contains(&operation_id) {
            return Ok((self.commit_operation(envelope).await?, operation_id));
        }
        if existing
            .as_ref()
            .map(|current| current.revision() != expected_revision)
            .unwrap_or(expected_revision != 0)
        {
            return Err("fact revision conflict".to_string());
        }
        Ok((self.commit_operation(envelope).await?, operation_id))
    }

    /// Fact-only delete.  The prior Fact is retained in the journal payload so
    /// Undo can compensate without touching the immutable raw-event store.
    pub async fn append_fact_delete(
        &self,
        fact: &Fact,
        expected_revision: u64,
    ) -> StoreResult<(bool, String)> {
        let payload = json!({ "action": "delete", "fact_id": fact.fact_id(), "prior": fact });
        let envelope = OperationEnvelope::new(
            OperationKind::Fact,
            fact.fact_id(),
            "1970-01-01T00:00:00Z",
            Some(expected_revision),
            payload,
        )
        .map_err(memory_error)?;
        let operation_id = envelope.operation_id().to_string();
        let journal = self.journal.clone();
        let index = journal.recover_index().map_err(|error| error.to_string())?;
        if index.committed_operation_ids.contains(&operation_id) {
            return Ok((false, operation_id));
        }
        if index.operation_ids.contains(&operation_id) {
            return Ok((self.commit_operation(envelope).await?, operation_id));
        }
        let existing = self
            .read_facts()
            .await?
            .into_iter()
            .find(|current| current.fact_id() == fact.fact_id());
        if existing
            .as_ref()
            .map(|current| current.revision() != expected_revision)
            .unwrap_or(true)
        {
            return Err("fact revision conflict".to_string());
        }
        Ok((self.commit_operation(envelope).await?, operation_id))
    }

    async fn commit_operation(&self, envelope: OperationEnvelope) -> StoreResult<bool> {
        let journal = self.journal.clone();
        let report = journal.recover().map_err(|error| error.to_string())?;
        if envelope.kind() == OperationKind::RawEvent {
            // Raw events are immutable by event identity.  Operation IDs are
            // content-addressed, so accepting a second envelope for the same
            // event ID would otherwise append a second journal operation that
            // becomes a silent no-op in the materialized table.
            for record in report.records().iter().filter(|record| {
                record.state() == JournalState::Operation
                    && record.operation_kind() == OperationKind::RawEvent.as_str()
            }) {
                let existing: OperationEnvelope = serde_json::from_value(record.payload().clone())
                    .map_err(|error| format!("invalid raw event journal envelope: {error}"))?;
                if existing.entity_id() == envelope.entity_id()
                    && existing.operation_id() != envelope.operation_id()
                {
                    // Older compatibility imports used the wall clock when a
                    // legacy timestamp could not be parsed.  Such an import
                    // can leave the same event with a different operation ID
                    // after a retry.  The event identity and content are the
                    // durable truth; tolerate only that historical timestamp
                    // drift and continue rejecting actual content conflicts.
                    if same_raw_event_except_timestamp(existing.payload(), envelope.payload()) {
                        return Ok(false);
                    }
                    return Err(format!(
                        "conflicting raw event operation for event {}",
                        envelope.entity_id()
                    ));
                }
            }
        }
        let already_present = report.records().iter().any(|record| {
            record.state() == JournalState::Operation
                && record.operation_id() == envelope.operation_id()
        });
        if let Some(record) = report
            .replayable_record_refs()
            .into_iter()
            .find(|record| record.operation_id() == envelope.operation_id())
        {
            self.apply_record(&record, false).await?;
            self.publish_manifest(&journal)?;
            return Ok(false);
        }

        if let Some(record) = report.records().iter().find(|record| {
            record.state() == JournalState::Operation
                && record.operation_id() == envelope.operation_id()
        }) {
            // An operation record can survive a crash before its commit frame.
            // Finish that existing batch rather than treating it as a retry
            // and leaving the durable intent unapplied forever.
            self.journal
                .commit_batch(record.batch_id())
                .map_err(|error| error.to_string())?;
        } else if let Some(begin) = report.records().iter().find(|record| {
            record.state() == JournalState::Begin
                && !report.records().iter().any(|operation| {
                    operation.batch_id() == record.batch_id()
                        && operation.state() == JournalState::Operation
                })
        }) {
            // Likewise, complete a batch that only made it through its begin
            // frame after an interrupted write.
            self.journal
                .append_operation_envelope(begin.batch_id(), &envelope)
                .map_err(|error| error.to_string())?;
            self.journal
                .commit_batch(begin.batch_id())
                .map_err(|error| error.to_string())?;
        } else {
            let batch_id = format!("batch-{}", Uuid::new_v4());
            self.journal
                .write_batch(
                    &batch_id,
                    &[(
                        envelope.operation_id().to_string(),
                        envelope.kind(),
                        serde_json::to_value(&envelope).map_err(|error| error.to_string())?,
                    )],
                )
                .map_err(|error| error.to_string())?;
        }
        let committed_report = self.journal.recover().map_err(|error| error.to_string())?;
        if let Some(record) = committed_report
            .replayable_record_refs()
            .into_iter()
            .find(|record| record.operation_id() == envelope.operation_id())
        {
            self.apply_record(&record, true).await?;
        }
        self.publish_manifest(&self.journal)?;
        Ok(!already_present)
    }

    /// Commit a bounded batch of operations with one journal append and
    /// materialized-table write per operation kind. Legacy migration uses this
    /// path so tens of thousands of rows do not require tens of thousands of
    /// individual LanceDB transactions.
    async fn commit_operations(&self, envelopes: Vec<OperationEnvelope>) -> StoreResult<usize> {
        if envelopes.is_empty() {
            return Ok(0);
        }
        let journal = self.journal.clone();
        let mut pending = Vec::new();
        let mut apply_envelopes = Vec::new();
        let mut seen = HashSet::new();
        {
            let index = journal.recover_index().map_err(|error| error.to_string())?;
            // Build the raw-event identity index once. The compact journal
            // view retains only an identity/content fingerprint, never the
            // full raw document or embedding vector.
            for envelope in &envelopes {
                if envelope.kind() == OperationKind::RawEvent {
                    if let Some(existing) = index.raw_events.get(envelope.entity_id()) {
                        if existing.operation_id != envelope.operation_id() {
                            if existing.fingerprint.as_deref()
                                != raw_event_fingerprint(envelope.payload()).as_deref()
                            {
                                return Err(format!(
                                    "conflicting raw event operation for event {}",
                                    envelope.entity_id()
                                ));
                            }
                        }
                    }
                }
                if seen.insert(envelope.operation_id().to_string()) {
                    apply_envelopes.push(envelope.clone());
                }
                if !index.operation_ids.contains(envelope.operation_id()) {
                    pending.push((
                        envelope.operation_id().to_string(),
                        envelope.kind(),
                        serde_json::to_value(envelope).map_err(|error| error.to_string())?,
                    ));
                }
            }
        }

        // An interrupted large-journal batch is safe to repair only when the
        // compact index still exposes its final open batch.  The normal
        // backfill path never leaves one open, but keeping this check prevents
        // silently accepting an uncommitted operation after a crash.
        if !pending.is_empty() {
            let batch_id = format!("batch-{}", Uuid::new_v4());
            journal
                .write_batch(&batch_id, &pending)
                .map_err(|error| error.to_string())?;
        }

        let records = apply_envelopes
            .into_iter()
            .map(synthetic_operation_record)
            .collect::<StoreResult<Vec<_>>>()?;
        self.apply_records_batched(&records).await?;
        let index = journal.recover_index().map_err(|error| error.to_string())?;
        self.publish_manifest_for_sequence(index.next_sequence.saturating_sub(1))?;
        Ok(pending.len())
    }

    async fn ensure_tables(&self) -> StoreResult<()> {
        let raw_schema = raw_schema();
        ensure_table(
            &self.paths.raw_events().to_path_buf(),
            RAW_TABLE,
            raw_schema.clone(),
            empty_raw_batch(raw_schema)?,
        )
        .await?;
        let facts_schema = facts_schema();
        ensure_table(
            &self.paths.facts().to_path_buf(),
            FACTS_TABLE,
            facts_schema.clone(),
            empty_facts_batch(facts_schema)?,
        )
        .await?;
        let embeddings_schema = embeddings_schema();
        ensure_table(
            &self.paths.embeddings().to_path_buf(),
            EMBEDDINGS_TABLE,
            embeddings_schema.clone(),
            empty_embeddings_batch(embeddings_schema)?,
        )
        .await
        .map(|_| ())
    }

    async fn apply_records_batched(&self, records: &[JournalRecord]) -> StoreResult<()> {
        self.apply_records_batched_refs(records.iter()).await
    }

    async fn apply_records_batched_refs<'a, I>(&self, records: I) -> StoreResult<()>
    where
        I: IntoIterator<Item = &'a JournalRecord>,
    {
        let _guard = materialize_lock().lock().await;
        self.apply_records_batched_locked(records).await
    }

    async fn apply_records_batched_locked<'a, I>(&self, records: I) -> StoreResult<()>
    where
        I: IntoIterator<Item = &'a JournalRecord>,
    {
        let mut raw_events = Vec::new();
        let mut embeddings = Vec::new();
        let mut facts = Vec::new();
        let mut fallback = Vec::new();
        for record in records {
            let envelope: OperationEnvelope = serde_json::from_value(record.payload().clone())
                .map_err(|error| format!("invalid journal envelope: {error}"))?;
            match envelope.kind() {
                OperationKind::RawEvent => {
                    let event: RawEvent = serde_json::from_value(envelope.payload().clone())
                        .map_err(|error| format!("invalid raw event payload: {error}"))?;
                    raw_events.push(event);
                }
                OperationKind::Embedding => {
                    if envelope.payload().get("action").and_then(Value::as_str)
                        == Some("retract_summary")
                    {
                        fallback.push(record.clone());
                        continue;
                    }
                    let entity_id = envelope
                        .payload()
                        .get("entity_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "embedding entity_id missing".to_string())?;
                    let values = envelope
                        .payload()
                        .get("embedding")
                        .and_then(Value::as_array)
                        .ok_or_else(|| "embedding values missing".to_string())?
                        .iter()
                        .map(decode_embedding_value)
                        .collect::<StoreResult<Vec<_>>>()?;
                    let source = envelope
                        .payload()
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or("document")
                        .to_string();
                    embeddings.push((entity_id.to_string(), values, source));
                }
                OperationKind::Fact => {
                    let action = envelope.payload().get("action").and_then(Value::as_str);
                    if action.is_none() {
                        let fact_value = envelope
                            .payload()
                            .get("fact")
                            .cloned()
                            .unwrap_or_else(|| envelope.payload().clone());
                        let fact: Fact = serde_json::from_value(fact_value)
                            .map_err(|error| format!("invalid fact payload: {error}"))?;
                        facts.push(fact);
                    } else {
                        fallback.push(record.clone());
                    }
                }
                _ => fallback.push(record.clone()),
            }
        }
        if !raw_events.is_empty() {
            self.apply_raw_events(raw_events).await?;
        }
        if !embeddings.is_empty() {
            self.apply_embeddings(embeddings).await?;
        }
        if !facts.is_empty() {
            self.apply_facts(facts).await?;
        }
        for record in fallback {
            self.apply_record_locked(&record, false).await?;
        }
        Ok(())
    }

    async fn apply_raw_events(&self, events: Vec<RawEvent>) -> StoreResult<()> {
        let table = open_table(&self.paths.raw_events().to_path_buf(), RAW_TABLE).await?;
        let ids = events
            .iter()
            .map(|event| event.event_id().to_string())
            .collect::<Vec<_>>();
        let existing = existing_ids(&table, &ids).await?;
        let mut seen = HashSet::new();
        let pending = events
            .into_iter()
            .filter(|event| {
                !existing.contains(&event.event_id().to_string())
                    && seen.insert(event.event_id().to_string())
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        let schema = raw_schema();
        let batch = raw_batch(&pending, schema.clone())?;
        add_batch(&table, batch, schema).await
    }

    async fn apply_embeddings(
        &self,
        embeddings: Vec<(String, Vec<f32>, String)>,
    ) -> StoreResult<()> {
        let table = open_table(&self.paths.embeddings().to_path_buf(), EMBEDDINGS_TABLE).await?;
        let ids = embeddings
            .iter()
            .map(|(entity_id, _, _)| entity_id.clone())
            .collect::<Vec<_>>();
        let existing = existing_ids(&table, &ids).await?;
        let mut adds = Vec::new();
        let mut seen = HashSet::new();
        for (entity_id, values, source) in embeddings {
            validate_embedding(&values)?;
            if !existing.contains(&entity_id) && seen.insert(entity_id.clone()) {
                adds.push((entity_id, values));
            } else if source == "summary" {
                table
                    .update()
                    .only_if(format!("entity_id = '{}'", escape_sql(&entity_id)))
                    .column("embedding", embedding_literal(&values))
                    .execute()
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        if adds.is_empty() {
            return Ok(());
        }
        let schema = embeddings_schema();
        let batch = embeddings_batch(&adds, schema.clone())?;
        add_batch(&table, batch, schema).await
    }

    async fn apply_facts(&self, facts: Vec<Fact>) -> StoreResult<()> {
        let table = open_table(&self.paths.facts().to_path_buf(), FACTS_TABLE).await?;
        let ids = facts
            .iter()
            .map(|fact| fact.fact_id().to_string())
            .collect::<Vec<_>>();
        let existing = existing_ids(&table, &ids).await?;
        let mut pending = Vec::new();
        let mut seen = HashSet::new();
        for fact in facts {
            let id = fact.fact_id().to_string();
            if !existing.contains(&id) && seen.insert(id) {
                pending.push(fact);
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let schema = facts_schema();
        let batch = fact_batch_slice(&pending, schema.clone())?;
        add_batch(&table, batch, schema).await
    }

    async fn apply_record(
        &self,
        record: &JournalRecord,
        enforce_fact_cas: bool,
    ) -> StoreResult<()> {
        let _guard = materialize_lock().lock().await;
        self.apply_record_locked(record, enforce_fact_cas).await
    }

    async fn apply_record_locked(
        &self,
        record: &JournalRecord,
        enforce_fact_cas: bool,
    ) -> StoreResult<()> {
        let envelope: OperationEnvelope = serde_json::from_value(record.payload().clone())
            .map_err(|error| format!("invalid journal envelope: {error}"))?;
        match envelope.kind() {
            OperationKind::RawEvent => {
                let event: RawEvent = serde_json::from_value(envelope.payload().clone())
                    .map_err(|error| format!("invalid raw event payload: {error}"))?;
                let table = open_table(&self.paths.raw_events().to_path_buf(), RAW_TABLE).await?;
                let existing = existing_ids(&table, &[event.event_id().to_string()]).await?;
                if existing.is_empty() {
                    let schema = raw_schema();
                    let batch = raw_batch(&[event], schema.clone())?;
                    add_batch(&table, batch, schema).await?;
                }
            }
            OperationKind::Embedding => {
                if envelope.payload().get("action").and_then(Value::as_str)
                    == Some("retract_summary")
                {
                    let entity_id = envelope
                        .payload()
                        .get("entity_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            "summary embedding retraction entity_id missing".to_string()
                        })?;
                    let table =
                        open_table(&self.paths.embeddings().to_path_buf(), EMBEDDINGS_TABLE)
                            .await?;
                    table
                        .delete(&format!("entity_id = '{}'", escape_sql(entity_id)))
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                let entity_id = envelope
                    .payload()
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "embedding entity_id missing".to_string())?;
                let values = envelope
                    .payload()
                    .get("embedding")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "embedding values missing".to_string())?
                    .iter()
                    .map(decode_embedding_value)
                    .collect::<StoreResult<Vec<_>>>()?;
                validate_embedding(&values)?;
                let source = envelope
                    .payload()
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("document");
                let table =
                    open_table(&self.paths.embeddings().to_path_buf(), EMBEDDINGS_TABLE).await?;
                let existing = existing_ids(&table, &[entity_id.to_string()]).await?;
                if existing.is_empty() {
                    let schema = embeddings_schema();
                    let batch = embedding_batch(entity_id, &values, schema.clone())?;
                    add_batch(&table, batch, schema).await?;
                } else if source == "summary" {
                    // The embeddings store keeps one retrieval vector per
                    // entity. A completed summary is authoritative, so its
                    // vector promotes/replaces the document vector; late
                    // document retries remain no-ops above.
                    table
                        .update()
                        .only_if(format!("entity_id = '{}'", entity_id.replace('\'', "''")))
                        .column("embedding", embedding_literal(&values))
                        .execute()
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
            OperationKind::Fact => {
                let table = open_table(&self.paths.facts().to_path_buf(), FACTS_TABLE).await?;
                let action = envelope.payload().get("action").and_then(Value::as_str);
                if action == Some("delete") {
                    let fact_id = envelope
                        .payload()
                        .get("fact_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "fact delete fact_id missing".to_string())?;
                    let predicate = if enforce_fact_cas {
                        let expected_revision = envelope
                            .expected_revision()
                            .ok_or_else(|| "fact delete expected_revision missing".to_string())?;
                        format!(
                            "fact_id = '{}' AND revision = {}",
                            escape_sql(fact_id),
                            expected_revision
                        )
                    } else {
                        format!("fact_id = '{}'", escape_sql(fact_id))
                    };
                    let result = table.delete(&predicate).await.map_err(|e| e.to_string())?;
                    if enforce_fact_cas && result.num_deleted_rows == 0 {
                        return Err("fact revision conflict".to_string());
                    }
                } else if action == Some("retract") {
                    let fact_id = envelope
                        .payload()
                        .get("fact_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "fact retraction fact_id missing".to_string())?;
                    let prior = envelope
                        .payload()
                        .get("prior")
                        .cloned()
                        .ok_or_else(|| "fact retraction prior missing".to_string())?;
                    let prior: Fact = serde_json::from_value(prior)
                        .map_err(|error| format!("invalid fact retraction prior: {error}"))?;
                    if !prior.is_auto_summary() || prior.fact_id() != fact_id {
                        return Err(
                            "fact retraction requires an automatic summary Fact".to_string()
                        );
                    }
                    let expected_revision = envelope
                        .expected_revision()
                        .ok_or_else(|| "fact retraction expected_revision missing".to_string())?;
                    // Re-derivation must never remove a row that a user has
                    // confirmed/edited (or a newer automatic repair has
                    // replaced). The predicate is retained during replay,
                    // where an already-retracted row is a harmless no-op.
                    table
                        .delete(&format!(
                            "fact_id = '{}' AND revision = {} AND status = 'auto'",
                            escape_sql(fact_id),
                            expected_revision
                        ))
                        .await
                        .map_err(|error| error.to_string())?;
                } else {
                    let fact_value = envelope
                        .payload()
                        .get("fact")
                        .cloned()
                        .unwrap_or_else(|| envelope.payload().clone());
                    let fact: Fact = serde_json::from_value(fact_value)
                        .map_err(|error| format!("invalid fact payload: {error}"))?;
                    let existing = existing_ids(&table, &[fact.fact_id().to_string()]).await?;
                    if existing.is_empty() {
                        if action == Some("upsert")
                            && envelope.payload().get("repair").and_then(Value::as_bool)
                                == Some(true)
                        {
                            // A replayed repair must not resurrect a Fact
                            // that another attempt retracted in the
                            // meantime.
                            return Ok(());
                        }
                        let schema = facts_schema();
                        let batch = fact_batch(&fact, schema.clone())?;
                        add_batch(&table, batch, schema).await?;
                    } else if action == Some("upsert") {
                        let repair =
                            envelope.payload().get("repair").and_then(Value::as_bool) == Some(true);
                        let only_if = if repair {
                            let expected_revision =
                                envelope.expected_revision().ok_or_else(|| {
                                    "summary repair expected_revision missing".to_string()
                                })?;
                            format!(
                                "fact_id = '{}' AND revision = {} AND status = 'auto'",
                                escape_sql(fact.fact_id()),
                                expected_revision
                            )
                        } else if enforce_fact_cas {
                            let expected_revision =
                                envelope.expected_revision().ok_or_else(|| {
                                    "fact upsert expected_revision missing".to_string()
                                })?;
                            format!(
                                "fact_id = '{}' AND revision = {}",
                                escape_sql(fact.fact_id()),
                                expected_revision
                            )
                        } else {
                            format!("fact_id = '{}'", escape_sql(fact.fact_id()))
                        };
                        let result = table
                            .update()
                            .only_if(only_if)
                            .column(
                                "subject",
                                format!("'{}'", escape_sql(&fact.subject().to_string())),
                            )
                            .column(
                                "predicate",
                                format!("'{}'", escape_sql(fact.predicate().as_str())),
                            )
                            .column("key", format!("'{}'", escape_sql(fact.key())))
                            .column("value", format!("'{}'", escape_sql(fact.value())))
                            .column(
                                "status",
                                format!("'{}'", format!("{:?}", fact.status()).to_lowercase()),
                            )
                            .column("source_event_id", format!("'{}'", fact.source_event_id()))
                            .column("revision", fact.revision().to_string())
                            .column(
                                "operation_id",
                                format!("'{}'", escape_sql(fact.operation_id())),
                            )
                            .execute()
                            .await
                            .map_err(|e| e.to_string())?;
                        if enforce_fact_cas && result.rows_updated == 0 {
                            return Err("fact revision conflict".to_string());
                        }
                    }
                }
            }
            OperationKind::Redaction => {
                let entity_id = envelope
                    .payload()
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "redaction entity_id missing".to_string())?;
                let raw = open_table(&self.paths.raw_events().to_path_buf(), RAW_TABLE).await?;
                raw.delete(&format!("event_id = '{}'", escape_sql(entity_id)))
                    .await
                    .map_err(|error| error.to_string())?;
                let facts = open_table(&self.paths.facts().to_path_buf(), FACTS_TABLE).await?;
                facts
                    .delete(&format!("source_event_id = '{}'", escape_sql(entity_id)))
                    .await
                    .map_err(|error| error.to_string())?;
                let embeddings =
                    open_table(&self.paths.embeddings().to_path_buf(), EMBEDDINGS_TABLE).await?;
                embeddings
                    .delete(&format!("entity_id = '{}'", escape_sql(entity_id)))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            OperationKind::SummaryStatus => {
                // Status/model columns remain in the compatibility projection
                // for source compatibility.  The journal operation provides a
                // durable intent and manifest sequence; projection repair is
                // performed by the caller after this commit.
            }
        }
        Ok(())
    }

    fn publish_manifest(&self, journal: &Journal) -> StoreResult<()> {
        let report = journal.recover().map_err(|error| error.to_string())?;
        self.publish_manifest_for_report(&report)
    }

    fn publish_manifest_for_report(
        &self,
        report: &super::journal::RecoveryReport,
    ) -> StoreResult<()> {
        let sequence = report
            .records()
            .iter()
            .map(|record| record.sequence())
            .max()
            .unwrap_or(0);
        self.publish_manifest_for_sequence(sequence)
    }

    fn publish_manifest_for_sequence(&self, sequence: u64) -> StoreResult<()> {
        let selector =
            ManifestSelector::from_paths(&self.paths).map_err(|error| error.to_string())?;
        let generation = selector
            .read()
            .map_err(|error| error.to_string())?
            .map(|manifest| manifest.generation().saturating_add(1))
            .unwrap_or(1);
        selector
            .select(generation, sequence)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn memory_error(error: MemoryError) -> String {
    error.to_string()
}

/// Materialization only needs the validated operation envelope.  Backfill
/// batches already have that value in memory, so reconstruct a lightweight
/// record facade instead of reparsing the entire journal after every commit.
fn synthetic_operation_record(envelope: OperationEnvelope) -> StoreResult<JournalRecord> {
    let operation_kind = envelope.kind().as_str().to_string();
    let operation_id = envelope.operation_id().to_string();
    let payload = serde_json::to_value(&envelope).map_err(|error| error.to_string())?;
    Ok(JournalRecord {
        schema: super::journal::JOURNAL_SCHEMA.to_string(),
        version: super::journal::JOURNAL_VERSION,
        sequence: 0,
        batch_id: "materialize".to_string(),
        state: JournalState::Operation,
        operation_id,
        operation_kind,
        payload,
        checksum: String::new(),
    })
}

fn summary_status_envelope_with_attempt(
    entity_id: &str,
    status: &str,
    model_id: Option<&str>,
    prompt_version: Option<&str>,
    reason: Option<&str>,
    attempt_id: Option<&str>,
) -> StoreResult<OperationEnvelope> {
    summary_status_envelope_with_lease(
        entity_id,
        status,
        model_id,
        prompt_version,
        reason,
        attempt_id,
        None,
    )
}

fn summary_status_envelope_with_lease(
    entity_id: &str,
    status: &str,
    model_id: Option<&str>,
    prompt_version: Option<&str>,
    reason: Option<&str>,
    attempt_id: Option<&str>,
    lease_expires_at: Option<&str>,
) -> StoreResult<OperationEnvelope> {
    if !SUMMARY_STATUSES.contains(&status) {
        return Err("invalid summary status".to_string());
    }
    if status.trim().is_empty() {
        return Err("summary status must not be empty".to_string());
    }
    let entity_id = stable_uuid(entity_id).to_string();
    let attempt_id = attempt_id
        .map(str::to_string)
        .unwrap_or_else(|| summary_attempt_id(&entity_id, status, model_id, reason));
    if attempt_id.trim().is_empty() {
        return Err("summary attempt_id must not be empty".to_string());
    }
    let model_id = model_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(SUMMARY_MODEL_ID);
    if let Some(reason) = reason {
        if reason.trim().is_empty() {
            return Err("summary reason must not be empty".to_string());
        }
    }
    let mut payload = serde_json::Map::new();
    payload.insert("entity_id".into(), Value::String(entity_id.clone()));
    payload.insert("event_id".into(), Value::String(entity_id.clone()));
    payload.insert("attempt_id".into(), Value::String(attempt_id));
    payload.insert("status".into(), Value::String(status.to_string()));
    payload.insert("model_id".into(), Value::String(model_id.to_string()));
    payload.insert(
        "prompt_version".into(),
        Value::String(SUMMARY_PROMPT_VERSION.to_string()),
    );
    payload.insert(
        "reason".into(),
        reason
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Null),
    );
    if let Some(lease_expires_at) = lease_expires_at {
        if lease_expires_at.trim().is_empty() {
            return Err("summary lease_expires_at must not be empty".to_string());
        }
        CanonicalUtc::parse(lease_expires_at)
            .map_err(|_| "summary lease_expires_at must be canonical UTC".to_string())?;
        payload.insert(
            "lease_expires_at".into(),
            Value::String(lease_expires_at.to_string()),
        );
    }
    // Omit the optional field for ordinary status writes to preserve the
    // operation identity of pre-attempt journal records.  Explicit retries
    // provide a fresh identity even when they return to `pending`.
    let _ = prompt_version;
    OperationEnvelope::new(
        OperationKind::SummaryStatus,
        entity_id.clone(),
        "1970-01-01T00:00:00Z",
        None,
        Value::Object(payload),
    )
    .map_err(memory_error)
}

fn summary_attempt_id(
    entity_id: &str,
    status: &str,
    model_id: Option<&str>,
    reason: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"gameassistant.memory_v2.summary-attempt.v2\0");
    for value in [
        entity_id,
        status,
        model_id.unwrap_or(SUMMARY_MODEL_ID),
        reason.unwrap_or(""),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    format!("attempt-{}", hex_prefix(&digest, 16))
}

fn hex_prefix(bytes: &[u8], length: usize) -> String {
    bytes
        .iter()
        .take(length)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn generated_summary_status_for_input(
    input: &SummaryBatchInput,
    status: &str,
    reason: &str,
) -> SummaryStatusBatchInput {
    SummaryStatusBatchInput {
        entity_id: input.entity_id.clone(),
        status: status.to_string(),
        // A row-local validation failure is still the result of the same
        // attempt that produced the payload.  Carry its metadata into the
        // terminal status instead of replacing it with the repository's
        // compatibility defaults.
        model_id: input
            .model_id
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
            .or_else(|| Some(SUMMARY_MODEL_ID.to_string())),
        prompt_version: input
            .prompt_version
            .as_deref()
            .filter(|version| !version.trim().is_empty())
            .map(str::to_string)
            .or_else(|| Some(SUMMARY_PROMPT_VERSION.to_string())),
        attempt_id: input
            .attempt_id
            .as_deref()
            .filter(|attempt| !attempt.trim().is_empty())
            .map(str::to_string),
        reason: Some(reason.to_string()),
    }
}

/// Normalize a reason at the backfill boundary.  New callers should pass the
/// machine-readable code directly; accepting a legacy diagnostic that contains
/// one of the codes keeps older workers compatible without persisting the
/// diagnostic itself.  Non-failure statuses retain their caller-provided
/// reason because values such as `retrying` are operational state, not error
/// classifications.
fn normalize_backfill_reason(status: &str, reason: Option<String>) -> Option<String> {
    let reason = reason?;
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !matches!(status, "fallback" | "error") {
        return Some(trimmed.to_string());
    }
    trimmed
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|token| SUMMARY_REASON_CODES.contains(token))
        .map(str::to_string)
        .or_else(|| Some("inference_failed".to_string()))
}

/// Keep a stale batch result from overwriting a newer durable attempt.  The
/// queue and explicit retry paths already write a fresh `pending` attempt;
/// terminal results may then advance that attempt, but an old completion or
/// failure must never be allowed to move the latest v2 status backwards.
fn summary_batch_status_transition_allowed(
    current: Option<&SummaryStatusRecord>,
    next: &str,
    has_summary_fact: bool,
    repairable_summary_fact: bool,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    if current.status == "deleted" {
        return false;
    }
    if !current.is_v2() {
        return true;
    }
    match current.status.as_str() {
        "pending" => true,
        "completed" if next == "pending" => !has_summary_fact || repairable_summary_fact,
        _ => false,
    }
}

fn is_should_store_false(status: &SummaryStatusBatchInput) -> bool {
    matches!(
        status.reason.as_deref(),
        Some(
            "model_declined"
                | "should_store_false"
                | "not_storable"
                | "non_persistent"
                | "subject_unresolved"
        )
    )
}

pub(crate) fn summary_fact_is_repairable(
    fact: &Fact,
    status: Option<&SummaryStatusRecord>,
    raw_event: Option<&RawEvent>,
) -> bool {
    if !fact.is_auto_summary() {
        return false;
    }
    if is_metadata_echo_value(fact.value(), raw_event) {
        return true;
    }
    status
        .map(|status| !status.is_v2() || status.status == "pending")
        .unwrap_or(true)
}

fn is_metadata_echo_value(value: &str, raw_event: Option<&RawEvent>) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let fixed_tokens = [
        "user_speech",
        "discord_speech",
        "twitch_chat",
        "human",
        "ai_response",
        "auto_commentary",
        "manual",
        "system",
        "microphone",
        "discord",
        "twitch",
        "event_type",
        "source",
        "timestamp",
        "occurred_at",
        "content",
        "summary",
    ];
    if fixed_tokens.contains(&normalized.as_str()) {
        return true;
    }
    let metadata_labels = [
        "event_type",
        "eventtype",
        "source",
        "timestamp",
        "occurred_at",
        "content",
        "summary",
        "field",
    ];
    if metadata_labels.iter().any(|label| {
        normalized == *label
            || normalized.starts_with(&format!("{}:", label))
            || normalized.starts_with(&format!("{}=", label))
    }) {
        return true;
    }
    let Some(event) = raw_event else {
        return false;
    };
    let metadata_values = [
        event.event_type().as_str().to_ascii_lowercase(),
        event.source().to_ascii_lowercase(),
        event.occurred_at().to_ascii_lowercase(),
    ];
    if metadata_values
        .iter()
        .any(|metadata| normalized == metadata.as_str())
    {
        return true;
    }
    let labels = ["event_type", "source", "timestamp", "occurred_at", "field"];
    labels.iter().any(|label| normalized.contains(label))
        && metadata_values
            .iter()
            .any(|metadata| normalized.contains(metadata))
}

fn summary_fact_retraction_envelope(prior: &Fact, reason: &str) -> StoreResult<OperationEnvelope> {
    if !prior.is_auto_summary() {
        return Err("only automatic summary Facts can be retracted".to_string());
    }
    if reason.trim().is_empty() {
        return Err("summary retraction reason must not be empty".to_string());
    }
    OperationEnvelope::new(
        OperationKind::Fact,
        prior.fact_id(),
        "1970-01-01T00:00:00Z",
        Some(prior.revision()),
        json!({
            "action": "retract",
            "fact_id": prior.fact_id(),
            "prior": prior,
            "reason": reason,
        }),
    )
    .map_err(memory_error)
}

fn summary_embedding_retraction_envelope(
    event_id: &Uuid,
    reason: &str,
) -> StoreResult<OperationEnvelope> {
    if reason.trim().is_empty() {
        return Err("summary embedding retraction reason must not be empty".to_string());
    }
    OperationEnvelope::new(
        OperationKind::Embedding,
        event_id.to_string(),
        "1970-01-01T00:00:00Z",
        None,
        json!({
            "action": "retract_summary",
            "entity_id": event_id.to_string(),
            "reason": reason,
        }),
    )
    .map_err(memory_error)
}

fn operation_payload(payload: &Value) -> StoreResult<&Value> {
    let Some(inner) = payload.get("payload") else {
        if payload.is_object() {
            return Ok(payload);
        }
        return Err("operation payload must be an object".to_string());
    };
    if inner.is_object() {
        return Ok(inner);
    }
    // A direct legacy payload can itself contain a null payload value. It is
    // valid only when its status fields are present; otherwise fail closed.
    if payload.get("entity_id").is_some() || payload.get("event_id").is_some() {
        return Ok(payload);
    }
    Err("operation payload must contain an object payload".to_string())
}

fn required_payload_string<'a>(
    payload: &'a serde_json::Map<String, Value>,
    name: &str,
) -> StoreResult<&'a str> {
    let value = payload
        .get(name)
        .ok_or_else(|| format!("summary status {name} is missing"))?;
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("summary status {name} must be a non-empty string"))
}

fn optional_payload_string(
    payload: &serde_json::Map<String, Value>,
    name: &str,
) -> StoreResult<Option<String>> {
    let Some(value) = payload.get(name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("summary status {name} must be a string or null"))?;
    Ok(Some(value.to_string()))
}

fn decode_summary_status_record(record: &JournalRecord) -> StoreResult<SummaryStatusRecord> {
    let payload = operation_payload(record.payload())?
        .as_object()
        .ok_or_else(|| "summary status payload must be an object".to_string())?;
    let entity_raw = required_payload_string(payload, "entity_id")?;
    let event_raw = optional_payload_string(payload, "event_id")?;
    let entity_id = MemoryRepository::canonical_event_id(entity_raw);
    let event_id = event_raw
        .as_deref()
        .map(MemoryRepository::canonical_event_id)
        .unwrap_or_else(|| entity_id.clone());
    if event_id != entity_id {
        return Err("summary status entity_id/event_id mismatch".to_string());
    }
    let status = required_payload_string(payload, "status")?;
    if !SUMMARY_STATUSES.contains(&status) {
        return Err(format!("invalid summary status: {status}"));
    }
    let lease_expires_at = optional_payload_string(payload, "lease_expires_at")?;
    if let Some(lease) = lease_expires_at.as_deref() {
        CanonicalUtc::parse(lease)
            .map_err(|_| "summary status lease_expires_at is not canonical UTC".to_string())?;
    }
    let reason =
        optional_payload_string(payload, "reason")?.or(optional_payload_string(payload, "error")?);
    Ok(SummaryStatusRecord {
        entity_id: entity_id.clone(),
        event_id,
        status: status.to_string(),
        reason,
        model_id: optional_payload_string(payload, "model_id")?,
        prompt_version: optional_payload_string(payload, "prompt_version")?,
        attempt_id: optional_payload_string(payload, "attempt_id")?,
        lease_expires_at,
        sequence: record.sequence(),
    })
}

fn escape_sql(value: &str) -> String {
    value.replace('\'', "''")
}

fn required_strings<'a>(batch: &'a RecordBatch, name: &str) -> StoreResult<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| format!("missing or invalid {name} column"))
}

fn map_event_type(memory_type: &str) -> EventType {
    match memory_type {
        "user_speech" | "discord_speech" | "twitch_chat" => EventType::Human,
        "ai_response" => EventType::AiResponse,
        "auto_commentary" => EventType::AutoCommentary,
        "manual" => EventType::Manual,
        _ => EventType::System,
    }
}

fn map_source(memory_type: &str) -> &'static str {
    match memory_type {
        "user_speech" => "microphone",
        "discord_speech" => "discord",
        "twitch_chat" => "twitch",
        "manual" => "manual",
        _ => "system",
    }
}

fn canonical_timestamp(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| {
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        // Invalid legacy timestamps must not use the wall clock: retries of
        // the same compatibility event would otherwise produce a new
        // operation ID and make migration non-idempotent.  Epoch is an
        // explicit deterministic sentinel for unknown historical time.
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn same_raw_event_except_timestamp(left: &Value, right: &Value) -> bool {
    let Ok(left) = serde_json::from_value::<RawEvent>(left.clone()) else {
        return false;
    };
    let Ok(right) = serde_json::from_value::<RawEvent>(right.clone()) else {
        return false;
    };
    left.event_id() == right.event_id()
        && left.subject() == right.subject()
        && left.event_type() == right.event_type()
        && left.source_kind() == right.source_kind()
        && left.content().as_str() == right.content().as_str()
}

fn stable_uuid(value: &str) -> Uuid {
    if let Ok(uuid) = Uuid::parse_str(value) {
        if !uuid.is_nil() {
            return uuid;
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(b"gameassistant.memory_v2.compat-id\0");
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn validate_embedding(values: &[f32]) -> StoreResult<()> {
    if values.len() != EMBEDDING_DIMENSIONS {
        return Err(format!(
            "embedding length must be {EMBEDDING_DIMENSIONS}, got {}",
            values.len()
        ));
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err("embedding contains a non-finite value".to_string());
    }
    Ok(())
}

fn embedding_operation_payload(
    entity_id: &str,
    values: &[f32],
    source: &str,
) -> StoreResult<Value> {
    let embedding = values
        .iter()
        // Store the source f32 bit pattern as a string. JSON number parsing
        // may round a handful of subnormal values differently on a retry;
        // bit-pattern strings keep operation IDs stable across journal
        // serialization while the materialized table remains Float32.
        .map(|value| Ok(Value::String(format!("{:08x}", value.to_bits()))))
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(json!({
        "entity_id": entity_id,
        "embedding": embedding,
        "source": source,
    }))
}

fn decode_embedding_value(value: &Value) -> StoreResult<f32> {
    if let Some(bits) = value.as_str() {
        if bits.len() == 8 {
            if let Ok(bits) = u32::from_str_radix(bits, 16) {
                return Ok(f32::from_bits(bits));
            }
        }
        return bits
            .parse::<f32>()
            .map_err(|_| "embedding value is not numeric".to_string());
    }
    value
        .as_f64()
        .map(|number| number as f32)
        .ok_or_else(|| "embedding value is not numeric".to_string())
}

fn raw_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("event_id", DataType::Utf8, false),
        Field::new("subject", DataType::Utf8, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("occurred_at", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
    ]))
}

fn facts_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("fact_id", DataType::Utf8, false),
        Field::new("subject", DataType::Utf8, false),
        Field::new("predicate", DataType::Utf8, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("source_event_id", DataType::Utf8, false),
        Field::new("revision", DataType::UInt64, false),
        Field::new("operation_id", DataType::Utf8, false),
    ]))
}

fn embeddings_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("entity_id", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, false)),
                VECTOR_DIM,
            ),
            false,
        ),
    ]))
}

fn empty_raw_batch(schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
        ],
    )
    .map_err(|error| error.to_string())
}

fn empty_facts_batch(schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(UInt64Array::from(Vec::<u64>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
        ],
    )
    .map_err(|error| error.to_string())
}

fn empty_embeddings_batch(schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    let child = Float32Builder::with_capacity(0);
    let vector = FixedSizeListBuilder::with_capacity(child, VECTOR_DIM, 0)
        .with_field(Field::new("item", DataType::Float32, false))
        .finish();
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(vector),
        ],
    )
    .map_err(|error| error.to_string())
}

fn raw_batch(events: &[RawEvent], schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.event_id().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.subject().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.event_type().as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.source().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.occurred_at().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                events
                    .iter()
                    .map(|event| event.content().as_str())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|error| error.to_string())
}

fn embedding_batch(
    entity_id: &str,
    values: &[f32],
    schema: Arc<Schema>,
) -> StoreResult<RecordBatch> {
    validate_embedding(values)?;
    let child = Float32Builder::with_capacity(VECTOR_DIM as usize);
    let mut builder = FixedSizeListBuilder::with_capacity(child, VECTOR_DIM, 1)
        .with_field(Field::new("item", DataType::Float32, false));
    for value in values {
        builder.values().append_value(*value);
    }
    builder.append(true);
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![entity_id.to_string()])),
            Arc::new(builder.finish()),
        ],
    )
    .map_err(|error| error.to_string())
}

fn embeddings_batch(
    embeddings: &[(String, Vec<f32>)],
    schema: Arc<Schema>,
) -> StoreResult<RecordBatch> {
    let child = Float32Builder::with_capacity(VECTOR_DIM as usize * embeddings.len());
    let mut builder = FixedSizeListBuilder::with_capacity(child, VECTOR_DIM, embeddings.len())
        .with_field(Field::new("item", DataType::Float32, false));
    for (_, values) in embeddings {
        validate_embedding(values)?;
        for value in values {
            builder.values().append_value(*value);
        }
        builder.append(true);
    }
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                embeddings
                    .iter()
                    .map(|(entity_id, _)| entity_id.clone())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(builder.finish()),
        ],
    )
    .map_err(|error| error.to_string())
}

fn embedding_literal(values: &[f32]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn fact_batch(fact: &Fact, schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![fact.fact_id().to_string()])),
            Arc::new(StringArray::from(vec![fact.subject().to_string()])),
            Arc::new(StringArray::from(vec![fact.predicate().as_str()])),
            Arc::new(StringArray::from(vec![fact.key().to_string()])),
            Arc::new(StringArray::from(vec![fact.value().to_string()])),
            Arc::new(StringArray::from(vec![
                format!("{:?}", fact.status()).to_lowercase()
            ])),
            Arc::new(StringArray::from(vec![fact.source_event_id().to_string()])),
            Arc::new(UInt64Array::from(vec![fact.revision()])),
            Arc::new(StringArray::from(vec![fact.operation_id().to_string()])),
        ],
    )
    .map_err(|error| error.to_string())
}

fn fact_batch_slice(facts: &[Fact], schema: Arc<Schema>) -> StoreResult<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.fact_id().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.subject().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.predicate().as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.key().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.value().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| format!("{:?}", fact.status()).to_lowercase())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.source_event_id().to_string())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                facts.iter().map(Fact::revision).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                facts
                    .iter()
                    .map(|fact| fact.operation_id().to_string())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|error| error.to_string())
}

async fn ensure_table(
    path: &Path,
    table_name: &str,
    schema: Arc<Schema>,
    empty_batch: RecordBatch,
) -> StoreResult<Table> {
    let connection = open_connection(path).await?;
    let names = connection
        .table_names()
        .execute()
        .await
        .map_err(|error| error.to_string())?;
    if names.iter().any(|name| name == table_name) {
        let table = connection
            .open_table(table_name)
            .execute()
            .await
            .map_err(|error| error.to_string())?;
        let actual_schema = table.schema().await.map_err(|error| error.to_string())?;
        if !compatible_schema(&actual_schema, &schema) {
            return Err(format!("incompatible schema for {table_name}"));
        }
        return Ok(table);
    }
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(empty_batch)], schema));
    connection
        .create_table(table_name, reader)
        .execute()
        .await
        .map_err(|error| error.to_string())
}

/// LanceDB normalizes FixedSizeList child nullability when reopening a table.
/// The outer list nullability and all logical fields remain strict; only that
/// storage-level child bit is tolerated so restart/recovery can proceed.
fn compatible_schema(actual: &Schema, expected: &Schema) -> bool {
    actual.fields().len() == expected.fields().len()
        && actual
            .fields()
            .iter()
            .zip(expected.fields())
            .all(|(actual, expected)| {
                actual.name() == expected.name()
                    && compatible_data_type(actual.data_type(), expected.data_type())
                    && actual.is_nullable() == expected.is_nullable()
            })
}

fn compatible_data_type(actual: &DataType, expected: &DataType) -> bool {
    match (actual, expected) {
        (
            DataType::FixedSizeList(actual_field, actual_width),
            DataType::FixedSizeList(expected_field, expected_width),
        ) => {
            actual_width == expected_width && actual_field.data_type() == expected_field.data_type()
        }
        _ => actual == expected,
    }
}

async fn open_connection(path: &Path) -> StoreResult<Connection> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("memory store path must be absolute and traversal-free".to_string());
    }
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    let path = path
        .to_str()
        .ok_or_else(|| "memory store path is not UTF-8".to_string())?;
    connect(path)
        .execute()
        .await
        .map_err(|error| error.to_string())
}

async fn open_table(path: &Path, name: &str) -> StoreResult<Table> {
    let connection = open_connection(path).await?;
    connection
        .open_table(name)
        .execute()
        .await
        .map_err(|error| error.to_string())
}

async fn add_batch(table: &Table, batch: RecordBatch, schema: Arc<Schema>) -> StoreResult<()> {
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    table
        .add(reader)
        .execute()
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn existing_ids(table: &Table, ids: &[String]) -> StoreResult<HashSet<String>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let values = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let column_name = id_column(table).await?;
    let mut stream = table
        .query()
        .only_if(format!("{} IN ({})", column_name, values))
        .execute()
        .await
        .map_err(|error| error.to_string())?;
    let mut found = HashSet::new();
    while let Some(batch) = stream.try_next().await.map_err(|error| error.to_string())? {
        let ids = batch
            .column_by_name(&column_name)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| format!("missing {column_name} column"))?;
        for row in 0..batch.num_rows() {
            found.insert(ids.value(row).to_string());
        }
    }
    Ok(found)
}

async fn id_column(table: &Table) -> StoreResult<String> {
    let schema = table.schema().await.map_err(|error| error.to_string())?;
    Ok(schema
        .fields()
        .first()
        .map(|field| field.name().to_string())
        .ok_or_else(|| "memory store schema has no identity column".to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lance_memory::SUMMARY_STATUS_COMPLETED;
    use crate::memory_v2::domain::FactStatus;

    #[tokio::test]
    async fn opens_three_stores_and_publishes_manifest() {
        let root = std::env::temp_dir().join(format!("memory-v2-repository-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        assert!(repository.paths().raw_events().is_dir());
        assert!(repository.paths().facts().is_dir());
        assert!(repository.paths().embeddings().is_dir());
        assert!(repository.paths().manifest().is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn compatibility_ids_are_stable() {
        assert_eq!(stable_uuid("not-a-uuid"), stable_uuid("not-a-uuid"));
        assert_ne!(stable_uuid("a"), stable_uuid("b"));
    }

    #[test]
    fn embedding_operation_payload_survives_json_roundtrip() {
        let mut values = vec![0.0_f32; EMBEDDING_DIMENSIONS];
        values[626] = 6.208817016073453e-9_f32;
        let entity_id = stable_uuid("embedding-roundtrip").to_string();
        let payload = embedding_operation_payload(&entity_id, &values, "document").unwrap();
        let envelope = OperationEnvelope::new(
            OperationKind::Embedding,
            &entity_id,
            "1970-01-01T00:00:00Z",
            None,
            payload,
        )
        .unwrap();
        let wire = serde_json::to_value(&envelope).unwrap();
        let decoded: OperationEnvelope = serde_json::from_value(wire).unwrap();
        assert!(decoded.verify_operation_id().is_ok());
        assert_eq!(decoded.operation_id(), envelope.operation_id());
    }

    #[tokio::test]
    async fn compatibility_event_with_invalid_timestamp_is_idempotent() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-invalid-timestamp-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let input = CompatMemory {
            id: "invalid-timestamp-event".to_string(),
            memory_type: "manual".to_string(),
            source: "manual".to_string(),
            timestamp: "not-a-timestamp".to_string(),
            document: "replayed content".to_string(),
        };

        assert!(repository.append_compat_event(input.clone()).await.unwrap());
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(!repository.append_compat_event(input).await.unwrap());

        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert_eq!(journal.lines().count(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn fact_only_delete_retains_raw_event_and_upsert_restores_fact() {
        let root = std::env::temp_dir().join(format!("memory-v2-fact-delete-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "fact-delete-event";
        assert!(repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "manual".into(),
                source: "manual".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "retain this evidence".into(),
            })
            .await
            .unwrap());
        assert!(repository
            .append_summary_fact(event_id, "derived value")
            .await
            .unwrap());
        let fact = repository.read_facts().await.unwrap().pop().unwrap();
        assert!(
            repository
                .append_fact_delete(&fact, fact.revision())
                .await
                .unwrap()
                .0
        );
        assert!(
            !repository
                .append_fact_delete(&fact, fact.revision())
                .await
                .unwrap()
                .0
        );
        assert!(repository.read_facts().await.unwrap().is_empty());
        assert_eq!(repository.read_raw_events().await.unwrap().len(), 1);
        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert!(journal.contains("\"action\":\"delete\""));
        assert!(journal.contains("retain this evidence"));
        assert!(
            repository
                .append_fact_upsert(&fact, fact.revision())
                .await
                .unwrap()
                .0
        );
        assert!(
            !repository
                .append_fact_upsert(&fact, fact.revision())
                .await
                .unwrap()
                .0
        );
        assert_eq!(repository.read_facts().await.unwrap().len(), 1);
        assert!(repository
            .append_summary_status(event_id, "fallback", None, None)
            .await
            .unwrap());
        assert!(!repository
            .append_summary_status(event_id, "fallback", None, None)
            .await
            .unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summary_backfill_keeps_valid_facts_when_one_summary_is_policy_prohibited() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-policy-isolation-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let result = repository
            .append_summary_backfill_batch(
                vec![
                    SummaryBatchInput {
                        entity_id: "valid-summary".into(),
                        summary: "ユーザーは猫が好きです".into(),
                        embedding: None,
                        attempt_id: None,
                        model_id: None,
                        prompt_version: None,
                    },
                    SummaryBatchInput {
                        entity_id: "prohibited-summary".into(),
                        summary: "ユーザーは糖尿病です".into(),
                        embedding: None,
                        attempt_id: None,
                        model_id: None,
                        prompt_version: None,
                    },
                ],
                Vec::new(),
            )
            .await;

        assert!(
            result.is_ok(),
            "policy rejection must not abort the batch: {result:?}"
        );
        let facts = repository.read_facts().await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value(), "ユーザーは猫が好きです");
        let statuses = repository.read_summary_statuses().unwrap();
        let prohibited_id = MemoryRepository::canonical_event_id("prohibited-summary");
        assert_eq!(
            statuses
                .get(&prohibited_id)
                .and_then(|status| status.reason.as_deref()),
            Some("policy_excluded")
        );
        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert!(journal.contains("policy_excluded"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summary_backfill_rejects_metadata_echo_at_repository_boundary() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-metadata-boundary-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "metadata-boundary-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "ユーザーは猫が好きです".into(),
            })
            .await
            .unwrap();
        let outcome = repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "user_speech".into(),
                    embedding: None,
                    attempt_id: Some("metadata-boundary-attempt".into()),
                    model_id: Some(SUMMARY_MODEL_ID.into()),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(outcome.completed_ids.is_empty());
        assert_eq!(
            outcome
                .terminal_statuses
                .iter()
                .find(|status| status.entity_id == event_id)
                .and_then(|status| status.reason.as_deref()),
            Some("metadata_echo")
        );
        let status = repository
            .read_summary_statuses()
            .unwrap()
            .remove(&MemoryRepository::canonical_event_id(event_id))
            .unwrap();
        assert_eq!(status.reason.as_deref(), Some("metadata_echo"));
        assert_eq!(
            status.attempt_id.as_deref(),
            Some("metadata-boundary-attempt")
        );
        assert_eq!(status.model_id.as_deref(), Some(SUMMARY_MODEL_ID));
        assert_eq!(
            status.prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        assert!(repository.read_facts().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn external_chat_without_actor_identity_never_becomes_self_fact() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-subject-unresolved-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "external-chat-without-id";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "twitch_chat".into(),
                source: "viewer-name".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "配信者ではなく視聴者の発話".into(),
            })
            .await
            .unwrap();

        let outcome = repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "視聴者の発話の要約".into(),
                    embedding: None,
                    attempt_id: Some("subject-unresolved-attempt".into()),
                    model_id: Some(SUMMARY_MODEL_ID.into()),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(outcome.completed_ids.is_empty());
        assert_eq!(
            outcome
                .terminal_statuses
                .iter()
                .find(|status| status.entity_id == event_id)
                .and_then(|status| status.reason.as_deref()),
            Some("subject_unresolved")
        );
        assert!(repository.read_facts().await.unwrap().is_empty());
        assert_eq!(
            repository
                .read_summary_statuses()
                .unwrap()
                .get(&MemoryRepository::canonical_event_id(event_id))
                .map(|status| status.status.as_str()),
            Some("skipped")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn retry_pending_status_gets_a_new_operation_identity() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-status-attempt-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "retry-status-event";

        assert!(repository
            .append_summary_status(event_id, "pending", None, None)
            .await
            .unwrap());
        assert!(repository
            .append_summary_status(event_id, "fallback", None, None)
            .await
            .unwrap());
        assert!(repository
            .append_summary_status_with_attempt(event_id, "pending", None, None, Some("retry-2"),)
            .await
            .unwrap());

        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        let operations: Vec<serde_json::Value> = journal
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .filter(|record: &serde_json::Value| record["state"] == "operation")
            .collect();
        let pending_ids: Vec<&str> = operations
            .iter()
            .filter(|record| {
                record["operation_kind"] == "summary_status"
                    && record["payload"]["payload"]["status"] == "pending"
            })
            .filter_map(|record| record["operation_id"].as_str())
            .collect();
        assert_eq!(pending_ids.len(), 2);
        assert_ne!(pending_ids[0], pending_ids[1]);
        assert!(journal.contains("\"attempt_id\":\"retry-2\""));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn deleted_summary_tombstone_is_read_from_fact_delete_journal() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-delete-tombstone-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        repository
            .append_summary_fact("deleted-summary", "要約された内容")
            .await
            .unwrap();
        let fact = repository.read_facts().await.unwrap().pop().unwrap();
        repository
            .append_fact_delete(&fact, fact.revision())
            .await
            .unwrap();

        let deleted = repository.read_deleted_summary_event_ids().await.unwrap();
        assert!(deleted.contains(&MemoryRepository::canonical_event_id("deleted-summary")));
        assert!(repository.read_facts().await.unwrap().is_empty());
        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: "deleted-summary".into(),
                    summary: "must not resurrect".into(),
                    embedding: None,
                    attempt_id: None,
                    model_id: None,
                    prompt_version: None,
                }],
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(repository.read_facts().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn fact_upsert_rejects_stale_revision_after_a_concurrent_change() {
        let root = std::env::temp_dir().join(format!("memory-v2-fact-cas-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        repository
            .append_summary_fact("cas-event", "original")
            .await
            .unwrap();
        let current = repository.read_facts().await.unwrap().pop().unwrap();
        let next = Fact::try_derive_with_metadata(
            current.subject().clone(),
            current.predicate(),
            current.key(),
            "updated",
            FactStatus::Edited,
            current.source_event_id(),
            current.revision() + 1,
            current.operation_id(),
            &Policy::default(),
        )
        .unwrap();
        assert!(
            repository
                .append_fact_upsert(&next, current.revision())
                .await
                .unwrap()
                .0
        );

        let stale = Fact::try_derive_with_metadata(
            current.subject().clone(),
            current.predicate(),
            current.key(),
            "stale",
            FactStatus::Edited,
            current.source_event_id(),
            current.revision() + 1,
            current.operation_id(),
            &Policy::default(),
        )
        .unwrap();
        let error = repository
            .append_fact_upsert(&stale, current.revision())
            .await
            .unwrap_err();
        assert!(error.contains("fact revision conflict"));
        assert_eq!(repository.read_facts().await.unwrap()[0].value(), "updated");
        drop(repository);
        let reopened = MemoryRepository::open(&root).await.unwrap();
        assert_eq!(reopened.read_facts().await.unwrap()[0].value(), "updated");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn compatibility_replay_accepts_legacy_timestamp_for_same_raw_event() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-legacy-timestamp-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let original = CompatMemory {
            id: "legacy-timestamp-event".to_string(),
            memory_type: "manual".to_string(),
            source: "manual".to_string(),
            timestamp: "2025-01-02T03:04:05Z".to_string(),
            document: "replayed content".to_string(),
        };
        assert!(repository.append_compat_event(original).await.unwrap());

        let replay = CompatMemory {
            id: "legacy-timestamp-event".to_string(),
            memory_type: "manual".to_string(),
            source: "manual".to_string(),
            timestamp: "not-a-timestamp".to_string(),
            document: "replayed content".to_string(),
        };
        assert!(!repository.append_compat_event(replay).await.unwrap());

        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert_eq!(journal.lines().count(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summaries_from_distinct_events_get_distinct_fact_rows() {
        let root = std::env::temp_dir().join(format!("memory-v2-summary-facts-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();

        assert!(repository
            .append_summary_fact("event-a", "first durable preference")
            .await
            .unwrap());
        assert!(repository
            .append_summary_fact("event-b", "second durable preference")
            .await
            .unwrap());

        let table = open_table(repository.paths().facts(), FACTS_TABLE)
            .await
            .unwrap();
        let mut rows = table.query().execute().await.unwrap();
        let mut fact_ids = HashSet::new();
        while let Some(batch) = rows.try_next().await.unwrap() {
            let ids = batch
                .column_by_name("fact_id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for row in 0..batch.num_rows() {
                fact_ids.insert(ids.value(row).to_string());
            }
        }
        assert_eq!(fact_ids.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn raw_event_identity_conflicts_are_rejected_before_journaling() {
        let root = std::env::temp_dir().join(format!("memory-v2-raw-conflict-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let input = CompatMemory {
            id: "stable-event".to_string(),
            memory_type: "manual".to_string(),
            source: "manual".to_string(),
            timestamp: "2025-01-02T03:04:05Z".to_string(),
            document: "original content".to_string(),
        };
        assert!(repository.append_compat_event(input.clone()).await.unwrap());

        let mut changed = input;
        changed.document = "changed content".to_string();
        let error = repository.append_compat_event(changed).await.unwrap_err();
        assert!(error.contains("conflicting raw event operation"));

        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert_eq!(journal.lines().count(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summary_embedding_promotes_document_embedding_idempotently() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-summary-embedding-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let document = vec![1.0; EMBEDDING_DIMENSIONS];
        let summary = vec![2.0; EMBEDDING_DIMENSIONS];

        assert!(repository
            .put_embedding("event", document, "document")
            .await
            .unwrap());
        assert!(repository
            .put_embedding("event", summary.clone(), "summary")
            .await
            .unwrap());
        assert!(!repository
            .put_embedding("event", summary, "summary")
            .await
            .unwrap());

        let table = open_table(repository.paths().embeddings(), EMBEDDINGS_TABLE)
            .await
            .unwrap();
        let mut rows = table.query().execute().await.unwrap();
        let batch = rows.try_next().await.unwrap().unwrap();
        let vectors = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<arrow_array::FixedSizeListArray>()
            .unwrap();
        let values = vectors
            .values()
            .as_any()
            .downcast_ref::<arrow_array::Float32Array>()
            .unwrap();
        assert_eq!(values.value(0), 2.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn concurrent_same_summary_ids_materialize_once() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-summary-concurrent-{}", Uuid::new_v4()));
        let repository = Arc::new(MemoryRepository::open(&root).await.unwrap());
        let mut workers = Vec::new();
        for _ in 0..8 {
            let repository = Arc::clone(&repository);
            workers.push(tokio::spawn(async move {
                repository
                    .append_summary_backfill_batch(
                        vec![SummaryBatchInput {
                            entity_id: "concurrent-summary".into(),
                            summary: "同じ要約".into(),
                            embedding: Some(vec![3.0; EMBEDDING_DIMENSIONS]),
                            attempt_id: None,
                            model_id: None,
                            prompt_version: None,
                        }],
                        Vec::new(),
                    )
                    .await
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.await.unwrap();
        }
        assert_eq!(repository.read_facts().await.unwrap().len(), 1);
        assert_eq!(repository.read_summary_embedding_ids().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn new_summary_attempts_have_explicit_v3_metadata() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-attempt-metadata-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        assert!(repository
            .append_summary_status_with_attempt(
                "attempt-metadata-event",
                "pending",
                Some("gemma-test"),
                Some("v1"),
                Some("attempt-1"),
            )
            .await
            .unwrap());

        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        let operation = journal
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|record| {
                record["state"] == "operation" && record["operation_kind"] == "summary_status"
            })
            .unwrap();
        let payload = &operation["payload"]["payload"];
        assert_eq!(payload["attempt_id"], "attempt-1");
        assert_eq!(payload["event_id"], payload["entity_id"]);
        assert_eq!(payload["model_id"], "gemma-test");
        assert_eq!(payload["prompt_version"], "v3");
        assert_eq!(payload["status"], "pending");
        assert!(payload.get("reason").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn latest_summary_status_wins_over_existing_fact() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-status-precedence-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        repository
            .append_summary_fact("status-precedence-event", "old summary")
            .await
            .unwrap();
        repository
            .append_summary_status_with_attempt(
                "status-precedence-event",
                "fallback",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-fallback"),
            )
            .await
            .unwrap();

        let statuses = repository.read_summary_statuses().unwrap();
        let status = statuses
            .get(&MemoryRepository::canonical_event_id(
                "status-precedence-event",
            ))
            .unwrap();
        assert_eq!(status.status, "fallback");
        assert_eq!(
            status.prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        assert_eq!(status.attempt_id.as_deref(), Some("attempt-fallback"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn backfill_repairs_only_legacy_auto_fact_with_same_identity_and_revision_cas() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-summary-repair-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "legacy-repair-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "私は猫を二匹飼っています".into(),
            })
            .await
            .unwrap();
        repository
            .append_summary_fact(event_id, "user_speech")
            .await
            .unwrap();
        let old = repository.read_facts().await.unwrap().pop().unwrap();

        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "ユーザーは猫を二匹飼っている".into(),
                    embedding: None,
                    attempt_id: Some("repair-attempt".into()),
                    model_id: Some("gemma-repair".into()),
                    prompt_version: Some("v1".into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();

        let repaired = repository.read_facts().await.unwrap().pop().unwrap();
        assert_eq!(repaired.fact_id(), old.fact_id());
        assert_eq!(repaired.source_event_id(), old.source_event_id());
        assert_eq!(repaired.revision(), old.revision() + 1);
        assert_eq!(repaired.value(), "ユーザーは猫を二匹飼っている");
        assert_eq!(repository.read_raw_events().await.unwrap().len(), 1);
        let status = repository
            .read_summary_statuses()
            .unwrap()
            .remove(&MemoryRepository::canonical_event_id(event_id))
            .unwrap();
        assert_eq!(status.attempt_id.as_deref(), Some("repair-attempt"));
        assert_eq!(status.model_id.as_deref(), Some("gemma-repair"));
        assert_eq!(
            status.prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn replaying_the_same_repair_does_not_increment_revision_twice() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-repair-replay-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "legacy-repair-replay-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "私は猫が好きです".into(),
            })
            .await
            .unwrap();
        repository
            .append_summary_fact(event_id, "user_speech")
            .await
            .unwrap();
        let input = SummaryBatchInput {
            entity_id: event_id.into(),
            summary: "ユーザーは猫が好き".into(),
            embedding: None,
            attempt_id: None,
            model_id: None,
            prompt_version: None,
        };
        repository
            .append_summary_backfill_batch(vec![input.clone()], Vec::new())
            .await
            .unwrap();
        repository
            .append_summary_backfill_batch(vec![input], Vec::new())
            .await
            .unwrap();

        let facts = repository.read_facts().await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].revision(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn v2_terminal_status_ignores_malformed_replay_payloads() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-terminal-replay-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "terminal-replay-event";
        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "保持する要約".into(),
                    embedding: None,
                    attempt_id: Some("attempt-good".into()),
                    model_id: Some(SUMMARY_MODEL_ID.into()),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();

        // A duplicate replay may be malformed, but it must not downgrade the
        // durable v2 completed attempt before payload validation runs.
        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "".into(),
                    embedding: Some(vec![f32::NAN; EMBEDDING_DIMENSIONS]),
                    attempt_id: Some("attempt-malformed-replay".into()),
                    model_id: Some("other-model".into()),
                    prompt_version: Some("v1".into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();

        let facts = repository.read_facts().await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value(), "保持する要約");
        let status = repository
            .read_summary_statuses()
            .unwrap()
            .remove(&MemoryRepository::canonical_event_id(event_id))
            .unwrap();
        assert_eq!(status.status, SUMMARY_STATUS_COMPLETED);
        assert_eq!(status.attempt_id.as_deref(), Some("attempt-good"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stale_terminal_batch_cannot_retract_newer_completed_summary() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-stale-terminal-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "stale-terminal-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "私は猫が好きです".into(),
            })
            .await
            .unwrap();
        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "ユーザーは猫が好き".into(),
                    embedding: None,
                    attempt_id: Some("attempt-completed".into()),
                    model_id: Some(SUMMARY_MODEL_ID.into()),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                }],
                Vec::new(),
            )
            .await
            .unwrap();

        repository
            .append_summary_backfill_batch(
                Vec::new(),
                vec![SummaryStatusBatchInput {
                    entity_id: event_id.into(),
                    status: "skipped".into(),
                    model_id: Some(SUMMARY_MODEL_ID.into()),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                    attempt_id: Some("stale-skipped".into()),
                    reason: Some("model_declined".into()),
                }],
            )
            .await
            .unwrap();

        assert_eq!(repository.read_facts().await.unwrap().len(), 1);
        assert_eq!(
            repository
                .read_summary_statuses()
                .unwrap()
                .get(&MemoryRepository::canonical_event_id(event_id))
                .map(|status| status.status.as_str()),
            Some("completed")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn confirmed_and_edited_facts_are_not_repaired() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-repair-protection-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "protected-repair-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "私は猫が好きです".into(),
            })
            .await
            .unwrap();
        repository
            .append_summary_fact(event_id, "old summary")
            .await
            .unwrap();
        let old = repository.read_facts().await.unwrap().pop().unwrap();
        let confirmed = Fact::try_derive_with_metadata(
            old.subject().clone(),
            old.predicate(),
            old.key(),
            old.value(),
            FactStatus::Confirmed,
            old.source_event_id(),
            old.revision() + 1,
            old.operation_id(),
            &Policy::default(),
        )
        .unwrap();
        repository
            .append_fact_upsert(&confirmed, old.revision())
            .await
            .unwrap();
        repository
            .append_summary_backfill_batch(
                vec![SummaryBatchInput {
                    entity_id: event_id.into(),
                    summary: "replacement must not overwrite".into(),
                    embedding: None,
                    attempt_id: None,
                    model_id: None,
                    prompt_version: None,
                }],
                Vec::new(),
            )
            .await
            .unwrap();

        let protected = repository.read_facts().await.unwrap().pop().unwrap();
        assert_eq!(protected.status(), FactStatus::Confirmed);
        assert_eq!(protected.value(), old.value());
        assert_eq!(protected.revision(), old.revision() + 1);
        let edited = Fact::try_derive_with_metadata(
            protected.subject().clone(),
            protected.predicate(),
            protected.key(),
            "user edited value",
            FactStatus::Edited,
            protected.source_event_id(),
            protected.revision() + 1,
            protected.operation_id(),
            &Policy::default(),
        )
        .unwrap();
        repository
            .append_fact_upsert(&edited, protected.revision())
            .await
            .unwrap();
        let edited = repository.read_facts().await.unwrap().pop().unwrap();
        assert_eq!(edited.status(), FactStatus::Edited);
        assert_eq!(edited.value(), "user edited value");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn should_store_false_retracts_auto_fact_but_keeps_raw_evidence() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-summary-retract-{}", Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "summary-retract-event";
        repository
            .append_compat_event(CompatMemory {
                id: event_id.into(),
                memory_type: "user_speech".into(),
                source: "microphone".into(),
                timestamp: "2025-01-02T03:04:05Z".into(),
                document: "今日はこんにちはと言いました".into(),
            })
            .await
            .unwrap();
        repository
            .append_summary_fact(event_id, "old auto summary")
            .await
            .unwrap();
        repository
            .put_embedding(event_id, vec![1.0; EMBEDDING_DIMENSIONS], "summary")
            .await
            .unwrap();
        repository
            .append_summary_backfill_batch(
                Vec::new(),
                vec![SummaryStatusBatchInput {
                    entity_id: event_id.into(),
                    status: "skipped".into(),
                    model_id: Some("gemma-test".into()),
                    prompt_version: Some("v2".into()),
                    attempt_id: None,
                    reason: Some("model_declined".into()),
                }],
            )
            .await
            .unwrap();

        assert!(repository.read_facts().await.unwrap().is_empty());
        assert!(repository.read_summary_embedding_ids().unwrap().is_empty());
        assert_eq!(repository.read_raw_events().await.unwrap().len(), 1);
        assert!(!repository
            .read_deleted_summary_event_ids()
            .await
            .unwrap()
            .contains(&MemoryRepository::canonical_event_id(event_id)));
        let statuses = repository.read_summary_statuses().unwrap();
        assert_eq!(
            statuses
                .get(&MemoryRepository::canonical_event_id(event_id))
                .and_then(|status| status.reason.as_deref()),
            Some("model_declined")
        );
        let journal = std::fs::read_to_string(repository.paths().journal()).unwrap();
        assert!(journal.contains("retract"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn malformed_summary_status_payload_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-malformed-status-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let envelope = OperationEnvelope::new(
            OperationKind::SummaryStatus,
            "malformed-summary-status-event",
            "1970-01-01T00:00:00Z",
            None,
            json!({"not_an_attempt": true}),
        )
        .unwrap();
        let journal = Journal::from_paths(repository.paths()).unwrap();
        journal
            .write_batch(
                "malformed-summary-status-batch",
                &[(
                    envelope.operation_id().into(),
                    OperationKind::SummaryStatus,
                    serde_json::to_value(envelope).unwrap(),
                )],
            )
            .unwrap();
        assert!(repository.read_summary_statuses().is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn legacy_nested_summary_status_with_null_metadata_remains_readable() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-legacy-status-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let entity_id = MemoryRepository::canonical_event_id("legacy-status-event");
        let envelope = OperationEnvelope::new(
            OperationKind::SummaryStatus,
            &entity_id,
            "1970-01-01T00:00:00Z",
            None,
            json!({
                "entity_id": entity_id,
                "status": "completed",
                "model_id": null,
                "prompt_version": null,
                "reason": null
            }),
        )
        .unwrap();
        Journal::from_paths(repository.paths())
            .unwrap()
            .write_batch(
                "legacy-summary-status-batch",
                &[(
                    envelope.operation_id().into(),
                    OperationKind::SummaryStatus,
                    serde_json::to_value(envelope).unwrap(),
                )],
            )
            .unwrap();

        let status = repository
            .read_summary_statuses()
            .unwrap()
            .remove(&entity_id)
            .unwrap();
        assert_eq!(status.status, "completed");
        assert_eq!(status.event_id, entity_id);
        assert!(status.model_id.is_none());
        assert!(status.prompt_version.is_none());
        assert!(status.attempt_id.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn summary_processing_decisions_are_status_first_and_lease_aware() {
        let completed = SummaryStatusRecord {
            status: "completed".into(),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
            ..SummaryStatusRecord::default()
        };
        assert_eq!(
            summary_processing_decision(
                Some(&completed),
                true,
                SummaryProcessMode::ProcessAll,
                None,
            ),
            SummaryProcessDecision::SkipCompleted
        );
        assert_eq!(
            summary_processing_decision(
                Some(&completed),
                false,
                SummaryProcessMode::ProcessAll,
                None,
            ),
            SummaryProcessDecision::Process
        );

        let fallback = SummaryStatusRecord {
            status: "fallback".into(),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
            ..SummaryStatusRecord::default()
        };
        assert_eq!(
            summary_processing_decision(
                Some(&fallback),
                true,
                SummaryProcessMode::ProcessAll,
                None,
            ),
            SummaryProcessDecision::RetryRequired
        );
        assert_eq!(
            summary_processing_decision(Some(&fallback), true, SummaryProcessMode::Retry, None,),
            SummaryProcessDecision::Process
        );

        let pending = SummaryStatusRecord {
            status: "pending".into(),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
            lease_expires_at: Some("2030-01-02T00:00:00Z".into()),
            ..SummaryStatusRecord::default()
        };
        assert_eq!(
            summary_processing_decision(
                Some(&pending),
                true,
                SummaryProcessMode::Automatic,
                Some("2030-01-01T00:00:00Z"),
            ),
            SummaryProcessDecision::LeaseActive
        );
        let stale = SummaryStatusRecord {
            lease_expires_at: Some("2030-01-01T00:00:00Z".into()),
            ..pending
        };
        assert_eq!(
            summary_processing_decision(
                Some(&stale),
                true,
                SummaryProcessMode::Automatic,
                Some("2030-01-02T00:00:00Z"),
            ),
            SummaryProcessDecision::ResumeStalePending
        );
        assert_eq!(
            summary_processing_decision(None, true, SummaryProcessMode::Automatic, None,),
            SummaryProcessDecision::SkipLegacy
        );
        assert_eq!(
            summary_processing_decision(None, true, SummaryProcessMode::ProcessAll, None,),
            SummaryProcessDecision::Process
        );
    }

    #[tokio::test]
    async fn unknown_backfill_failure_reasons_use_the_compatibility_fallback() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-summary-unknown-reason-{}",
            Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "unknown-backfill-reason-event";
        let status = SummaryStatusBatchInput {
            entity_id: event_id.into(),
            status: "fallback".into(),
            model_id: Some("gemma-reason-test".into()),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
            attempt_id: Some("attempt-unknown-reason".into()),
            reason: Some("future_unclassified_failure".into()),
        };

        repository
            .append_summary_backfill_batch(Vec::new(), vec![status.clone()])
            .await
            .unwrap();
        let before_replay = std::fs::read_to_string(repository.paths().journal()).unwrap();
        let first = repository
            .read_summary_statuses()
            .unwrap()
            .remove(&MemoryRepository::canonical_event_id(event_id))
            .unwrap();
        assert_eq!(first.reason.as_deref(), Some("inference_failed"));
        assert_eq!(first.attempt_id.as_deref(), Some("attempt-unknown-reason"));
        assert_eq!(first.model_id.as_deref(), Some("gemma-reason-test"));
        assert_eq!(
            first.prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );

        repository
            .append_summary_backfill_batch(Vec::new(), vec![status])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(repository.paths().journal()).unwrap(),
            before_replay,
            "same terminal attempt must be a journal no-op"
        );

        drop(repository);
        let reopened = MemoryRepository::open(&root).await.unwrap();
        let replayed = reopened
            .read_summary_statuses()
            .unwrap()
            .remove(&MemoryRepository::canonical_event_id(event_id))
            .unwrap();
        assert_eq!(replayed.reason.as_deref(), Some("inference_failed"));
        assert_eq!(
            replayed.attempt_id.as_deref(),
            Some("attempt-unknown-reason")
        );
        assert_eq!(replayed.model_id.as_deref(), Some("gemma-reason-test"));
        assert_eq!(
            replayed.prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
