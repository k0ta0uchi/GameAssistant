//! Tauri-facing contract for the memory manager.
//!
//! DTOs live here so the wire format is independent of LanceDB and the domain
//! types remain deliberately strict.  Commands are registered by `lib.rs`.

use super::domain::{Fact, FactPredicate, FactStatus, RawEvent};
use super::journal::Journal;
use super::repository::{MemoryRepository, SummaryStatusRecord};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, State};

const MAX_PAGE_SIZE: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryFilters {
    #[serde(default)]
    pub statuses: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub occurred_from: Option<String>,
    #[serde(default)]
    pub occurred_to: Option<String>,
    #[serde(default)]
    pub has_summary: Option<bool>,
}

impl Default for MemoryFilters {
    fn default() -> Self {
        Self {
            statuses: vec![],
            sources: vec![],
            event_types: vec![],
            subjects: vec![],
            occurred_from: None,
            occurred_to: None,
            has_summary: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default)]
    pub filters: MemoryFilters,
}

fn default_page_size() -> usize {
    50
}
fn default_sort() -> String {
    "newest".to_string()
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            page_size: 50,
            cursor: None,
            search: None,
            sort: default_sort(),
            filters: MemoryFilters::default(),
        }
    }
}

impl PageRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        if !(1..=MAX_PAGE_SIZE).contains(&self.page_size) {
            return Err(ApiError::invalid("page_size must be 1..200"));
        }
        if !matches!(self.sort.as_str(), "newest" | "oldest" | "relevance") {
            return Err(ApiError::invalid("unsupported sort"));
        }
        if let Some(cursor) = &self.cursor {
            let Some(value) = cursor
                .strip_prefix("offset:")
                .and_then(|v| v.parse::<usize>().ok())
            else {
                return Err(ApiError::invalid("invalid cursor"));
            };
            if value > usize::MAX / 2 {
                return Err(ApiError::invalid("invalid cursor"));
            }
        }
        Ok(())
    }
    fn offset(&self) -> Result<usize, ApiError> {
        self.cursor
            .as_deref()
            .unwrap_or("offset:0")
            .strip_prefix("offset:")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| ApiError::invalid("invalid cursor"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub total: Option<usize>,
    pub snapshot_sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RawEventRow {
    pub event_id: String,
    pub legacy_id: Option<String>,
    pub subject: String,
    pub event_type: String,
    pub source: String,
    pub occurred_at: String,
    pub content_preview: String,
    pub content: Option<String>,
    pub summary_status: Option<String>,
    pub derived_fact_ids: Vec<String>,
    pub vector_source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FactRow {
    pub fact_id: String,
    pub subject: String,
    pub predicate: String,
    pub key: String,
    pub value: String,
    pub status: FactStatus,
    pub evidence_count: usize,
    pub latest_evidence_at: Option<String>,
    pub source_event_ids: Vec<String>,
    pub revision: u64,
    pub operation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SummaryRow {
    pub summary_id: String,
    pub event_id: String,
    pub summary: Option<String>,
    pub status: String,
    pub error: Option<String>,
    /// Stable machine-readable reason for a non-completed attempt.  `error`
    /// remains as the compatibility alias consumed by older clients.
    pub reason: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
    #[serde(alias = "attemptId")]
    pub attempt_id: Option<String>,
    /// Number of distinct attempts after the first attempt for this event.
    /// This is derived from durable status records, so corrective retries do
    /// not inflate the row's terminal failure count.
    #[serde(default)]
    #[serde(alias = "retryCount")]
    pub retry_count: usize,
    pub vector_source: Option<String>,
    pub derived_fact_id: Option<String>,
    pub occurred_at: String,
    pub source: String,
    pub event_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FactEvidence {
    pub event_id: String,
    pub subject: String,
    pub event_type: String,
    pub source: String,
    pub occurred_at: String,
    pub content: String,
    pub relation: String,
    pub match_score: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JournalMutationReceipt {
    pub operation_id: String,
    pub journal_sequence: u64,
    pub committed_at: String,
    pub undo_token: Option<String>,
    pub undo_expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MutationResult {
    pub changed: bool,
    pub items: Vec<Value>,
    pub receipt: JournalMutationReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactMutationTarget {
    pub fact_id: String,
    pub expected_revision: u64,
    pub expected_status: FactStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactEdit {
    pub fact_id: String,
    pub expected_revision: u64,
    pub expected_status: FactStatus,
    pub predicate: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FactConflict {
    pub conflict_id: String,
    pub fact_id: String,
    pub current: FactRow,
    pub attempted: Value,
    pub evidence: Vec<FactEvidence>,
    pub resolution: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SummaryRetryRequest {
    pub event_id: String,
    pub expected_status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SummaryRetryResult {
    pub attempt_id: String,
    pub event_id: String,
    pub status: String,
    pub receipt: JournalMutationReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Value,
}

impl ApiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_request".into(),
            message: message.into(),
            retryable: false,
            details: json!({}),
        }
    }
    fn from_store(message: String) -> Self {
        Self {
            code: "storage_unavailable".into(),
            message,
            retryable: true,
            details: json!({}),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: message.into(),
            retryable: false,
            details: json!({}),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Start the explicit all-memory Fact/Summary pass. The operation itself is
/// detached from the command so the Memory Manager remains responsive; live
/// progress is delivered through `memory-manager-backfill-progress` events.
#[derive(Clone, Debug, Serialize)]
pub struct MemoryBackfillFinalCounts {
    pub processed: usize,
    pub persisted: usize,
    pub skipped: usize,
    pub failed: usize,
    pub remaining: usize,
}

/// Additive API projection for backfill progress.  The session worker owns
/// the legacy snake_case payload; this DTO keeps those fields and adds the
/// reason/attempt/retry metadata expected by newer clients.  Both fatal error
/// spellings are emitted so old snake_case and newer camelCase consumers stay
/// compatible during rollout.
#[derive(Clone, Debug, Serialize)]
pub struct MemoryBackfillProgress {
    pub state: String,
    pub processed: usize,
    pub total: usize,
    pub queued: usize,
    pub skipped: usize,
    pub failed: usize,
    pub persisted: usize,
    pub excluded: usize,
    pub attempted: usize,
    pub remaining: usize,
    pub reason_counts: std::collections::BTreeMap<String, usize>,
    #[serde(rename = "reasonCounts")]
    pub reason_counts_camel: std::collections::BTreeMap<String, usize>,
    pub reason: Option<String>,
    pub last_error_reason: Option<String>,
    #[serde(rename = "lastErrorReason")]
    pub last_error_reason_camel: Option<String>,
    pub attempt_id: Option<String>,
    #[serde(rename = "attemptId")]
    pub attempt_id_camel: Option<String>,
    pub retry_count: usize,
    #[serde(rename = "retryCount")]
    pub retry_count_camel: usize,
    pub fatal_error: Option<String>,
    #[serde(rename = "fatalError")]
    pub fatal_error_camel: Option<String>,
    pub final_counts: MemoryBackfillFinalCounts,
    #[serde(rename = "finalCounts")]
    pub final_counts_camel: MemoryBackfillFinalCounts,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryBackfillStart {
    pub accepted: bool,
    pub progress: MemoryBackfillProgress,
}

impl From<crate::session::MemoryBackfillProgress> for MemoryBackfillProgress {
    fn from(progress: crate::session::MemoryBackfillProgress) -> Self {
        let latest_reason = progress.reason_counts.keys().next_back().cloned();
        let final_counts = MemoryBackfillFinalCounts {
            processed: progress.processed,
            persisted: progress.persisted,
            skipped: progress.skipped,
            failed: progress.failed,
            remaining: progress.remaining,
        };
        Self {
            state: progress.state,
            processed: progress.processed,
            total: progress.total,
            queued: progress.queued,
            skipped: progress.skipped,
            failed: progress.failed,
            persisted: progress.persisted,
            excluded: progress.excluded,
            attempted: progress.attempted,
            remaining: progress.remaining,
            reason_counts: progress.reason_counts.clone(),
            reason_counts_camel: progress.reason_counts,
            reason: latest_reason.clone(),
            last_error_reason: latest_reason.clone(),
            last_error_reason_camel: latest_reason,
            attempt_id: None,
            attempt_id_camel: None,
            retry_count: progress.retry_count,
            retry_count_camel: progress.retry_count,
            fatal_error: progress.fatal_error.clone(),
            fatal_error_camel: progress.fatal_error,
            final_counts: final_counts.clone(),
            final_counts_camel: final_counts,
            message: progress.message,
            error: progress.error,
        }
    }
}

#[tauri::command]
pub async fn memory_manager_process_all(
    app: AppHandle,
    state: State<'_, crate::AppState>,
) -> ApiResult<MemoryBackfillStart> {
    let start = state
        .session_mgr
        .start_memory_backfill(app)
        .await
        .map_err(ApiError::from_store)?;
    Ok(MemoryBackfillStart {
        accepted: start.accepted,
        progress: start.progress.into(),
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub page: PageInfo,
}

fn store<T>(result: super::repository::StoreResult<T>) -> ApiResult<T> {
    result.map_err(ApiError::from_store)
}
fn snapshot(repository: &MemoryRepository) -> u64 {
    repository.journal_sequence().unwrap_or(0)
}
fn build_page<T>(
    mut rows: Vec<T>,
    request: &PageRequest,
    snapshot_sequence: u64,
) -> ApiResult<Page<T>> {
    request.validate()?;
    let offset = request.offset()?;
    let total = rows.len();
    if offset > total {
        return Err(ApiError::invalid("cursor is outside result set"));
    }
    let end = (offset + request.page_size).min(total);
    let has_more = end < total;
    rows = rows
        .into_iter()
        .skip(offset)
        .take(request.page_size)
        .collect();
    Ok(Page {
        rows,
        page: PageInfo {
            next_cursor: has_more.then(|| format!("offset:{end}")),
            has_more,
            total: Some(total),
            snapshot_sequence,
        },
    })
}

fn contains_any(values: &[String], candidate: &str) -> bool {
    values.is_empty() || values.iter().any(|v| v == candidate)
}
fn in_range(value: &str, from: &Option<String>, to: &Option<String>) -> bool {
    from.as_deref().map(|v| value >= v).unwrap_or(true)
        && to.as_deref().map(|v| value <= v).unwrap_or(true)
}
fn search_matches(search: &Option<String>, haystacks: &[&str]) -> bool {
    search
        .as_deref()
        .map(|needle| {
            let needle = needle.trim().to_lowercase();
            needle.is_empty() || haystacks.iter().any(|v| v.to_lowercase().contains(&needle))
        })
        .unwrap_or(true)
}
fn raw_row(
    event: &RawEvent,
    facts: &[Fact],
    statuses: &HashMap<String, SummaryStatusInfo>,
    content: bool,
) -> RawEventRow {
    let event_id = event.event_id().to_string();
    let derived: Vec<String> = facts
        .iter()
        .filter(|fact| fact.source_event_id() == event.event_id())
        .map(|fact| fact.fact_id().to_string())
        .collect();
    let summary = facts.iter().find(|fact| {
        fact.source_event_id() == event.event_id() && fact.key().starts_with("summary-")
    });
    let summary_status = effective_summary_status(&event_id, summary.is_some(), statuses);
    RawEventRow {
        event_id,
        legacy_id: None,
        subject: event.subject().to_string(),
        event_type: event.event_type().as_str().into(),
        source: event.source().into(),
        occurred_at: event.occurred_at().into(),
        content_preview: event.content().as_str().chars().take(240).collect(),
        content: content.then(|| event.content().as_str().to_string()),
        summary_status: Some(summary_status.clone()),
        derived_fact_ids: derived,
        vector_source: summary_vector_source(&summary_status, summary.is_some()),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SummaryStatusInfo {
    status: String,
    reason: Option<String>,
    model_id: Option<String>,
    prompt_version: Option<String>,
    attempt_id: Option<String>,
    retry_count: usize,
}

/// Journal records contain a serialized OperationEnvelope.  Keep the unwrap
/// in one place and tolerate pre-v2 status records that stored the payload at
/// the record root.
fn operation_payload(record: &super::journal::JournalRecord) -> &Value {
    record
        .payload()
        .get("payload")
        .filter(|value| value.is_object())
        .unwrap_or_else(|| record.payload())
}

fn summary_statuses(
    repository: &MemoryRepository,
) -> ApiResult<HashMap<String, SummaryStatusInfo>> {
    let retry_counts = summary_retry_counts(repository)?;
    repository
        .read_summary_statuses()
        .map(|statuses| {
            statuses
                .into_iter()
                .map(|(event_id, status)| {
                    let mut info = SummaryStatusInfo::from(status);
                    info.retry_count = retry_counts.get(&event_id).copied().unwrap_or(0);
                    (event_id, info)
                })
                .collect()
        })
        .map_err(ApiError::from_store)
}

/// Count re-attempts from the durable journal rather than from transient
/// progress updates.  A pending and terminal status sharing an attempt ID is
/// one attempt; an explicit Retry receives a fresh ID and increments this
/// count once.  Older records without an attempt ID are counted individually
/// for compatibility.
fn summary_retry_counts(repository: &MemoryRepository) -> ApiResult<HashMap<String, usize>> {
    let journal = Journal::from_paths(repository.paths())
        .map_err(|error| ApiError::from_store(error.to_string()))?;
    let report = journal
        .recover()
        .map_err(|error| ApiError::from_store(error.to_string()))?;
    let mut attempts: HashMap<String, HashSet<String>> = HashMap::new();
    let mut legacy_attempts: HashMap<String, usize> = HashMap::new();
    for record in report.replayable_record_refs() {
        if record.operation_kind() != "summary_status" {
            continue;
        }
        let payload = operation_payload(&record);
        let Some(entity_id) = payload
            .get("entity_id")
            .or_else(|| payload.get("event_id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let event_id = MemoryRepository::canonical_event_id(entity_id);
        if let Some(attempt_id) = payload.get("attempt_id").and_then(Value::as_str) {
            if !attempt_id.trim().is_empty() {
                attempts
                    .entry(event_id)
                    .or_default()
                    .insert(attempt_id.to_string());
                continue;
            }
        }
        *legacy_attempts.entry(event_id).or_default() += 1;
    }
    let mut result = HashMap::new();
    for event_id in attempts
        .keys()
        .chain(legacy_attempts.keys())
        .cloned()
        .collect::<HashSet<_>>()
    {
        let count = attempts.get(&event_id).map_or(0, HashSet::len)
            + legacy_attempts.get(&event_id).copied().unwrap_or(0);
        result.insert(event_id, count.saturating_sub(1));
    }
    Ok(result)
}

impl From<SummaryStatusRecord> for SummaryStatusInfo {
    fn from(status: SummaryStatusRecord) -> Self {
        let status_name = status.status;
        let reason = canonical_summary_reason(status.reason).or_else(|| {
            matches!(status_name.as_str(), "fallback" | "error")
                .then(|| "inference_failed".to_string())
        });
        Self {
            status: status_name,
            reason,
            model_id: status.model_id,
            prompt_version: status.prompt_version,
            attempt_id: status.attempt_id,
            retry_count: 0,
        }
    }
}

/// Keep the API reason field machine-readable even when it is reading an old
/// compatibility row whose `error`/`reason` value contained free-form text.
/// Unknown values intentionally collapse to the final compatibility code so a
/// diagnostic string is never promoted into the UI contract.
fn canonical_summary_reason(reason: Option<String>) -> Option<String> {
    let value = reason?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    const CODES: [&str; 20] = [
        "policy_excluded",
        "model_declined",
        "metadata_echo",
        "ungrounded_summary",
        "invalid_model_output",
        "empty_source",
        "empty_summary",
        "source_too_long",
        "summary_runtime_timeout",
        "summary_runtime_failed",
        "summary_queue_failed",
        "embedding_failed",
        "invalid_embedding",
        "fact_validation_failed",
        "subject_unresolved",
        "journal_commit_failed",
        "not_candidate",
        "candidate_not_eligible",
        "source_not_allowed",
        "inference_failed",
    ];
    CODES
        .iter()
        .find(|code| value == **code || value.contains(**code))
        .map(|code| (*code).to_string())
        .or_else(|| Some("inference_failed".into()))
}

fn summary_retry_allowed(status: &SummaryStatusInfo) -> bool {
    if !matches!(status.status.as_str(), "fallback" | "error") {
        return false;
    }
    matches!(
        status.reason.as_deref(),
        Some(
            "metadata_echo"
                | "ungrounded_summary"
                | "invalid_model_output"
                | "summary_runtime_timeout"
                | "summary_runtime_failed"
                | "summary_queue_failed"
                | "embedding_failed"
                | "invalid_embedding"
                | "fact_validation_failed"
                | "inference_failed"
        )
    )
}

fn effective_summary_status(
    event_id: &str,
    has_summary_fact: bool,
    statuses: &HashMap<String, SummaryStatusInfo>,
) -> String {
    match statuses.get(event_id).map(|status| status.status.as_str()) {
        // `legacy` is the compatibility value for rows without an attempt.
        // Preserve the old completed projection when an old Fact exists.
        Some("legacy") if has_summary_fact => "completed".into(),
        Some(status) => status.to_string(),
        None if has_summary_fact => "completed".into(),
        None => "legacy".into(),
    }
}

fn summary_vector_source(status: &str, has_summary_fact: bool) -> Option<String> {
    if status == "completed" && has_summary_fact {
        Some("summary".into())
    } else if matches!(status, "pending" | "fallback" | "error") {
        Some("document".into())
    } else {
        Some("none".into())
    }
}
fn fact_row(fact: &Fact, events: &[RawEvent]) -> FactRow {
    let latest = events
        .iter()
        .find(|event| event.event_id() == fact.source_event_id())
        .map(|event| event.occurred_at().to_string());
    FactRow {
        fact_id: fact.fact_id().into(),
        subject: fact.subject().to_string(),
        predicate: fact.predicate().as_str().into(),
        key: fact.key().into(),
        value: fact.value().into(),
        status: fact.status(),
        evidence_count: usize::from(latest.is_some()),
        latest_evidence_at: latest,
        source_event_ids: vec![fact.source_event_id().to_string()],
        revision: fact.revision(),
        operation_id: fact.operation_id().into(),
    }
}
fn receipt(
    repository: &MemoryRepository,
    operation_id: String,
    undo_token: Option<String>,
) -> JournalMutationReceipt {
    JournalMutationReceipt {
        operation_id,
        journal_sequence: snapshot(repository),
        committed_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        undo_expires_at: undo_token.as_ref().map(|_| {
            (Utc::now() + chrono::Duration::minutes(10))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        }),
        undo_token,
    }
}

#[tauri::command]
pub async fn memory_manager_list_raw(
    state: State<'_, crate::AppState>,
    request: PageRequest,
) -> ApiResult<Page<RawEventRow>> {
    request.validate()?;
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let facts = store(repository.read_facts().await)?;
    let statuses = summary_statuses(&repository)?;
    let mut events = store(repository.read_raw_events().await)?;
    events.retain(|event| {
        let has_summary_fact = facts
            .iter()
            .any(|f| f.source_event_id() == event.event_id() && f.key().starts_with("summary-"));
        let summary_status =
            effective_summary_status(&event.event_id().to_string(), has_summary_fact, &statuses);
        contains_any(&request.filters.statuses, &summary_status)
            && contains_any(&request.filters.sources, event.source())
            && contains_any(&request.filters.event_types, event.event_type().as_str())
            && contains_any(&request.filters.subjects, &event.subject().to_string())
            && in_range(
                event.occurred_at(),
                &request.filters.occurred_from,
                &request.filters.occurred_to,
            )
            && request
                .filters
                .has_summary
                .map(|wanted| wanted == (summary_status == "completed" && has_summary_fact))
                .unwrap_or(true)
            && search_matches(
                &request.search,
                &[
                    event.content().as_str(),
                    event.source(),
                    event.event_type().as_str(),
                ],
            )
    });
    events.sort_by(|a, b| {
        if request.sort == "oldest" {
            a.occurred_at()
                .cmp(b.occurred_at())
                .then(a.event_id().cmp(&b.event_id()))
        } else {
            b.occurred_at()
                .cmp(a.occurred_at())
                .then(b.event_id().cmp(&a.event_id()))
        }
    });
    build_page(
        events
            .into_iter()
            .map(|event| raw_row(&event, &facts, &statuses, false))
            .collect(),
        &request,
        snapshot(&repository),
    )
}

#[tauri::command]
pub async fn memory_manager_list_facts(
    state: State<'_, crate::AppState>,
    request: PageRequest,
) -> ApiResult<Page<FactRow>> {
    request.validate()?;
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let mut facts = store(repository.read_facts().await)?;
    facts.retain(|fact| {
        let source_event_id = fact.source_event_id().to_string();
        let subject = fact.subject().to_string();
        fact_matches_metadata_filters(fact, &events, &request.filters)
            && contains_any(
                &request.filters.statuses,
                &format!("{:?}", fact.status()).to_lowercase(),
            )
            && contains_any(&request.filters.subjects, &fact.subject().to_string())
            && search_matches(
                &request.search,
                &[
                    fact.fact_id(),
                    subject.as_str(),
                    fact.key(),
                    fact.value(),
                    fact.predicate().as_str(),
                    source_event_id.as_str(),
                ],
            )
            && in_range(
                &fact_row(fact, &events)
                    .latest_evidence_at
                    .clone()
                    .unwrap_or_default(),
                &request.filters.occurred_from,
                &request.filters.occurred_to,
            )
    });
    facts.sort_by(|a, b| {
        let ar = fact_row(a, &events);
        let br = fact_row(b, &events);
        if request.sort == "oldest" {
            ar.latest_evidence_at
                .cmp(&br.latest_evidence_at)
                .then(a.fact_id().cmp(b.fact_id()))
        } else {
            br.latest_evidence_at
                .cmp(&ar.latest_evidence_at)
                .then(b.fact_id().cmp(a.fact_id()))
        }
    });
    build_page(
        facts
            .into_iter()
            .map(|fact| fact_row(&fact, &events))
            .collect(),
        &request,
        snapshot(&repository),
    )
}

#[tauri::command]
pub async fn memory_manager_list_summaries(
    state: State<'_, crate::AppState>,
    request: PageRequest,
) -> ApiResult<Page<SummaryRow>> {
    request.validate()?;
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let facts = store(repository.read_facts().await)?;
    let statuses = summary_statuses(&repository)?;
    let mut rows: Vec<_> = events
        .iter()
        .filter_map(|event| {
            let event_id = event.event_id().to_string();
            let subject = event.subject().to_string();
            let summary = facts.iter().find(|fact| {
                fact.source_event_id() == event.event_id() && fact.key().starts_with("summary-")
            });
            let status = effective_summary_status(&event_id, summary.is_some(), &statuses);
            if !contains_any(&request.filters.statuses, &status)
                || !contains_any(&request.filters.sources, event.source())
                || !contains_any(&request.filters.event_types, event.event_type().as_str())
                || !contains_any(&request.filters.subjects, &event.subject().to_string())
                || !in_range(
                    event.occurred_at(),
                    &request.filters.occurred_from,
                    &request.filters.occurred_to,
                )
                || !search_matches(
                    &request.search,
                    &[
                        event_id.as_str(),
                        subject.as_str(),
                        event.content().as_str(),
                        event.source(),
                        event.event_type().as_str(),
                        status.as_str(),
                    ],
                )
            {
                return None;
            }
            let status_info = statuses.get(&event_id);
            let active_summary = (status == "completed").then_some(summary).flatten();
            let vector_source = summary_vector_source(&status, active_summary.is_some());
            let reason = status_info.and_then(|info| info.reason.clone());
            Some(SummaryRow {
                summary_id: event.event_id().to_string(),
                event_id: event.event_id().to_string(),
                summary: active_summary.map(|f| f.value().to_string()),
                status,
                error: reason.clone(),
                reason,
                model_id: status_info.and_then(|info| info.model_id.clone()),
                prompt_version: status_info.and_then(|info| info.prompt_version.clone()),
                attempt_id: status_info.and_then(|info| info.attempt_id.clone()),
                retry_count: status_info.map(|info| info.retry_count).unwrap_or(0),
                vector_source,
                derived_fact_id: active_summary.map(|f| f.fact_id().into()),
                occurred_at: event.occurred_at().into(),
                source: event.source().into(),
                event_type: event.event_type().as_str().into(),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        if request.sort == "oldest" {
            a.occurred_at.cmp(&b.occurred_at)
        } else {
            b.occurred_at
                .cmp(&a.occurred_at)
                .then(b.summary_id.cmp(&a.summary_id))
        }
    });
    build_page(rows, &request, snapshot(&repository))
}

#[tauri::command]
pub async fn memory_manager_get_raw_event(
    state: State<'_, crate::AppState>,
    event_id: String,
) -> ApiResult<Value> {
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let event = events
        .iter()
        .find(|e| e.event_id().to_string() == event_id)
        .ok_or_else(|| ApiError::not_found("raw event not found"))?;
    let facts = store(repository.read_facts().await)?;
    let statuses = summary_statuses(&repository)?;
    let event_facts: Vec<_> = facts
        .iter()
        .filter(|f| f.source_event_id() == event.event_id())
        .map(|f| fact_row(f, &events))
        .collect();
    let summary_fact = event_facts
        .iter()
        .find(|f| f.key.starts_with("summary-"))
        .cloned();
    let status = effective_summary_status(&event_id, summary_fact.is_some(), &statuses);
    let active_summary = if status == "completed" {
        summary_fact.clone()
    } else {
        None
    };
    let status_info = statuses.get(&event_id);
    let reason = status_info.and_then(|info| info.reason.clone());
    let summary = (status != "legacy" || summary_fact.is_some()).then(|| SummaryRow {
        summary_id: event_id.clone(),
        event_id: event_id.clone(),
        summary: active_summary.as_ref().map(|f| f.value.clone()),
        status: status.clone(),
        error: reason.clone(),
        reason: reason.clone(),
        model_id: status_info.and_then(|info| info.model_id.clone()),
        prompt_version: status_info.and_then(|info| info.prompt_version.clone()),
        attempt_id: status_info.and_then(|info| info.attempt_id.clone()),
        retry_count: status_info.map(|info| info.retry_count).unwrap_or(0),
        vector_source: summary_vector_source(&status, summary_fact.is_some()),
        derived_fact_id: active_summary.as_ref().map(|f| f.fact_id.clone()),
        occurred_at: event.occurred_at().into(),
        source: event.source().into(),
        event_type: event.event_type().as_str().into(),
    });
    Ok(
        json!({ "event": raw_row(event, &facts, &statuses, true), "facts": event_facts, "summary": summary }),
    )
}

#[tauri::command]
pub async fn memory_manager_get_fact_evidence(
    state: State<'_, crate::AppState>,
    fact_id: String,
    page: PageRequest,
) -> ApiResult<Value> {
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let facts = store(repository.read_facts().await)?;
    let fact = facts
        .iter()
        .find(|f| f.fact_id() == fact_id)
        .ok_or_else(|| ApiError::not_found("fact not found"))?;
    let evidence = evidence_for_fact(fact, &events);
    let page = build_page(evidence, &page, snapshot(&repository))?;
    Ok(json!({ "fact": fact_row(fact, &events), "evidence": page.rows, "page": page.page }))
}

#[tauri::command]
pub async fn memory_manager_get_fact_conflict(
    state: State<'_, crate::AppState>,
    conflict_id: String,
) -> ApiResult<FactConflict> {
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let facts = store(repository.read_facts().await)?;
    let fact = current_fact(&facts, &conflict_id)?;
    Ok(FactConflict {
        conflict_id,
        fact_id: fact.fact_id().into(),
        current: fact_row(fact, &events),
        // A standalone read has no failed write request to recover. Return a
        // valid compare target based on the current row so clients can still
        // render the conflict dialog and retry with an explicit CAS payload.
        attempted: json!({
            "fact_id": fact.fact_id(),
            "predicate": fact.predicate().as_str(),
            "value": fact.value(),
            "expected_revision": fact.revision(),
            "expected_status": fact.status(),
        }),
        evidence: evidence_for_fact(fact, &events),
        resolution: "keep_current".into(),
    })
}

fn current_fact<'a>(facts: &'a [Fact], fact_id: &str) -> ApiResult<&'a Fact> {
    facts
        .iter()
        .find(|fact| fact.fact_id() == fact_id)
        .ok_or_else(|| ApiError::not_found("fact not found"))
}

fn evidence_for_fact(fact: &Fact, events: &[RawEvent]) -> Vec<FactEvidence> {
    events
        .iter()
        .filter(|event| event.event_id() == fact.source_event_id())
        .map(|event| FactEvidence {
            event_id: event.event_id().to_string(),
            subject: event.subject().to_string(),
            event_type: event.event_type().as_str().into(),
            source: event.source().into(),
            occurred_at: event.occurred_at().into(),
            content: event.content().as_str().into(),
            relation: "source".into(),
            match_score: None,
        })
        .collect()
}

fn fact_matches_metadata_filters(
    fact: &Fact,
    events: &[RawEvent],
    filters: &MemoryFilters,
) -> bool {
    let Some(event) = events
        .iter()
        .find(|event| event.event_id() == fact.source_event_id())
    else {
        return filters.sources.is_empty() && filters.event_types.is_empty();
    };
    contains_any(&filters.sources, event.source())
        && contains_any(&filters.event_types, event.event_type().as_str())
}

fn conflict(fact: &Fact, attempted: Value, events: &[RawEvent]) -> ApiError {
    ApiError {
        code: "conflict".into(),
        message: "fact revision or status changed".into(),
        retryable: false,
        details: json!({ "conflict_id": fact.fact_id(), "fact_id": fact.fact_id(), "current": fact_row(fact, events), "attempted": attempted, "evidence": evidence_for_fact(fact, events) }),
    }
}
fn mutation_result(
    repository: &MemoryRepository,
    changed: bool,
    operation_id: String,
    items: Vec<Value>,
    token: Option<String>,
) -> MutationResult {
    MutationResult {
        changed,
        items,
        receipt: receipt(repository, operation_id, token),
    }
}

#[tauri::command]
pub async fn memory_manager_confirm_facts(
    state: State<'_, crate::AppState>,
    targets: Vec<FactMutationTarget>,
) -> ApiResult<MutationResult> {
    if targets.is_empty() {
        return Err(ApiError::invalid("targets must not be empty"));
    }
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let facts = store(repository.read_facts().await)?;
    let events = store(repository.read_raw_events().await)?;
    for target in &targets {
        let fact = current_fact(&facts, &target.fact_id)?;
        if fact.revision() != target.expected_revision
            || fact.status() != target.expected_status
            || fact.status() != FactStatus::Auto
        {
            return Err(conflict(fact, json!(target), &events));
        }
    }
    let mut changed = false;
    let mut operation_id = String::new();
    let mut items = Vec::new();
    for target in targets {
        let fact = current_fact(&facts, &target.fact_id)?;
        let proposed = Fact::try_derive_with_metadata(
            fact.subject().clone(),
            fact.predicate(),
            fact.key(),
            fact.value(),
            FactStatus::Confirmed,
            fact.source_event_id(),
            fact.revision() + 1,
            fact.operation_id(),
            &super::policy::Policy::default(),
        )
        .map_err(|e| ApiError::invalid(e.to_string()))?;
        let next = fact
            .apply_transition(
                &proposed,
                Some(target.expected_revision),
                Some(target.expected_status),
            )
            .map_err(|_| conflict(fact, json!(target), &events))?;
        let (did_change, id) = store(
            repository
                .append_fact_upsert(&next, target.expected_revision)
                .await,
        )?;
        changed |= did_change;
        operation_id = id;
        items.push(serde_json::to_value(fact_row(&next, &events)).unwrap_or(json!({})));
    }
    Ok(mutation_result(
        &repository,
        changed,
        operation_id,
        items,
        None,
    ))
}

#[tauri::command]
pub async fn memory_manager_edit_fact(
    state: State<'_, crate::AppState>,
    edit: FactEdit,
) -> ApiResult<MutationResult> {
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let facts = store(repository.read_facts().await)?;
    let events = store(repository.read_raw_events().await)?;
    let fact = current_fact(&facts, &edit.fact_id)?;
    if fact.revision() != edit.expected_revision || fact.status() != edit.expected_status {
        return Err(conflict(fact, json!(edit), &events));
    }
    let predicate: FactPredicate = serde_json::from_value(json!(edit.predicate))
        .map_err(|_| ApiError::invalid("invalid predicate"))?;
    let proposed = Fact::try_derive_with_metadata(
        fact.subject().clone(),
        predicate,
        fact.key(),
        &edit.value,
        FactStatus::Edited,
        fact.source_event_id(),
        fact.revision() + 1,
        fact.operation_id(),
        &super::policy::Policy::default(),
    )
    .map_err(|e| ApiError::invalid(e.to_string()))?;
    let next = fact
        .apply_transition(
            &proposed,
            Some(edit.expected_revision),
            Some(edit.expected_status),
        )
        .map_err(|_| conflict(fact, json!(edit), &events))?;
    let (changed, operation_id) = store(
        repository
            .append_fact_upsert(&next, edit.expected_revision)
            .await,
    )?;
    Ok(mutation_result(
        &repository,
        changed,
        operation_id.clone(),
        vec![serde_json::to_value(fact_row(&next, &events)).unwrap_or(json!({}))],
        Some(operation_id),
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkFactEdit {
    pub targets: Vec<FactMutationTarget>,
    pub predicate: Option<String>,
    pub value: Option<String>,
}

#[tauri::command]
pub async fn memory_manager_edit_facts_bulk(
    state: State<'_, crate::AppState>,
    edit: BulkFactEdit,
) -> ApiResult<MutationResult> {
    if edit.targets.is_empty() || edit.predicate.is_none() && edit.value.is_none() {
        return Err(ApiError::invalid("bulk edit requires targets and a field"));
    }
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let facts = store(repository.read_facts().await)?;
    let events = store(repository.read_raw_events().await)?;
    for target in &edit.targets {
        let fact = current_fact(&facts, &target.fact_id)?;
        if fact.revision() != target.expected_revision || fact.status() != target.expected_status {
            return Err(conflict(fact, json!(edit), &events));
        }
    }
    let mut items = Vec::new();
    let mut operation_id = String::new();
    let mut changed = false;
    for target in &edit.targets {
        let fact = current_fact(&facts, &target.fact_id)?;
        let predicate = match &edit.predicate {
            Some(value) => serde_json::from_value(json!(value))
                .map_err(|_| ApiError::invalid("invalid predicate"))?,
            None => fact.predicate(),
        };
        let value = edit.value.as_deref().unwrap_or(fact.value());
        let proposed = Fact::try_derive_with_metadata(
            fact.subject().clone(),
            predicate,
            fact.key(),
            value,
            FactStatus::Edited,
            fact.source_event_id(),
            fact.revision() + 1,
            fact.operation_id(),
            &super::policy::Policy::default(),
        )
        .map_err(|e| ApiError::invalid(e.to_string()))?;
        let next = fact
            .apply_transition(
                &proposed,
                Some(target.expected_revision),
                Some(target.expected_status),
            )
            .map_err(|_| conflict(fact, json!(&edit), &events))?;
        let (did_change, id) = store(
            repository
                .append_fact_upsert(&next, target.expected_revision)
                .await,
        )?;
        changed |= did_change;
        operation_id = id;
        items.push(serde_json::to_value(fact_row(&next, &events)).unwrap_or(json!({})));
    }
    Ok(mutation_result(
        &repository,
        changed,
        operation_id,
        items,
        None,
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteFactsRequest {
    pub fact_ids: Vec<String>,
    pub expected_revisions: HashMap<String, u64>,
}

#[tauri::command]
pub async fn memory_manager_delete_facts(
    state: State<'_, crate::AppState>,
    request: DeleteFactsRequest,
) -> ApiResult<MutationResult> {
    if request.fact_ids.is_empty() {
        return Err(ApiError::invalid("fact_ids must not be empty"));
    }
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let facts = store(repository.read_facts().await)?;
    let events = store(repository.read_raw_events().await)?;
    for id in &request.fact_ids {
        let fact = current_fact(&facts, id)?;
        if request.expected_revisions.get(id) != Some(&fact.revision()) {
            return Err(conflict(fact, json!(request), &events));
        }
    }
    let mut items = Vec::new();
    let mut operation_id = String::new();
    let mut changed = false;
    for id in request.fact_ids {
        let fact = current_fact(&facts, &id)?;
        let (did_change, op) = store(repository.append_fact_delete(fact, fact.revision()).await)?;
        changed |= did_change;
        operation_id = op.clone();
        items.push(serde_json::to_value(fact_row(fact, &events)).unwrap_or(json!({})));
    }
    Ok(mutation_result(
        &repository,
        changed,
        operation_id.clone(),
        items,
        Some(operation_id),
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndoRequest {
    pub undo_token: String,
}

#[tauri::command]
pub async fn memory_manager_undo(
    state: State<'_, crate::AppState>,
    request: UndoRequest,
) -> ApiResult<MutationResult> {
    if request.undo_token.trim().is_empty() {
        return Err(ApiError::invalid("undo_token must not be empty"));
    }
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let journal =
        Journal::from_paths(repository.paths()).map_err(|e| ApiError::from_store(e.to_string()))?;
    let report = journal
        .recover()
        .map_err(|e| ApiError::from_store(e.to_string()))?;
    let record = report
        .replayable_record_refs()
        .into_iter()
        .find(|record| record.operation_id() == request.undo_token)
        .ok_or_else(|| ApiError {
            code: "undo_expired".into(),
            message: "undo token is expired or invalid".into(),
            retryable: false,
            details: json!({}),
        })?;
    let prior = record
        .payload()
        .get("prior")
        .cloned()
        .ok_or_else(|| ApiError::invalid("undo token has no compensation"))?;
    let prior: Fact =
        serde_json::from_value(prior).map_err(|_| ApiError::invalid("invalid undo payload"))?;
    let facts = store(repository.read_facts().await)?;
    let current = facts
        .iter()
        .find(|fact| fact.fact_id() == prior.fact_id())
        .cloned();
    let (restored, expected) = match current {
        Some(current) => {
            let restored = Fact::try_derive_with_metadata(
                prior.subject().clone(),
                prior.predicate(),
                prior.key(),
                prior.value(),
                prior.status(),
                prior.source_event_id(),
                current.revision() + 1,
                prior.operation_id(),
                &super::policy::Policy::default(),
            )
            .map_err(|error| ApiError::invalid(error.to_string()))?;
            (restored, current.revision())
        }
        None => {
            let revision = prior.revision();
            (prior, revision)
        }
    };
    let (changed, operation_id) = store(repository.append_fact_upsert(&restored, expected).await)?;
    Ok(mutation_result(
        &repository,
        changed,
        operation_id,
        vec![serde_json::to_value(fact_row(&restored, &[])).unwrap_or(json!({}))],
        None,
    ))
}

#[tauri::command]
pub async fn memory_manager_retry_summary(
    state: State<'_, crate::AppState>,
    request: SummaryRetryRequest,
) -> ApiResult<SummaryRetryResult> {
    if !matches!(
        request.expected_status.as_str(),
        "fallback" | "error" | "skipped"
    ) {
        return Err(ApiError::invalid(
            "summary retry requires fallback, error, or skipped status",
        ));
    }
    let repository = store(MemoryRepository::open(&state.root_dir).await)?;
    let events = store(repository.read_raw_events().await)?;
    let event = events
        .iter()
        .find(|event| event.event_id().to_string() == request.event_id)
        .ok_or_else(|| ApiError::not_found("raw event not found"))?;
    if event.event_type().as_str() != "human" {
        return Err(ApiError {
            code: "unsupported".into(),
            message: "summary retry is unsupported for this event type".into(),
            retryable: false,
            details: json!({}),
        });
    }
    let statuses = summary_statuses(&repository)?;
    let current_info = statuses.get(&request.event_id);
    let current_status = current_info
        .map(|status| status.status.clone())
        .unwrap_or_else(|| "legacy".into());
    if current_status != request.expected_status {
        return Err(ApiError {
            code: "conflict".into(),
            message: "summary status changed".into(),
            retryable: false,
            details: json!({
                "event_id": request.event_id,
                "expected_status": request.expected_status,
                "current_status": current_status,
            }),
        });
    }
    if let Some(status) = current_info {
        if !summary_retry_allowed(status) {
            let code = if status.reason.as_deref() == Some("policy_excluded") {
                "policy_excluded"
            } else {
                "not_retryable"
            };
            return Err(ApiError {
                code: code.into(),
                message: "summary row is not eligible for retry".into(),
                retryable: false,
                details: json!({
                    "event_id": request.event_id,
                    "reason": status.reason,
                }),
            });
        }
    }
    let attempt_id = format!("attempt-{}", uuid::Uuid::new_v4().simple());
    let _changed = store(
        repository
            .append_summary_status_with_attempt(
                &request.event_id,
                "pending",
                None,
                None,
                Some(attempt_id.as_str()),
            )
            .await,
    )?;
    // Move the compatibility projection back to pending and enqueue the same
    // curation path used for newly captured candidates.  The journal intent
    // above is written first, so a crash cannot report a successful retry
    // without a durable semantic mutation.
    let queued = state
        .session_mgr
        .retry_summary(&request.event_id)
        .await
        .map_err(ApiError::from_store)?;
    if !queued {
        return Err(ApiError {
            code: "summary_unavailable".into(),
            message: "summary retry could not be queued".into(),
            retryable: true,
            details: json!({}),
        });
    }
    let journal =
        Journal::from_paths(repository.paths()).map_err(|e| ApiError::from_store(e.to_string()))?;
    let report = journal
        .recover()
        .map_err(|e| ApiError::from_store(e.to_string()))?;
    let operation_id = report
        .replayable_record_refs()
        .into_iter()
        .rev()
        .find(|record| {
            record.operation_kind() == "summary_status"
                && operation_payload(record)
                    .get("entity_id")
                    .and_then(Value::as_str)
                    == Some(request.event_id.as_str())
                && operation_payload(record)
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    == Some(attempt_id.as_str())
        })
        .map(|record| record.operation_id().to_string())
        .unwrap_or_else(|| format!("summary-retry-{}", request.event_id));
    Ok(SummaryRetryResult {
        attempt_id,
        event_id: request.event_id,
        status: "pending".into(),
        receipt: receipt(&repository, operation_id, None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_request_rejects_invalid_size_sort_and_cursor() {
        let mut request = PageRequest {
            page_size: 0,
            ..Default::default()
        };
        assert_eq!(request.validate().unwrap_err().code, "invalid_request");
        request.page_size = 1;
        request.sort = "random".into();
        assert!(request.validate().is_err());
        request.sort = "newest".into();
        request.cursor = Some("bad".into());
        assert!(request.validate().is_err());
    }
    #[test]
    fn page_request_accepts_bounded_wire_defaults() {
        let request: PageRequest =
            serde_json::from_value(json!({"page_size": 2, "filters": {}})).unwrap();
        assert_eq!(request.sort, "newest");
        assert!(request.validate().is_ok());
    }

    #[test]
    fn pagination_is_stable_and_reports_total_and_cursor() {
        let request = PageRequest {
            page_size: 2,
            ..Default::default()
        };
        let page = build_page(vec!["a", "b", "c"], &request, 17).unwrap();
        assert_eq!(page.rows, vec!["a", "b"]);
        assert_eq!(page.page.total, Some(3));
        assert_eq!(page.page.next_cursor.as_deref(), Some("offset:2"));
        let next = PageRequest {
            cursor: page.page.next_cursor,
            ..request
        };
        assert_eq!(
            build_page(vec!["a", "b", "c"], &next, 17).unwrap().rows,
            vec!["c"]
        );
    }

    #[test]
    fn filters_match_only_allowlisted_values_and_search_is_case_insensitive() {
        assert!(contains_any(&["manual".into()], "manual"));
        assert!(!contains_any(&["manual".into()], "twitch"));
        assert!(search_matches(
            &Some("Ada".into()),
            &["display name", "ada"]
        ));
        assert!(!search_matches(&Some("Ada".into()), &["Bea"]));
    }

    #[test]
    fn manager_dtos_round_trip_and_reject_malformed_paging() {
        let request = PageRequest {
            page_size: 7,
            cursor: Some("offset:14".into()),
            search: Some("fact:self:name".into()),
            sort: "oldest".into(),
            filters: MemoryFilters {
                statuses: vec!["auto".into()],
                ..Default::default()
            },
        };
        let wire = serde_json::to_value(&request).unwrap();
        let round_trip: PageRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(round_trip, request);
        assert!(serde_json::from_value::<PageRequest>(json!({
            "page_size": 0,
            "sort": "newest",
            "filters": {}
        }))
        .unwrap()
        .validate()
        .is_err());
        assert!(serde_json::from_value::<PageRequest>(json!({
            "page_size": 1,
            "cursor": "offset:not-a-number",
            "filters": {}
        }))
        .unwrap()
        .validate()
        .is_err());
    }

    #[test]
    fn mutation_status_wire_round_trip_is_closed() {
        let target = FactMutationTarget {
            fact_id: "fact:self:name".into(),
            expected_revision: 3,
            expected_status: FactStatus::Confirmed,
        };
        let decoded: FactMutationTarget =
            serde_json::from_value(serde_json::to_value(&target).unwrap()).unwrap();
        assert_eq!(decoded, target);
        assert!(serde_json::from_value::<FactMutationTarget>(json!({
            "fact_id": "fact:self:name",
            "expected_revision": 3,
            "expected_status": "removed"
        }))
        .is_err());
        assert!(serde_json::from_value::<FactMutationTarget>(json!({
            "fact_id": "fact:self:name",
            "expected_revision": 3,
            "expected_status": "auto",
            "provenance": "forged"
        }))
        .is_err());
    }

    #[test]
    fn conflict_payload_contains_attempted_values_and_supporting_evidence() {
        let event_id = uuid::Uuid::new_v4();
        let fact = Fact::try_derive_with_metadata(
            super::super::subject::Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Ada",
            FactStatus::Auto,
            event_id,
            2,
            "operation-2",
            &super::super::policy::Policy::default(),
        )
        .unwrap();
        let event = RawEvent::try_new(
            event_id,
            super::super::subject::Subject::SelfSubject,
            "manual",
            super::super::redaction::RedactedText::from_validated("name is Ada".into()),
        )
        .unwrap();
        let error = conflict(
            &fact,
            json!({
                "fact_id": fact.fact_id(),
                "predicate": "fact",
                "value": "Bea",
                "expected_revision": 1,
                "expected_status": "auto"
            }),
            &[event],
        );
        let details = error.details.as_object().unwrap();
        assert_eq!(details["attempted"]["value"], "Bea");
        assert_eq!(details["attempted"]["expected_revision"], 1);
        assert_eq!(details["evidence"].as_array().unwrap().len(), 1);
        assert_eq!(details["evidence"][0]["event_id"], event_id.to_string());
    }

    #[test]
    fn fact_metadata_filters_match_supporting_raw_event() {
        let event_id = uuid::Uuid::new_v4();
        let fact = Fact::try_derive_with_metadata(
            super::super::subject::Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Ada",
            FactStatus::Auto,
            event_id,
            0,
            "operation-1",
            &super::super::policy::Policy::default(),
        )
        .unwrap();
        let event = RawEvent::try_new(
            event_id,
            super::super::subject::Subject::SelfSubject,
            "manual",
            super::super::redaction::RedactedText::from_validated("name is Ada".into()),
        )
        .unwrap();
        let mut filters = MemoryFilters::default();
        filters.sources = vec!["manual".into()];
        filters.event_types = vec!["manual".into()];
        assert!(fact_matches_metadata_filters(
            &fact,
            &[event.clone()],
            &filters
        ));
        filters.sources = vec!["discord".into()];
        assert!(!fact_matches_metadata_filters(&fact, &[event], &filters));
    }

    #[tokio::test]
    async fn summary_statuses_reads_status_from_nested_operation_payload() {
        let root =
            std::env::temp_dir().join(format!("memory-v2-api-status-{}", uuid::Uuid::new_v4()));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = uuid::Uuid::new_v4().to_string();
        repository
            .append_summary_status_with_attempt(
                &event_id,
                "fallback",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-1"),
            )
            .await
            .unwrap();

        let statuses = summary_statuses(&repository).unwrap();
        assert_eq!(
            statuses.get(&event_id).map(|status| status.status.as_str()),
            Some("fallback")
        );
        assert_eq!(
            statuses
                .get(&event_id)
                .and_then(|status| status.reason.as_deref()),
            Some("inference_failed")
        );
        assert_eq!(
            statuses
                .get(&event_id)
                .and_then(|status| status.attempt_id.as_deref()),
            Some("attempt-1")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summary_retry_count_counts_distinct_attempt_ids_not_status_rows() {
        let root = std::env::temp_dir().join(format!(
            "memory-v2-api-retry-count-{}",
            uuid::Uuid::new_v4()
        ));
        let repository = MemoryRepository::open(&root).await.unwrap();
        let event_id = "retry-count-event";
        repository
            .append_summary_status_with_attempt(
                event_id,
                "pending",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-1"),
            )
            .await
            .unwrap();
        repository
            .append_summary_status_with_attempt(
                event_id,
                "fallback",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-1"),
            )
            .await
            .unwrap();
        repository
            .append_summary_status_with_attempt(
                event_id,
                "pending",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-2"),
            )
            .await
            .unwrap();
        repository
            .append_summary_status_with_attempt(
                event_id,
                "fallback",
                Some("gemma-test"),
                Some("v2"),
                Some("attempt-2"),
            )
            .await
            .unwrap();

        let statuses = summary_statuses(&repository).unwrap();
        let canonical_id = MemoryRepository::canonical_event_id(event_id);
        assert_eq!(
            statuses.get(&canonical_id).map(|status| status.retry_count),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn policy_excluded_summary_is_not_retryable() {
        let info = SummaryStatusInfo {
            status: "skipped".into(),
            reason: Some("policy_excluded".into()),
            ..SummaryStatusInfo::default()
        };
        assert!(!summary_retry_allowed(&info));
        assert!(!summary_retry_allowed(&SummaryStatusInfo {
            status: "skipped".into(),
            reason: Some("model_declined".into()),
            ..SummaryStatusInfo::default()
        }));
        assert!(summary_retry_allowed(&SummaryStatusInfo {
            status: "fallback".into(),
            reason: Some("inference_failed".into()),
            ..SummaryStatusInfo::default()
        }));
    }

    #[test]
    fn retry_matrix_rejects_non_retryable_statuses_and_reasons() {
        for reason in [
            "policy_excluded",
            "model_declined",
            "empty_source",
            "source_too_long",
            "not_candidate",
            "candidate_not_eligible",
            "source_not_allowed",
            "subject_unresolved",
        ] {
            assert!(!summary_retry_allowed(&SummaryStatusInfo {
                status: "fallback".into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
            assert!(!summary_retry_allowed(&SummaryStatusInfo {
                status: "skipped".into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
        }
        assert!(!summary_retry_allowed(&SummaryStatusInfo {
            status: "skipped".into(),
            reason: Some("model_declined".into()),
            ..SummaryStatusInfo::default()
        }));
        assert!(!summary_retry_allowed(&SummaryStatusInfo {
            status: "fallback".into(),
            reason: Some("unclassified_reason".into()),
            ..SummaryStatusInfo::default()
        }));
    }

    #[test]
    fn retry_matrix_allows_known_inference_and_runtime_failures() {
        for reason in [
            "metadata_echo",
            "ungrounded_summary",
            "invalid_model_output",
            "summary_runtime_timeout",
            "summary_runtime_failed",
            "summary_queue_failed",
            "embedding_failed",
            "invalid_embedding",
            "inference_failed",
        ] {
            assert!(summary_retry_allowed(&SummaryStatusInfo {
                status: "fallback".into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
            assert!(summary_retry_allowed(&SummaryStatusInfo {
                status: "error".into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
        }
    }

    #[test]
    fn summary_row_wire_exposes_retry_count_and_keeps_old_fields_optional() {
        let row = SummaryRow {
            summary_id: "summary-1".into(),
            event_id: "event-1".into(),
            summary: None,
            status: "fallback".into(),
            error: Some("inference_failed".into()),
            reason: Some("inference_failed".into()),
            model_id: None,
            prompt_version: None,
            attempt_id: Some("attempt-2".into()),
            retry_count: 1,
            vector_source: Some("document".into()),
            derived_fact_id: None,
            occurred_at: "2026-09-04T00:00:00Z".into(),
            source: "manual".into(),
            event_type: "human".into(),
        };
        let wire = serde_json::to_value(&row).unwrap();
        assert_eq!(wire["reason"], "inference_failed");
        assert_eq!(wire["attempt_id"], "attempt-2");
        assert_eq!(wire["retry_count"], 1);
        let old: SummaryRow = serde_json::from_value(json!({
            "summary_id": "summary-1",
            "event_id": "event-1",
            "summary": null,
            "status": "legacy",
            "error": null,
            "model_id": null,
            "prompt_version": null,
            "vector_source": "none",
            "derived_fact_id": null,
            "occurred_at": "2026-09-04T00:00:00Z",
            "source": "manual",
            "event_type": "human"
        }))
        .unwrap();
        assert_eq!(old.retry_count, 0);
    }

    #[test]
    fn backfill_progress_wire_exposes_additive_retry_and_final_counts() {
        let source = crate::session::MemoryBackfillProgress {
            state: "completed".into(),
            processed: 4,
            total: 4,
            queued: 3,
            skipped: 1,
            failed: 1,
            persisted: 2,
            excluded: 0,
            attempted: 3,
            retry_count: 1,
            remaining: 0,
            reason_counts: [("invalid_model_output".to_string(), 1)]
                .into_iter()
                .collect(),
            fatal_error: None,
            message: "completed with warnings".into(),
            error: None,
        };
        let wire = serde_json::to_value(MemoryBackfillProgress::from(source)).unwrap();
        assert_eq!(wire["reason"], "invalid_model_output");
        assert_eq!(wire["attempt_id"], Value::Null);
        assert_eq!(wire["retry_count"], 1);
        assert_eq!(wire["fatalError"], Value::Null);
        assert_eq!(wire["remaining"], 0);
        assert_eq!(wire["final_counts"]["processed"], 4);
        assert_eq!(wire["finalCounts"]["failed"], 1);
    }

    #[test]
    fn latest_summary_attempt_status_wins_over_existing_fact() {
        let event_id = uuid::Uuid::new_v4();
        let event = RawEvent::try_new(
            event_id,
            super::super::subject::Subject::SelfSubject,
            "manual",
            super::super::redaction::RedactedText::from_validated("name is Ada".into()),
        )
        .unwrap();
        let summary_fact = Fact::try_derive_with_metadata(
            super::super::subject::Subject::SelfSubject,
            FactPredicate::Fact,
            &format!("summary-{event_id}"),
            "Ada likes cats",
            FactStatus::Auto,
            event_id,
            0,
            "operation-summary",
            &super::super::policy::Policy::default(),
        )
        .unwrap();
        let mut statuses = HashMap::new();
        statuses.insert(
            event_id.to_string(),
            SummaryStatusInfo {
                status: "pending".into(),
                reason: Some("retrying".into()),
                ..SummaryStatusInfo::default()
            },
        );

        let row = raw_row(&event, &[summary_fact], &statuses, false);

        assert_eq!(row.summary_status.as_deref(), Some("pending"));
        assert_eq!(row.vector_source.as_deref(), Some("document"));
    }

    #[test]
    fn retry_matrix_accepts_terminal_model_failures_but_not_non_candidates() {
        for (status, reason) in [
            ("fallback", "inference_failed"),
            ("error", "invalid_model_output"),
        ] {
            assert!(summary_retry_allowed(&SummaryStatusInfo {
                status: status.into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
        }
        for reason in ["policy_excluded", "not_candidate", "candidate_not_eligible"] {
            assert!(!summary_retry_allowed(&SummaryStatusInfo {
                status: "skipped".into(),
                reason: Some(reason.into()),
                ..SummaryStatusInfo::default()
            }));
        }
        assert!(!summary_retry_allowed(&SummaryStatusInfo {
            status: "pending".into(),
            ..SummaryStatusInfo::default()
        }));
    }

    #[test]
    fn old_summary_rows_decode_without_additive_attempt_fields() {
        let row: SummaryRow = serde_json::from_value(json!({
            "summary_id": "summary-1",
            "event_id": "event-1",
            "summary": null,
            "status": "fallback",
            "error": null,
            "model_id": null,
            "prompt_version": null,
            "vector_source": "document",
            "derived_fact_id": null,
            "occurred_at": "2026-09-04T00:00:00Z",
            "source": "microphone",
            "event_type": "human"
        }))
        .unwrap();
        assert_eq!(row.reason, None);
        assert_eq!(row.attempt_id, None);
        assert_eq!(row.retry_count, 0);
    }
}
