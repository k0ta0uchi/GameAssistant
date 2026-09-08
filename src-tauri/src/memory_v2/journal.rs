//! Durable, checksummed JSONL journal for memory-v2 operations.
//!
//! A batch is `begin`, zero or more `operation` records, and `commit`. Only
//! committed batches are returned by `RecoveryReport::replayable_records`.

use super::canonical::canonical_json;
use super::operation::{OperationEnvelope, OperationKind};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const JOURNAL_SCHEMA: &str = "gameassistant.memory_v2.journal";
pub const JOURNAL_VERSION: u32 = 2;
const COMPACT_JOURNAL_THRESHOLD: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    Begin,
    Operation,
    Commit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    pub schema: String,
    pub version: u32,
    pub sequence: u64,
    pub batch_id: String,
    pub state: JournalState,
    pub operation_id: String,
    pub operation_kind: String,
    pub payload: Value,
    pub checksum: String,
}

impl JournalRecord {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn state(&self) -> JournalState {
        self.state
    }
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn operation_kind(&self) -> &str {
        &self.operation_kind
    }
    pub fn payload(&self) -> &Value {
        &self.payload
    }
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryClassification {
    Committed,
    UnmatchedIntent,
    NoOpRetry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationClassification {
    pub operation_id: String,
    pub classification: RecoveryClassification,
    pub intent_sequence: Option<u64>,
    pub commit_sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryReport {
    // Journal payloads can be large (the compatibility import may contain
    // hundreds of megabytes of JSON).  Keep the recovered record objects
    // behind per-record Arcs so cloning a report for a cache lookup only
    // clones a small vector of pointers instead of every payload.
    records: Vec<Arc<JournalRecord>>,
    classifications: Vec<Arc<OperationClassification>>,
    incomplete_final_line: bool,
    uncommitted_final_batch: bool,
    next_sequence: u64,
    truncate_at: u64,
}

#[derive(Clone)]
struct RecoveryCache {
    file_len: u64,
    modified: Option<SystemTime>,
    report: RecoveryReport,
}

/// A bounded-memory journal view used by the production repository.  The
/// full `RecoveryReport` intentionally remains available for compatibility,
/// but materializing every historical JSON payload is prohibitively expensive
/// for a large import.  This index validates every frame while retaining only
/// the metadata needed for idempotency, summary status decisions, and the
/// append path.
#[derive(Clone, Debug)]
pub(crate) struct JournalIndex {
    pub(crate) file_len: u64,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) next_sequence: u64,
    pub(crate) truncate_at: u64,
    pub(crate) incomplete_final_line: bool,
    pub(crate) uncommitted_final_batch: bool,
    pub(crate) operation_ids: HashSet<String>,
    pub(crate) committed_operation_ids: HashSet<String>,
    pub(crate) committed_batches: HashSet<String>,
    pub(crate) committed_batch_sequences: HashMap<String, u64>,
    pub(crate) batch_ids: HashSet<String>,
    pub(crate) operations: HashMap<String, IndexedOperation>,
    pub(crate) raw_events: HashMap<String, IndexedRawEvent>,
    pub(crate) summary_statuses: Vec<IndexedSummaryStatus>,
    pub(crate) summary_fact_deletes: Vec<IndexedSummaryFactDelete>,
    pub(crate) summary_embedding_ids: HashSet<String>,
    pub(crate) summary_status_error: Option<String>,
    pub(crate) summary_fact_delete_error: Option<String>,
    pub(crate) summary_embedding_error: Option<String>,
    pub(crate) open_batch: Option<IndexedBatch>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedOperation {
    pub(crate) operation_id: String,
    pub(crate) batch_id: String,
    pub(crate) operation_kind: String,
    pub(crate) payload_digest: String,
    pub(crate) sequence: u64,
    /// Byte offset of the operation frame in the journal.  The compact index
    /// deliberately omits the payload, but a stale manifest still needs to
    /// replay only the missing operations without loading the whole journal.
    /// Incremental in-process updates may leave this unset; a fresh index
    /// rebuilt from disk always records it.
    pub(crate) offset: Option<u64>,
    pub(crate) raw_event: Option<IndexedRawEvent>,
    pub(crate) summary_status: Option<IndexedSummaryStatus>,
    pub(crate) summary_status_error: Option<String>,
    pub(crate) summary_fact_delete: Option<IndexedSummaryFactDelete>,
    pub(crate) summary_fact_delete_error: Option<String>,
    pub(crate) summary_embedding: Option<IndexedSummaryEmbedding>,
    pub(crate) summary_embedding_error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedBatch {
    pub(crate) batch_id: String,
    pub(crate) begin_offset: u64,
    pub(crate) operations: Vec<IndexedOperation>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedRawEvent {
    pub(crate) entity_id: String,
    pub(crate) operation_id: String,
    pub(crate) fingerprint: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedSummaryStatus {
    pub(crate) entity_id: String,
    pub(crate) event_id: Option<String>,
    pub(crate) status: String,
    pub(crate) reason: Option<String>,
    pub(crate) model_id: Option<String>,
    pub(crate) prompt_version: Option<String>,
    pub(crate) attempt_id: Option<String>,
    pub(crate) lease_expires_at: Option<String>,
    pub(crate) sequence: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedSummaryFactDelete {
    pub(crate) entity_id: String,
    pub(crate) sequence: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedSummaryEmbedding {
    pub(crate) entity_id: String,
    pub(crate) retract: bool,
}

impl RecoveryReport {
    pub fn records(&self) -> &[Arc<JournalRecord>] {
        &self.records
    }
    pub fn classifications(&self) -> &[Arc<OperationClassification>] {
        &self.classifications
    }
    pub fn incomplete_final_line(&self) -> bool {
        self.incomplete_final_line
    }
    pub fn uncommitted_final_batch(&self) -> bool {
        self.uncommitted_final_batch
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
    pub fn replayable_records(&self) -> Vec<JournalRecord> {
        self.replayable_record_refs()
            .into_iter()
            .map(|record| record.clone())
            .collect()
    }

    /// Borrow the committed operation records without cloning their JSON
    /// payloads.  Recovery callers that only need to inspect or decode a
    /// record should use this method; the owned `replayable_records` facade is
    /// retained for compatibility with mutation APIs.
    pub fn replayable_record_refs(&self) -> Vec<&JournalRecord> {
        let committed: BTreeSet<&str> = self
            .classifications
            .iter()
            .filter(|item| item.classification == RecoveryClassification::Committed)
            .map(|item| item.operation_id.as_str())
            .collect();
        let mut seen = BTreeSet::new();
        self.records
            .iter()
            .filter(|record| {
                record.state == JournalState::Operation
                    && committed.contains(record.operation_id.as_str())
                    && seen.insert(record.operation_id.as_str())
            })
            .map(Arc::as_ref)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended { sequence: u64 },
    NoOpRetry { sequence: u64 },
}

#[derive(Debug)]
pub enum JournalError {
    Io(io::Error),
    MalformedCompleteLine {
        line: usize,
        message: String,
    },
    UnknownState {
        line: usize,
        state: String,
    },
    NonCanonicalRecord {
        line: usize,
    },
    ChecksumMismatch {
        line: usize,
    },
    SequenceMismatch {
        line: usize,
        expected: u64,
        actual: u64,
    },
    DuplicateOperation {
        operation_id: String,
    },
    ConflictingOperation {
        operation_id: String,
    },
    MissingIntent {
        operation_id: String,
    },
    InvalidOperationId,
    InvalidOperationKind,
    InvalidBatchId,
    InvalidBatchState {
        batch_id: String,
    },
    MidFileCorruption {
        line: usize,
        message: String,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "journal I/O error: {e}"),
            Self::MalformedCompleteLine { line, message } => {
                write!(f, "malformed journal line {line}: {message}")
            }
            Self::UnknownState { line, state } => {
                write!(f, "unknown journal state on line {line}: {state}")
            }
            Self::NonCanonicalRecord { line } => {
                write!(f, "non-canonical journal record on line {line}")
            }
            Self::ChecksumMismatch { line } => {
                write!(f, "journal checksum mismatch on line {line}")
            }
            Self::SequenceMismatch {
                line,
                expected,
                actual,
            } => write!(
                f,
                "journal sequence mismatch on line {line}: expected {expected}, got {actual}"
            ),
            Self::DuplicateOperation { operation_id } => {
                write!(f, "duplicate journal operation: {operation_id}")
            }
            Self::ConflictingOperation { operation_id } => {
                write!(f, "conflicting journal operation: {operation_id}")
            }
            Self::MissingIntent { operation_id } => write!(f, "batch has no begin: {operation_id}"),
            Self::InvalidOperationId => write!(f, "operation ID must not be empty"),
            Self::InvalidOperationKind => write!(f, "operation kind must not be empty"),
            Self::InvalidBatchId => write!(f, "batch ID must not be empty"),
            Self::InvalidBatchState { batch_id } => write!(f, "invalid batch state: {batch_id}"),
            Self::MidFileCorruption { line, message } => {
                write!(f, "mid-file journal corruption on line {line}: {message}")
            }
        }
    }
}
impl std::error::Error for JournalError {}
impl From<io::Error> for JournalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct Journal {
    path: PathBuf,
    lock_path: PathBuf,
    file: Arc<Mutex<File>>,
    recovery_cache: Arc<Mutex<Option<RecoveryCache>>>,
    index_cache: Arc<Mutex<Option<Arc<JournalIndex>>>>,
    #[cfg(test)]
    read_report_count: Arc<AtomicUsize>,
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Journal")
            .field("path", &self.path)
            .field("lock_path", &self.lock_path)
            .finish()
    }
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let lock_path = path.with_extension("lock");
        Self::open_with_lock(path, lock_path)
    }
    pub fn open_with_lock(
        path: impl AsRef<Path>,
        lock_path: impl AsRef<Path>,
    ) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let lock_path = lock_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        Ok(Self {
            path,
            lock_path,
            file: Arc::new(Mutex::new(file)),
            recovery_cache: Arc::new(Mutex::new(None)),
            index_cache: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            read_report_count: Arc::new(AtomicUsize::new(0)),
        })
    }
    pub fn from_paths(paths: &super::paths::MemoryPaths) -> Result<Self, JournalError> {
        Self::open_with_lock(paths.journal(), paths.lock())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn begin_batch(&self, batch_id: &str) -> Result<AppendOutcome, JournalError> {
        self.append_new(
            JournalState::Begin,
            batch_id,
            batch_id,
            "batch_begin",
            json!({"batch_id": batch_id}),
        )
    }
    pub fn append_operation(
        &self,
        batch_id: &str,
        operation_id: &str,
        kind: OperationKind,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        verify_operation_payload(operation_id, kind, &payload)?;
        self.append_new(
            JournalState::Operation,
            batch_id,
            operation_id,
            kind.as_str(),
            payload,
        )
    }
    pub fn append_operation_envelope(
        &self,
        batch_id: &str,
        envelope: &OperationEnvelope,
    ) -> Result<AppendOutcome, JournalError> {
        envelope
            .verify_operation_id()
            .map_err(|_| JournalError::ConflictingOperation {
                operation_id: envelope.operation_id().to_string(),
            })?;
        let payload = serde_json::to_value(envelope).map_err(|error| {
            JournalError::MalformedCompleteLine {
                line: 0,
                message: error.to_string(),
            }
        })?;
        self.append_operation(batch_id, envelope.operation_id(), envelope.kind(), payload)
    }
    pub fn commit_batch(&self, batch_id: &str) -> Result<AppendOutcome, JournalError> {
        let _lock = FileLock::acquire(&self.lock_path)?;
        // Incremental batches remain open between begin/operation/commit.  A
        // normal append must only remove a torn final JSON line; removing the
        // whole uncommitted batch here would make the public incremental API
        // impossible to use after `begin_batch`.
        let report = self.prepare_for_incremental_append_locked()?;
        if let Some(existing) = report
            .records
            .iter()
            .find(|r| r.batch_id == batch_id && r.state == JournalState::Commit)
        {
            return Ok(AppendOutcome::NoOpRetry {
                sequence: existing.sequence,
            });
        }
        ensure_open_batch(&report, batch_id)?;
        let ids: Vec<&str> = report
            .records
            .iter()
            .filter(|r| r.batch_id == batch_id && r.state == JournalState::Operation)
            .map(|r| r.operation_id.as_str())
            .collect();
        self.append_record(
            JournalState::Commit,
            batch_id,
            batch_id,
            "batch_commit",
            json!({"operations": ids}),
            report.next_sequence,
        )
    }
    pub fn write_batch(
        &self,
        batch_id: &str,
        operations: &[(String, OperationKind, Value)],
    ) -> Result<Vec<AppendOutcome>, JournalError> {
        validate_id(batch_id, true)?;
        for (id, kind, payload) in operations {
            validate_id(id, false)?;
            verify_operation_payload(id, *kind, payload)?;
        }
        if std::fs::metadata(&self.path)
            .map(|metadata| metadata.len() >= COMPACT_JOURNAL_THRESHOLD)
            .unwrap_or(false)
        {
            return self.write_batch_indexed(batch_id, operations);
        }
        let _lock = FileLock::acquire(&self.lock_path)?;
        let report = self.prepare_for_append_locked()?;
        if let Some(existing) = report
            .records
            .iter()
            .find(|r| r.batch_id == batch_id && r.state == JournalState::Commit)
        {
            return Ok(vec![AppendOutcome::NoOpRetry {
                sequence: existing.sequence,
            }]);
        }
        if report.records.iter().any(|r| r.batch_id == batch_id) {
            return Err(JournalError::InvalidBatchState {
                batch_id: batch_id.to_string(),
            });
        }
        let mut outcomes = Vec::with_capacity(operations.len() + 2);
        let mut appended_records = Vec::with_capacity(operations.len() + 2);
        let mut next = report.next_sequence;
        let (outcome, record) = self.append_record_unflushed_with_record(
            JournalState::Begin,
            batch_id,
            batch_id,
            "batch_begin",
            json!({"batch_id": batch_id}),
            next,
        )?;
        outcomes.push(outcome);
        appended_records.push(record);
        next += 1;
        let mut seen = BTreeMap::<String, (String, Value, u64)>::new();
        let mut ordered_ids = Vec::with_capacity(operations.len());
        for (id, kind, payload) in operations {
            let canonical = canonical_value(payload.clone())?;
            if let Some((old_kind, old_payload, old_sequence)) = seen.get(id) {
                if old_kind != kind.as_str() || old_payload != &canonical {
                    return Err(JournalError::ConflictingOperation {
                        operation_id: id.clone(),
                    });
                }
                outcomes.push(AppendOutcome::NoOpRetry {
                    sequence: *old_sequence,
                });
                continue;
            }
            if let Some(existing) = report
                .records
                .iter()
                .find(|r| r.state == JournalState::Operation && r.operation_id == *id)
            {
                if existing.batch_id != batch_id {
                    return Err(JournalError::DuplicateOperation {
                        operation_id: id.clone(),
                    });
                }
                if existing.operation_kind != kind.as_str() || existing.payload != canonical {
                    return Err(JournalError::ConflictingOperation {
                        operation_id: id.clone(),
                    });
                }
                seen.insert(
                    id.clone(),
                    (kind.as_str().to_string(), canonical, existing.sequence),
                );
                ordered_ids.push(id.as_str());
                outcomes.push(AppendOutcome::NoOpRetry {
                    sequence: existing.sequence,
                });
                continue;
            }
            let (outcome, record) = self.append_record_unflushed_with_record(
                JournalState::Operation,
                batch_id,
                id,
                kind.as_str(),
                canonical.clone(),
                next,
            )?;
            appended_records.push(record);
            seen.insert(id.clone(), (kind.as_str().to_string(), canonical, next));
            ordered_ids.push(id.as_str());
            outcomes.push(outcome);
            next += 1;
        }
        let (outcome, record) = self.append_record_unflushed_with_record(
            JournalState::Commit,
            batch_id,
            batch_id,
            "batch_commit",
            json!({"operations": ordered_ids}),
            next,
        )?;
        outcomes.push(outcome);
        appended_records.push(record);
        // A bounded atomic batch is written as one append transaction.  Sync
        // once after all frames instead of fsyncing every operation; the
        // journal's torn-tail recovery handles an interrupted final append.
        self.file.lock().sync_all()?;
        self.store_appended_batch(report, appended_records)?;
        Ok(outcomes)
    }

    fn write_batch_indexed(
        &self,
        batch_id: &str,
        operations: &[(String, OperationKind, Value)],
    ) -> Result<Vec<AppendOutcome>, JournalError> {
        validate_id(batch_id, true)?;
        let _lock = FileLock::acquire(&self.lock_path)?;
        let metadata = std::fs::metadata(&self.path)?;
        let mut index = {
            let mut cache = self.index_cache.lock();
            let cached = cache.take();
            match cached.filter(|index| {
                index.file_len == metadata.len() && index.modified == metadata.modified().ok()
            }) {
                Some(index) => index,
                None => Arc::new(self.read_index()?),
            }
        };

        // A torn tail or an incomplete final batch is discarded before a new
        // atomic transaction, matching the full recovery path.
        if index.truncate_at < metadata.len() {
            let file = OpenOptions::new().write(true).open(&self.path)?;
            file.set_len(index.truncate_at)?;
            file.sync_all()?;
            self.invalidate_cache();
            self.invalidate_index();
            index = Arc::new(self.read_index()?);
        }
        if let Some(sequence) = index.committed_batch_sequences.get(batch_id) {
            let outcome = AppendOutcome::NoOpRetry {
                sequence: *sequence,
            };
            *self.index_cache.lock() = Some(index);
            return Ok(vec![outcome]);
        }
        if index.batch_ids.contains(batch_id) {
            *self.index_cache.lock() = Some(index);
            return Err(JournalError::InvalidBatchState {
                batch_id: batch_id.to_string(),
            });
        }

        let mut outcomes = Vec::with_capacity(operations.len() + 2);
        let mut appended_records = Vec::with_capacity(operations.len() + 2);
        let mut next = index.next_sequence;
        let (outcome, record) = self.append_record_unflushed_with_record(
            JournalState::Begin,
            batch_id,
            batch_id,
            "batch_begin",
            json!({"batch_id": batch_id}),
            next,
        )?;
        outcomes.push(outcome);
        appended_records.push(record);
        next = next.saturating_add(1);

        let mut seen = HashMap::<String, (String, String, u64)>::new();
        let mut ordered_ids = Vec::with_capacity(operations.len());
        for (id, kind, payload) in operations {
            let canonical = canonical_value(payload.clone())?;
            let digest = canonical_payload_digest(&canonical)?;
            if let Some((old_kind, old_digest, old_sequence)) = seen.get(id) {
                if old_kind != kind.as_str() || old_digest != &digest {
                    return Err(JournalError::ConflictingOperation {
                        operation_id: id.clone(),
                    });
                }
                outcomes.push(AppendOutcome::NoOpRetry {
                    sequence: *old_sequence,
                });
                continue;
            }
            if let Some(existing) = index.operations.get(id) {
                if existing.batch_id != batch_id {
                    let result = if existing.operation_kind == kind.as_str()
                        && existing.payload_digest == digest
                    {
                        JournalError::DuplicateOperation {
                            operation_id: id.clone(),
                        }
                    } else {
                        JournalError::ConflictingOperation {
                            operation_id: id.clone(),
                        }
                    };
                    return Err(result);
                }
                if existing.operation_kind != kind.as_str() || existing.payload_digest != digest {
                    return Err(JournalError::ConflictingOperation {
                        operation_id: id.clone(),
                    });
                }
                seen.insert(
                    id.clone(),
                    (kind.as_str().to_string(), digest, existing.sequence),
                );
                ordered_ids.push(id.as_str());
                outcomes.push(AppendOutcome::NoOpRetry {
                    sequence: existing.sequence,
                });
                continue;
            }
            let (outcome, record) = self.append_record_unflushed_with_record(
                JournalState::Operation,
                batch_id,
                id,
                kind.as_str(),
                canonical,
                next,
            )?;
            outcomes.push(outcome);
            appended_records.push(record);
            seen.insert(id.clone(), (kind.as_str().to_string(), digest, next));
            ordered_ids.push(id.as_str());
            next = next.saturating_add(1);
        }
        let (outcome, record) = self.append_record_unflushed_with_record(
            JournalState::Commit,
            batch_id,
            batch_id,
            "batch_commit",
            json!({"operations": ordered_ids}),
            next,
        )?;
        outcomes.push(outcome);
        appended_records.push(record);
        self.file.lock().sync_all()?;

        let index_mut = Arc::make_mut(&mut index);
        update_index_after_batch(index_mut, &self.path, appended_records)?;
        *self.index_cache.lock() = Some(index);
        Ok(outcomes)
    }

    // Phase 2A compatibility facade. It writes a two-record batch, while new
    // callers should use explicit begin/operation/commit records.
    pub fn append_intent(
        &self,
        operation_id: &str,
        kind: OperationKind,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        validate_id(operation_id, false)?;
        verify_operation_payload(operation_id, kind, &payload)?;
        Err(JournalError::InvalidBatchState {
            batch_id: operation_id.to_string(),
        })
    }
    pub fn append_commit(
        &self,
        operation_id: &str,
        kind: OperationKind,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        validate_id(operation_id, false)?;
        verify_operation_payload(operation_id, kind, &payload)?;
        Err(JournalError::InvalidBatchState {
            batch_id: operation_id.to_string(),
        })
    }
    pub fn write_intent(
        &self,
        id: &str,
        kind: OperationKind,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        self.append_intent(id, kind, payload)
    }
    pub fn write_commit(
        &self,
        id: &str,
        kind: OperationKind,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        self.append_commit(id, kind, payload)
    }
    pub fn recover(&self) -> Result<RecoveryReport, JournalError> {
        let _lock = FileLock::acquire(&self.lock_path)?;
        let report = self.load_report()?;
        self.truncate_report(&report)?;
        let report = if report.incomplete_final_line || report.uncommitted_final_batch {
            let mut clean = self.read_report()?;
            clean.incomplete_final_line = report.incomplete_final_line;
            clean.uncommitted_final_batch = report.uncommitted_final_batch;
            clean
        } else {
            report
        };
        self.store_report(&report)?;
        Ok(report)
    }

    /// Recover only the validated journal metadata needed by the production
    /// repository.  This scans the JSONL stream one line at a time, so a
    /// several-hundred-megabyte compatibility journal does not become a
    /// multi-gigabyte graph of `serde_json::Value` allocations in memory.
    pub(crate) fn recover_index(&self) -> Result<Arc<JournalIndex>, JournalError> {
        let _lock = FileLock::acquire(&self.lock_path)?;
        if let Some(index) = self.cached_index()? {
            return Ok(index);
        }
        let mut index = self.read_index()?;
        let size = std::fs::metadata(&self.path)?.len();
        if index.truncate_at < size {
            let file = OpenOptions::new().write(true).open(&self.path)?;
            file.set_len(index.truncate_at)?;
            file.sync_all()?;
            self.invalidate_cache();
            index = self.read_index()?;
        }
        let index = Arc::new(index);
        *self.index_cache.lock() = Some(index.clone());
        Ok(index)
    }

    /// Read operation frames at offsets supplied by a previously validated
    /// compact index.  Keeping this seek/read step in the journal layer lets a
    /// large-journal reconciliation materialize bounded chunks without
    /// rebuilding a `RecoveryReport` containing every historical payload.
    pub(crate) fn read_operation_records_at_offsets(
        &self,
        offsets: &[u64],
    ) -> Result<Vec<JournalRecord>, JournalError> {
        if offsets.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = FileLock::acquire(&self.lock_path)?;
        let file = File::open(&self.path)?;
        let mut reader = BufReader::new(file);
        let mut line_bytes = Vec::new();
        let mut records = Vec::with_capacity(offsets.len());
        for offset in offsets {
            reader.seek(SeekFrom::Start(*offset))?;
            line_bytes.clear();
            let read = reader.read_until(b'\n', &mut line_bytes)?;
            if read == 0 || !line_bytes.ends_with(b"\n") {
                return Err(JournalError::MalformedCompleteLine {
                    line: 0,
                    message: format!("missing operation frame at offset {offset}"),
                });
            }
            let mut raw = &line_bytes[..line_bytes.len() - 1];
            raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let line =
                std::str::from_utf8(raw).map_err(|error| JournalError::MalformedCompleteLine {
                    line: 0,
                    message: error.to_string(),
                })?;
            let record = parse_record(line, 0)?;
            if record.state != JournalState::Operation {
                return Err(JournalError::MidFileCorruption {
                    line: 0,
                    message: format!("indexed offset {offset} is not an operation frame"),
                });
            }
            records.push(record);
        }
        Ok(records)
    }

    fn cached_index(&self) -> Result<Option<Arc<JournalIndex>>, JournalError> {
        let metadata = std::fs::metadata(&self.path)?;
        let file_len = metadata.len();
        let modified = metadata.modified().ok();
        Ok(self
            .index_cache
            .lock()
            .as_ref()
            .filter(|index| index.file_len == file_len && index.modified == modified)
            .cloned())
    }

    fn read_index(&self) -> Result<JournalIndex, JournalError> {
        let metadata = std::fs::metadata(&self.path)?;
        let file_len = metadata.len();
        let modified = metadata.modified().ok();
        let file = File::open(&self.path)?;
        let mut reader = BufReader::new(file);
        let mut line_bytes = Vec::new();
        let mut offset = 0u64;
        let mut line_number = 0usize;
        let mut complete_len = 0u64;
        let mut incomplete_final_line = false;
        let mut expected = 1u64;
        let mut current: Option<IndexedBatch> = None;
        let mut operation_ids = HashSet::new();
        let mut committed_operation_ids = HashSet::new();
        let mut committed_batches = HashSet::new();
        let mut committed_batch_sequences = HashMap::new();
        let mut batch_ids = HashSet::new();
        let mut operations = HashMap::new();
        let mut raw_events = HashMap::new();
        let mut summary_statuses = Vec::new();
        let mut summary_fact_deletes = Vec::new();
        let mut summary_embedding_ids = HashSet::new();
        let mut summary_status_error = None;
        let mut summary_fact_delete_error = None;
        let mut summary_embedding_error = None;

        loop {
            line_bytes.clear();
            let read = reader.read_until(b'\n', &mut line_bytes)?;
            if read == 0 {
                break;
            }
            let line_start = offset;
            offset = offset.saturating_add(read as u64);
            if !line_bytes.ends_with(b"\n") {
                incomplete_final_line = true;
                break;
            }
            complete_len = offset;
            let mut raw = &line_bytes[..line_bytes.len() - 1];
            raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let line =
                std::str::from_utf8(raw).map_err(|error| JournalError::MalformedCompleteLine {
                    line: line_number + 1,
                    message: error.to_string(),
                })?;
            line_number += 1;
            let record = parse_record(line, line_number)?;
            if record.sequence != expected {
                return Err(JournalError::SequenceMismatch {
                    line: line_number,
                    expected,
                    actual: record.sequence,
                });
            }
            expected = expected.saturating_add(1);
            validate_record_frame(
                record.state,
                &record.batch_id,
                &record.operation_id,
                &record.operation_kind,
                &record.payload,
                Some(line_number),
            )?;
            match record.state {
                JournalState::Begin => {
                    if current.is_some() {
                        return Err(JournalError::MidFileCorruption {
                            line: line_number,
                            message: "nested batch".into(),
                        });
                    }
                    if committed_batches.contains(&record.batch_id) {
                        return Err(JournalError::MidFileCorruption {
                            line: line_number,
                            message: "batch repeated".into(),
                        });
                    }
                    batch_ids.insert(record.batch_id.clone());
                    current = Some(IndexedBatch {
                        batch_id: record.batch_id,
                        begin_offset: line_start,
                        operations: Vec::new(),
                    });
                }
                JournalState::Operation => {
                    let Some(batch) = current.as_mut() else {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id,
                        });
                    };
                    if batch.batch_id != record.batch_id {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id,
                        });
                    }
                    if !operation_ids.insert(record.operation_id.clone()) {
                        return Err(JournalError::DuplicateOperation {
                            operation_id: record.operation_id,
                        });
                    }
                    let metadata = indexed_operation_metadata(&record)?;
                    let operation = IndexedOperation {
                        operation_id: record.operation_id,
                        batch_id: record.batch_id,
                        operation_kind: record.operation_kind,
                        payload_digest: canonical_payload_digest(&record.payload)?,
                        sequence: record.sequence,
                        offset: Some(line_start),
                        raw_event: metadata.raw_event,
                        summary_status: metadata.summary_status,
                        summary_status_error: metadata.summary_status_error,
                        summary_fact_delete: metadata.summary_fact_delete,
                        summary_fact_delete_error: metadata.summary_fact_delete_error,
                        summary_embedding: metadata.summary_embedding,
                        summary_embedding_error: metadata.summary_embedding_error,
                    };
                    batch.operations.push(operation);
                }
                JournalState::Commit => {
                    let Some(batch) = current.take() else {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id,
                        });
                    };
                    if batch.batch_id != record.batch_id {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id,
                        });
                    }
                    validate_commit_operation_ids(&record, &batch.operations, line_number)?;
                    committed_batches.insert(batch.batch_id.clone());
                    committed_batch_sequences.insert(batch.batch_id.clone(), record.sequence);
                    for operation in &batch.operations {
                        committed_operation_ids.insert(operation.operation_id.clone());
                        if let Some(raw_event) = &operation.raw_event {
                            raw_events
                                .entry(raw_event.entity_id.clone())
                                .or_insert_with(|| raw_event.clone());
                        }
                        if let Some(status) = &operation.summary_status {
                            summary_statuses.push(status.clone());
                        }
                        if summary_status_error.is_none() {
                            summary_status_error = operation.summary_status_error.clone();
                        }
                        if let Some(delete) = &operation.summary_fact_delete {
                            summary_fact_deletes.push(delete.clone());
                        }
                        if summary_fact_delete_error.is_none() {
                            summary_fact_delete_error = operation.summary_fact_delete_error.clone();
                        }
                        if let Some(embedding) = &operation.summary_embedding {
                            let canonical = embedding.entity_id.clone();
                            if embedding.retract {
                                summary_embedding_ids.remove(&canonical);
                            } else {
                                summary_embedding_ids.insert(canonical);
                            }
                        }
                        if summary_embedding_error.is_none() {
                            summary_embedding_error = operation.summary_embedding_error.clone();
                        }
                        operations.insert(operation.operation_id.clone(), operation.clone());
                    }
                }
            }
        }

        let truncate_at = current
            .as_ref()
            .map(|batch| batch.begin_offset)
            .unwrap_or(complete_len);
        let uncommitted_final_batch = current.is_some();
        Ok(JournalIndex {
            file_len,
            modified,
            next_sequence: expected,
            truncate_at,
            incomplete_final_line,
            uncommitted_final_batch,
            operation_ids,
            committed_operation_ids,
            committed_batches,
            committed_batch_sequences,
            batch_ids,
            operations,
            raw_events,
            summary_statuses,
            summary_fact_deletes,
            summary_embedding_ids,
            summary_status_error,
            summary_fact_delete_error,
            summary_embedding_error,
            open_batch: current,
        })
    }

    fn append_new(
        &self,
        state: JournalState,
        batch_id: &str,
        operation_id: &str,
        kind: &str,
        payload: Value,
    ) -> Result<AppendOutcome, JournalError> {
        validate_id(batch_id, true)?;
        validate_id(operation_id, false)?;
        if kind.trim().is_empty() {
            return Err(JournalError::InvalidOperationKind);
        }
        let _lock = FileLock::acquire(&self.lock_path)?;
        let report = self.prepare_for_incremental_append_locked()?;
        if state == JournalState::Begin {
            if report.uncommitted_final_batch
                && !report.records.iter().any(|r| r.batch_id == batch_id)
            {
                // A caller may continue the open batch, but starting another
                // one would create nested frames and make the journal
                // unrecoverable.  Atomic `write_batch` intentionally uses the
                // crash-recovery path when it needs to discard such a batch.
                return Err(JournalError::InvalidBatchState {
                    batch_id: batch_id.to_string(),
                });
            }
            if let Some(existing) = report.records.iter().find(|r| r.batch_id == batch_id) {
                if existing.state == JournalState::Commit {
                    return Ok(AppendOutcome::NoOpRetry {
                        sequence: existing.sequence,
                    });
                }
                if existing.state == JournalState::Begin
                    && existing.operation_kind == kind
                    && existing.payload == canonical_value(payload.clone())?
                {
                    return Ok(AppendOutcome::NoOpRetry {
                        sequence: existing.sequence,
                    });
                }
                return Err(JournalError::InvalidBatchState {
                    batch_id: batch_id.to_string(),
                });
            }
        } else if state == JournalState::Operation {
            // Batch membership is authoritative. Check it before considering
            // an operation-id retry so an id from another batch can never be
            // silently accepted as a no-op.
            ensure_open_batch(&report, batch_id)?;
            if let Some(existing) = report.records.iter().find(|r| {
                r.state == JournalState::Operation
                    && r.operation_id == operation_id
                    && r.batch_id == batch_id
            }) {
                if existing.operation_kind == kind
                    && existing.payload == canonical_value(payload.clone())?
                {
                    return Ok(AppendOutcome::NoOpRetry {
                        sequence: existing.sequence,
                    });
                }
                return Err(JournalError::ConflictingOperation {
                    operation_id: operation_id.to_string(),
                });
            }
            if report.records.iter().any(|r| {
                r.state == JournalState::Operation
                    && r.operation_id == operation_id
                    && r.batch_id != batch_id
            }) {
                return Err(JournalError::DuplicateOperation {
                    operation_id: operation_id.to_string(),
                });
            }
        }
        self.append_record(
            state,
            batch_id,
            operation_id,
            kind,
            payload,
            report.next_sequence,
        )
    }
    fn append_record(
        &self,
        state: JournalState,
        batch_id: &str,
        operation_id: &str,
        kind: &str,
        payload: Value,
        sequence: u64,
    ) -> Result<AppendOutcome, JournalError> {
        let outcome =
            self.append_record_unflushed(state, batch_id, operation_id, kind, payload, sequence)?;
        self.file.lock().sync_all()?;
        self.invalidate_cache();
        self.invalidate_index();
        Ok(outcome)
    }

    fn append_record_unflushed(
        &self,
        state: JournalState,
        batch_id: &str,
        operation_id: &str,
        kind: &str,
        payload: Value,
        sequence: u64,
    ) -> Result<AppendOutcome, JournalError> {
        self.append_record_unflushed_with_record(
            state,
            batch_id,
            operation_id,
            kind,
            payload,
            sequence,
        )
        .map(|(outcome, _)| outcome)
    }

    fn append_record_unflushed_with_record(
        &self,
        state: JournalState,
        batch_id: &str,
        operation_id: &str,
        kind: &str,
        payload: Value,
        sequence: u64,
    ) -> Result<(AppendOutcome, JournalRecord), JournalError> {
        validate_record_frame(state, batch_id, operation_id, kind, &payload, None)?;
        let record = make_record(
            sequence,
            state,
            batch_id,
            operation_id,
            kind.to_string(),
            canonical_value(payload)?,
        )?;
        let line =
            canonical_json(&serde_json::to_value(&record).expect("journal record is serializable"))
                .map_err(|e| JournalError::MalformedCompleteLine {
                    line: 0,
                    message: e.to_string(),
                })?;
        let mut file = self.file.lock();
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok((AppendOutcome::Appended { sequence }, record))
    }
    fn prepare_for_append_locked(&self) -> Result<RecoveryReport, JournalError> {
        let report = self.load_report()?;
        self.truncate_report(&report)?;
        if report.incomplete_final_line || report.uncommitted_final_batch {
            let report = self.read_report()?;
            self.store_report(&report)?;
            Ok(report)
        } else {
            Ok(report)
        }
    }
    fn prepare_for_incremental_append_locked(&self) -> Result<RecoveryReport, JournalError> {
        let report = self.load_report()?;
        if !report.incomplete_final_line {
            return Ok(report);
        }

        // `truncate_at` intentionally points at the beginning of an
        // uncommitted batch for crash recovery.  That is correct for
        // `recover()`/a new atomic write, but not for continuing an open
        // incremental batch.  Compute the end of the last complete line so
        // only the torn tail is discarded here.
        let bytes = std::fs::read(&self.path)?;
        let truncate_at = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0) as u64;
        let file = OpenOptions::new().write(true).open(&self.path)?;
        file.set_len(truncate_at)?;
        file.sync_all()?;
        self.invalidate_cache();
        self.invalidate_index();
        let report = self.read_report()?;
        self.store_report(&report)?;
        Ok(report)
    }
    fn truncate_report(&self, report: &RecoveryReport) -> Result<(), JournalError> {
        let size = std::fs::metadata(&self.path)?.len();
        if report.truncate_at < size {
            let file = OpenOptions::new().write(true).open(&self.path)?;
            file.set_len(report.truncate_at)?;
            file.sync_all()?;
            self.invalidate_cache();
            self.invalidate_index();
        }
        Ok(())
    }

    fn load_report(&self) -> Result<RecoveryReport, JournalError> {
        if let Some(report) = self.cached_report()? {
            return Ok(report);
        }
        let report = self.read_report()?;
        self.store_report(&report)?;
        Ok(report)
    }

    fn cached_report(&self) -> Result<Option<RecoveryReport>, JournalError> {
        let metadata = std::fs::metadata(&self.path)?;
        let file_len = metadata.len();
        let modified = metadata.modified().ok();
        Ok(self
            .recovery_cache
            .lock()
            .as_ref()
            .filter(|cache| cache.file_len == file_len && cache.modified == modified)
            .map(|cache| cache.report.clone()))
    }

    fn store_report(&self, report: &RecoveryReport) -> Result<(), JournalError> {
        let metadata = std::fs::metadata(&self.path)?;
        *self.recovery_cache.lock() = Some(RecoveryCache {
            file_len: metadata.len(),
            modified: metadata.modified().ok(),
            report: report.clone(),
        });
        Ok(())
    }

    fn invalidate_cache(&self) {
        *self.recovery_cache.lock() = None;
    }

    fn invalidate_index(&self) {
        *self.index_cache.lock() = None;
    }

    fn store_appended_batch(
        &self,
        mut report: RecoveryReport,
        appended: Vec<JournalRecord>,
    ) -> Result<(), JournalError> {
        if appended.is_empty() {
            return self.store_report(&report);
        }
        let commit_sequence = appended
            .iter()
            .find(|record| record.state == JournalState::Commit)
            .map(|record| record.sequence);
        let mut operation_count = 0usize;
        for record in &appended {
            if record.state == JournalState::Operation {
                operation_count += 1;
                report
                    .classifications
                    .push(Arc::new(OperationClassification {
                        operation_id: record.operation_id.clone(),
                        classification: RecoveryClassification::Committed,
                        intent_sequence: Some(record.sequence),
                        commit_sequence,
                    }));
            }
            report.records.push(Arc::new(record.clone()));
        }
        if operation_count == 0 {
            if let Some(begin) = appended
                .iter()
                .find(|record| record.state == JournalState::Begin)
            {
                report
                    .classifications
                    .push(Arc::new(OperationClassification {
                        operation_id: begin.operation_id.clone(),
                        classification: RecoveryClassification::Committed,
                        intent_sequence: Some(begin.sequence),
                        commit_sequence,
                    }));
            }
        }
        report
            .classifications
            .sort_by_key(|item| item.intent_sequence);
        report.incomplete_final_line = false;
        report.uncommitted_final_batch = false;
        report.next_sequence = appended
            .last()
            .map(|record| record.sequence.saturating_add(1))
            .unwrap_or(report.next_sequence);
        report.truncate_at = std::fs::metadata(&self.path)?.len();
        self.store_report(&report)
    }
    fn read_report(&self) -> Result<RecoveryReport, JournalError> {
        #[cfg(test)]
        self.read_report_count.fetch_add(1, Ordering::Relaxed);
        let mut bytes = Vec::new();
        File::open(&self.path)?.read_to_end(&mut bytes)?;
        let incomplete = !bytes.is_empty() && !bytes.ends_with(b"\n");
        let complete_len = if incomplete {
            bytes
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0)
        } else {
            bytes.len()
        };
        let mut records: Vec<Arc<JournalRecord>> = Vec::new();
        let mut starts = Vec::new();
        let mut offset = 0usize;
        for raw_with_newline in bytes[..complete_len].split_inclusive(|b| *b == b'\n') {
            let raw = &raw_with_newline[..raw_with_newline.len() - 1];
            if raw.is_empty() {
                return Err(JournalError::MalformedCompleteLine {
                    line: records.len() + 1,
                    message: "blank journal line".into(),
                });
            }
            let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let line = String::from_utf8(raw.to_vec()).map_err(|e| {
                JournalError::MalformedCompleteLine {
                    line: records.len() + 1,
                    message: e.to_string(),
                }
            })?;
            starts.push(offset as u64);
            records.push(Arc::new(parse_record(&line, records.len() + 1)?));
            offset += raw_with_newline.len();
        }
        let mut expected = 1;
        let mut current: Option<(&str, usize)> = None;
        let mut committed = BTreeSet::new();
        let mut batches = BTreeMap::<String, Vec<usize>>::new();
        let mut operation_batches = BTreeMap::<String, String>::new();
        for (index, record) in records.iter().enumerate() {
            if record.sequence != expected {
                return Err(JournalError::SequenceMismatch {
                    line: index + 1,
                    expected,
                    actual: record.sequence,
                });
            }
            expected += 1;
            validate_record_frame(
                record.state,
                &record.batch_id,
                &record.operation_id,
                &record.operation_kind,
                &record.payload,
                Some(index + 1),
            )?;
            match record.state {
                JournalState::Begin => {
                    if current.is_some() {
                        return Err(JournalError::MidFileCorruption {
                            line: index + 1,
                            message: "nested batch".into(),
                        });
                    }
                    if committed.contains(&record.batch_id) {
                        return Err(JournalError::MidFileCorruption {
                            line: index + 1,
                            message: "batch repeated".into(),
                        });
                    }
                    current = Some((&record.batch_id, index));
                }
                JournalState::Operation => {
                    if current.map(|(id, _)| id) != Some(record.batch_id.as_str()) {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id.clone(),
                        });
                    }
                    if operation_batches
                        .insert(record.operation_id.clone(), record.batch_id.clone())
                        .is_some()
                    {
                        return Err(JournalError::DuplicateOperation {
                            operation_id: record.operation_id.clone(),
                        });
                    }
                }
                JournalState::Commit => {
                    if current.map(|(id, _)| id) != Some(record.batch_id.as_str()) {
                        return Err(JournalError::MissingIntent {
                            operation_id: record.operation_id.clone(),
                        });
                    }
                    validate_commit_membership(record, &records[..index], index + 1)?;
                    committed.insert(record.batch_id.clone());
                    current = None;
                }
            }
            batches
                .entry(record.batch_id.clone())
                .or_default()
                .push(index);
        }
        let uncommitted_start = current.map(|(_, index)| starts[index]);
        let truncate_at = uncommitted_start.unwrap_or(complete_len as u64);
        let mut classifications: Vec<Arc<OperationClassification>> = Vec::new();
        for (batch_id, indices) in &batches {
            let is_committed = committed.contains(batch_id);
            let operation_indices: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|i| records[*i].state == JournalState::Operation)
                .collect();
            if operation_indices.is_empty() {
                if let Some(begin) = indices
                    .iter()
                    .find(|i| records[**i].state == JournalState::Begin)
                {
                    classifications.push(Arc::new(OperationClassification {
                        operation_id: records[*begin].operation_id.clone(),
                        classification: if is_committed {
                            RecoveryClassification::Committed
                        } else {
                            RecoveryClassification::UnmatchedIntent
                        },
                        intent_sequence: Some(records[*begin].sequence),
                        commit_sequence: indices
                            .iter()
                            .find(|i| records[**i].state == JournalState::Commit)
                            .map(|i| records[*i].sequence),
                    }));
                }
            } else {
                for i in operation_indices {
                    classifications.push(Arc::new(OperationClassification {
                        operation_id: records[i].operation_id.clone(),
                        classification: if is_committed {
                            RecoveryClassification::Committed
                        } else {
                            RecoveryClassification::UnmatchedIntent
                        },
                        intent_sequence: Some(records[i].sequence),
                        commit_sequence: indices
                            .iter()
                            .find(|j| records[**j].state == JournalState::Commit)
                            .map(|j| records[*j].sequence),
                    }));
                }
            }
        }
        classifications.sort_by_key(|item| item.intent_sequence);
        Ok(RecoveryReport {
            records,
            classifications,
            incomplete_final_line: incomplete,
            uncommitted_final_batch: uncommitted_start.is_some(),
            next_sequence: expected,
            truncate_at,
        })
    }
}

#[derive(Default)]
struct IndexedOperationMetadata {
    raw_event: Option<IndexedRawEvent>,
    summary_status: Option<IndexedSummaryStatus>,
    summary_status_error: Option<String>,
    summary_fact_delete: Option<IndexedSummaryFactDelete>,
    summary_fact_delete_error: Option<String>,
    summary_embedding: Option<IndexedSummaryEmbedding>,
    summary_embedding_error: Option<String>,
}

fn canonical_payload_digest(payload: &Value) -> Result<String, JournalError> {
    let canonical =
        canonical_json(payload).map_err(|error| JournalError::MalformedCompleteLine {
            line: 0,
            message: error.to_string(),
        })?;
    Ok(sha256_hex(canonical.as_bytes()))
}

/// Stable identity/content fingerprint for a raw-event envelope.  Historical
/// compatibility imports may differ only in their timestamp; callers use this
/// digest to recognize that safe idempotent retry without retaining the full
/// event payload in the compact journal index.
pub(crate) fn raw_event_fingerprint(payload: &Value) -> Option<String> {
    let object = payload.as_object()?;
    let fingerprint = json!({
        "event_id": object.get("event_id")?,
        "subject": object.get("subject")?,
        "event_type": object.get("event_type")?,
        "source": object.get("source")?,
        "content": object.get("content")?,
    });
    canonical_payload_digest(&fingerprint).ok()
}

fn indexed_operation_metadata(
    record: &JournalRecord,
) -> Result<IndexedOperationMetadata, JournalError> {
    let mut metadata = IndexedOperationMetadata::default();
    let envelope: OperationEnvelope =
        serde_json::from_value(record.payload.clone()).map_err(|e| {
            JournalError::MalformedCompleteLine {
                line: 0,
                message: e.to_string(),
            }
        })?;
    match record.operation_kind.as_str() {
        "raw_event" => {
            let payload = envelope.payload();
            metadata.raw_event = Some(IndexedRawEvent {
                operation_id: record.operation_id.clone(),
                fingerprint: raw_event_fingerprint(payload),
                entity_id: envelope.entity_id().to_string(),
            });
        }
        "summary_status" => match indexed_summary_status(&envelope, record.sequence()) {
            Ok(status) => metadata.summary_status = Some(status),
            Err(error) => metadata.summary_status_error = Some(error),
        },
        "fact" => {
            let payload = envelope.payload();
            if payload.get("action").and_then(Value::as_str) == Some("delete") {
                match indexed_summary_fact_delete(payload, record.sequence()) {
                    Ok(Some(delete)) => metadata.summary_fact_delete = Some(delete),
                    Ok(None) => {}
                    Err(error) => metadata.summary_fact_delete_error = Some(error),
                }
            }
        }
        "embedding" => {
            let payload = envelope.payload();
            match indexed_summary_embedding(payload) {
                Ok(embedding) => metadata.summary_embedding = embedding,
                Err(error) => metadata.summary_embedding_error = Some(error),
            }
        }
        _ => {}
    }
    Ok(metadata)
}

impl IndexedRawEvent {
    pub(crate) fn entity_id(&self) -> &str {
        &self.entity_id
    }
}

fn indexed_summary_status(
    envelope: &OperationEnvelope,
    sequence: u64,
) -> Result<IndexedSummaryStatus, String> {
    let payload = envelope
        .payload()
        .as_object()
        .ok_or_else(|| "summary status payload must be an object".to_string())?;
    let entity_id = payload
        .get("entity_id")
        .or_else(|| payload.get("event_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "summary status entity_id is missing".to_string())?
        .to_string();
    let event_id = payload
        .get("event_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "summary status status is missing".to_string())?
        .to_string();
    Ok(IndexedSummaryStatus {
        entity_id,
        event_id,
        status,
        reason: payload
            .get("reason")
            .or_else(|| payload.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string),
        model_id: payload
            .get("model_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        prompt_version: payload
            .get("prompt_version")
            .and_then(Value::as_str)
            .map(str::to_string),
        attempt_id: payload
            .get("attempt_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        lease_expires_at: payload
            .get("lease_expires_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        sequence,
    })
}

fn indexed_summary_fact_delete(
    payload: &Value,
    sequence: u64,
) -> Result<Option<IndexedSummaryFactDelete>, String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "fact delete payload must be an object".to_string())?;
    let prior = object
        .get("prior")
        .and_then(Value::as_object)
        .ok_or_else(|| "summary fact delete prior is missing".to_string())?;
    let key = prior
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| "summary fact delete prior key is missing".to_string())?;
    if !key.starts_with("summary-") {
        return Ok(None);
    }
    let entity_id = prior
        .get("source_event_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "summary fact delete source_event_id is missing".to_string())?;
    Ok(Some(IndexedSummaryFactDelete {
        entity_id: entity_id.to_string(),
        sequence,
    }))
}

fn indexed_summary_embedding(payload: &Value) -> Result<Option<IndexedSummaryEmbedding>, String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "embedding payload must be an object".to_string())?;
    let action = object.get("action").and_then(Value::as_str);
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("document");
    if action != Some("retract_summary") && source != "summary" {
        return Ok(None);
    }
    let entity_id = object
        .get("entity_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "embedding entity_id is missing".to_string())?;
    Ok(Some(IndexedSummaryEmbedding {
        entity_id: entity_id.to_string(),
        retract: action == Some("retract_summary"),
    }))
}

fn update_index_after_batch(
    index: &mut JournalIndex,
    path: &Path,
    appended: Vec<JournalRecord>,
) -> Result<(), JournalError> {
    let Some(begin) = appended
        .iter()
        .find(|record| record.state == JournalState::Begin)
    else {
        return Err(JournalError::InvalidBatchState {
            batch_id: "missing begin".to_string(),
        });
    };
    let Some(commit) = appended
        .iter()
        .find(|record| record.state == JournalState::Commit)
    else {
        return Err(JournalError::InvalidBatchState {
            batch_id: begin.batch_id.clone(),
        });
    };
    let operations = appended
        .iter()
        .filter(|record| record.state == JournalState::Operation)
        .map(|record| {
            let metadata = indexed_operation_metadata(record)?;
            Ok(IndexedOperation {
                operation_id: record.operation_id.clone(),
                batch_id: record.batch_id.clone(),
                operation_kind: record.operation_kind.clone(),
                payload_digest: canonical_payload_digest(&record.payload)?,
                sequence: record.sequence,
                offset: None,
                raw_event: metadata.raw_event,
                summary_status: metadata.summary_status,
                summary_status_error: metadata.summary_status_error,
                summary_fact_delete: metadata.summary_fact_delete,
                summary_fact_delete_error: metadata.summary_fact_delete_error,
                summary_embedding: metadata.summary_embedding,
                summary_embedding_error: metadata.summary_embedding_error,
            })
        })
        .collect::<Result<Vec<_>, JournalError>>()?;
    validate_commit_operation_ids(commit, &operations, 0)?;
    index.batch_ids.insert(begin.batch_id.clone());
    index.committed_batches.insert(begin.batch_id.clone());
    index
        .committed_batch_sequences
        .insert(begin.batch_id.clone(), commit.sequence);
    for operation in operations {
        index.operation_ids.insert(operation.operation_id.clone());
        index
            .committed_operation_ids
            .insert(operation.operation_id.clone());
        if let Some(raw_event) = &operation.raw_event {
            index
                .raw_events
                .entry(raw_event.entity_id.clone())
                .or_insert_with(|| raw_event.clone());
        }
        if let Some(status) = &operation.summary_status {
            index.summary_statuses.push(status.clone());
        }
        if index.summary_status_error.is_none() {
            index.summary_status_error = operation.summary_status_error.clone();
        }
        if let Some(delete) = &operation.summary_fact_delete {
            index.summary_fact_deletes.push(delete.clone());
        }
        if index.summary_fact_delete_error.is_none() {
            index.summary_fact_delete_error = operation.summary_fact_delete_error.clone();
        }
        if let Some(embedding) = &operation.summary_embedding {
            if embedding.retract {
                index.summary_embedding_ids.remove(&embedding.entity_id);
            } else {
                index
                    .summary_embedding_ids
                    .insert(embedding.entity_id.clone());
            }
        }
        if index.summary_embedding_error.is_none() {
            index.summary_embedding_error = operation.summary_embedding_error.clone();
        }
        index
            .operations
            .insert(operation.operation_id.clone(), operation);
    }
    let metadata = std::fs::metadata(path)?;
    index.file_len = metadata.len();
    index.modified = metadata.modified().ok();
    index.next_sequence = commit.sequence.saturating_add(1);
    index.truncate_at = index.file_len;
    index.incomplete_final_line = false;
    index.uncommitted_final_batch = false;
    index.open_batch = None;
    Ok(())
}

fn ensure_open_batch(report: &RecoveryReport, batch_id: &str) -> Result<(), JournalError> {
    if report
        .records
        .iter()
        .any(|r| r.batch_id == batch_id && r.state == JournalState::Commit)
    {
        return Err(JournalError::InvalidBatchState {
            batch_id: batch_id.to_string(),
        });
    }
    if !report
        .records
        .iter()
        .any(|r| r.batch_id == batch_id && r.state == JournalState::Begin)
    {
        return Err(JournalError::MissingIntent {
            operation_id: batch_id.to_string(),
        });
    }
    Ok(())
}
fn validate_id(value: &str, batch: bool) -> Result<(), JournalError> {
    if value.trim().is_empty() {
        return Err(if batch {
            JournalError::InvalidBatchId
        } else {
            JournalError::InvalidOperationId
        });
    }
    Ok(())
}

fn validate_record_frame(
    state: JournalState,
    batch_id: &str,
    operation_id: &str,
    kind: &str,
    payload: &Value,
    line: Option<usize>,
) -> Result<(), JournalError> {
    let malformed = |message: &str| JournalError::MalformedCompleteLine {
        line: line.unwrap_or(0),
        message: message.to_string(),
    };
    validate_id(batch_id, true)?;
    validate_id(operation_id, false)?;
    match state {
        JournalState::Begin => {
            if operation_id != batch_id || kind != "batch_begin" {
                return Err(malformed("begin frame identity is invalid"));
            }
            let expected = json!({"batch_id": batch_id});
            if canonical_value(payload.clone())? != expected {
                return Err(malformed("begin payload must contain only batch_id"));
            }
        }
        JournalState::Operation => {
            let operation_kind = match kind {
                "raw_event" => OperationKind::RawEvent,
                "fact" => OperationKind::Fact,
                "redaction" => OperationKind::Redaction,
                "embedding" => OperationKind::Embedding,
                "summary_status" => OperationKind::SummaryStatus,
                _ => return Err(malformed("unknown operation kind")),
            };
            verify_operation_payload(operation_id, operation_kind, payload)?;
        }
        JournalState::Commit => {
            if operation_id != batch_id || kind != "batch_commit" {
                return Err(malformed("commit frame identity is invalid"));
            }
            let Some(object) = payload.as_object() else {
                return Err(malformed("commit payload must be an object"));
            };
            if object.len() != 1 {
                return Err(malformed("commit payload has unexpected fields"));
            }
            let Some(ids) = object.get("operations").and_then(Value::as_array) else {
                return Err(malformed("commit operations must be an array"));
            };
            let mut seen = BTreeSet::new();
            for id in ids {
                let Some(id) = id.as_str() else {
                    return Err(malformed("commit operation IDs must be strings"));
                };
                validate_id(id, false)?;
                if !seen.insert(id) {
                    return Err(malformed("commit operation IDs must be unique"));
                }
            }
        }
    }
    Ok(())
}

fn validate_commit_membership(
    commit: &JournalRecord,
    prior: &[Arc<JournalRecord>],
    line: usize,
) -> Result<(), JournalError> {
    let ids = commit
        .payload
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| JournalError::MalformedCompleteLine {
            line,
            message: "commit operations must be an array".into(),
        })?;
    let actual: Vec<&str> = prior
        .iter()
        .filter(|record| {
            record.batch_id == commit.batch_id && record.state == JournalState::Operation
        })
        .map(|record| record.operation_id.as_str())
        .collect();
    let declared: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
    if declared != actual {
        return Err(JournalError::InvalidBatchState {
            batch_id: commit.batch_id.clone(),
        });
    }
    Ok(())
}

fn validate_commit_operation_ids(
    commit: &JournalRecord,
    operations: &[IndexedOperation],
    line: usize,
) -> Result<(), JournalError> {
    let ids = commit
        .payload
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| JournalError::MalformedCompleteLine {
            line,
            message: "commit operations must be an array".into(),
        })?;
    let declared: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
    let actual: Vec<&str> = operations
        .iter()
        .map(|operation| operation.operation_id.as_str())
        .collect();
    if declared != actual {
        return Err(JournalError::InvalidBatchState {
            batch_id: commit.batch_id.clone(),
        });
    }
    Ok(())
}

fn verify_operation_payload(
    operation_id: &str,
    kind: OperationKind,
    payload: &Value,
) -> Result<(), JournalError> {
    let envelope: OperationEnvelope = serde_json::from_value(payload.clone()).map_err(|_| {
        JournalError::ConflictingOperation {
            operation_id: operation_id.to_string(),
        }
    })?;
    envelope
        .verify_operation_id()
        .map_err(|_| JournalError::ConflictingOperation {
            operation_id: operation_id.to_string(),
        })?;
    if envelope.operation_id() != operation_id || envelope.kind() != kind {
        return Err(JournalError::ConflictingOperation {
            operation_id: operation_id.to_string(),
        });
    }
    Ok(())
}
fn canonical_value(payload: Value) -> Result<Value, JournalError> {
    let text = canonical_json(&payload).map_err(|e| JournalError::MalformedCompleteLine {
        line: 0,
        message: e.to_string(),
    })?;
    serde_json::from_str(&text).map_err(|e| JournalError::MalformedCompleteLine {
        line: 0,
        message: e.to_string(),
    })
}
fn make_record(
    sequence: u64,
    state: JournalState,
    batch_id: &str,
    operation_id: &str,
    operation_kind: String,
    payload: Value,
) -> Result<JournalRecord, JournalError> {
    let unsigned = json!({"batch_id": batch_id, "operation_id": operation_id, "operation_kind": operation_kind, "payload": payload, "schema": JOURNAL_SCHEMA, "sequence": sequence, "state": state, "version": JOURNAL_VERSION});
    let text = canonical_json(&unsigned).map_err(|e| JournalError::MalformedCompleteLine {
        line: 0,
        message: e.to_string(),
    })?;
    Ok(JournalRecord {
        schema: JOURNAL_SCHEMA.into(),
        version: JOURNAL_VERSION,
        sequence,
        batch_id: batch_id.into(),
        state,
        operation_id: operation_id.into(),
        operation_kind: unsigned["operation_kind"]
            .as_str()
            .unwrap_or_default()
            .into(),
        payload: unsigned["payload"].clone(),
        checksum: sha256_hex(text.as_bytes()),
    })
}
fn parse_record(line: &str, line_number: usize) -> Result<JournalRecord, JournalError> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| JournalError::MalformedCompleteLine {
            line: line_number,
            message: e.to_string(),
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| JournalError::MalformedCompleteLine {
            line: line_number,
            message: "record must be an object".into(),
        })?;
    let state = object.get("state").and_then(Value::as_str).ok_or_else(|| {
        JournalError::MalformedCompleteLine {
            line: line_number,
            message: "state must be a string".into(),
        }
    })?;
    if !matches!(state, "begin" | "operation" | "commit") {
        return Err(JournalError::UnknownState {
            line: line_number,
            state: state.into(),
        });
    }
    if canonical_json(&value).map_err(|e| JournalError::MalformedCompleteLine {
        line: line_number,
        message: e.to_string(),
    })? != line
    {
        return Err(JournalError::NonCanonicalRecord { line: line_number });
    }
    let checksum = object
        .get("checksum")
        .and_then(Value::as_str)
        .ok_or_else(|| JournalError::MalformedCompleteLine {
            line: line_number,
            message: "checksum must be a string".into(),
        })?;
    let mut unsigned = value.clone();
    unsigned
        .as_object_mut()
        .expect("validated object")
        .remove("checksum");
    let expected = canonical_json(&unsigned).map_err(|e| JournalError::MalformedCompleteLine {
        line: line_number,
        message: e.to_string(),
    })?;
    if checksum != sha256_hex(expected.as_bytes()) {
        return Err(JournalError::ChecksumMismatch { line: line_number });
    }
    let record: JournalRecord =
        serde_json::from_value(value).map_err(|e| JournalError::MalformedCompleteLine {
            line: line_number,
            message: e.to_string(),
        })?;
    if record.schema != JOURNAL_SCHEMA
        || record.version != JOURNAL_VERSION
        || record.batch_id.trim().is_empty()
        || record.operation_id.trim().is_empty()
        || record.operation_kind.trim().is_empty()
    {
        return Err(JournalError::MalformedCompleteLine {
            line: line_number,
            message: "unsupported schema/version or empty operation fields".into(),
        });
    }
    if record.state == JournalState::Operation {
        let kind = match record.operation_kind.as_str() {
            "raw_event" => OperationKind::RawEvent,
            "fact" => OperationKind::Fact,
            "redaction" => OperationKind::Redaction,
            "embedding" => OperationKind::Embedding,
            "summary_status" => OperationKind::SummaryStatus,
            _ => {
                return Err(JournalError::MalformedCompleteLine {
                    line: line_number,
                    message: "unknown operation kind".into(),
                })
            }
        };
        verify_operation_payload(&record.operation_id, kind, &record.payload)?;
    }
    Ok(record)
}
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

struct FileLock {
    file: File,
}

// Recovery parses the append-only journal while holding this process-shared
// lock.  A several-hundred-megabyte journal can legitimately take longer than
// the old 30-second budget, especially while LanceDB is flushing a batch.
// Keep waiting for the owner rather than turning normal contention into a
// failed backfill; the cap still prevents an abandoned lock from hanging a
// caller forever.
const JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(600);
const JOURNAL_LOCK_INITIAL_BACKOFF: Duration = Duration::from_millis(2);
const JOURNAL_LOCK_MAX_BACKOFF: Duration = Duration::from_millis(50);

impl FileLock {
    fn acquire(path: &Path) -> Result<Self, JournalError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        let deadline = Instant::now() + JOURNAL_LOCK_TIMEOUT;
        let mut backoff = JOURNAL_LOCK_INITIAL_BACKOFF;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(backoff);
                    backoff = backoff.saturating_mul(2).min(JOURNAL_LOCK_MAX_BACKOFF);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(JournalError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "journal lock timeout",
                    )))
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(JournalError::Io(e)),
            }
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    fn journal() -> (Journal, PathBuf) {
        let root = std::env::temp_dir().join(format!("memory-v2-journal-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("journal.jsonl");
        (Journal::open(&path).unwrap(), path)
    }

    fn envelope(entity_id: &str, kind: OperationKind, payload: Value) -> OperationEnvelope {
        OperationEnvelope::new(kind, entity_id, "2025-01-02T03:04:05Z", Some(0), payload).unwrap()
    }

    #[test]
    fn repeated_recovery_of_an_unchanged_journal_uses_one_scan() {
        let (journal, _) = journal();
        journal.recover().unwrap();
        journal.recover().unwrap();
        assert_eq!(
            journal.read_report_count.load(Ordering::Relaxed),
            1,
            "an unchanged journal must not be reparsed for every chunk"
        );
    }

    #[test]
    fn a_batch_append_advances_the_cached_report_without_a_second_full_scan() {
        let (journal, _) = journal();
        journal.recover().unwrap();
        let event = envelope("event-cache", OperationKind::RawEvent, json!({"a": 1}));
        journal
            .write_batch(
                "batch-cache",
                &[(
                    event.operation_id().to_string(),
                    event.kind(),
                    serde_json::to_value(&event).unwrap(),
                )],
            )
            .unwrap();
        let report = journal.recover().unwrap();
        assert_eq!(report.replayable_records().len(), 1);
        assert_eq!(
            journal.read_report_count.load(Ordering::Relaxed),
            1,
            "a repository batch must extend its cached report instead of reparsing the journal"
        );
    }

    #[test]
    fn only_committed_batches_replay_after_recovery() {
        let (journal, _) = journal();
        let committed = envelope("event-1", OperationKind::RawEvent, json!({"a": 1}));
        journal
            .write_batch(
                "committed",
                &[(
                    committed.operation_id().to_string(),
                    committed.kind(),
                    serde_json::to_value(&committed).unwrap(),
                )],
            )
            .unwrap();
        let torn = envelope("event-2", OperationKind::RawEvent, json!({"a": 2}));
        journal.begin_batch("torn-batch").unwrap();
        journal
            .append_operation(
                "torn-batch",
                torn.operation_id(),
                torn.kind(),
                serde_json::to_value(&torn).unwrap(),
            )
            .unwrap();
        let report = journal.recover().unwrap();
        assert!(report.uncommitted_final_batch());
        assert_eq!(
            report
                .replayable_records()
                .iter()
                .map(|r| r.operation_id())
                .collect::<Vec<_>>(),
            vec![committed.operation_id()]
        );
        assert_eq!(
            report
                .records()
                .iter()
                .filter(|r| r.batch_id() == "torn-batch")
                .count(),
            0
        );
    }

    #[test]
    fn torn_final_record_is_truncated_before_next_append() {
        let (journal, path) = journal();
        journal.write_batch("first", &[]).unwrap();
        let valid_len = fs::metadata(&path).unwrap().len();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"broken\"")
            .unwrap();
        assert!(journal.recover().unwrap().incomplete_final_line());
        assert_eq!(fs::metadata(&path).unwrap().len(), valid_len);
        let second = envelope("event-2", OperationKind::Fact, json!({"a": 2}));
        journal
            .write_batch(
                "second",
                &[(
                    second.operation_id().to_string(),
                    second.kind(),
                    serde_json::to_value(&second).unwrap(),
                )],
            )
            .unwrap();
        assert_eq!(journal.recover().unwrap().replayable_records().len(), 1);
    }

    #[test]
    fn operation_frames_require_verified_envelopes_and_batch_membership() {
        let (journal, _) = journal();
        journal.begin_batch("batch-a").unwrap();
        let envelope = OperationEnvelope::new(
            OperationKind::Fact,
            "fact:self:name",
            "2025-01-02T03:04:05Z",
            Some(0),
            json!({"key": "name", "value": "Ada"}),
        )
        .unwrap();
        let payload = serde_json::to_value(&envelope).unwrap();
        journal
            .append_operation(
                "batch-a",
                envelope.operation_id(),
                OperationKind::Fact,
                payload.clone(),
            )
            .unwrap();
        journal.commit_batch("batch-a").unwrap();

        journal.begin_batch("batch-b").unwrap();
        assert!(matches!(
            journal.append_operation(
                "batch-b",
                envelope.operation_id(),
                OperationKind::Fact,
                payload,
            ),
            Err(JournalError::DuplicateOperation { .. })
        ));
    }
}
