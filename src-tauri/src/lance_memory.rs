use crate::memory_v2::policy::{Policy, PrivacyAdmission};
use crate::memory_v2::repository::{
    summary_fact_is_repairable, CompatMemory, MemoryRepository, SummaryBatchInput,
    SummaryStatusBatchInput,
};
use arrow_array::{
    builder::{FixedSizeListBuilder, Float32Builder},
    Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Connection, Table};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

pub const LANCE_DB_DIR: &str = "data/lancedb";
/// The historical table is intentionally retained as an import-only source.
pub const MEMORIES_TABLE: &str = "memories";
/// New writes go to this table when the historical table is still v1.
pub const MEMORY_V2_TABLE: &str = "memories_v2";
pub const VECTOR_DIM: i32 = 768;
const LEGACY_IMPORT_MARKER: &str = ".legacy_memories_v1.imported";
const IMPORT_BATCH_SIZE: usize = 2048;
// DataFusion parses `only_if` expressions recursively. Keep the guarded
// update bounded so a large all-memory pass cannot overflow the Windows
// worker stack while parsing one enormous `OR` tree.
const SUMMARY_QUEUE_PROJECTION_CHUNK_SIZE: usize = 256;

/// Progress for the one-time legacy LanceDB -> memory-v2 import.
///
/// The migration runs as part of the first memory query, so callers need a
/// thread-safe snapshot while the query itself is still awaiting LanceDB I/O.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryMigrationStatus {
    pub status: String,
    pub processed: usize,
    pub total: Option<usize>,
    pub message: String,
    pub error: Option<String>,
}

impl Default for MemoryMigrationStatus {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            processed: 0,
            total: None,
            message: String::new(),
            error: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct MemoryMigrationProgress {
    status: Mutex<MemoryMigrationStatus>,
}

pub type MigrationProgressHandle = Arc<MemoryMigrationProgress>;

impl MemoryMigrationProgress {
    pub fn snapshot(&self) -> MemoryMigrationStatus {
        self.status
            .lock()
            .expect("memory migration progress mutex poisoned")
            .clone()
    }

    fn begin(&self, total: usize) {
        let mut status = self
            .status
            .lock()
            .expect("memory migration progress mutex poisoned");
        status.status = "running".to_string();
        status.processed = 0;
        status.total = Some(total);
        status.message = "Migrating legacy LanceDB memories...".to_string();
        status.error = None;
    }

    fn update(&self, processed: usize) {
        let mut status = self
            .status
            .lock()
            .expect("memory migration progress mutex poisoned");
        status.processed = status
            .total
            .map(|total| processed.min(total))
            .unwrap_or(processed);
    }

    fn complete(&self, total: usize) {
        let mut status = self
            .status
            .lock()
            .expect("memory migration progress mutex poisoned");
        status.status = "completed".to_string();
        status.processed = total;
        status.total = Some(total);
        status.message = "Legacy LanceDB memories migrated.".to_string();
        status.error = None;
    }

    fn fail(&self, error: &str) {
        let mut status = self
            .status
            .lock()
            .expect("memory migration progress mutex poisoned");
        status.status = "error".to_string();
        status.message = "Legacy LanceDB migration failed.".to_string();
        status.error = Some(error.to_string());
    }
}

/// Spec §3.6: versioned table. v1 = 7 columns, v2 = 12 columns.
pub const MEMORY_SCHEMA_VERSION: u32 = 2;

pub const SUMMARY_STATUS_PENDING: &str = "pending";
pub const SUMMARY_STATUS_COMPLETED: &str = "completed";
pub const SUMMARY_STATUS_SKIPPED: &str = "skipped";
pub const SUMMARY_STATUS_FALLBACK: &str = "fallback";
pub const SUMMARY_STATUS_ERROR: &str = "error";
pub const SUMMARY_STATUS_LEGACY: &str = "legacy";
pub const SUMMARY_STATUS_DELETED: &str = "deleted";

pub const VECTOR_SOURCE_DOCUMENT: &str = "document";
pub const VECTOR_SOURCE_SUMMARY: &str = "summary";
pub const VECTOR_SOURCE_NONE: &str = "none";

pub const SUMMARY_MODEL_ID: &str = "gemma-3-1b-it-Q4_K_S.gguf";
/// Version of the local-summary input contract.  Bump the storage-facing
/// constant whenever the prompt or admission policy changes so an explicit
/// Process-all pass can safely re-evaluate rows produced by an older contract.
pub const SUMMARY_PROMPT_VERSION: &str = crate::memory_v2::repository::SUMMARY_PROMPT_VERSION;

/// The only event types that may enter the local summary model.  Keep this
/// allowlist in the storage-facing module so live capture, retry, queueing,
/// and explicit backfill all share one admission policy.
pub const SUMMARY_EVENT_ALLOWLIST: &[&str] = &["user_speech", "discord_speech", "twitch_chat"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryAdmission {
    /// The event type is allowed and the content can be made redaction-safe.
    Eligible,
    /// The event type is outside the summary allowlist.
    NotApplicable,
    /// The event type is allowed, but content is empty or cannot safely enter
    /// the model prompt.
    Invalid,
}

/// Classify a summary input without changing the persisted raw/document row.
/// This is intentionally pure from the caller's perspective: redaction is
/// performed only to verify that the content is safe and deterministic.
pub fn summary_admission(event_type: &str, content: &str) -> SummaryAdmission {
    if !is_summary_candidate_type(event_type) {
        return SummaryAdmission::NotApplicable;
    }
    if let Some(redacted) = redacted_summary_document(content) {
        if is_ephemeral_summary_text(&redacted) {
            return SummaryAdmission::Invalid;
        }
        SummaryAdmission::Eligible
    } else {
        SummaryAdmission::Invalid
    }
}

/// Deterministically remove the tiny, high-volume utterances that cannot
/// carry a durable fact.  This keeps Process all from spending a model call
/// on stream noise while leaving all other human text for Gemma's decision.
/// The list is intentionally conservative and exact-match only; raw rows are
/// never deleted or rewritten by this admission check.
fn is_ephemeral_summary_text(content: &str) -> bool {
    let normalized = content
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.chars().count() <= 1 {
        return true;
    }
    matches!(
        normalized.as_str(),
        "ああ"
            | "あー"
            | "ええ"
            | "えー"
            | "おお"
            | "おー"
            | "はい"
            | "はいはい"
            | "うん"
            | "うんうん"
            | "そう"
            | "そうそう"
            | "そうですね"
            | "なるほど"
            | "なるほどね"
            | "いいね"
            | "どうも"
            | "こんにちは"
            | "こんばんは"
            | "よろしく"
            | "よろしくお願いします"
            | "ありがとう"
            | "ありがとうございます"
            | "またね"
            | "お疲れ様"
            | "お疲れ様でした"
            | "おつかれさま"
            | "おつかれさまでした"
            | "了解"
            | "わかりました"
            | "分かりました"
            | "ok"
            | "ｗ"
            | "www"
    )
}

/// Return the redaction-safe document for an admitted candidate.  Callers use
/// this value for model material while retaining their original raw/document
/// value unchanged in storage.
pub fn admit_summary_document(event_type: &str, content: &str) -> Result<String, SummaryAdmission> {
    match summary_admission(event_type, content) {
        SummaryAdmission::Eligible => {
            redacted_summary_document(content).ok_or(SummaryAdmission::Invalid)
        }
        other => Err(other),
    }
}

fn redacted_summary_document(content: &str) -> Option<String> {
    let input = content.trim();
    if input.is_empty()
        || input.chars().any(|character| {
            character == '\0'
                || character == '\u{7f}'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
    {
        return None;
    }
    let redacted = Policy::default()
        .admit_raw_text(input, PrivacyAdmission::public())
        .ok()?;
    let redacted = redacted.as_str().trim();
    (!redacted.is_empty()).then(|| redacted.to_string())
}

const WINDOWS_REPARSE_POINT: u32 = 0x0400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryBackfillApplyResult {
    pub persisted: usize,
    pub policy_excluded: usize,
    /// Row-level exclusions discovered during persistence (policy rejections,
    /// empty/invalid summaries) so the backfill console can show why each row
    /// was not stored.
    pub exclusions: Vec<SummaryExclusionDetail>,
    /// Terminal failures produced while validating the batch (for example an
    /// invalid embedding).  These are separate from inference failures the
    /// caller already counted before persistence.
    pub terminal_failed: usize,
    /// Terminal skipped rows produced while validating/policy-checking the
    /// batch.  These are separate from model-declined rows already counted by
    /// the caller.
    pub terminal_skipped: usize,
    /// The durable journal commit may succeed even when the compatibility
    /// projection cannot be updated.  Keep the committed counts and expose
    /// the repairable projection error to the caller instead of discarding the
    /// successful durable result behind a plain `Err`.
    pub projection_error: Option<String>,
}

/// One row the memory-v2 policy or validation refused to persist, with the
/// machine-readable reason (`policy_excluded`, `empty_summary`,
/// `invalid_embedding`, `fact_validation_failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryExclusionDetail {
    pub entity_id: String,
    pub reason: String,
}

fn validate_root_dir(root_dir: &Path) -> Result<(), String> {
    if !root_dir.is_absolute() {
        return Err("LanceDB root must be absolute".to_string());
    }
    if root_dir
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("LanceDB root must not contain traversal components".to_string());
    }
    validate_no_reparse_components(root_dir, "LanceDB root")
}

fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        return metadata.file_attributes() & WINDOWS_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn validate_no_reparse_components(path: &Path, label: &str) -> Result<(), String> {
    let mut current = path.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata_is_link_or_reparse(&metadata) {
                    return Err(format!(
                        "{} must not contain links or reparse points",
                        label
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("Failed to inspect {}: {}", label, error));
            }
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    Ok(())
}

fn validate_path_inside_root(root_dir: &Path, path: &Path, label: &str) -> Result<(), String> {
    validate_root_dir(root_dir)?;
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(format!("{} must not contain traversal components", label));
    }
    if !path.starts_with(root_dir) {
        return Err(format!("{} must remain inside the LanceDB root", label));
    }
    validate_no_reparse_components(path, label)
}

fn validate_safe_component(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value == "." || value == ".." {
        return Err(format!("{} must be a single safe component", label));
    }
    if value.contains('/') || value.contains('\\') || value.contains(':') || value.contains('\0') {
        return Err(format!("{} must be a single safe component", label));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.components().count() != 1
    {
        return Err(format!("{} must be a single safe component", label));
    }
    Ok(())
}

fn validate_safe_backup_name(name: &str) -> Result<(), String> {
    validate_safe_component(name, "Backup name")?;
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("Backup name must be a single safe component".to_string());
    }
    Ok(())
}

fn validate_memory_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("memory id cannot be used for a raw document backup".to_string());
    }
    Ok(())
}

fn validate_copy_tree(path: &Path, label: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {}: {}", label, error))?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(format!(
            "{} must not contain links or reparse points",
            label
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("{} must be a directory", label));
    }
    for entry in std::fs::read_dir(path)
        .map_err(|error| format!("Failed to inspect {}: {}", label, error))?
    {
        let entry = entry.map_err(|error| format!("Failed to inspect {}: {}", label, error))?;
        let child = entry.path();
        let child_label = format!("{} entry", label);
        let child_metadata = std::fs::symlink_metadata(&child)
            .map_err(|error| format!("Failed to inspect {}: {}", child_label, error))?;
        if metadata_is_link_or_reparse(&child_metadata) {
            return Err(format!(
                "{} must not contain links or reparse points",
                child_label
            ));
        }
        if child_metadata.is_dir() {
            validate_copy_tree(&child, &child_label)?;
        }
    }
    Ok(())
}

/// Summary state is a one-way pipeline.  Repeating an already applied state
/// is safe, while terminal states cannot be downgraded or replaced.
pub fn summary_status_transition_allowed(current: Option<&str>, next: &str) -> bool {
    current == Some(next)
        || (current == Some(SUMMARY_STATUS_PENDING)
            && matches!(
                next,
                SUMMARY_STATUS_COMPLETED
                    | SUMMARY_STATUS_SKIPPED
                    | SUMMARY_STATUS_FALLBACK
                    | SUMMARY_STATUS_ERROR
            ))
        // An explicit user retry is the only operation that may reopen a
        // terminal attempt.  The caller must still restrict this transition
        // to the retry command; ordinary status markers never request
        // `pending`.
        || (next == SUMMARY_STATUS_PENDING
            && matches!(
                current,
                Some(SUMMARY_STATUS_FALLBACK)
                    | Some(SUMMARY_STATUS_ERROR)
                    | Some(SUMMARY_STATUS_SKIPPED)
            ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub document: String,
    pub memory_type: String,
    pub source: String,
    pub timestamp: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryListResponse {
    pub success: bool,
    pub total: usize,
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStats {
    pub success: bool,
    pub imported_count: usize,
    pub message: String,
}

/// Full row including the §3.6 summary pipeline columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMemory {
    pub id: String,
    pub document: String,
    pub memory_type: String,
    pub source: String,
    pub timestamp: String,
    pub user_id: Option<String>,
    pub summary: Option<String>,
    pub summary_status: Option<String>,
    pub summary_model: Option<String>,
    pub summary_prompt_version: Option<String>,
    pub vector_source: Option<String>,
}

/// Vector-search hit. `item.document` always keeps the raw text (§3.4.10);
/// `searchable_text()` prefers the completed summary for retrieval/display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchHit {
    pub item: MemoryItem,
    pub summary: Option<String>,
    pub summary_status: Option<String>,
    pub summary_model: Option<String>,
    pub summary_prompt_version: Option<String>,
    pub vector_source: Option<String>,
}

impl MemorySearchHit {
    pub fn searchable_text(&self) -> &str {
        if self.summary_status.as_deref() == Some(SUMMARY_STATUS_COMPLETED) {
            if let Some(s) = self.summary.as_deref() {
                if !s.is_empty() {
                    return s;
                }
            }
        }
        &self.item.document
    }
}

impl From<StoredMemory> for MemorySearchHit {
    fn from(row: StoredMemory) -> Self {
        Self {
            item: MemoryItem {
                id: row.id.clone(),
                document: row.document.clone(),
                memory_type: row.memory_type.clone(),
                source: row.source.clone(),
                timestamp: row.timestamp.clone(),
                user_id: row.user_id.clone(),
            },
            summary: row.summary,
            summary_status: row.summary_status,
            summary_model: row.summary_model,
            summary_prompt_version: row.summary_prompt_version,
            vector_source: row.vector_source,
        }
    }
}

/// Spec §3.3: only human-origin events are summarized. Local function so the
/// LanceDB layer never depends on the ASR/llama runtime (cycle avoidance).
pub fn is_summary_candidate_type(memory_type: &str) -> bool {
    SUMMARY_EVENT_ALLOWLIST.contains(&memory_type)
}

pub fn get_memory_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("document", DataType::Utf8, false),
        Field::new("memory_type", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("user_id", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, false)),
                VECTOR_DIM,
            ),
            true,
        ),
        Field::new("summary", DataType::Utf8, true),
        Field::new("summary_status", DataType::Utf8, true),
        Field::new("summary_model", DataType::Utf8, true),
        Field::new("summary_prompt_version", DataType::Utf8, true),
        Field::new("vector_source", DataType::Utf8, true),
    ]))
}

pub fn legacy_memory_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("document", DataType::Utf8, false),
        Field::new("memory_type", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("user_id", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, false)),
                VECTOR_DIM,
            ),
            true,
        ),
    ]))
}

pub fn detect_memory_schema_version(schema: &Schema) -> u32 {
    if schema_matches_memory_v2(schema) {
        MEMORY_SCHEMA_VERSION
    } else {
        1
    }
}

fn schema_matches_memory_v2(schema: &Schema) -> bool {
    let expected = get_memory_schema();
    schema.fields().len() == expected.fields().len()
        && schema
            .fields()
            .iter()
            .zip(expected.fields())
            .all(|(actual, expected)| {
                actual.name() == expected.name()
                    && compatible_memory_data_type(actual.data_type(), expected.data_type())
                    && actual.is_nullable() == expected.is_nullable()
            })
}

/// LanceDB currently normalizes FixedSizeList child fields to nullable on
/// read, even when the Arrow schema used for creation marks them non-nullable.
/// The outer vector validity remains significant (missing vectors are null),
/// so compatibility ignores only this storage-level child nullability bit.
fn compatible_memory_data_type(actual: &DataType, expected: &DataType) -> bool {
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

/// Validate a vector at the persistence boundary. Missing vectors are handled
/// separately as outer-null list values and never enter this function.
pub fn validate_vector(values: &[f32]) -> Result<(), String> {
    if values.len() != VECTOR_DIM as usize {
        return Err(format!(
            "Vector length mismatch: expected {}, got {}",
            VECTOR_DIM,
            values.len()
        ));
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err("Vector values must be finite".to_string());
    }
    Ok(())
}

/// Construct the nullable outer FixedSizeList used by memory-v2. Child slots
/// for an outer-null row are only physical padding; the row validity bit is
/// what represents absence and prevents a zero vector from being searchable.
fn build_vector_array(values: &[Option<Vec<f32>>]) -> Result<Arc<FixedSizeListArray>, String> {
    let child = Float32Builder::with_capacity(values.len() * VECTOR_DIM as usize);
    let mut builder = FixedSizeListBuilder::with_capacity(child, VECTOR_DIM, values.len())
        .with_field(Field::new("item", DataType::Float32, false));

    for value in values {
        match value {
            Some(vector) => {
                validate_vector(vector)?;
                for component in vector {
                    builder.values().append_value(*component);
                }
                builder.append(true);
            }
            None => {
                // Fixed-size lists require physical child slots even when the
                // outer list is null. They are masked by append(false); these
                // finite padding values are not logical vector components.
                for _ in 0..VECTOR_DIM {
                    builder.values().append_value(f32::MIN_POSITIVE);
                }
                builder.append(false);
            }
        }
    }
    Ok(Arc::new(builder.finish()))
}

#[cfg(test)]
fn vector_array(values: &[f32]) -> Arc<FixedSizeListArray> {
    build_vector_array(&[Some(values.to_vec())]).expect("valid test vector")
}

fn empty_v2_batch(schema: &Arc<Schema>) -> Result<RecordBatch, String> {
    let id_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<String>::new()));
    let doc_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<String>::new()));
    let type_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<String>::new()));
    let src_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<String>::new()));
    let ts_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<String>::new()));
    let uid_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    let vector_array: Arc<dyn Array> = build_vector_array(&[])?;
    let summary_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    let status_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    let model_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    let prompt_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    let vsrc_array: Arc<dyn Array> = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
    RecordBatch::try_new(
        schema.clone(),
        vec![
            id_array,
            doc_array,
            type_array,
            src_array,
            ts_array,
            uid_array,
            vector_array,
            summary_array,
            status_array,
            model_array,
            prompt_array,
            vsrc_array,
        ],
    )
    .map_err(|e| format!("Failed to create empty v2 batch: {}", e))
}

fn legacy_batch_to_items(
    batch: &RecordBatch,
) -> Result<(Vec<MemoryItem>, Vec<Option<Vec<f32>>>), String> {
    fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, String> {
        batch
            .column_by_name(name)
            .ok_or_else(|| format!("Missing legacy {} column", name))?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| format!("Invalid legacy {} column", name))
    }

    let ids = strings(batch, "id")?;
    let documents = strings(batch, "document")?;
    let memory_types = strings(batch, "memory_type")?;
    let sources = strings(batch, "source")?;
    let timestamps = strings(batch, "timestamp")?;
    let users = strings(batch, "user_id")?;
    let vectors = batch
        .column_by_name("vector")
        .ok_or_else(|| "Missing legacy vector column".to_string())?
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .ok_or_else(|| "Invalid legacy vector column".to_string())?;

    let mut items = Vec::with_capacity(batch.num_rows());
    let mut vector_values = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let vector = if vectors.is_null(row) {
            None
        } else {
            let vector_array = vectors.value(row);
            let values = vector_array
                .as_any()
                .downcast_ref::<Float32Array>()
                .ok_or_else(|| "Invalid legacy vector values".to_string())?;
            let vector: Vec<f32> = (0..values.len()).map(|index| values.value(index)).collect();
            validate_vector(&vector)?;
            Some(vector)
        };
        items.push(MemoryItem {
            id: ids.value(row).to_string(),
            document: documents.value(row).to_string(),
            memory_type: memory_types.value(row).to_string(),
            source: sources.value(row).to_string(),
            timestamp: timestamps.value(row).to_string(),
            user_id: (!users.is_null(row)).then(|| users.value(row).to_string()),
        });
        vector_values.push(vector);
    }
    Ok((items, vector_values))
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn batch_row_to_stored(batch: &RecordBatch, row: usize) -> Result<StoredMemory, String> {
    fn opt_string(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>, String> {
        match batch.column_by_name(name) {
            Some(col) => {
                let arr = col
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| format!("Invalid {} column", name))?;
                if arr.is_null(row) {
                    Ok(None)
                } else {
                    Ok(Some(arr.value(row).to_string()))
                }
            }
            None => Ok(None),
        }
    }
    fn req_string(batch: &RecordBatch, name: &str, row: usize) -> Result<String, String> {
        let col = batch
            .column_by_name(name)
            .ok_or_else(|| format!("Missing {} column", name))?;
        let arr = col
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| format!("Invalid {} column", name))?;
        if arr.is_null(row) {
            Ok(String::new())
        } else {
            Ok(arr.value(row).to_string())
        }
    }
    Ok(StoredMemory {
        id: req_string(batch, "id", row)?,
        document: req_string(batch, "document", row)?,
        memory_type: req_string(batch, "memory_type", row)?,
        source: req_string(batch, "source", row)?,
        timestamp: req_string(batch, "timestamp", row)?,
        user_id: opt_string(batch, "user_id", row)?,
        summary: opt_string(batch, "summary", row)?,
        summary_status: opt_string(batch, "summary_status", row)?,
        summary_model: opt_string(batch, "summary_model", row)?,
        summary_prompt_version: opt_string(batch, "summary_prompt_version", row)?,
        vector_source: opt_string(batch, "vector_source", row)?,
    })
}

pub async fn get_or_create_db(root_dir: &Path) -> Result<Connection, String> {
    validate_root_dir(root_dir)?;
    let db_path = root_dir.join(LANCE_DB_DIR);
    validate_path_inside_root(root_dir, &db_path, "LanceDB directory")?;
    if !db_path.exists() {
        std::fs::create_dir_all(&db_path)
            .map_err(|e| format!("Failed to create LanceDB dir: {}", e))?;
    }
    let db_path_str = db_path
        .to_str()
        .ok_or_else(|| "Invalid DB path".to_string())?;
    connect(db_path_str)
        .execute()
        .await
        .map_err(|e| format!("Failed to connect to LanceDB: {}", e))
}

pub async fn get_or_create_memories_table(
    db: &Connection,
    root_dir: &Path,
) -> Result<Table, String> {
    get_or_create_memories_table_with_progress(db, root_dir, None).await
}

async fn get_or_create_memories_table_with_progress(
    db: &Connection,
    root_dir: &Path,
    progress: Option<MigrationProgressHandle>,
) -> Result<Table, String> {
    let table_names = db
        .table_names()
        .execute()
        .await
        .map_err(|e| format!("Failed to list tables: {}", e))?;
    if table_names.contains(&MEMORY_V2_TABLE.to_string()) {
        let table = db
            .open_table(MEMORY_V2_TABLE)
            .execute()
            .await
            .map_err(|e| format!("Failed to open memory-v2 table: {}", e))?;
        let schema = table
            .schema()
            .await
            .map_err(|e| format!("Failed to read memory-v2 schema: {}", e))?;
        if !schema_matches_memory_v2(&schema) {
            return Err("Existing memory-v2 table has an incompatible schema".to_string());
        }
        if table_names.contains(&MEMORIES_TABLE.to_string())
            && !root_dir
                .join(LANCE_DB_DIR)
                .join(LEGACY_IMPORT_MARKER)
                .exists()
        {
            let legacy = db
                .open_table(MEMORIES_TABLE)
                .execute()
                .await
                .map_err(|e| format!("Failed to open legacy memories table: {}", e))?;
            let legacy_schema = legacy
                .schema()
                .await
                .map_err(|e| format!("Read legacy schema: {}", e))?;
            if detect_memory_schema_version(&legacy_schema) < MEMORY_SCHEMA_VERSION {
                import_legacy_table_with_progress(root_dir, &legacy, &table, progress.clone())
                    .await?;
            }
        }
        return Ok(table);
    }

    if table_names.contains(&MEMORIES_TABLE.to_string()) {
        let legacy = db
            .open_table(MEMORIES_TABLE)
            .execute()
            .await
            .map_err(|e| format!("Failed to open legacy memories table: {}", e))?;
        let schema = legacy
            .schema()
            .await
            .map_err(|e| format!("Failed to read legacy schema: {}", e))?;
        if schema_matches_memory_v2(&schema) {
            // Existing installs that already have an exact v2 schema remain
            // compatible; only a v1 table is treated as import-only legacy.
            return Ok(legacy);
        }
        let active = create_memory_v2_table(db).await?;
        import_legacy_table_with_progress(root_dir, &legacy, &active, progress).await?;
        return Ok(active);
    }

    create_memory_v2_table(db).await
}

async fn create_memory_v2_table(db: &Connection) -> Result<Table, String> {
    let schema = get_memory_schema();
    let batch = empty_v2_batch(&schema)?;
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    db.create_table(MEMORY_V2_TABLE, reader)
        .execute()
        .await
        .map_err(|e| format!("Failed to create memory-v2 table: {}", e))
}

async fn import_legacy_table_with_progress(
    root_dir: &Path,
    legacy: &Table,
    active: &Table,
    progress: Option<MigrationProgressHandle>,
) -> Result<usize, String> {
    let result = async {
        let total = legacy
            .count_rows(None)
            .await
            .map_err(|e| format!("Legacy migration count failed: {}", e))?;
        if let Some(progress) = &progress {
            progress.begin(total);
        }
        let repository = MemoryRepository::open(root_dir).await?;
        let mut stream = legacy
            .query()
            .execute()
            .await
            .map_err(|e| format!("Legacy migration scan failed: {}", e))?;
        let mut imported = 0;
        let mut processed = 0;
        let mut projection_items = Vec::new();
        let mut projection_vectors = Vec::new();
        // LanceDB may return many small RecordBatches (especially when the
        // legacy table has accumulated many fragments).  Do not run a full
        // Journal recovery for each one; coalesce them into the same bounded
        // chunks used by the importer.
        let mut pending_items: Vec<MemoryItem> = Vec::new();
        let mut pending_vectors: Vec<Option<Vec<f32>>> = Vec::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| format!("Legacy migration read batch: {}", e))?
        {
            let (items, vectors) = legacy_batch_to_items(&batch)?;
            pending_items.extend(items);
            pending_vectors.extend(vectors);
            while pending_items.len() >= IMPORT_BATCH_SIZE {
                let items: Vec<MemoryItem> = pending_items.drain(..IMPORT_BATCH_SIZE).collect();
                let vectors: Vec<Option<Vec<f32>>> =
                    pending_vectors.drain(..IMPORT_BATCH_SIZE).collect();
                for vector in vectors.iter().flatten() {
                    validate_vector(vector)?;
                }
                let compat_items = items
                    .iter()
                    .map(|item| CompatMemory {
                        id: item.id.clone(),
                        memory_type: item.memory_type.clone(),
                        source: item.source.clone(),
                        timestamp: item.timestamp.clone(),
                        document: item.document.clone(),
                    })
                    .collect();
                let embeddings = items
                    .iter()
                    .zip(vectors.iter())
                    .filter_map(|(item, vector)| {
                        vector
                            .as_ref()
                            .map(|vector| (item.id.clone(), vector.clone()))
                    })
                    .collect();
                repository
                    .append_compat_events_with_embeddings_batch(compat_items, embeddings)
                    .await?;
                projection_items.extend(items.iter().cloned());
                projection_vectors.extend(vectors.iter().cloned());
                processed += items.len();
                if let Some(progress) = &progress {
                    progress.update(processed);
                }
            }
        }
        if !pending_items.is_empty() {
            let items: Vec<MemoryItem> = pending_items.drain(..).collect();
            let vectors: Vec<Option<Vec<f32>>> = pending_vectors.drain(..).collect();
            for vector in vectors.iter().flatten() {
                validate_vector(vector)?;
            }
            let compat_items = items
                .iter()
                .map(|item| CompatMemory {
                    id: item.id.clone(),
                    memory_type: item.memory_type.clone(),
                    source: item.source.clone(),
                    timestamp: item.timestamp.clone(),
                    document: item.document.clone(),
                })
                .collect();
            let embeddings = items
                .iter()
                .zip(vectors.iter())
                .filter_map(|(item, vector)| {
                    vector
                        .as_ref()
                        .map(|vector| (item.id.clone(), vector.clone()))
                })
                .collect();
            repository
                .append_compat_events_with_embeddings_batch(compat_items, embeddings)
                .await?;
            let count = items.len();
            projection_items.extend(items);
            projection_vectors.extend(vectors);
            processed += count;
            if let Some(progress) = &progress {
                progress.update(processed);
            }
        }
        // The compatibility projection is not part of the authoritative
        // Journal transaction. Write it once after all durable rows are
        // committed so a large import is not dominated by repeated LanceDB
        // table-version commits.
        imported +=
            insert_on_table(active, projection_items, projection_vectors, true, true).await?;
        let marker = root_dir.join(LANCE_DB_DIR).join(LEGACY_IMPORT_MARKER);
        std::fs::write(&marker, b"verified\n")
            .map_err(|e| format!("Write legacy import marker: {}", e))?;
        if let Some(progress) = &progress {
            progress.complete(total);
        }
        Ok::<usize, String>(imported)
    }
    .await;

    if let Err(error) = &result {
        if let Some(progress) = &progress {
            progress.fail(error);
        }
    }
    result
}

/// Import the historical `memories` table without opening it for writes.
/// Calling this repeatedly is safe: completed imports are marked and each
/// bounded batch also skips IDs already present in memory-v2.
pub async fn import_legacy_memories(root_dir: &Path) -> Result<MigrationStats, String> {
    let db = get_or_create_db(root_dir).await?;
    let names = db
        .table_names()
        .execute()
        .await
        .map_err(|e| format!("Failed to list tables: {}", e))?;
    if !names.contains(&MEMORIES_TABLE.to_string()) {
        return Ok(MigrationStats {
            success: true,
            imported_count: 0,
            message: "Legacy memories table not found; nothing to import".to_string(),
        });
    }
    let source = db
        .open_table(MEMORIES_TABLE)
        .execute()
        .await
        .map_err(|e| format!("Failed to open legacy memories table: {}", e))?;
    let source_schema = source
        .schema()
        .await
        .map_err(|e| format!("Failed to read legacy schema: {}", e))?;
    if schema_matches_memory_v2(&source_schema) {
        return Ok(MigrationStats {
            success: true,
            imported_count: 0,
            message: "Memories table already uses memory-v2 schema".to_string(),
        });
    }
    let existing_count = if names.contains(&MEMORY_V2_TABLE.to_string()) {
        let existing = db
            .open_table(MEMORY_V2_TABLE)
            .execute()
            .await
            .map_err(|e| format!("Failed to open memory-v2 table: {}", e))?;
        table_row_count(&existing).await?
    } else {
        0
    };
    let active = get_or_create_memories_table(&db, root_dir).await?;
    let imported_count = table_row_count(&active)
        .await?
        .saturating_sub(existing_count);
    Ok(MigrationStats {
        success: true,
        imported_count,
        message: "Legacy memories imported into memory-v2; source retained read-only".to_string(),
    })
}

async fn table_row_count(table: &Table) -> Result<usize, String> {
    let mut stream = table
        .query()
        .execute()
        .await
        .map_err(|e| format!("Table count scan failed: {}", e))?;
    let mut count = 0;
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Table count read failed: {}", e))?
    {
        count += batch.num_rows();
    }
    Ok(count)
}

pub async fn get_memory_by_id(root_dir: &Path, id: &str) -> Result<Option<StoredMemory>, String> {
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let predicate = format!("id = '{}'", escape_sql_literal(id));
    let mut stream = table
        .query()
        .only_if(predicate)
        .execute()
        .await
        .map_err(|e| format!("Query by id error: {}", e))?;
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Stream batch error: {}", e))?
    {
        for row in 0..batch.num_rows() {
            return Ok(Some(batch_row_to_stored(&batch, row)?));
        }
    }
    Ok(None)
}

/// Resolve a compatibility-projection row using either its legacy ID or the
/// canonical memory-v2 event ID.  The API exposes canonical IDs, while old
/// LanceDB rows may retain arbitrary import IDs, so retry/delete flows must
/// bridge both representations.
pub async fn get_memory_by_event_id(
    root_dir: &Path,
    event_id: &str,
) -> Result<Option<StoredMemory>, String> {
    if let Some(row) = get_memory_by_id(root_dir, event_id).await? {
        return Ok(Some(row));
    }
    let canonical = MemoryRepository::canonical_event_id(event_id);
    // A canonical UUID may still have an arbitrary legacy ID in the
    // compatibility projection.  Do not short-circuit merely because the
    // requested value already parses as a UUID; scan the projection to bridge
    // that representation as well.
    Ok(list_stored_memories(root_dir)
        .await?
        .into_iter()
        .find(|row| MemoryRepository::canonical_event_id(&row.id) == canonical))
}

pub async fn list_memories(
    root_dir: &Path,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<MemoryListResponse, String> {
    list_memories_with_progress(root_dir, limit, offset, None).await
}

pub async fn list_memories_with_progress(
    root_dir: &Path,
    limit: Option<usize>,
    offset: Option<usize>,
    progress: Option<MigrationProgressHandle>,
) -> Result<MemoryListResponse, String> {
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table_with_progress(&db, root_dir, progress).await?;

    let query = table.query();
    let mut stream = query
        .execute()
        .await
        .map_err(|e| format!("Query execute error: {}", e))?;
    let mut memories = Vec::new();

    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Stream batch error: {}", e))?
    {
        let id_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Invalid id column")?;
        let doc_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Invalid doc column")?;
        let type_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Invalid type column")?;
        let src_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Invalid src column")?;
        let ts_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Invalid ts column")?;
        let uid_col = batch.column(5).as_any().downcast_ref::<StringArray>();

        for row in 0..batch.num_rows() {
            let id = id_col.value(row).to_string();
            let document = doc_col.value(row).to_string();
            let memory_type = type_col.value(row).to_string();
            let source = src_col.value(row).to_string();
            let timestamp = ts_col.value(row).to_string();
            let user_id = uid_col.and_then(|c| {
                if c.is_null(row) {
                    None
                } else {
                    Some(c.value(row).to_string())
                }
            });

            memories.push(MemoryItem {
                id,
                document,
                memory_type,
                source,
                timestamp,
                user_id,
            });
        }
    }

    let total = memories.len();

    // タイムスタンプ降順（最新の記憶が先頭に来るようにソート）
    memories.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // offset & limit を最新順の配列に適用
    let start = offset.unwrap_or(0);
    let memories_slice = if start < memories.len() {
        let end = match limit {
            Some(l) => (start + l).min(memories.len()),
            None => memories.len(),
        };
        memories[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(MemoryListResponse {
        success: true,
        total,
        memories: memories_slice,
    })
}

/// Read the complete compatibility projection in one LanceDB scan.  The
/// explicit backfill path needs summary/status columns as well as the raw
/// event fields; calling `get_memory_by_id` for every row would reopen the
/// table and rescan the journal tens of thousands of times.
pub async fn list_stored_memories(root_dir: &Path) -> Result<Vec<StoredMemory>, String> {
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let mut stream = table
        .query()
        .execute()
        .await
        .map_err(|e| format!("Stored memory query execute error: {}", e))?;
    let mut memories = Vec::new();
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Stored memory stream error: {}", e))?
    {
        for row in 0..batch.num_rows() {
            memories.push(batch_row_to_stored(&batch, row)?);
        }
    }
    memories.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(memories)
}

/// 768 次元ベクトルによるセマンティック類似度検索 (LanceDB Vector Search)
/// skipped 行は除外し、completed 行は要約を優先して返す(document は保持)。
pub async fn search_similar_memories(
    root_dir: &Path,
    query_vector: &[f32],
    limit: usize,
) -> Result<Vec<MemorySearchHit>, String> {
    validate_vector(query_vector)?;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;

    let q_vec = query_vector.to_vec();
    // Fetch extra rows so Rust-side skipped filtering rarely starves the limit.
    let fetch_limit = limit
        .saturating_mul(2)
        .max(limit)
        .min(limit + 50)
        .max(limit);
    let stream = table
        .vector_search(q_vec)
        .map_err(|e| format!("Vector search build error: {}", e))?
        .only_if("vector IS NOT NULL")
        .limit(fetch_limit)
        .execute()
        .await
        .map_err(|e| format!("Vector search execute error: {}", e))?;

    let mut hits = Vec::new();
    let mut s = stream;
    while let Some(batch) = s
        .try_next()
        .await
        .map_err(|e| format!("Stream error: {}", e))?
    {
        for row in 0..batch.num_rows() {
            let stored = batch_row_to_stored(&batch, row)?;
            if stored.summary_status.as_deref() == Some(SUMMARY_STATUS_SKIPPED) {
                continue;
            }
            if stored.document.is_empty() {
                // Keep parity with legacy behavior: skip empty documents unless
                // a completed summary provides searchable text.
                let has_summary = stored.summary_status.as_deref()
                    == Some(SUMMARY_STATUS_COMPLETED)
                    && stored
                        .summary
                        .as_deref()
                        .map(|v| !v.is_empty())
                        .unwrap_or(false);
                if !has_summary {
                    continue;
                }
            }
            hits.push(MemorySearchHit::from(stored));
            if hits.len() >= limit {
                break;
            }
        }
        if hits.len() >= limit {
            break;
        }
    }

    hits.truncate(limit);
    Ok(hits)
}

pub async fn insert_memory_batch(
    root_dir: &Path,
    items: Vec<MemoryItem>,
    vectors: Option<Vec<Vec<f32>>>,
) -> Result<usize, String> {
    if items.is_empty() {
        return Ok(0);
    }
    let vectors = match vectors {
        Some(vectors) if vectors.len() != items.len() => {
            return Err(format!(
                "Vector count mismatch: expected {}, got {}",
                items.len(),
                vectors.len()
            ));
        }
        Some(vectors) => Some(vectors.into_iter().map(Some).collect()),
        None => None,
    };
    // Event IDs are stable at the boundary, so repeated saves are no-ops
    // rather than duplicate physical rows.
    insert_memory_batch_nullable(root_dir, items, vectors, false).await
}

/// Insert rows whose vectors may be absent. This is the adapter used by the
/// legacy importer; the existing public API remains source-compatible.
pub async fn insert_memory_batch_nullable(
    root_dir: &Path,
    items: Vec<MemoryItem>,
    vectors: Option<Vec<Option<Vec<f32>>>>,
    legacy: bool,
) -> Result<usize, String> {
    if items.is_empty() {
        return Ok(0);
    }
    let vectors = vectors.unwrap_or_else(|| vec![None; items.len()]);
    if vectors.len() != items.len() {
        return Err(format!(
            "Vector count mismatch: expected {}, got {}",
            items.len(),
            vectors.len()
        ));
    }
    for vector in vectors.iter().flatten() {
        validate_vector(vector)?;
    }
    // The memory-v2 repository is the authoritative journal/manifest boundary.
    // Keep the existing table as a compatibility projection until the UI and
    // migration cut over completely; it must never receive a write first.
    let repository = MemoryRepository::open(root_dir).await?;
    for (item, vector) in items.iter().zip(vectors.iter().cloned()) {
        repository
            .append_compat_event(CompatMemory {
                id: item.id.clone(),
                memory_type: item.memory_type.clone(),
                source: item.source.clone(),
                timestamp: item.timestamp.clone(),
                document: item.document.clone(),
            })
            .await?;
        if let Some(vector) = vector {
            repository
                .put_embedding(&item.id, vector, VECTOR_SOURCE_DOCUMENT)
                .await?;
        }
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    insert_on_table(&table, items, vectors, legacy, true).await
}

async fn existing_ids(table: &Table, ids: &[String]) -> Result<HashSet<String>, String> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let predicate = format!(
        "id IN ({})",
        ids.iter()
            .map(|id| format!("'{}'", escape_sql_literal(id)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut stream = table
        .query()
        .only_if(predicate)
        .execute()
        .await
        .map_err(|e| format!("Existing ID query error: {}", e))?;
    let mut result = HashSet::new();
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Existing ID stream error: {}", e))?
    {
        let ids = batch
            .column_by_name("id")
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| "Invalid id column".to_string())?;
        for row in 0..batch.num_rows() {
            result.insert(ids.value(row).to_string());
        }
    }
    Ok(result)
}

async fn insert_on_table(
    table: &Table,
    mut items: Vec<MemoryItem>,
    mut vectors: Vec<Option<Vec<f32>>>,
    legacy: bool,
    skip_existing: bool,
) -> Result<usize, String> {
    if vectors.len() != items.len() {
        return Err(format!(
            "Vector count mismatch: expected {}, got {}",
            items.len(),
            vectors.len()
        ));
    }
    for vector in vectors.iter().flatten() {
        validate_vector(vector)?;
    }
    if skip_existing {
        let existing = existing_ids(
            table,
            &items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
        )
        .await?;
        let mut new_items = Vec::with_capacity(items.len());
        let mut new_vectors = Vec::with_capacity(vectors.len());
        let mut seen = HashSet::with_capacity(items.len());
        for (item, vector) in items.into_iter().zip(vectors.into_iter()) {
            if !existing.contains(&item.id) && seen.insert(item.id.clone()) {
                new_items.push(item);
                new_vectors.push(vector);
            }
        }
        items = new_items;
        vectors = new_vectors;
    }
    if items.is_empty() {
        return Ok(0);
    }

    let schema = get_memory_schema();
    let count = items.len();
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let docs: Vec<String> = items.iter().map(|i| i.document.clone()).collect();
    let types: Vec<String> = items.iter().map(|i| i.memory_type.clone()).collect();
    let srcs: Vec<String> = items.iter().map(|i| i.source.clone()).collect();
    let tss: Vec<String> = items.iter().map(|i| i.timestamp.clone()).collect();
    let uids: Vec<Option<String>> = items.iter().map(|i| i.user_id.clone()).collect();
    let statuses: Vec<Option<String>> = items
        .iter()
        .map(|item| {
            if legacy {
                Some(SUMMARY_STATUS_LEGACY.to_string())
            } else if summary_admission(&item.memory_type, &item.document)
                == SummaryAdmission::Eligible
            {
                Some(SUMMARY_STATUS_PENDING.to_string())
            } else {
                None
            }
        })
        .collect();
    let vsources: Vec<Option<String>> = vectors
        .iter()
        .map(|vector| {
            Some(if vector.is_some() {
                VECTOR_SOURCE_DOCUMENT.to_string()
            } else {
                VECTOR_SOURCE_NONE.to_string()
            })
        })
        .collect();
    let vector_array: Arc<dyn Array> = build_vector_array(&vectors)?;
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(docs)),
            Arc::new(StringArray::from(types)),
            Arc::new(StringArray::from(srcs)),
            Arc::new(StringArray::from(tss)),
            Arc::new(StringArray::from(uids)),
            vector_array,
            Arc::new(StringArray::from(vec![None::<String>; count])),
            Arc::new(StringArray::from(statuses)),
            Arc::new(StringArray::from(vec![None::<String>; count])),
            Arc::new(StringArray::from(vec![None::<String>; count])),
            Arc::new(StringArray::from(vsources)),
        ],
    )
    .map_err(|e| format!("Failed to create insert batch: {}", e))?;
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    table
        .add(reader)
        .execute()
        .await
        .map_err(|e| format!("Failed to add to table: {}", e))?;
    Ok(count)
}

fn vector_literal(vector: &[f32]) -> String {
    format!(
        "[{}]",
        vector
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Promote the raw row's nullable vector after asynchronous embedding.  A
/// summary vector wins permanently; late document embeddings must not replace
/// it.  The predicate also makes duplicate embedding retries harmless.
pub async fn update_document_vector(
    root_dir: &Path,
    id: &str,
    vector: Vec<f32>,
) -> Result<bool, String> {
    validate_vector(&vector).map_err(|error| format!("Invalid document vector: {}", error))?;
    let repository = MemoryRepository::open(root_dir).await?;
    // The repository's boolean reports whether a journal operation was newly
    // appended, which is distinct from whether this document vector actually
    // filled an absent compatibility-projection vector. A retry with a
    // different late document embedding must remain a no-op once any vector
    // (especially a promoted summary vector) exists.
    repository
        .put_embedding(id, vector.clone(), VECTOR_SOURCE_DOCUMENT)
        .await?;
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    // A document embedding is a one-time promotion from an absent vector.
    // Checking the vector itself (rather than only vector_source) protects
    // both document and summary vectors from late or retried work, including
    // rows whose metadata is stale or null.
    let predicate = format!("id = '{}' AND vector IS NULL", escape_sql_literal(id));
    let result = table
        .update()
        .only_if(predicate)
        .column("vector", vector_literal(&vector))
        .column("vector_source", format!("'{}'", VECTOR_SOURCE_DOCUMENT))
        .execute()
        .await
        .map_err(|error| format!("Update document vector error: {}", error))?;
    Ok(result.rows_updated > 0)
}

/// Apply a completed Gemma summary without replacing the raw document.
/// If `summary_vector` is Some, the embedding is swapped and
/// `vector_source=summary`; otherwise the document embedding is kept.
pub async fn apply_summary(
    root_dir: &Path,
    id: &str,
    summary: &str,
    _model_id: &str,
    _prompt_version: &str,
    summary_vector: Option<Vec<f32>>,
) -> Result<bool, String> {
    if summary.trim().is_empty() {
        return Err("summary must not be empty".to_string());
    }
    if let Some(ref v) = summary_vector {
        validate_vector(v).map_err(|error| format!("Invalid summary vector: {}", error))?;
    }
    let existing = get_memory_by_id(root_dir, id).await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    if summary_admission(&existing.memory_type, &existing.document) != SummaryAdmission::Eligible {
        // A stale caller must not turn an excluded raw event into a summary
        // result, even if an old projection happened to retain `pending`.
        return Ok(false);
    }
    if existing.summary_status.as_deref() == Some(SUMMARY_STATUS_COMPLETED) {
        // A completed edit is authoritative.  Duplicate retries are
        // successful no-ops and must preserve its text and vector.
        return Ok(true);
    }
    if existing.summary_status.as_deref() != Some(SUMMARY_STATUS_PENDING) {
        return Ok(false);
    }

    let repository = MemoryRepository::open(root_dir).await?;
    // Use the same batch boundary as Process all: Fact, optional summary
    // embedding, and terminal attempt status are journaled atomically.  The
    // helper then repairs the legacy projection as a replayable post-commit
    // step, so a projection failure cannot hide a durable semantic result.
    let result = apply_summary_backfill_batch_with_repository(
        &repository,
        root_dir,
        vec![SummaryBatchInput {
            entity_id: id.to_string(),
            summary: summary.to_string(),
            embedding: summary_vector,
            attempt_id: None,
            model_id: Some(SUMMARY_MODEL_ID.to_string()),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.to_string()),
        }],
        Vec::new(),
        Vec::new(),
    )
    .await?;
    Ok(result.persisted > 0)
}

async fn mark_summary_status(root_dir: &Path, id: &str, next_status: &str) -> Result<bool, String> {
    let existing = get_memory_by_id(root_dir, id).await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    if summary_admission(&existing.memory_type, &existing.document) != SummaryAdmission::Eligible {
        // Non-candidates and invalid candidate content stay raw-only.  In
        // particular, a late live caller must not write `skipped` for them.
        return Ok(false);
    }
    if !summary_status_transition_allowed(existing.summary_status.as_deref(), next_status) {
        return Ok(false);
    }
    if existing.summary_status.as_deref() == Some(next_status) {
        return Ok(true);
    }

    let repository = MemoryRepository::open(root_dir).await?;
    repository
        .append_summary_status(
            id,
            next_status,
            Some(SUMMARY_MODEL_ID),
            Some(SUMMARY_PROMPT_VERSION),
        )
        .await?;
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let predicate = format!(
        "id = '{}' AND summary_status = '{}'",
        escape_sql_literal(id),
        SUMMARY_STATUS_PENDING
    );
    let result = table
        .update()
        .only_if(predicate)
        .column("summary_status", format!("'{}'", next_status))
        .execute()
        .await
        .map_err(|error| format!("Mark summary status error: {}", error))?;
    if result.rows_updated > 0 {
        return Ok(true);
    }
    Ok(get_memory_by_id(root_dir, id)
        .await?
        .and_then(|row| row.summary_status)
        .as_deref()
        == Some(next_status))
}

/// Re-open a terminal summary attempt for an explicit UI retry.  Raw content
/// and its document embedding remain untouched; only the summary projection
/// is moved back to pending and the intent is journaled.
pub async fn retry_summary(root_dir: &Path, id: &str) -> Result<bool, String> {
    let existing = get_memory_by_id(root_dir, id).await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    if summary_admission(&existing.memory_type, &existing.document) != SummaryAdmission::Eligible {
        return Ok(false);
    }
    if existing.summary_status.as_deref() == Some(SUMMARY_STATUS_PENDING) {
        return Ok(true);
    }
    if !summary_status_transition_allowed(
        existing.summary_status.as_deref(),
        SUMMARY_STATUS_PENDING,
    ) {
        return Ok(false);
    }

    let repository = MemoryRepository::open(root_dir).await?;
    // The API may have already journaled the retry intent before asking the
    // session manager to update the compatibility projection.  Do not append
    // a second identical transition; otherwise one click creates two active
    // attempts and obscures the receipt's operation ID.
    let canonical_id = MemoryRepository::canonical_event_id(id);
    let journal_pending = repository
        .read_summary_statuses()?
        .get(&canonical_id)
        .is_some_and(|status| status.status == SUMMARY_STATUS_PENDING);
    if !journal_pending {
        let attempt_id = format!("retry-{}", uuid::Uuid::new_v4().simple());
        repository
            .append_summary_status_with_attempt(
                id,
                SUMMARY_STATUS_PENDING,
                Some(SUMMARY_MODEL_ID),
                Some(SUMMARY_PROMPT_VERSION),
                Some(attempt_id.as_str()),
            )
            .await?;
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let result = table
        .update()
        .only_if(format!(
            "id = '{}' AND summary_status = '{}'",
            escape_sql_literal(id),
            escape_sql_literal(existing.summary_status.as_deref().unwrap_or_default())
        ))
        .column("summary_status", format!("'{}'", SUMMARY_STATUS_PENDING))
        .execute()
        .await
        .map_err(|error| format!("Retry summary status error: {}", error))?;
    if result.rows_updated > 0 {
        return Ok(true);
    }
    Ok(get_memory_by_id(root_dir, id)
        .await?
        .and_then(|row| row.summary_status)
        .as_deref()
        == Some(SUMMARY_STATUS_PENDING))
}

/// Queue a raw memory for the explicit all-memory semantic backfill action.
/// Legacy/null status rows are recoverable, but the same fixed event/content
/// admission predicate as live capture still applies. Completed summaries with
/// a durable Fact and durable terminal outcomes are authoritative and are left
/// untouched; pending rows remain recoverable after a crash.
pub async fn queue_summary_backfill(root_dir: &Path, id: &str) -> Result<bool, String> {
    let existing = get_memory_by_id(root_dir, id).await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    if summary_admission(&existing.memory_type, &existing.document) != SummaryAdmission::Eligible {
        return Ok(false);
    }
    if matches!(
        existing.summary_status.as_deref(),
        Some(SUMMARY_STATUS_COMPLETED)
            | Some(SUMMARY_STATUS_PENDING)
            | Some(SUMMARY_STATUS_DELETED)
    ) {
        return Ok(false);
    }

    let previous_status = existing.summary_status.clone();
    let repository = MemoryRepository::open(root_dir).await?;
    let canonical_id = MemoryRepository::canonical_event_id(id);
    let journal_status = repository
        .read_summary_statuses()?
        .get(&canonical_id)
        .cloned();
    if journal_status
        .as_ref()
        .and_then(|status| status.reason.as_deref())
        == Some("policy_excluded")
    {
        return Ok(false);
    }
    if journal_status
        .as_ref()
        .is_some_and(|status| status.status == SUMMARY_STATUS_DELETED)
    {
        // A Fact-delete tombstone is authoritative even when an old
        // compatibility projection row still exists.  Explicit queueing must
        // not resurrect a summary the user deleted.
        return Ok(false);
    }
    let journal_pending = journal_status
        .as_ref()
        .is_some_and(|status| status.status == SUMMARY_STATUS_PENDING);
    if !journal_pending {
        let attempt_id = if previous_status.is_some() {
            Some(format!("retry-{}", uuid::Uuid::new_v4().simple()))
        } else {
            None
        };
        if let Some(attempt_id) = attempt_id {
            repository
                .append_summary_status_with_attempt(
                    id,
                    SUMMARY_STATUS_PENDING,
                    Some(SUMMARY_MODEL_ID),
                    Some(SUMMARY_PROMPT_VERSION),
                    Some(attempt_id.as_str()),
                )
                .await?;
        } else {
            repository
                .append_summary_status(
                    id,
                    SUMMARY_STATUS_PENDING,
                    Some(SUMMARY_MODEL_ID),
                    Some(SUMMARY_PROMPT_VERSION),
                )
                .await?;
        }
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let id_predicate = format!("id = '{}'", escape_sql_literal(id));
    let status_predicate = match previous_status.as_deref() {
        Some(status) => format!("summary_status = '{}'", escape_sql_literal(status)),
        None => "summary_status IS NULL".to_string(),
    };
    let result = table
        .update()
        .only_if(format!("{} AND {}", id_predicate, status_predicate))
        .column("summary_status", format!("'{}'", SUMMARY_STATUS_PENDING))
        .execute()
        .await
        .map_err(|error| format!("Queue summary backfill error: {}", error))?;
    if result.rows_updated > 0 {
        return Ok(true);
    }
    Ok(get_memory_by_id(root_dir, id)
        .await?
        .and_then(|row| row.summary_status)
        .as_deref()
        == Some(SUMMARY_STATUS_PENDING))
}

/// Return whether a projection row is eligible for a normal explicit
/// all-memory pass.  A completed summary is authoritative, while a pending
/// marker is only an in-flight hint and must be recoverable after a crash.
pub fn is_summary_backfill_candidate(row: &StoredMemory) -> bool {
    if summary_admission(&row.memory_type, &row.document) != SummaryAdmission::Eligible {
        return false;
    }
    if matches!(
        row.summary_status.as_deref(),
        Some(SUMMARY_STATUS_SKIPPED)
            | Some(SUMMARY_STATUS_FALLBACK)
            | Some(SUMMARY_STATUS_ERROR)
            | Some(SUMMARY_STATUS_DELETED)
    ) {
        return false;
    }
    !row.summary.as_deref().is_some_and(|summary| {
        !summary.trim().is_empty() && row.summary_status.as_deref() != Some(SUMMARY_STATUS_PENDING)
    })
}

/// Queue all eligible rows with a single journal transaction and a single
/// LanceDB update.  The old implementation performed both operations once
/// per row, which made a large journal spend minutes holding its file lock and
/// caused concurrent callers to hit `journal lock timeout`.
pub async fn queue_summary_backfill_batch(
    root_dir: &Path,
    rows: &[StoredMemory],
) -> Result<usize, String> {
    let repository = MemoryRepository::open(root_dir).await?;
    queue_summary_backfill_batch_with_repository(&repository, root_dir, rows).await
}

/// Repository-scoped variant of [`queue_summary_backfill_batch`] so a long
/// all-memory backfill can reuse one opened store instead of recovering the
/// entire journal again for the queueing step.
pub async fn queue_summary_backfill_batch_with_repository(
    repository: &MemoryRepository,
    root_dir: &Path,
    rows: &[StoredMemory],
) -> Result<usize, String> {
    Ok(
        queue_summary_backfill_batch_ids_with_repository(repository, root_dir, rows)
            .await?
            .len(),
    )
}

/// Queue the rows admitted by the current durable/projection snapshot and
/// return their IDs.  Returning the admitted set prevents a caller that took
/// an earlier snapshot from processing a row that completed or was deleted
/// while the queue update was in flight.
pub async fn queue_summary_backfill_batch_ids_with_repository(
    repository: &MemoryRepository,
    root_dir: &Path,
    rows: &[StoredMemory],
) -> Result<Vec<String>, String> {
    let facts = repository.read_facts().await?;
    let raw_events = repository.read_raw_events().await?;
    let raw_events_by_id = raw_events
        .iter()
        .map(|event| (event.event_id(), event))
        .collect::<std::collections::HashMap<_, _>>();
    let durable_summary_event_ids = facts
        .iter()
        .filter(|fact| fact.key().starts_with("summary-"))
        .map(|fact| fact.source_event_id().to_string())
        .collect::<HashSet<_>>();
    let durable_statuses = repository.read_summary_statuses()?;
    // A Fact is normally terminal and therefore excludes a row from a fresh
    // Process all pass.  Legacy/versionless automatic summaries are the
    // deliberate exception: they are the repair source for prompt-v2 and
    // must be admitted even though the old Fact is still present.  Protected
    // confirmed/edited Facts never enter this set and remain untouched.
    let repairable_summary_event_ids = facts
        .iter()
        .filter(|fact| fact.key().starts_with("summary-"))
        .filter(|fact| {
            summary_fact_is_repairable(
                fact,
                durable_statuses.get(&fact.source_event_id().to_string()),
                raw_events_by_id.get(&fact.source_event_id()).copied(),
            )
        })
        .map(|fact| fact.source_event_id().to_string())
        .collect::<HashSet<_>>();
    let deleted_summary_event_ids = durable_statuses
        .iter()
        .filter_map(|(event_id, status)| {
            (status.status == SUMMARY_STATUS_DELETED).then_some(event_id.clone())
        })
        .collect::<HashSet<_>>();
    let candidates = rows
        .iter()
        .filter(|row| {
            let event_id = MemoryRepository::canonical_event_id(&row.id);
            (!durable_summary_event_ids.contains(&event_id)
                || repairable_summary_event_ids.contains(&event_id))
                && !deleted_summary_event_ids.contains(&event_id)
                && summary_backfill_snapshot_candidate(
                    row,
                    durable_statuses
                        .get(&event_id)
                        .map(|status| status.status.as_str()),
                    durable_statuses
                        .get(&event_id)
                        .and_then(|status| status.prompt_version.as_deref()),
                )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let statuses = candidates
        .iter()
        .map(|row| SummaryStatusBatchInput {
            entity_id: row.id.clone(),
            status: SUMMARY_STATUS_PENDING.to_string(),
            model_id: Some(SUMMARY_MODEL_ID.to_string()),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.to_string()),
            attempt_id: Some(format!("attempt-{}", uuid::Uuid::new_v4().simple())),
            reason: None,
        })
        .collect();
    repository
        .append_summary_backfill_batch(Vec::new(), statuses)
        .await?;

    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let mut admitted = Vec::new();
    for chunk in candidates.chunks(SUMMARY_QUEUE_PROJECTION_CHUNK_SIZE) {
        // Guard every row with the projection state observed above. This
        // keeps a concurrent completion/deletion from being rewritten to
        // pending, while the bounded chunk keeps DataFusion's recursive
        // expression parser off the stack-overflow path.
        let guards = chunk
            .iter()
            .map(|row| {
                let status_predicate = match row.summary_status.as_deref() {
                    Some(status) => format!("summary_status = '{}'", escape_sql_literal(status)),
                    None => "summary_status IS NULL".to_string(),
                };
                format!(
                    "(id = '{}' AND {})",
                    escape_sql_literal(&row.id),
                    status_predicate
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let result = table
            .update()
            .only_if(guards)
            .column("summary_status", format!("'{}'", SUMMARY_STATUS_PENDING))
            .execute()
            .await
            .map_err(|error| format!("Queue summary backfill batch error: {}", error))?;
        if result.rows_updated as usize == chunk.len() {
            admitted.extend(chunk.iter().map(|row| row.id.clone()));
            continue;
        }

        // Lance reports only a count. Re-read this bounded chunk when a
        // concurrent writer changed part of the guarded predicate so the
        // caller processes exactly the rows that are still pending.
        let candidate_ids = chunk.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let mut stream = table
            .query()
            .only_if(ids_predicate(&candidate_ids).expect("chunk is non-empty"))
            .execute()
            .await
            .map_err(|error| format!("Queue summary backfill verification error: {}", error))?;
        while let Some(batch) = stream.try_next().await.map_err(|error| {
            format!(
                "Queue summary backfill verification stream error: {}",
                error
            )
        })? {
            for row in 0..batch.num_rows() {
                let stored = batch_row_to_stored(&batch, row)?;
                if stored.summary_status.as_deref() == Some(SUMMARY_STATUS_PENDING) {
                    admitted.push(stored.id);
                }
            }
        }
    }
    admitted.sort();
    admitted.dedup();
    Ok(admitted)
}

fn summary_backfill_snapshot_candidate(
    row: &StoredMemory,
    durable_status: Option<&str>,
    durable_prompt_version: Option<&str>,
) -> bool {
    if summary_admission(&row.memory_type, &row.document) != SummaryAdmission::Eligible {
        return false;
    }
    // A durable Fact is checked by the caller.  A projection-only completed
    // marker is intentionally recoverable: its Fact may have been lost before
    // the journal transaction committed.
    let status = durable_status.or(row.summary_status.as_deref());
    // A changed prompt/admission contract makes prior terminal decisions
    // stale.  Reopen them during an explicit Process-all pass so a new
    // contract can repair a previous false decline or malformed summary.
    let stale_prompt = durable_prompt_version
        .or(row.summary_prompt_version.as_deref())
        .is_some_and(|version| version != SUMMARY_PROMPT_VERSION);
    if stale_prompt {
        return !matches!(status, Some(SUMMARY_STATUS_DELETED));
    }
    matches!(
        status,
        None | Some(SUMMARY_STATUS_PENDING)
            | Some(SUMMARY_STATUS_LEGACY)
            | Some(SUMMARY_STATUS_COMPLETED)
    )
}

fn ids_predicate(ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let values = ids
        .iter()
        .map(|id| format!("'{}'", escape_sql_literal(id)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("id IN ({})", values))
}

/// Commit all terminal backfill results in one memory-v2 journal transaction
/// and then update the compatibility projection.  Summary text is row-specific
/// and therefore still receives one guarded projection update per completed
/// row, but no update reopens or recovers the journal.
pub async fn apply_summary_backfill_batch(
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    skipped_ids: Vec<String>,
    fallback_ids: Vec<String>,
) -> Result<SummaryBackfillApplyResult, String> {
    let repository = MemoryRepository::open(root_dir).await?;
    let fallback_reasons = fallback_ids
        .into_iter()
        .map(|id| (id, "inference_failed".to_string()))
        .collect();
    apply_summary_backfill_batch_with_reasons_with_repository(
        &repository,
        root_dir,
        summaries,
        skipped_ids,
        fallback_reasons,
    )
    .await
}

/// Compatibility adapter for backfill callers that can preserve the
/// row-specific failure reason but do not reuse an already-open repository.
/// The old ID-only adapter above intentionally keeps `inference_failed` as its
/// fallback for callers that have no reason information.
pub async fn apply_summary_backfill_batch_with_reasons(
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    skipped_ids: Vec<String>,
    fallback_reasons: Vec<(String, String)>,
) -> Result<SummaryBackfillApplyResult, String> {
    let repository = MemoryRepository::open(root_dir).await?;
    apply_summary_backfill_batch_with_reasons_with_repository(
        &repository,
        root_dir,
        summaries,
        skipped_ids,
        fallback_reasons,
    )
    .await
}

/// Repository-scoped variant of [`apply_summary_backfill_batch`] so chunked
/// persistence during a backfill reuses the already recovered journal instead
/// of reopening it once per chunk.
pub async fn apply_summary_backfill_batch_with_repository(
    repository: &MemoryRepository,
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    skipped_ids: Vec<String>,
    fallback_ids: Vec<String>,
) -> Result<SummaryBackfillApplyResult, String> {
    let fallback_reasons = fallback_ids
        .into_iter()
        .map(|id| (id, "inference_failed".to_string()))
        .collect();
    apply_summary_backfill_batch_with_reasons_with_repository(
        repository,
        root_dir,
        summaries,
        skipped_ids,
        fallback_reasons,
    )
    .await
}

/// Repository-scoped reason-preserving variant used by the asynchronous
/// backfill worker.  Each pair is `(entity_id, machine_readable_reason)`;
/// status/model/prompt metadata is supplied by this adapter and the repository
/// supplies a deterministic attempt identity when the caller has none.
pub async fn apply_summary_backfill_batch_with_reasons_with_repository(
    repository: &MemoryRepository,
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    skipped_ids: Vec<String>,
    fallback_reasons: Vec<(String, String)>,
) -> Result<SummaryBackfillApplyResult, String> {
    let mut statuses = Vec::with_capacity(skipped_ids.len() + fallback_reasons.len());
    statuses.extend(skipped_ids.into_iter().map(|id| SummaryStatusBatchInput {
        entity_id: id,
        status: SUMMARY_STATUS_SKIPPED.to_string(),
        model_id: Some(SUMMARY_MODEL_ID.to_string()),
        prompt_version: Some(SUMMARY_PROMPT_VERSION.to_string()),
        attempt_id: None,
        reason: Some("model_declined".to_string()),
    }));
    statuses.extend(fallback_reasons.into_iter().map(|(entity_id, reason)| {
        SummaryStatusBatchInput {
            entity_id,
            status: SUMMARY_STATUS_FALLBACK.to_string(),
            model_id: Some(SUMMARY_MODEL_ID.to_string()),
            prompt_version: Some(SUMMARY_PROMPT_VERSION.to_string()),
            attempt_id: None,
            reason: Some(reason),
        }
    }));
    apply_summary_backfill_batch_with_repository_and_statuses(
        repository, root_dir, summaries, statuses,
    )
    .await
}

/// Apply a batch whose terminal statuses already carry their complete
/// metadata.  This additive API is useful for callers that need to preserve a
/// caller-generated attempt ID or an `error` status in addition to fallback.
pub async fn apply_summary_backfill_batch_with_statuses(
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    statuses: Vec<SummaryStatusBatchInput>,
) -> Result<SummaryBackfillApplyResult, String> {
    // This entry point receives the complete terminal status set directly,
    // rather than the split `skipped_ids`/`fallback_reasons` arguments used by
    // the live backfill worker.  Report those caller-supplied terminal rows in
    // the result as well; the worker-facing adapter deliberately leaves them
    // out because it has already counted each inference outcome before the
    // durable commit.
    let supplied_failed = statuses
        .iter()
        .filter(|status| {
            matches!(
                status.status.as_str(),
                SUMMARY_STATUS_FALLBACK | SUMMARY_STATUS_ERROR
            )
        })
        .count();
    let supplied_skipped = statuses
        .iter()
        .filter(|status| status.status == SUMMARY_STATUS_SKIPPED)
        .count();
    let repository = MemoryRepository::open(root_dir).await?;
    let mut result = apply_summary_backfill_batch_with_repository_and_statuses(
        &repository,
        root_dir,
        summaries,
        statuses,
    )
    .await?;
    result.terminal_failed = result.terminal_failed.saturating_add(supplied_failed);
    result.terminal_skipped = result.terminal_skipped.saturating_add(supplied_skipped);
    Ok(result)
}

async fn apply_summary_backfill_batch_with_repository_and_statuses(
    repository: &MemoryRepository,
    root_dir: &Path,
    summaries: Vec<SummaryBatchInput>,
    statuses: Vec<SummaryStatusBatchInput>,
) -> Result<SummaryBackfillApplyResult, String> {
    if summaries.is_empty() && statuses.is_empty() {
        return Ok(SummaryBackfillApplyResult::default());
    }
    let requested_skipped: HashSet<String> = statuses
        .iter()
        .filter(|status| status.status == SUMMARY_STATUS_SKIPPED)
        .map(|status| status.entity_id.clone())
        .collect();
    let requested_fallback: HashSet<String> = statuses
        .iter()
        .filter(|status| {
            matches!(
                status.status.as_str(),
                SUMMARY_STATUS_FALLBACK | SUMMARY_STATUS_ERROR
            )
        })
        .map(|status| status.entity_id.clone())
        .collect();
    let outcome = repository
        .append_summary_backfill_batch(summaries.clone(), statuses)
        .await?;

    let persisted = outcome.completed_ids.len();
    let terminal_failed = outcome
        .terminal_statuses
        .iter()
        .filter(|status| {
            matches!(
                status.status.as_str(),
                SUMMARY_STATUS_FALLBACK | SUMMARY_STATUS_ERROR
            ) && !requested_fallback.contains(&status.entity_id)
        })
        .count();
    let terminal_skipped = outcome
        .terminal_statuses
        .iter()
        .filter(|status| {
            status.status == SUMMARY_STATUS_SKIPPED
                && !requested_skipped.contains(&status.entity_id)
        })
        .count();
    let policy_exclusion_reasons = [
        "policy_excluded",
        "empty_summary",
        "invalid_embedding",
        "fact_validation_failed",
    ];
    let exclusions: Vec<SummaryExclusionDetail> = outcome
        .terminal_statuses
        .iter()
        .filter(|status| {
            status
                .reason
                .as_deref()
                .map(|reason| policy_exclusion_reasons.contains(&reason))
                .unwrap_or(false)
        })
        .map(|status| SummaryExclusionDetail {
            entity_id: status.entity_id.clone(),
            reason: format!(
                "{} ({})",
                match status.reason.as_deref() {
                    Some("policy_excluded") => {
                        "プライバシーポリシーに抵触したため永続化対象外"
                    }
                    Some("empty_summary") => "要約が空だったため永続化対象外",
                    Some("invalid_embedding") => {
                        "埋め込みベクトルが不正なため永続化対象外"
                    }
                    _ => "事実の検証に失敗したため永続化対象外",
                },
                status.reason.as_deref().unwrap_or("")
            ),
        })
        .collect();
    let policy_excluded = exclusions
        .iter()
        .filter(|exclusion| exclusion.reason.contains("policy_excluded"))
        .count();
    let completed_ids: HashSet<String> = outcome.completed_ids.iter().cloned().collect();

    let projection_error = async {
        // The journal is authoritative for both the terminal state and its
        // metadata.  Reading it after the commit also filters out stale
        // statuses that were rejected by the repository's status-first/CAS
        // rules before this compatibility projection is touched.
        let durable_statuses = repository.read_summary_statuses()?;
        let db = get_or_create_db(root_dir).await?;
        let table = get_or_create_memories_table(&db, root_dir).await?;
        let mut updated = 0usize;
        for input in summaries
            .into_iter()
            .filter(|input| completed_ids.contains(&input.entity_id))
        {
            let Some(durable) = durable_statuses
                .get(&MemoryRepository::canonical_event_id(&input.entity_id))
                .filter(|status| status.status == SUMMARY_STATUS_COMPLETED)
            else {
                continue;
            };
            let mut builder = table.update().only_if(format!(
                "id = '{}' AND summary_status = '{}'",
                escape_sql_literal(&input.entity_id),
                SUMMARY_STATUS_PENDING
            ));
            builder = builder
                .column(
                    "summary",
                    format!("'{}'", escape_sql_literal(&input.summary)),
                )
                .column("summary_status", format!("'{}'", SUMMARY_STATUS_COMPLETED));
            if let Some(model_id) = durable.model_id.as_deref() {
                builder = builder.column(
                    "summary_model",
                    format!("'{}'", escape_sql_literal(model_id)),
                );
            }
            if let Some(prompt_version) = durable.prompt_version.as_deref() {
                builder = builder.column(
                    "summary_prompt_version",
                    format!("'{}'", escape_sql_literal(prompt_version)),
                );
            }
            if let Some(vector) = input.embedding {
                builder = builder
                    .column("vector_source", format!("'{}'", VECTOR_SOURCE_SUMMARY))
                    .column("vector", vector_literal(&vector));
            }
            let result = builder
                .execute()
                .await
                .map_err(|error| format!("Apply summary backfill row error: {}", error))?;
            updated = updated.saturating_add(result.rows_updated as usize);
        }

        // Group by the complete projection metadata, not just status.  A
        // single backfill batch may contain attempts from different models or
        // prompt versions, and grouping only by status would silently erase
        // those caller-supplied values on neighboring rows.
        let mut status_ids: HashMap<(String, Option<String>, Option<String>), Vec<String>> =
            HashMap::new();
        let mut seen_status_entities = HashSet::new();
        for status in &outcome.terminal_statuses {
            if status.status == SUMMARY_STATUS_PENDING
                || !seen_status_entities.insert(status.entity_id.clone())
            {
                continue;
            }
            let Some(durable) =
                durable_statuses.get(&MemoryRepository::canonical_event_id(&status.entity_id))
            else {
                continue;
            };
            if durable.status == SUMMARY_STATUS_PENDING {
                continue;
            }
            status_ids
                .entry((
                    durable.status.clone(),
                    durable.model_id.clone(),
                    durable.prompt_version.clone(),
                ))
                .or_default()
                .push(status.entity_id.clone());
        }
        for ((status, model_id, prompt_version), ids) in status_ids {
            if let Some(id_list) = ids_predicate(&ids) {
                let mut builder = table.update().only_if(format!(
                    "{} AND summary_status = '{}'",
                    id_list, SUMMARY_STATUS_PENDING
                ));
                builder = builder.column(
                    "summary_status",
                    format!("'{}'", escape_sql_literal(&status)),
                );
                if let Some(model_id) = model_id {
                    builder = builder.column(
                        "summary_model",
                        format!("'{}'", escape_sql_literal(&model_id)),
                    );
                }
                if let Some(prompt_version) = prompt_version {
                    builder = builder.column(
                        "summary_prompt_version",
                        format!("'{}'", escape_sql_literal(&prompt_version)),
                    );
                }
                let result = builder
                    .execute()
                    .await
                    .map_err(|error| format!("Apply summary backfill status error: {}", error))?;
                updated = updated.saturating_add(result.rows_updated as usize);
            }
        }
        let _ = updated;
        Ok::<(), String>(())
    }
    .await
    .err();
    Ok(SummaryBackfillApplyResult {
        persisted,
        policy_excluded,
        exclusions,
        terminal_failed,
        terminal_skipped,
        projection_error,
    })
}

/// Mark a row as skipped (Gemma judged it non-persistent). The row is kept;
/// search excludes it. Never deletes.
pub async fn mark_summary_skipped(root_dir: &Path, id: &str) -> Result<bool, String> {
    mark_summary_status(root_dir, id, SUMMARY_STATUS_SKIPPED).await
}

/// Mark a row as fallback (summary failed; raw text stays searchable).
/// The row is kept; never deletes.
pub async fn mark_summary_fallback(root_dir: &Path, id: &str) -> Result<bool, String> {
    mark_summary_status(root_dir, id, SUMMARY_STATUS_FALLBACK).await
}

/// Mark a row as error. The row is kept; never deletes.
pub async fn mark_summary_error(root_dir: &Path, id: &str) -> Result<bool, String> {
    mark_summary_status(root_dir, id, SUMMARY_STATUS_ERROR).await
}

/// Startup reprocessing source: only pending rows, idempotent per event ID.
pub async fn list_pending_summaries(
    root_dir: &Path,
    limit: usize,
) -> Result<Vec<StoredMemory>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let mut stream = table
        .query()
        .only_if(format!("summary_status = '{}'", SUMMARY_STATUS_PENDING))
        .execute()
        .await
        .map_err(|e| format!("Pending query error: {}", e))?;
    let mut rows = Vec::new();
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| format!("Stream batch error: {}", e))?
    {
        for r in 0..batch.num_rows() {
            let row = batch_row_to_stored(&batch, r)?;
            if summary_admission(&row.memory_type, &row.document) != SummaryAdmission::Eligible {
                continue;
            }
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
        if rows.len() >= limit {
            break;
        }
    }
    // Deterministic order for restart reprocessing and tests.
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.truncate(limit);
    Ok(rows)
}

/// Legacy helper kept for compatibility. Prefer `apply_summary`, which stores
/// the summary beside the raw document instead of replacing it.
pub async fn update_memory_document(
    _root_dir: &Path,
    _id: &str,
    _document: &str,
) -> Result<bool, String> {
    // Raw events are immutable in memory-v2.  Keeping this compatibility
    // symbol as an explicit failure prevents older callers from silently
    // mutating only the projection and diverging from the journaled store.
    Err("memory documents are immutable; append a new event instead".to_string())
}

/// Keep the original event text beside the database before a curated summary
/// replaces the searchable document. The identifier is validated because this
/// path is user-writable in portable mode.
pub fn preserve_raw_document(root_dir: &Path, id: &str, document: &str) -> Result<(), String> {
    validate_root_dir(root_dir)?;
    validate_memory_id(id)?;
    let directory = root_dir.join("data").join("memory_raw");
    validate_path_inside_root(root_dir, &directory, "Raw memory backup directory")?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Create raw memory backup directory: {}", error))?;
    let target = directory.join(format!("{}.txt", id));
    validate_path_inside_root(root_dir, &target, "Raw memory backup target")?;
    let temporary = directory.join(format!(
        ".{}.tmp-{}-{}",
        id,
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    validate_path_inside_root(root_dir, &temporary, "Raw memory backup temporary file")?;
    std::fs::write(&temporary, document)
        .map_err(|error| format!("Write raw memory backup: {}", error))?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Install raw memory backup: {}", error));
    }
    Ok(())
}

pub async fn delete_memory(root_dir: &Path, id: &str) -> Result<bool, String> {
    if get_memory_by_id(root_dir, id).await?.is_none() {
        return Ok(false);
    }
    // Journal the redaction and remove it from the authoritative stores before
    // updating the compatibility projection.  Recovery can therefore replay a
    // deletion that was interrupted between the two materializations.
    let repository = MemoryRepository::open(root_dir).await?;
    repository.append_redaction(id).await?;
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let predicate = format!("id = '{}'", escape_sql_literal(id));
    table
        .delete(&predicate)
        .await
        .map_err(|e| format!("Delete error: {}", e))?;
    Ok(true)
}

pub async fn delete_memories_bulk(root_dir: &Path, ids: &[String]) -> Result<usize, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db, root_dir).await?;
    let existing = existing_ids(&table, ids).await?;
    if existing.is_empty() {
        return Ok(0);
    }
    let repository = MemoryRepository::open(root_dir).await?;
    for id in ids.iter().filter(|id| existing.contains(*id)) {
        repository.append_redaction(id).await?;
    }
    let escaped_ids: Vec<String> = ids
        .iter()
        .filter(|id| existing.contains(*id))
        .map(|id| format!("'{}'", escape_sql_literal(id)))
        .collect();
    let predicate = format!("id IN ({})", escaped_ids.join(", "));
    table
        .delete(&predicate)
        .await
        .map_err(|e| format!("Delete bulk error: {}", e))?;
    Ok(existing.len())
}

/// 再帰的ディレクトリコピー
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    let src_metadata = std::fs::symlink_metadata(src)?;
    if metadata_is_link_or_reparse(&src_metadata) || !src_metadata.is_dir() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "source contains a link, reparse point, or non-directory",
        ));
    }
    if let Ok(dst_metadata) = std::fs::symlink_metadata(dst) {
        if metadata_is_link_or_reparse(&dst_metadata) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "destination contains a link or reparse point",
            ));
        }
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let source_path = entry.path();
        let source_metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata_is_link_or_reparse(&source_metadata) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "source contains a link or reparse point",
            ));
        }
        let destination_path = dst.join(entry.file_name());
        if let Ok(destination_metadata) = std::fs::symlink_metadata(&destination_path) {
            if metadata_is_link_or_reparse(&destination_metadata) {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidInput,
                    "destination contains a link or reparse point",
                ));
            }
        }
        if ty.is_dir() {
            copy_dir_all(&source_path, &destination_path)?;
        } else {
            std::fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

/// LanceDB のタイムスタンプ付きスナップショットバックアップ作成（世代管理: 5世代保持）
pub fn backup_lance_db(root_dir: &Path) -> Result<String, String> {
    validate_root_dir(root_dir)?;
    let db_src = root_dir.join(LANCE_DB_DIR);
    validate_path_inside_root(root_dir, &db_src, "LanceDB directory")?;
    if !db_src.exists() {
        return Err("LanceDB directory does not exist".to_string());
    }
    validate_copy_tree(&db_src, "LanceDB directory")?;

    let backups_base = root_dir.join("data/lancedb_backups");
    validate_path_inside_root(root_dir, &backups_base, "LanceDB backup directory")?;
    let _ = std::fs::create_dir_all(&backups_base);
    validate_path_inside_root(root_dir, &backups_base, "LanceDB backup directory")?;

    let now_str = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    // Include a random suffix so backups created during the same second never
    // select the same directory. Reserve the directory atomically as an
    // additional guard against an unlikely UUID collision or concurrent call.
    let (target_dir_name, target_dir) = loop {
        let target_dir_name = format!("backup_{}_{}", now_str, uuid::Uuid::new_v4().simple());
        validate_safe_backup_name(&target_dir_name)?;
        let target_dir = backups_base.join(&target_dir_name);
        validate_path_inside_root(root_dir, &target_dir, "LanceDB backup target")?;

        match std::fs::create_dir(&target_dir) {
            Ok(()) => break (target_dir_name, target_dir),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("Failed to create LanceDB backup target: {}", error));
            }
        }
    };

    copy_dir_all(&db_src, &target_dir)
        .map_err(|e| format!("Failed to copy LanceDB snapshot: {}", e))?;

    // The compatibility LanceDB tree is not the authoritative memory-v2
    // state. Keep its journal, manifest, and materialized stores in the same
    // snapshot so restore cannot silently roll back only one projection.
    let memory_v2_src = root_dir.join("data").join("memory_v2");
    validate_path_inside_root(root_dir, &memory_v2_src, "Memory-v2 directory")?;
    if memory_v2_src.exists() {
        validate_copy_tree(&memory_v2_src, "Memory-v2 directory")?;
        let memory_v2_target = target_dir.join("memory_v2");
        validate_path_inside_root(root_dir, &memory_v2_target, "Memory-v2 backup target")?;
        copy_dir_all(&memory_v2_src, &memory_v2_target)
            .map_err(|e| format!("Failed to copy memory-v2 snapshot: {}", e))?;
    }

    // 世代管理 (最新 5 件を残して古いものを削除)
    if let Ok(entries) = std::fs::read_dir(&backups_base) {
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to inspect LanceDB backups: {}", e))?;
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|e| format!("Failed to inspect LanceDB backup: {}", e))?;
            if metadata_is_link_or_reparse(&metadata) {
                return Err(
                    "LanceDB backup directory must not contain links or reparse points".to_string(),
                );
            }
            if metadata.is_dir() {
                let file_name = entry.file_name();
                let name = file_name
                    .to_str()
                    .ok_or_else(|| "LanceDB backup name is not valid UTF-8".to_string())?;
                validate_safe_backup_name(name)?;
                dirs.push(entry);
            }
        }
        dirs.sort_by_key(|e| e.file_name());

        if dirs.len() > 5 {
            let to_remove = dirs.len() - 5;
            for d in dirs.iter().take(to_remove) {
                let _ = std::fs::remove_dir_all(d.path());
            }
        }
    }

    Ok(target_dir_name)
}

/// バックアップ一覧取得
pub fn list_lance_backups(root_dir: &Path) -> Result<Vec<String>, String> {
    validate_root_dir(root_dir)?;
    let backups_base = root_dir.join("data/lancedb_backups");
    validate_path_inside_root(root_dir, &backups_base, "LanceDB backup directory")?;
    if !backups_base.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    let entries = std::fs::read_dir(&backups_base)
        .map_err(|e| format!("Failed to inspect LanceDB backups: {}", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to inspect LanceDB backups: {}", e))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|e| format!("Failed to inspect LanceDB backup: {}", e))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(
                "LanceDB backup directory must not contain links or reparse points".to_string(),
            );
        }
        if metadata.is_dir() {
            let file_name = entry.file_name();
            let name = file_name
                .to_str()
                .ok_or_else(|| "LanceDB backup name is not valid UTF-8".to_string())?;
            validate_safe_backup_name(name)?;
            let backup_dir = backups_base.join(name);
            validate_path_inside_root(root_dir, &backup_dir, "LanceDB backup")?;
            names.push(name.to_string());
        }
    }
    names.sort();
    names.reverse(); // 最新順
    Ok(names)
}

/// 指定バックアップから LanceDB を復元
pub fn restore_lance_backup(root_dir: &Path, backup_name: &str) -> Result<(), String> {
    validate_root_dir(root_dir)?;
    let clean_name = backup_name.trim();
    validate_safe_backup_name(clean_name)?;

    let backups_base = root_dir.join("data/lancedb_backups");
    validate_path_inside_root(root_dir, &backups_base, "LanceDB backup directory")?;
    let backup_dir = backups_base.join(clean_name);
    validate_path_inside_root(root_dir, &backup_dir, "LanceDB backup")?;
    if !backup_dir.exists() {
        return Err(format!("Backup folder '{}' not found", clean_name));
    }
    validate_copy_tree(&backup_dir, "LanceDB backup")?;

    let memory_v2_backup = backup_dir.join("memory_v2");
    if !memory_v2_backup.exists() {
        return Err("Backup does not contain the memory-v2 journal/manifest snapshot".to_string());
    }
    validate_path_inside_root(root_dir, &memory_v2_backup, "Memory-v2 backup")?;
    validate_copy_tree(&memory_v2_backup, "Memory-v2 backup")?;

    let db_dst = root_dir.join(LANCE_DB_DIR);
    validate_path_inside_root(root_dir, &db_dst, "LanceDB directory")?;
    if db_dst.exists() {
        std::fs::remove_dir_all(&db_dst)
            .map_err(|e| format!("Failed to clear current LanceDB dir: {}", e))?;
    }

    copy_dir_all(&backup_dir, &db_dst).map_err(|e| format!("Failed to restore backup: {}", e))?;
    // The backup container keeps memory-v2 beside the compatibility files;
    // remove that container entry from the LanceDB destination before
    // installing the authoritative snapshot at its real location.
    let embedded_memory_v2 = db_dst.join("memory_v2");
    validate_path_inside_root(root_dir, &embedded_memory_v2, "Embedded memory-v2 backup")?;
    if embedded_memory_v2.exists() {
        std::fs::remove_dir_all(&embedded_memory_v2)
            .map_err(|e| format!("Failed to clear embedded memory-v2 backup: {}", e))?;
    }

    // `memory_v2` is stored beside (not inside) the LanceDB tree. Remove the
    // previous authoritative snapshot only after the backup tree has passed
    // all validation, then install the validated snapshot.
    let memory_v2_dst = root_dir.join("data").join("memory_v2");
    validate_path_inside_root(root_dir, &memory_v2_dst, "Memory-v2 directory")?;
    if memory_v2_dst.exists() {
        std::fs::remove_dir_all(&memory_v2_dst)
            .map_err(|e| format!("Failed to clear current memory-v2 dir: {}", e))?;
    }
    copy_dir_all(&memory_v2_backup, &memory_v2_dst)
        .map_err(|e| format!("Failed to restore memory-v2 snapshot: {}", e))?;

    Ok(())
}

//-----------------------------------------------------------------------------
// Tests (TDD)
//-----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::RecordBatchReader;
    use std::path::PathBuf;

    #[test]
    fn migration_progress_is_bounded_and_reports_terminal_state() {
        let progress = MemoryMigrationProgress::default();
        assert_eq!(progress.snapshot().status, "idle");

        progress.begin(10);
        progress.update(4);
        let running = progress.snapshot();
        assert_eq!(running.status, "running");
        assert_eq!(running.processed, 4);
        assert_eq!(running.total, Some(10));

        progress.update(99);
        assert_eq!(progress.snapshot().processed, 10);

        progress.complete(10);
        let completed = progress.snapshot();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.processed, completed.total.unwrap());

        progress.fail("test failure");
        let failed = progress.snapshot();
        assert_eq!(failed.status, "error");
        assert_eq!(failed.error.as_deref(), Some("test failure"));
    }

    fn unique_test_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ga-lance-memory-{}-{}-{}",
            name,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create test root");
        dir
    }

    fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    fn sample_vector(seed: f32) -> Vec<f32> {
        (0..VECTOR_DIM)
            .map(|i| ((i as f32) + seed) * 0.001)
            .collect()
    }

    fn vector_array_with_values(values: &[f32]) -> Arc<FixedSizeListArray> {
        let flat = Float32Array::from(values.to_vec());
        Arc::new(FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, false)),
            VECTOR_DIM,
            Arc::new(flat),
            None,
        ))
    }

    fn item(id: &str, memory_type: &str, document: &str) -> MemoryItem {
        MemoryItem {
            id: id.to_string(),
            document: document.to_string(),
            memory_type: memory_type.to_string(),
            source: "User".to_string(),
            timestamp: "2026-09-02T12:00:00+09:00".to_string(),
            user_id: Some("tester".to_string()),
        }
    }

    /// 旧バージョン (v1) スキーマの memories テーブルを 1 行付きで用意する。
    async fn seed_legacy_table(root: &Path) {
        let db = get_or_create_db(root).await.expect("connect db");
        let schema = legacy_memory_schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["legacy-1"])),
                Arc::new(StringArray::from(vec![
                    "ユーザーはエルデンリングが好きです。",
                ])),
                Arc::new(StringArray::from(vec!["user_speech"])),
                Arc::new(StringArray::from(vec!["User"])),
                Arc::new(StringArray::from(vec!["2026-08-31T12:00:00Z"])),
                Arc::new(StringArray::from(vec![Option::<String>::None])),
                vector_array(&sample_vector(1.0)),
            ],
        )
        .expect("legacy batch");
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        db.create_table(MEMORIES_TABLE, reader)
            .execute()
            .await
            .expect("create legacy table");
    }

    /// Create a compact but representative v1 table for migration regression
    /// tests.  The fixture deliberately exercises the cases that used to make
    /// a real install painfully slow or fail late in the import: invalid
    /// timestamps, nullable user/vector fields, and a very small f32 value
    /// that must survive JSON round-tripping exactly.
    async fn seed_legacy_fixture(root: &Path, row_count: usize) {
        let db = get_or_create_db(root).await.expect("connect db");
        let schema = legacy_memory_schema();
        let mut ids = Vec::with_capacity(row_count);
        let mut documents = Vec::with_capacity(row_count);
        let mut memory_types = Vec::with_capacity(row_count);
        let mut sources = Vec::with_capacity(row_count);
        let mut timestamps = Vec::with_capacity(row_count);
        let mut users = Vec::with_capacity(row_count);
        let mut vectors = Vec::with_capacity(row_count);
        for index in 0..row_count {
            ids.push(format!("fixture-{index:05}"));
            documents.push(format!(
                "移行フィクスチャ {index}: ユーザーのゲーム設定と検証用メモ。"
            ));
            let memory_type = match index % 5 {
                0 => "user_speech",
                1 => "discord_speech",
                2 => "manual",
                3 => "ai_response",
                _ => "auto_commentary",
            };
            memory_types.push(memory_type.to_string());
            sources.push(
                match memory_type {
                    "user_speech" => "User",
                    "discord_speech" => "Discord",
                    "manual" => "User",
                    "ai_response" => "AI",
                    _ => "System",
                }
                .to_string(),
            );
            timestamps.push(if index % 37 == 0 {
                "legacy-invalid-timestamp".to_string()
            } else {
                "2026-08-31T12:00:00Z".to_string()
            });
            users.push((index % 7 != 0).then(|| format!("fixture-user-{}", index % 3)));
            let vector = if index % 11 == 0 {
                None
            } else {
                let mut value = sample_vector(1.0 + index as f32 / 1000.0);
                if index == 62 {
                    value[0] = 6.208817016073453e-9_f32;
                }
                Some(value)
            };
            vectors.push(vector);
        }
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(documents)),
                Arc::new(StringArray::from(memory_types)),
                Arc::new(StringArray::from(sources)),
                Arc::new(StringArray::from(timestamps)),
                Arc::new(StringArray::from(users)),
                build_vector_array(&vectors).expect("fixture vectors"),
            ],
        )
        .expect("legacy fixture batch");
        // Fragment the source intentionally. Real v1 tables often contain
        // many tiny fragments, so the importer must coalesce them before
        // recovering and writing the Journal.
        let batches = (0..row_count)
            .step_by(3)
            .map(|offset| {
                Ok::<_, arrow_schema::ArrowError>(batch.slice(offset, (row_count - offset).min(3)))
            })
            .collect::<Vec<_>>();
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(batches, schema));
        db.create_table(MEMORIES_TABLE, reader)
            .execute()
            .await
            .expect("create legacy fixture table");
    }

    #[test]
    fn schema_version_is_detected_from_summary_columns() {
        assert_eq!(detect_memory_schema_version(&legacy_memory_schema()), 1);
        assert_eq!(
            detect_memory_schema_version(&get_memory_schema()),
            MEMORY_SCHEMA_VERSION
        );
    }

    #[test]
    fn memory_v2_vector_schema_has_nullable_outer_list_and_non_nullable_items() {
        let schema = get_memory_schema();
        let field = schema.field_with_name("vector").unwrap();
        assert!(field.is_nullable(), "missing vectors must be outer nulls");
        assert_eq!(
            field.data_type(),
            &DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, false)),
                VECTOR_DIM
            )
        );
    }

    #[test]
    fn vector_validation_rejects_wrong_shape_and_non_finite_values() {
        assert!(validate_vector(&sample_vector(1.0)).is_ok());
        assert!(validate_vector(&vec![0.0; VECTOR_DIM as usize - 1]).is_err());

        let mut nan_vector = sample_vector(1.0);
        nan_vector[10] = f32::NAN;
        assert!(validate_vector(&nan_vector).is_err());

        let mut infinity_vector = sample_vector(1.0);
        infinity_vector[10] = f32::INFINITY;
        assert!(validate_vector(&infinity_vector).is_err());
    }

    #[test]
    fn missing_vector_is_an_outer_null_not_a_zero_vector() {
        let array = build_vector_array(&[None]).expect("nullable vector array");
        assert!(array.is_null(0));
        assert_eq!(array.null_count(), 1);
    }

    #[tokio::test]
    async fn migration_imports_without_mutating_legacy_source() {
        let root = unique_test_root("migrate");
        seed_legacy_table(&root).await;

        let db = get_or_create_db(&root).await.unwrap();
        let table = get_or_create_memories_table(&db, &root)
            .await
            .expect("migration succeeds");

        let schema = table.schema().await.unwrap();
        assert_eq!(detect_memory_schema_version(&schema), MEMORY_SCHEMA_VERSION);
        for column in [
            "summary",
            "summary_status",
            "summary_model",
            "summary_prompt_version",
            "vector_source",
        ] {
            assert!(schema.field_with_name(column).is_ok(), "missing {}", column);
        }

        let row = get_memory_by_id(&root, "legacy-1")
            .await
            .unwrap()
            .expect("existing row must survive migration");
        assert_eq!(
            row.summary_status.as_deref(),
            Some(SUMMARY_STATUS_LEGACY),
            "existing rows become legacy"
        );
        assert_eq!(
            row.vector_source.as_deref(),
            Some(VECTOR_SOURCE_DOCUMENT),
            "legacy rows stay searchable by raw text"
        );
        assert_eq!(row.document, "ユーザーはエルデンリングが好きです。");
        assert!(row.summary.is_none());

        // The source table remains v1 and is still readable after import.
        let source = db.open_table(MEMORIES_TABLE).execute().await.unwrap();
        assert_eq!(
            detect_memory_schema_version(&source.schema().await.unwrap()),
            1,
            "legacy source must not be upgraded in place"
        );
        let source_rows = source
            .query()
            .execute()
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(
            source_rows
                .iter()
                .map(|batch| batch.num_rows())
                .sum::<usize>(),
            1
        );
        let journal = std::fs::read_to_string(root.join("data/memory_v2/journal.jsonl"))
            .expect("legacy import must be journaled");
        assert!(journal.contains("\"operation_kind\":\"raw_event\""));
        assert!(journal.contains("\"operation_kind\":\"embedding\""));
        assert!(root.join("data/memory_v2/manifest.json").is_file());
        assert!(list_lance_backups(&root).unwrap().is_empty());

        // Restart is idempotent: no duplicate import and no backup side effect.
        let _ = get_or_create_memories_table(&db, &root)
            .await
            .expect("reopen after migration");
        assert_eq!(list_memories(&root, None, None).await.unwrap().total, 1);

        cleanup(&root);
    }

    #[tokio::test]
    async fn migration_progress_reaches_completed_after_legacy_import() {
        let root = unique_test_root("migrate-progress");
        seed_legacy_table(&root).await;
        let progress = Arc::new(MemoryMigrationProgress::default());

        let response =
            list_memories_with_progress(&root, Some(5000), Some(0), Some(progress.clone()))
                .await
                .expect("migration-backed list succeeds");
        assert_eq!(response.total, 1);

        let status = progress.snapshot();
        assert_eq!(status.status, "completed");
        assert_eq!(status.processed, status.total.unwrap());
        assert!(root.join(LANCE_DB_DIR).join(LEGACY_IMPORT_MARKER).is_file());
        cleanup(&root);
    }

    #[tokio::test]
    async fn migration_fixture_covers_batches_and_edge_cases() {
        // Keep this fixture intentionally small: it is a fast smoke/regression
        // test for every migration edge case, while the production-sized
        // migration remains an explicit manual check against a real install.
        const ROWS: usize = 97;
        let root = unique_test_root("migrate-fixture");
        seed_legacy_fixture(&root, ROWS).await;
        let progress = Arc::new(MemoryMigrationProgress::default());

        let response =
            list_memories_with_progress(&root, Some(ROWS + 10), Some(0), Some(progress.clone()))
                .await
                .expect("fixture migration succeeds");
        assert_eq!(response.total, ROWS);
        let status = progress.snapshot();
        assert_eq!(status.status, "completed");
        assert_eq!(status.processed, ROWS);
        assert_eq!(status.total, Some(ROWS));
        assert!(root.join(LANCE_DB_DIR).join(LEGACY_IMPORT_MARKER).is_file());

        // Re-opening/relisting must be idempotent and must validate every
        // operation, including the problematic f32 value at fixture-00062.
        let second = list_memories(&root, Some(ROWS + 10), Some(0))
            .await
            .expect("fixture relist succeeds");
        assert_eq!(second.total, ROWS);
        assert!(get_memory_by_id(&root, "fixture-00062")
            .await
            .expect("fixture row lookup")
            .is_some());

        let journal = std::fs::read_to_string(root.join("data/memory_v2/journal.jsonl"))
            .expect("fixture journal");
        assert_eq!(
            journal
                .lines()
                .filter(|line| line.contains("\"operation_kind\":\"raw_event\""))
                .count(),
            ROWS
        );
        assert_eq!(
            journal
                .lines()
                .filter(|line| line.contains("\"operation_kind\":\"embedding\""))
                .count(),
            ROWS - ROWS.div_ceil(11)
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn migration_failure_leaves_legacy_table_intact() {
        let root = unique_test_root("migrate-fail");
        let db = get_or_create_db(&root).await.unwrap();
        let schema = legacy_memory_schema();
        let mut invalid = sample_vector(1.0);
        invalid[0] = f32::NAN;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["legacy-invalid"])),
                Arc::new(StringArray::from(vec!["unchanged"])),
                Arc::new(StringArray::from(vec!["user_speech"])),
                Arc::new(StringArray::from(vec!["User"])),
                Arc::new(StringArray::from(vec!["2026-08-31T12:00:00Z"])),
                Arc::new(StringArray::from(vec![Option::<String>::None])),
                vector_array_with_values(&invalid),
            ],
        )
        .unwrap();
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        db.create_table(MEMORIES_TABLE, reader)
            .execute()
            .await
            .unwrap();

        let result = get_or_create_memories_table(&db, &root).await;
        assert!(result.is_err(), "migration must fail loudly");

        // 旧テーブルは v1 のまま壊れていないこと
        let table = db.open_table(MEMORIES_TABLE).execute().await.unwrap();
        let schema = table.schema().await.unwrap();
        assert_eq!(detect_memory_schema_version(&schema), 1);

        assert!(!root.join(LANCE_DB_DIR).join(LEGACY_IMPORT_MARKER).exists());

        cleanup(&root);
    }

    #[tokio::test]
    async fn legacy_missing_vector_imports_as_outer_null() {
        let root = unique_test_root("legacy-null-vector");
        let db = get_or_create_db(&root).await.unwrap();
        let schema = legacy_memory_schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["legacy-null"])),
                Arc::new(StringArray::from(vec!["without embedding"])),
                Arc::new(StringArray::from(vec!["manual"])),
                Arc::new(StringArray::from(vec!["User"])),
                Arc::new(StringArray::from(vec!["2026-08-31T12:00:00Z"])),
                Arc::new(StringArray::from(vec![Option::<String>::None])),
                build_vector_array(&[None]).unwrap(),
            ],
        )
        .unwrap();
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        db.create_table(MEMORIES_TABLE, reader)
            .execute()
            .await
            .unwrap();

        let active = get_or_create_memories_table(&db, &root).await.unwrap();
        let mut stream = active.query().execute().await.unwrap();
        let batch = stream.try_next().await.unwrap().unwrap();
        let vectors = batch
            .column_by_name("vector")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(vectors.is_null(0));
        let row = get_memory_by_id(&root, "legacy-null")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.summary_status.as_deref(), Some(SUMMARY_STATUS_LEGACY));
        assert_eq!(row.vector_source.as_deref(), Some(VECTOR_SOURCE_NONE));

        cleanup(&root);
    }

    #[tokio::test]
    async fn insert_defaults_follow_candidate_rule() {
        let root = unique_test_root("insert-defaults");

        insert_memory_batch_nullable(
            &root,
            vec![
                item("cand-1", "user_speech", "ユーザーは猫を2匹飼っている。"),
                item("ai-1", "ai_response", "AIの返答本文"),
                item("manual-1", "manual", "手動で保存した記憶"),
            ],
            Some(vec![
                Some(sample_vector(1.0)),
                Some(sample_vector(1.0)),
                None,
            ]),
            false,
        )
        .await
        .unwrap();

        let candidate = get_memory_by_id(&root, "cand-1")
            .await
            .unwrap()
            .expect("candidate row");
        assert_eq!(
            candidate.summary_status.as_deref(),
            Some(SUMMARY_STATUS_PENDING)
        );
        assert_eq!(
            candidate.vector_source.as_deref(),
            Some(VECTOR_SOURCE_DOCUMENT)
        );

        let ai = get_memory_by_id(&root, "ai-1")
            .await
            .unwrap()
            .expect("ai row");
        assert!(
            ai.summary_status.is_none(),
            "non-candidates stay outside the summary pipeline"
        );
        assert_eq!(ai.vector_source.as_deref(), Some(VECTOR_SOURCE_DOCUMENT));

        let manual = get_memory_by_id(&root, "manual-1")
            .await
            .unwrap()
            .expect("manual row");
        assert!(manual.summary_status.is_none());
        assert_eq!(
            manual.vector_source.as_deref(),
            Some(VECTOR_SOURCE_NONE),
            "no vector means vector_source=none"
        );

        cleanup(&root);
    }

    #[tokio::test]
    async fn vector_boundary_stores_valid_values_and_outer_nulls() {
        let root = unique_test_root("vectors");
        insert_memory_batch_nullable(
            &root,
            vec![
                item("vector-valid", "manual", "valid"),
                item("vector-missing", "manual", "missing"),
            ],
            Some(vec![Some(sample_vector(7.0)), None]),
            false,
        )
        .await
        .unwrap();

        let db = get_or_create_db(&root).await.unwrap();
        let table = get_or_create_memories_table(&db, &root).await.unwrap();
        let mut stream = table.query().execute().await.unwrap();
        let batch = stream.try_next().await.unwrap().unwrap();
        let vectors = batch
            .column_by_name("vector")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(!vectors.is_null(0));
        assert!(
            vectors
                .value(0)
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .value(0)
                > 0.0
        );
        assert!(vectors.is_null(1), "absence must not become a zero vector");

        cleanup(&root);
    }

    #[tokio::test]
    async fn vector_boundary_rejects_invalid_insert_without_writing() {
        let root = unique_test_root("vectors-invalid");
        let mut invalid = sample_vector(1.0);
        invalid[4] = f32::INFINITY;
        let result = insert_memory_batch(
            &root,
            vec![item("invalid", "manual", "bad")],
            Some(vec![invalid]),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(list_memories(&root, None, None).await.unwrap().total, 0);
        cleanup(&root);
    }

    #[tokio::test]
    async fn raw_event_can_be_read_before_embedding_and_then_promoted() {
        let root = unique_test_root("raw-before-embedding");
        insert_memory_batch_nullable(
            &root,
            vec![item(
                "raw-first",
                "user_speech",
                "raw survives embedding delay",
            )],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();

        let raw = get_memory_by_id(&root, "raw-first").await.unwrap().unwrap();
        assert_eq!(raw.document, "raw survives embedding delay");
        assert_eq!(raw.vector_source.as_deref(), Some(VECTOR_SOURCE_NONE));

        assert!(
            update_document_vector(&root, "raw-first", sample_vector(9.0))
                .await
                .unwrap()
        );
        let embedded = get_memory_by_id(&root, "raw-first").await.unwrap().unwrap();
        assert_eq!(
            embedded.vector_source.as_deref(),
            Some(VECTOR_SOURCE_DOCUMENT)
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn document_vector_update_only_fills_absent_vectors() {
        let root = unique_test_root("document-vector-idempotent");
        let original = sample_vector(1.0);
        let retry = sample_vector(9.0);
        insert_memory_batch(
            &root,
            vec![item("already-embedded", "manual", "existing vector")],
            Some(vec![original.clone()]),
        )
        .await
        .unwrap();

        assert!(
            !update_document_vector(&root, "already-embedded", retry.clone())
                .await
                .unwrap()
        );
        let row = get_memory_by_id(&root, "already-embedded")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.vector_source.as_deref(), Some(VECTOR_SOURCE_DOCUMENT));

        let db = get_or_create_db(&root).await.unwrap();
        let table = get_or_create_memories_table(&db, &root).await.unwrap();
        let mut stream = table
            .vector_search(original.clone())
            .unwrap()
            .limit(1)
            .execute()
            .await
            .unwrap();
        let batch = stream.try_next().await.unwrap().unwrap();
        let vectors = batch
            .column_by_name("vector")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        let stored_values = vectors.value(0);
        let stored = stored_values
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(stored.value(0), original[0]);

        insert_memory_batch_nullable(
            &root,
            vec![item("late-embedded", "manual", "waiting for embedding")],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();
        assert!(
            update_document_vector(&root, "late-embedded", original.clone())
                .await
                .unwrap()
        );
        assert!(!update_document_vector(&root, "late-embedded", retry)
            .await
            .unwrap());

        insert_memory_batch_nullable(
            &root,
            vec![item("summary-embedded", "user_speech", "summary wins")],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();
        apply_summary(
            &root,
            "summary-embedded",
            "authoritative summary",
            "model",
            "v2",
            Some(original),
        )
        .await
        .unwrap();
        assert!(
            !update_document_vector(&root, "summary-embedded", sample_vector(12.0))
                .await
                .unwrap()
        );
        let summary_row = get_memory_by_id(&root, "summary-embedded")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary_row.vector_source.as_deref(),
            Some(VECTOR_SOURCE_SUMMARY)
        );

        cleanup(&root);
    }

    #[test]
    fn summary_status_transitions_are_monotonic_and_idempotent() {
        assert!(summary_status_transition_allowed(
            Some(SUMMARY_STATUS_PENDING),
            SUMMARY_STATUS_COMPLETED
        ));
        assert!(summary_status_transition_allowed(
            Some(SUMMARY_STATUS_COMPLETED),
            SUMMARY_STATUS_COMPLETED
        ));
        assert!(!summary_status_transition_allowed(
            Some(SUMMARY_STATUS_COMPLETED),
            SUMMARY_STATUS_FALLBACK
        ));
        assert!(!summary_status_transition_allowed(
            Some(SUMMARY_STATUS_SKIPPED),
            SUMMARY_STATUS_COMPLETED
        ));
        assert!(summary_status_transition_allowed(
            Some(SUMMARY_STATUS_FALLBACK),
            SUMMARY_STATUS_PENDING
        ));
        assert!(!summary_status_transition_allowed(
            Some(SUMMARY_STATUS_COMPLETED),
            SUMMARY_STATUS_PENDING
        ));
    }

    #[tokio::test]
    async fn explicit_summary_retry_reopens_terminal_projection_without_touching_raw() {
        let root = unique_test_root("summary-retry");
        insert_memory_batch_nullable(
            &root,
            vec![item("retry-event", "user_speech", "immutable raw text")],
            Some(vec![Some(sample_vector(1.0))]),
            false,
        )
        .await
        .unwrap();
        assert!(mark_summary_fallback(&root, "retry-event").await.unwrap());
        assert!(retry_summary(&root, "retry-event").await.unwrap());
        let row = get_memory_by_id(&root, "retry-event")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.document, "immutable raw text");
        assert_eq!(row.summary_status.as_deref(), Some(SUMMARY_STATUS_PENDING));
        assert_eq!(row.vector_source.as_deref(), Some(VECTOR_SOURCE_DOCUMENT));
        let journal = std::fs::read_to_string(
            MemoryRepository::open(&root)
                .await
                .unwrap()
                .paths()
                .journal(),
        )
        .unwrap();
        assert!(journal.contains("\"status\":\"fallback\""));
        assert!(journal.contains("\"status\":\"pending\""));
        cleanup(&root);
    }

    #[tokio::test]
    async fn event_lookup_bridges_canonical_id_to_legacy_projection_id() {
        let root = unique_test_root("event-lookup-canonical");
        let legacy_id = "legacy-retry-event";
        insert_memory_batch_nullable(
            &root,
            vec![item(legacy_id, "user_speech", "immutable raw text")],
            Some(vec![Some(sample_vector(1.0))]),
            false,
        )
        .await
        .unwrap();

        let canonical = MemoryRepository::canonical_event_id(legacy_id);
        let row = get_memory_by_event_id(&root, &canonical)
            .await
            .unwrap()
            .expect("canonical event ID must resolve its legacy projection row");
        assert_eq!(row.id, legacy_id);
        cleanup(&root);
    }

    #[tokio::test]
    async fn skipped_rows_are_excluded_from_search_but_not_deleted() {
        let root = unique_test_root("skipped");

        insert_memory_batch(
            &root,
            vec![item("sk-1", "user_speech", "挨拶だけの発話")],
            Some(vec![sample_vector(1.0)]),
        )
        .await
        .unwrap();

        let marked = mark_summary_skipped(&root, "sk-1").await.unwrap();
        assert!(marked, "row must be marked skipped");
        let journal = std::fs::read_to_string(root.join("data/memory_v2/journal.jsonl"))
            .expect("summary status must be journaled");
        assert!(journal.contains("\"operation_kind\":\"summary_status\""));

        let hits = search_similar_memories(&root, &sample_vector(1.0), 5)
            .await
            .unwrap();
        assert!(
            hits.iter().all(|hit| hit.item.id != "sk-1"),
            "skipped rows must not surface in search"
        );

        // 削除ではなく skipped 化: 行は残る
        let row = get_memory_by_id(&root, "sk-1")
            .await
            .unwrap()
            .expect("row is kept, not deleted");
        assert_eq!(row.summary_status.as_deref(), Some(SUMMARY_STATUS_SKIPPED));
        assert_eq!(row.document, "挨拶だけの発話");

        cleanup(&root);
    }

    #[tokio::test]
    async fn delete_is_journaled_and_removes_authoritative_entity() {
        let root = unique_test_root("delete-journal");
        insert_memory_batch_nullable(
            &root,
            vec![item("delete-1", "manual", "to be deleted")],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();

        assert!(delete_memory(&root, "delete-1").await.unwrap());
        assert!(get_memory_by_id(&root, "delete-1").await.unwrap().is_none());
        let journal = std::fs::read_to_string(root.join("data/memory_v2/journal.jsonl"))
            .expect("redaction must be journaled");
        assert!(journal.contains("\"operation_kind\":\"redaction\""));

        cleanup(&root);
    }

    #[tokio::test]
    async fn completed_summary_is_preferred_in_search_results() {
        let root = unique_test_root("completed");
        let raw_document = "ねえぐり、猫2匹の世話で朝6時に起きることにした。";
        insert_memory_batch(
            &root,
            vec![item("comp-1", "user_speech", raw_document)],
            Some(vec![sample_vector(1.0)]),
        )
        .await
        .unwrap();

        let summary_text = "ユーザーは猫を2匹飼っており、朝6時起きを決意した。";
        let applied = apply_summary(
            &root,
            "comp-1",
            summary_text,
            "gemma-3-1b-it-Q4_K_S.gguf",
            "v2",
            Some(sample_vector(2.0)),
        )
        .await
        .unwrap();
        assert!(applied);

        let row = get_memory_by_id(&root, "comp-1")
            .await
            .unwrap()
            .expect("row kept");
        assert_eq!(row.summary.as_deref(), Some(summary_text));
        assert_eq!(
            row.summary_status.as_deref(),
            Some(SUMMARY_STATUS_COMPLETED)
        );
        assert_eq!(
            row.summary_model.as_deref(),
            Some("gemma-3-1b-it-Q4_K_S.gguf")
        );
        assert_eq!(
            row.summary_prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        assert_eq!(row.vector_source.as_deref(), Some(VECTOR_SOURCE_SUMMARY));
        // document は生テキストのまま保持される (§3.6)
        assert_eq!(row.document, raw_document);

        let hits = search_similar_memories(&root, &sample_vector(2.0), 5)
            .await
            .unwrap();
        let hit = hits
            .iter()
            .find(|hit| hit.item.id == "comp-1")
            .expect("completed row is searchable");
        assert_eq!(hit.searchable_text(), summary_text);
        assert_eq!(hit.item.document, raw_document);

        cleanup(&root);
    }

    #[tokio::test]
    async fn apply_summary_without_vector_keeps_document_vector() {
        let root = unique_test_root("completed-no-vec");
        insert_memory_batch(
            &root,
            vec![item("nv-1", "user_speech", "生テキストの行")],
            Some(vec![sample_vector(1.0)]),
        )
        .await
        .unwrap();

        apply_summary(&root, "nv-1", "要約のみ", "model", "v2", None)
            .await
            .unwrap();

        let row = get_memory_by_id(&root, "nv-1").await.unwrap().unwrap();
        assert_eq!(row.summary.as_deref(), Some("要約のみ"));
        assert_eq!(
            row.summary_status.as_deref(),
            Some(SUMMARY_STATUS_COMPLETED)
        );
        assert_eq!(
            row.vector_source.as_deref(),
            Some(VECTOR_SOURCE_DOCUMENT),
            "no summary vector: keep the raw text embedding"
        );

        cleanup(&root);
    }

    #[tokio::test]
    async fn apply_summary_is_idempotent_per_event_id() {
        let root = unique_test_root("idempotent");
        insert_memory_batch(
            &root,
            vec![item("idem-1", "user_speech", "同じイベント")],
            Some(vec![sample_vector(1.0)]),
        )
        .await
        .unwrap();

        for _ in 0..2 {
            apply_summary(
                &root,
                "idem-1",
                "冪等な要約",
                "model",
                "v2",
                Some(sample_vector(3.0)),
            )
            .await
            .unwrap();
        }

        let list = list_memories(&root, None, None).await.unwrap();
        assert_eq!(list.total, 1, "re-applying must not duplicate rows");
        let row = get_memory_by_id(&root, "idem-1").await.unwrap().unwrap();
        assert_eq!(row.summary.as_deref(), Some("冪等な要約"));

        cleanup(&root);
    }

    #[tokio::test]
    async fn apply_summary_on_missing_row_reports_false() {
        let root = unique_test_root("missing");
        let applied = apply_summary(&root, "nope", "要約", "model", "v2", None)
            .await
            .unwrap();
        assert!(!applied);
        cleanup(&root);
    }

    #[tokio::test]
    async fn list_pending_summaries_returns_only_pending_candidate_rows() {
        let root = unique_test_root("pending");

        insert_memory_batch(
            &root,
            vec![
                item("pend-1", "user_speech", "再処理が必要な発話"),
                item("pend-2", "twitch_chat", "スキップ済みのコメント"),
                item("pend-3", "discord_speech", "まだ処理されていない発話"),
            ],
            None,
        )
        .await
        .unwrap();
        mark_summary_skipped(&root, "pend-2").await.unwrap();

        let pending = list_pending_summaries(&root, 10).await.unwrap();
        let ids: Vec<&str> = pending.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["pend-1", "pend-3"],
            "only pending rows are listed"
        );
        assert_eq!(pending[0].memory_type, "user_speech");
        assert_eq!(pending[0].document, "再処理が必要な発話");

        // limit が効くこと
        let limited = list_pending_summaries(&root, 1).await.unwrap();
        assert_eq!(limited.len(), 1);

        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_excludes_noncandidate_rows_but_skips_active_rows() {
        let root = unique_test_root("backfill-queue");
        insert_memory_batch(
            &root,
            vec![
                item("backfill-manual", "manual", "手動メモ"),
                item("backfill-user", "user_speech", "進行中の発話"),
            ],
            None,
        )
        .await
        .unwrap();

        assert!(!queue_summary_backfill(&root, "backfill-manual")
            .await
            .unwrap());
        assert!(!queue_summary_backfill(&root, "backfill-manual")
            .await
            .unwrap());
        assert!(!queue_summary_backfill(&root, "backfill-user")
            .await
            .unwrap());
        assert_eq!(
            get_memory_by_id(&root, "backfill-manual")
                .await
                .unwrap()
                .unwrap()
                .summary_status,
            None
        );
        assert!(!queue_summary_backfill(&root, "backfill-manual")
            .await
            .unwrap());
        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_batches_projection_and_journal_work() {
        let root = unique_test_root("backfill-batch");
        insert_memory_batch(
            &root,
            vec![
                item("batch-a", "user_speech", "猫が好き"),
                item("batch-b", "manual", "一時的なメモ"),
            ],
            None,
        )
        .await
        .unwrap();
        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(queue_summary_backfill_batch(&root, &rows).await.unwrap(), 1);

        let completed = vec![SummaryBatchInput {
            entity_id: "batch-a".into(),
            summary: "ユーザーは猫が好き".into(),
            embedding: None,
            attempt_id: None,
            model_id: None,
            prompt_version: None,
        }];
        apply_summary_backfill_batch(&root, completed, vec![], vec![])
            .await
            .unwrap();
        assert_eq!(
            get_memory_by_id(&root, "batch-a")
                .await
                .unwrap()
                .unwrap()
                .summary_status
                .as_deref(),
            Some(SUMMARY_STATUS_COMPLETED)
        );
        assert_eq!(
            get_memory_by_id(&root, "batch-b")
                .await
                .unwrap()
                .unwrap()
                .summary_status,
            None
        );
        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(
            queue_summary_backfill_batch(&root, &rows).await.unwrap(),
            0,
            "terminal backfill rows must not be requeued by a fresh all-memory pass"
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_requeues_legacy_auto_fact_for_v2_repair() {
        let root = unique_test_root("backfill-legacy-auto-fact");
        insert_memory_batch_nullable(
            &root,
            vec![item("legacy-auto-fact", "user_speech", "元の本文")],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();
        let repository = MemoryRepository::open(&root).await.unwrap();
        // Simulate a v1 automatic summary: a Fact exists, but no v2 attempt
        // status has been journaled yet. Process all must still admit it so a
        // later chunk can compare-and-swap the same Fact into prompt v2.
        repository
            .append_summary_fact("legacy-auto-fact", "旧版の要約")
            .await
            .unwrap();

        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(queue_summary_backfill_batch(&root, &rows).await.unwrap(), 1);
        assert_eq!(
            get_memory_by_id(&root, "legacy-auto-fact")
                .await
                .unwrap()
                .unwrap()
                .summary_status
                .as_deref(),
            Some(SUMMARY_STATUS_PENDING)
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_handles_large_candidate_sets_without_stack_overflow() {
        let root = unique_test_root("backfill-large-candidate-set");
        let db = get_or_create_db(&root).await.unwrap();
        let table = get_or_create_memories_table(&db, &root).await.unwrap();
        const ROW_COUNT: usize = 20_000;
        let items = (0..ROW_COUNT)
            .map(|index| item(&format!("large-backfill-{index:05}"), "user_speech", "raw"))
            .collect::<Vec<_>>();
        insert_on_table(&table, items, vec![None; ROW_COUNT], false, false)
            .await
            .unwrap();

        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(rows.len(), ROW_COUNT);
        let repository = MemoryRepository::open(&root).await.unwrap();
        let admitted = queue_summary_backfill_batch_ids_with_repository(&repository, &root, &rows)
            .await
            .unwrap();
        assert_eq!(admitted.len(), ROW_COUNT);
        cleanup(&root);
    }

    #[tokio::test]
    async fn backfill_reports_row_local_validation_failures_after_durable_commit() {
        let root = unique_test_root("backfill-validation-result");
        insert_memory_batch(
            &root,
            vec![
                item("valid-validation", "user_speech", "有効な行"),
                item("invalid-validation", "user_speech", "ベクトルが壊れた行"),
            ],
            None,
        )
        .await
        .unwrap();
        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(queue_summary_backfill_batch(&root, &rows).await.unwrap(), 2);

        let result = apply_summary_backfill_batch(
            &root,
            vec![
                SummaryBatchInput {
                    entity_id: "valid-validation".into(),
                    summary: "有効な要約".into(),
                    embedding: None,
                    attempt_id: None,
                    model_id: None,
                    prompt_version: None,
                },
                SummaryBatchInput {
                    entity_id: "invalid-validation".into(),
                    summary: "保存はできる要約".into(),
                    embedding: Some(vec![0.0]),
                    attempt_id: None,
                    model_id: None,
                    prompt_version: None,
                },
            ],
            vec![],
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(result.persisted, 1);
        assert_eq!(result.terminal_failed, 1);
        assert_eq!(result.terminal_skipped, 0);
        assert!(result.projection_error.is_none());
        assert_eq!(
            get_memory_by_id(&root, "invalid-validation")
                .await
                .unwrap()
                .unwrap()
                .summary_status
                .as_deref(),
            Some(SUMMARY_STATUS_FALLBACK)
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_respects_deleted_summary_tombstones() {
        let root = unique_test_root("backfill-delete-tombstone");
        insert_memory_batch_nullable(
            &root,
            vec![item("deleted-backfill", "user_speech", "削除済みの要約元")],
            Some(vec![None]),
            false,
        )
        .await
        .unwrap();
        apply_summary(
            &root,
            "deleted-backfill",
            "削除される要約",
            "model",
            "v2",
            None,
        )
        .await
        .unwrap();
        let repository = MemoryRepository::open(&root).await.unwrap();
        let fact = repository.read_facts().await.unwrap().pop().unwrap();
        repository
            .append_fact_delete(&fact, fact.revision())
            .await
            .unwrap();

        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(queue_summary_backfill_batch(&root, &rows).await.unwrap(), 0);
        assert!(
            !queue_summary_backfill(&root, "deleted-backfill")
                .await
                .unwrap(),
            "single-row queue must honor the durable delete tombstone"
        );
        cleanup(&root);
    }

    #[tokio::test]
    async fn explicit_backfill_recovers_stale_pending_rows() {
        let root = unique_test_root("backfill-stale-pending");
        insert_memory_batch(
            &root,
            vec![item("stale-pending", "user_speech", "再実行が必要な発話")],
            None,
        )
        .await
        .unwrap();

        // Simulate a prior run that queued the row but crashed before writing
        // a Fact.  An explicit all-memory pass must recover this row instead
        // of treating the stale pending marker as active forever.
        assert!(
            mark_summary_status(&root, "stale-pending", SUMMARY_STATUS_PENDING)
                .await
                .unwrap()
        );
        let before_attempt = MemoryRepository::open(&root)
            .await
            .unwrap()
            .read_summary_statuses()
            .unwrap()
            .get(&MemoryRepository::canonical_event_id("stale-pending"))
            .and_then(|status| status.attempt_id.clone());
        let rows = list_stored_memories(&root).await.unwrap();
        assert_eq!(
            queue_summary_backfill_batch(&root, &rows).await.unwrap(),
            1,
            "stale pending rows must be eligible for explicit recovery"
        );
        let after_attempt = MemoryRepository::open(&root)
            .await
            .unwrap()
            .read_summary_statuses()
            .unwrap()
            .get(&MemoryRepository::canonical_event_id("stale-pending"))
            .and_then(|status| status.attempt_id.clone());
        assert_ne!(before_attempt, after_attempt);

        cleanup(&root);
    }

    #[tokio::test]
    async fn candidate_rule_matches_spec_section_3_3() {
        assert!(is_summary_candidate_type("user_speech"));
        assert!(is_summary_candidate_type("discord_speech"));
        assert!(is_summary_candidate_type("twitch_chat"));
        assert!(!is_summary_candidate_type("ai_response"));
        assert!(!is_summary_candidate_type("auto_commentary"));
        assert!(!is_summary_candidate_type("manual"));
    }

    #[test]
    fn candidate_admission_distinguishes_type_and_content_before_gemma() {
        assert_eq!(
            summary_admission("twitch_chat", "ユーザーは猫が好きです。"),
            SummaryAdmission::Eligible
        );
        assert_eq!(
            summary_admission("ai_response", "モデルの返答"),
            SummaryAdmission::NotApplicable
        );
        assert_eq!(
            summary_admission("unknown", "不明なイベント"),
            SummaryAdmission::NotApplicable
        );
        assert_eq!(
            summary_admission("user_speech", ""),
            SummaryAdmission::Invalid
        );
        assert_eq!(
            summary_admission("user_speech", "壊れた\0本文"),
            SummaryAdmission::Invalid
        );
    }

    #[test]
    fn candidate_admission_rejects_exact_ephemeral_filler_before_gemma() {
        for content in [
            "はい",
            " は い ",
            "うんうん",
            "こんにちは",
            "ありがとうございます",
            "OK",
            "www",
            "あ",
        ] {
            assert_eq!(
                summary_admission("user_speech", content),
                SummaryAdmission::Invalid,
                "ephemeral content must not reach the model: {content:?}"
            );
        }
        assert_eq!(
            summary_admission("user_speech", "はい、猫を2匹飼っています。"),
            SummaryAdmission::Eligible,
            "exact-match filtering must remain conservative"
        );
    }

    #[test]
    fn noncandidate_rows_are_never_backfill_candidates_or_marked_skipped() {
        for memory_type in [
            "ai_response",
            "auto_commentary",
            "manual",
            "system",
            "unknown",
        ] {
            let row = StoredMemory {
                id: format!("noncandidate-{memory_type}"),
                document: "保持する本文".into(),
                memory_type: memory_type.into(),
                source: "source".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
                user_id: None,
                summary: None,
                summary_status: None,
                summary_model: None,
                summary_prompt_version: None,
                vector_source: Some(VECTOR_SOURCE_DOCUMENT.into()),
            };
            assert!(!is_summary_backfill_candidate(&row));
        }
    }

    #[test]
    fn new_summary_attempts_use_contract_v3() {
        assert_eq!(SUMMARY_PROMPT_VERSION, "v3");
        assert_eq!(SUMMARY_MODEL_ID, "gemma-3-1b-it-Q4_K_S.gguf");
    }

    #[test]
    fn terminal_summary_projection_rows_are_not_backfill_candidates() {
        let mut row = StoredMemory {
            id: "terminal-candidate".into(),
            document: "raw".into(),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: None,
        };
        row.summary_status = Some(SUMMARY_STATUS_SKIPPED.into());
        assert!(!is_summary_backfill_candidate(&row));
        row.summary_status = Some(SUMMARY_STATUS_FALLBACK.into());
        assert!(!is_summary_backfill_candidate(&row));
        row.summary_status = Some(SUMMARY_STATUS_PENDING.into());
        assert!(is_summary_backfill_candidate(&row));
    }

    #[test]
    fn raw_document_rejects_traversal_and_absolute_ids() {
        let root = unique_test_root("raw-unsafe-id");

        assert!(preserve_raw_document(&root, "../escape", "raw").is_err());
        assert!(preserve_raw_document(&root, "/absolute", "raw").is_err());

        cleanup(&root);
    }

    #[tokio::test]
    async fn filesystem_boundaries_reject_traversal_root() {
        let root = unique_test_root("traversal-root");
        let traversal_root = root.join("..").join("outside");

        assert!(get_or_create_db(&traversal_root).await.is_err());

        cleanup(&root);
    }

    #[test]
    fn backup_boundaries_reject_traversal_names_before_restore() {
        let root = unique_test_root("backup-unsafe-name");
        std::fs::create_dir_all(root.join("data/lancedb_backups")).expect("create backup root");

        assert!(restore_lance_backup(&root, "../outside").is_err());
        assert!(list_lance_backups(&root).is_ok());

        cleanup(&root);
    }

    #[test]
    fn backups_created_in_the_same_second_use_distinct_directories() {
        let root = unique_test_root("backup-collision");
        let db_dir = root.join(LANCE_DB_DIR);
        std::fs::create_dir_all(&db_dir).expect("create LanceDB directory");
        std::fs::write(db_dir.join("marker"), "snapshot").expect("seed LanceDB directory");
        let memory_v2_dir = root.join("data/memory_v2");
        std::fs::create_dir_all(&memory_v2_dir).expect("create memory-v2 directory");
        std::fs::write(memory_v2_dir.join("journal.jsonl"), "journal snapshot")
            .expect("seed memory-v2 journal");

        let first = backup_lance_db(&root).expect("first backup");
        let second = backup_lance_db(&root).expect("second backup");

        assert_ne!(first, second);
        assert!(first.starts_with("backup_"));
        assert!(second.starts_with("backup_"));
        assert_eq!(list_lance_backups(&root).unwrap().len(), 2);
        assert!(root
            .join("data/lancedb_backups")
            .join(&first)
            .join("marker")
            .is_file());
        assert!(root
            .join("data/lancedb_backups")
            .join(second)
            .join("marker")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(
                root.join("data/lancedb_backups")
                    .join(&first)
                    .join("memory_v2/journal.jsonl")
            )
            .unwrap(),
            "journal snapshot"
        );
        std::fs::write(memory_v2_dir.join("journal.jsonl"), "changed")
            .expect("mutate current memory-v2 snapshot");
        restore_lance_backup(&root, &first).expect("restore full memory snapshot");
        assert_eq!(
            std::fs::read_to_string(memory_v2_dir.join("journal.jsonl")).unwrap(),
            "journal snapshot"
        );

        cleanup(&root);
    }

    #[tokio::test]
    async fn export_rejects_traversal_filename() {
        let root = unique_test_root("export-unsafe-name");

        assert!(
            export_lance_memories_json(&root, Some("../escape.json".to_string()))
                .await
                .is_err()
        );

        cleanup(&root);
    }

    #[tokio::test]
    async fn reason_bearing_backfill_statuses_update_projection_and_survive_replay() {
        let root = unique_test_root("backfill-reason-replay");
        insert_memory_batch_nullable(
            &root,
            vec![
                item("metadata-reason", "user_speech", "ユーザーは猫が好きです"),
                item("grounding-reason", "user_speech", "ユーザーは犬が好きです"),
            ],
            Some(vec![None, None]),
            false,
        )
        .await
        .unwrap();

        let statuses = vec![
            SummaryStatusBatchInput {
                entity_id: "metadata-reason".into(),
                status: SUMMARY_STATUS_FALLBACK.into(),
                model_id: Some("gemma-metadata-test".into()),
                prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                attempt_id: Some("attempt-metadata-reason".into()),
                reason: Some("metadata_echo".into()),
            },
            SummaryStatusBatchInput {
                entity_id: "grounding-reason".into(),
                status: SUMMARY_STATUS_FALLBACK.into(),
                model_id: Some("gemma-grounding-test".into()),
                prompt_version: Some(SUMMARY_PROMPT_VERSION.into()),
                attempt_id: Some("attempt-grounding-reason".into()),
                reason: Some("ungrounded_summary".into()),
            },
        ];
        let first = apply_summary_backfill_batch_with_statuses(&root, Vec::new(), statuses.clone())
            .await
            .unwrap();
        assert_eq!(first.terminal_failed, 2);

        let metadata_row = get_memory_by_id(&root, "metadata-reason")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            metadata_row.summary_status.as_deref(),
            Some(SUMMARY_STATUS_FALLBACK)
        );
        assert_eq!(
            metadata_row.summary_model.as_deref(),
            Some("gemma-metadata-test")
        );
        assert_eq!(
            metadata_row.summary_prompt_version.as_deref(),
            Some(SUMMARY_PROMPT_VERSION)
        );
        let grounding_row = get_memory_by_id(&root, "grounding-reason")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            grounding_row.summary_status.as_deref(),
            Some(SUMMARY_STATUS_FALLBACK)
        );
        assert_eq!(
            grounding_row.summary_model.as_deref(),
            Some("gemma-grounding-test")
        );

        let repository = MemoryRepository::open(&root).await.unwrap();
        let statuses_after_commit = repository.read_summary_statuses().unwrap();
        for (id, reason, attempt, model) in [
            (
                "metadata-reason",
                "metadata_echo",
                "attempt-metadata-reason",
                "gemma-metadata-test",
            ),
            (
                "grounding-reason",
                "ungrounded_summary",
                "attempt-grounding-reason",
                "gemma-grounding-test",
            ),
        ] {
            let status = statuses_after_commit
                .get(&MemoryRepository::canonical_event_id(id))
                .unwrap();
            assert_eq!(status.reason.as_deref(), Some(reason));
            assert_eq!(status.attempt_id.as_deref(), Some(attempt));
            assert_eq!(status.model_id.as_deref(), Some(model));
            assert_eq!(
                status.prompt_version.as_deref(),
                Some(SUMMARY_PROMPT_VERSION)
            );
        }
        let journal_before_replay = std::fs::read_to_string(repository.paths().journal()).unwrap();

        // A malformed duplicate result from an older attempt is ignored before
        // payload validation and cannot replace either durable reason.
        let malformed = vec![SummaryBatchInput {
            entity_id: "metadata-reason".into(),
            summary: String::new(),
            embedding: Some(vec![f32::NAN; VECTOR_DIM as usize]),
            attempt_id: Some("attempt-malformed-replay".into()),
            model_id: Some("other-model".into()),
            prompt_version: Some("v1".into()),
        }];
        apply_summary_backfill_batch_with_statuses(&root, malformed, statuses)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(repository.paths().journal()).unwrap(),
            journal_before_replay,
            "malformed duplicate must not append another terminal attempt"
        );

        drop(repository);
        let reopened = MemoryRepository::open(&root).await.unwrap();
        let replayed = reopened.read_summary_statuses().unwrap();
        assert_eq!(
            replayed
                .get(&MemoryRepository::canonical_event_id("metadata-reason"))
                .and_then(|status| status.reason.as_deref()),
            Some("metadata_echo")
        );
        assert_eq!(
            replayed
                .get(&MemoryRepository::canonical_event_id("grounding-reason"))
                .and_then(|status| status.reason.as_deref()),
            Some("ungrounded_summary")
        );
        assert_eq!(
            get_memory_by_id(&root, "metadata-reason")
                .await
                .unwrap()
                .unwrap()
                .summary_status
                .as_deref(),
            Some(SUMMARY_STATUS_FALLBACK)
        );
        cleanup(&root);
    }
}

/// 全件を JSON ファイルにエクスポート
pub async fn export_lance_memories_json(
    root_dir: &Path,
    output_filename: Option<String>,
) -> Result<String, String> {
    validate_root_dir(root_dir)?;
    let data_dir = root_dir.join("data");
    validate_path_inside_root(root_dir, &data_dir, "Export data directory")?;
    let _ = std::fs::create_dir_all(&data_dir);

    let fname = output_filename.unwrap_or_else(|| "lance_export.json".to_string());
    validate_safe_component(&fname, "Export filename")?;
    let out_path = data_dir.join(&fname);
    validate_path_inside_root(root_dir, &out_path, "Export output")?;

    let res = list_memories(root_dir, None, None).await?;

    let json_str = serde_json::to_string_pretty(&res.memories)
        .map_err(|e| format!("JSON serialization error: {}", e))?;

    std::fs::write(&out_path, json_str)
        .map_err(|e| format!("Failed to write export JSON: {}", e))?;

    Ok(format!("Exported {} memories to {:?}", res.total, out_path))
}
