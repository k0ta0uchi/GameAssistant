//! Authoritative, atomically published pointer to memory-v2 stores.

use super::canonical::canonical_json;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const MANIFEST_SCHEMA: &str = "gameassistant.memory_v2.manifest";
pub const MANIFEST_VERSION: u32 = 2;
pub const TABLE_SCHEMA_VERSION: u32 = 1;
pub const RAW_EVENTS_TABLE: &str = "raw_events";
pub const FACTS_TABLE: &str = "facts";
pub const EMBEDDINGS_TABLE: &str = "embeddings";
const GENERATION_WIDTH: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryManifest {
    pub schema: String,
    pub version: u32,
    pub manifest_version: u32,
    pub schema_version: u32,
    pub generation: u64,
    pub generation_name: String,
    pub committed_sequence: u64,
    // Alias retained for source compatibility with the Phase 2A selector.
    pub journal_sequence: u64,
    pub journal_segment: String,
    pub journal_sha256: String,
    pub raw_events: String,
    pub facts: String,
    pub embeddings: String,
    pub schema_versions: BTreeMap<String, u32>,
    pub schema_hashes: BTreeMap<String, String>,
    pub created_at: String,
}

pub type Manifest = MemoryManifest;

impl MemoryManifest {
    pub fn new(generation: u64, journal_sequence: u64) -> Self {
        Self::with_journal(
            generation,
            journal_sequence,
            "journal.jsonl",
            &sha256_hex(b""),
        )
    }
    pub fn with_journal(
        generation: u64,
        committed_sequence: u64,
        journal_segment: &str,
        journal_sha256: &str,
    ) -> Self {
        let mut schema_versions = BTreeMap::new();
        let mut schema_hashes = BTreeMap::new();
        for table in [RAW_EVENTS_TABLE, FACTS_TABLE, EMBEDDINGS_TABLE] {
            schema_versions.insert(table.to_string(), TABLE_SCHEMA_VERSION);
            schema_hashes.insert(table.to_string(), expected_schema_hash(table));
        }
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        Self {
            schema: MANIFEST_SCHEMA.to_string(),
            version: MANIFEST_VERSION,
            manifest_version: MANIFEST_VERSION,
            schema_version: TABLE_SCHEMA_VERSION,
            generation,
            generation_name: generation_name(generation),
            committed_sequence,
            journal_sequence: committed_sequence,
            journal_segment: journal_segment.to_string(),
            journal_sha256: journal_sha256.to_string(),
            raw_events: RAW_EVENTS_TABLE.to_string(),
            facts: FACTS_TABLE.to_string(),
            embeddings: EMBEDDINGS_TABLE.to_string(),
            schema_versions,
            schema_hashes,
            created_at,
        }
    }
    pub fn generation_name(&self) -> &str {
        &self.generation_name
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn committed_sequence(&self) -> u64 {
        self.committed_sequence
    }
    pub fn journal_sequence(&self) -> u64 {
        self.committed_sequence
    }
    pub fn journal_sha256(&self) -> &str {
        &self.journal_sha256
    }
    pub fn raw_events(&self) -> &str {
        &self.raw_events
    }
    pub fn facts(&self) -> &str {
        &self.facts
    }
    pub fn embeddings(&self) -> &str {
        &self.embeddings
    }
    // Legacy names are aliases to the approved three stores.
    pub fn events(&self) -> &str {
        self.raw_events()
    }
    pub fn subjects(&self) -> &str {
        self.embeddings()
    }
    pub fn set_events(&mut self, events: String) {
        self.raw_events = events;
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != MANIFEST_SCHEMA
            || self.version != MANIFEST_VERSION
            || self.manifest_version != MANIFEST_VERSION
            || self.schema_version != TABLE_SCHEMA_VERSION
        {
            return Err(ManifestError::UnsupportedVersion);
        }
        if self.generation == 0 || self.generation_name != generation_name(self.generation) {
            return Err(ManifestError::InvalidGeneration);
        }
        if self.committed_sequence != self.journal_sequence {
            return Err(ManifestError::Malformed("sequence aliases differ".into()));
        }
        if self.journal_segment != "journal.jsonl" || !valid_sha256(&self.journal_sha256) {
            return Err(ManifestError::InvalidJournal);
        }
        if self.raw_events != RAW_EVENTS_TABLE
            || self.facts != FACTS_TABLE
            || self.embeddings != EMBEDDINGS_TABLE
        {
            return Err(ManifestError::InvalidStoreName);
        }
        let expected = [RAW_EVENTS_TABLE, FACTS_TABLE, EMBEDDINGS_TABLE];
        for table in expected {
            if self.schema_versions.get(table) != Some(&TABLE_SCHEMA_VERSION)
                || !self
                    .schema_hashes
                    .get(table)
                    .is_some_and(|hash| hash == &expected_schema_hash(table))
            {
                return Err(ManifestError::InvalidSchema);
            }
        }
        if self.schema_versions.len() != expected.len()
            || self.schema_hashes.len() != expected.len()
        {
            return Err(ManifestError::InvalidSchema);
        }
        if !self.created_at.ends_with('Z')
            || DateTime::parse_from_rfc3339(&self.created_at).is_err()
        {
            return Err(ManifestError::InvalidTimestamp);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ManifestError {
    Io(io::Error),
    InvalidPath,
    InvalidStoreName,
    UnsupportedVersion,
    Malformed(String),
    LockTimeout,
    InvalidGeneration,
    GenerationNotMonotonic { current: u64, requested: u64 },
    InvalidJournal,
    InvalidSchema,
    InvalidTimestamp,
    TargetNotVerified,
}
impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "manifest I/O error: {e}"),
            Self::InvalidPath => write!(f, "manifest paths must be absolute and traversal-free"),
            Self::InvalidStoreName => write!(f, "manifest store name is invalid"),
            Self::UnsupportedVersion => write!(f, "unsupported manifest schema/version"),
            Self::Malformed(e) => write!(f, "malformed manifest: {e}"),
            Self::LockTimeout => write!(f, "timed out waiting for manifest lock"),
            Self::InvalidGeneration => write!(f, "manifest generation is invalid"),
            Self::GenerationNotMonotonic { current, requested } => write!(
                f,
                "manifest generation {requested} is not newer than {current}"
            ),
            Self::InvalidJournal => write!(f, "manifest journal pointer is invalid"),
            Self::InvalidSchema => write!(f, "manifest table schema metadata is invalid"),
            Self::InvalidTimestamp => write!(f, "manifest creation timestamp is invalid"),
            Self::TargetNotVerified => write!(f, "manifest target was not verified"),
        }
    }
}
impl std::error::Error for ManifestError {}
impl From<io::Error> for ManifestError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct ManifestSelector {
    path: PathBuf,
    staging: PathBuf,
    lock_path: PathBuf,
    root: PathBuf,
}
impl fmt::Debug for ManifestSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestSelector")
            .field("path", &self.path)
            .field("staging", &self.staging)
            .finish()
    }
}

impl ManifestSelector {
    pub fn new(path: impl AsRef<Path>, staging: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref().to_path_buf();
        let lock_path = path.with_extension("lock");
        Self::new_with_lock(path, staging, lock_path)
    }

    /// Construct a selector that shares the journal's single-writer lock.
    /// The default `new` constructor is retained for older callers/tests that
    /// use the manifest-local lock filename.
    pub fn new_with_lock(
        path: impl AsRef<Path>,
        staging: impl AsRef<Path>,
        lock_path: impl AsRef<Path>,
    ) -> Result<Self, ManifestError> {
        let path = path.as_ref().to_path_buf();
        let staging = staging.as_ref().to_path_buf();
        let lock_path = lock_path.as_ref().to_path_buf();
        if !safe_absolute(&path) || !safe_absolute(&staging) {
            return Err(ManifestError::InvalidPath);
        }
        let root = path
            .parent()
            .ok_or(ManifestError::InvalidPath)?
            .to_path_buf();
        if !staging.starts_with(&root) {
            return Err(ManifestError::InvalidPath);
        }
        reject_existing_symlinks(&root)?;
        reject_existing_symlinks(&staging)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&staging)?;
        if !safe_absolute(&lock_path) || !lock_path.starts_with(&root) {
            return Err(ManifestError::InvalidPath);
        }
        reject_existing_symlinks(&path)?;
        reject_existing_symlinks(&lock_path)?;
        reject_existing_symlinks(&staging)?;
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        Ok(Self {
            path,
            staging,
            lock_path,
            root,
        })
    }
    pub fn from_paths(paths: &super::paths::MemoryPaths) -> Result<Self, ManifestError> {
        Self::new_with_lock(paths.manifest(), paths.staging(), paths.lock())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn read(&self) -> Result<Option<MemoryManifest>, ManifestError> {
        reject_existing_symlinks(&self.path)?;
        reject_existing_symlinks(&self.lock_path)?;
        let mut bytes = Vec::new();
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        file.read_to_end(&mut bytes)?;
        let manifest: MemoryManifest =
            serde_json::from_slice(&bytes).map_err(|e| ManifestError::Malformed(e.to_string()))?;
        manifest.validate()?;
        Ok(Some(manifest))
    }
    pub fn select(
        &self,
        generation: u64,
        committed_sequence: u64,
    ) -> Result<MemoryManifest, ManifestError> {
        let journal = self.root.join("journal.jsonl");
        reject_existing_symlinks(&journal)?;
        let bytes = fs::read(&journal)?;
        let manifest = MemoryManifest::with_journal(
            generation,
            committed_sequence,
            "journal.jsonl",
            &sha256_hex(&bytes),
        );
        let targets = self.default_targets(&manifest)?;
        self.replace_verified(&manifest, &targets)?;
        Ok(manifest)
    }
    pub fn select_verified(
        &self,
        generation: u64,
        committed_sequence: u64,
        journal_sha256: &str,
        targets: &[PathBuf],
    ) -> Result<MemoryManifest, ManifestError> {
        let manifest = MemoryManifest::with_journal(
            generation,
            committed_sequence,
            "journal.jsonl",
            journal_sha256,
        );
        self.replace_verified(&manifest, targets)?;
        Ok(manifest)
    }
    pub fn replace(&self, manifest: &MemoryManifest) -> Result<(), ManifestError> {
        let targets = self.default_targets(manifest)?;
        self.replace_inner(manifest, Some(&targets))
    }
    pub fn replace_verified(
        &self,
        manifest: &MemoryManifest,
        targets: &[PathBuf],
    ) -> Result<(), ManifestError> {
        self.replace_inner(manifest, Some(targets))
    }
    fn replace_inner(
        &self,
        manifest: &MemoryManifest,
        targets: Option<&[PathBuf]>,
    ) -> Result<(), ManifestError> {
        manifest.validate()?;
        let targets = targets.ok_or(ManifestError::TargetNotVerified)?;
        self.verify_targets(manifest, targets)?;
        let _lock = FileLock::acquire(&self.lock_path)?;
        if let Some(current) = self.read()? {
            if manifest.generation <= current.generation {
                return Err(ManifestError::GenerationNotMonotonic {
                    current: current.generation,
                    requested: manifest.generation,
                });
            }
        }
        let value =
            serde_json::to_value(manifest).map_err(|e| ManifestError::Malformed(e.to_string()))?;
        let text = canonical_json(&value).map_err(|e| ManifestError::Malformed(e.to_string()))?;
        let temp = self
            .staging
            .join(format!("manifest-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            atomic_replace(&temp, &self.path)?;
            sync_directory(self.path.parent())?;
            Ok::<(), ManifestError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
    fn verify_targets(
        &self,
        manifest: &MemoryManifest,
        targets: &[PathBuf],
    ) -> Result<(), ManifestError> {
        self.verify_journal(manifest)?;
        if targets.len() != 3 {
            return Err(ManifestError::TargetNotVerified);
        }
        // On Windows `canonicalize` commonly returns an extended-length path
        // (`\\?\C:\...`) while `self.root` retains its normal spelling. Keep
        // the lexical containment check above, then compare canonical paths
        // using the canonical root so the verification does not reject every
        // valid target on Windows.
        let canonical_root = fs::canonicalize(&self.root)?;
        let names = [
            manifest.raw_events(),
            manifest.facts(),
            manifest.embeddings(),
        ];
        for (target, name) in targets.iter().zip(names) {
            if !safe_absolute(target)
                || !target.starts_with(&self.root)
                || target.file_name().and_then(|n| n.to_str()) != Some(name)
                || !target.exists()
            {
                return Err(ManifestError::TargetNotVerified);
            }
            reject_existing_symlinks(target)?;
            let real = fs::canonicalize(target)?;
            if !real.starts_with(&canonical_root) {
                return Err(ManifestError::TargetNotVerified);
            }
        }
        Ok(())
    }
    fn verify_journal(&self, manifest: &MemoryManifest) -> Result<(), ManifestError> {
        let journal = self.root.join(&manifest.journal_segment);
        if !safe_absolute(&journal) || !journal.starts_with(&self.root) || !journal.is_file() {
            return Err(ManifestError::TargetNotVerified);
        }
        reject_existing_symlinks(&journal)?;
        let bytes = fs::read(&journal)?;
        if sha256_hex(&bytes) != manifest.journal_sha256 {
            return Err(ManifestError::TargetNotVerified);
        }
        Ok(())
    }
    fn default_targets(&self, manifest: &MemoryManifest) -> Result<Vec<PathBuf>, ManifestError> {
        Ok(vec![
            self.root.join(manifest.raw_events()),
            self.root.join(manifest.facts()),
            self.root.join(manifest.embeddings()),
        ])
    }
}

fn generation_name(generation: u64) -> String {
    format!("generation-{generation:0GENERATION_WIDTH$}")
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn expected_schema_hash(table: &str) -> String {
    let fields = match table {
        RAW_EVENTS_TABLE => json!([
            "event_id",
            "subject",
            "event_type",
            "source",
            "occurred_at",
            "content"
        ]),
        FACTS_TABLE => json!([
            "fact_id",
            "subject",
            "predicate",
            "key",
            "value",
            "status",
            "source_event_id",
            "revision",
            "operation_id"
        ]),
        EMBEDDINGS_TABLE => json!(["entity_id", "embedding"]),
        _ => json!([]),
    };
    let descriptor = json!({
        "schema": "gameassistant.memory_v2.table",
        "table": table,
        "version": TABLE_SCHEMA_VERSION,
        "fields": fields,
    });
    let canonical = canonical_json(&descriptor).expect("schema descriptor is canonicalizable");
    sha256_hex(canonical.as_bytes())
}
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}
fn reject_existing_symlinks(path: &Path) -> Result<(), ManifestError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                return Err(ManifestError::InvalidPath);
            }
        }
    }
    Ok(())
}
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}
fn atomic_replace(source: &Path, target: &Path) -> Result<(), ManifestError> {
    reject_existing_symlinks(source)?;
    reject_existing_symlinks(target)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source_w: Vec<u16> = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let target_w: Vec<u16> = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            MoveFileExW(
                PCWSTR(source_w.as_ptr()),
                PCWSTR(target_w.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
            .map_err(|e| ManifestError::Io(io::Error::other(e.to_string())))
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, target).map_err(ManifestError::Io)
    }
}
fn sync_directory(path: Option<&Path>) -> Result<(), ManifestError> {
    #[cfg(unix)]
    if let Some(path) = path {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

struct FileLock {
    file: File,
}
impl FileLock {
    fn acquire(path: &Path) -> Result<Self, ManifestError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(ManifestError::LockTimeout),
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
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

    fn selector() -> (ManifestSelector, PathBuf) {
        let root = std::env::temp_dir().join(format!("memory-v2-manifest-{}", Uuid::new_v4()));
        let path = root.join("manifest.json");
        (
            ManifestSelector::new(&path, root.join("staging")).unwrap(),
            root,
        )
    }

    #[test]
    fn generation_is_fixed_width_and_monotonic() {
        let (selector, root) = selector();
        fs::write(root.join("journal.jsonl"), b"").unwrap();
        for name in [RAW_EVENTS_TABLE, FACTS_TABLE, EMBEDDINGS_TABLE] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let first = selector.select(1, 2).unwrap();
        assert_eq!(first.generation_name(), "generation-00000000000000000001");
        assert!(matches!(
            selector.select(1, 3),
            Err(ManifestError::GenerationNotMonotonic { .. })
        ));
        let second = selector.select(2, 3).unwrap();
        assert!(second.generation() > first.generation());
        assert_eq!(selector.read().unwrap(), Some(second));
    }

    #[test]
    fn verified_publication_rejects_missing_or_escaping_targets() {
        let (selector, root) = selector();
        fs::write(root.join("journal.jsonl"), b"journal").unwrap();
        let manifest = MemoryManifest::with_journal(1, 4, "journal.jsonl", &sha256_hex(b"journal"));
        assert!(selector.replace_verified(&manifest, &[]).is_err());
        for name in [RAW_EVENTS_TABLE, FACTS_TABLE, EMBEDDINGS_TABLE] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let targets = [
            root.join(RAW_EVENTS_TABLE),
            root.join(FACTS_TABLE),
            root.join(EMBEDDINGS_TABLE),
        ];
        selector.replace_verified(&manifest, &targets).unwrap();
        let mut escaping = targets.to_vec();
        escaping[0] = root.parent().unwrap().join(RAW_EVENTS_TABLE);
        assert!(selector
            .replace_verified(
                &MemoryManifest::with_journal(2, 5, "journal.jsonl", &sha256_hex(b"journal")),
                &escaping
            )
            .is_err());
    }

    #[test]
    fn select_hashes_the_on_disk_journal_and_rejects_tampered_schema_metadata() {
        let (selector, root) = selector();
        fs::write(root.join("journal.jsonl"), b"journal\n").unwrap();
        for name in [RAW_EVENTS_TABLE, FACTS_TABLE, EMBEDDINGS_TABLE] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let manifest = selector.select(1, 1).unwrap();
        assert_eq!(manifest.journal_sha256(), sha256_hex(b"journal\n"));
        let mut tampered = manifest.clone();
        tampered.schema_hashes.insert(
            RAW_EVENTS_TABLE.to_string(),
            sha256_hex(b"different-schema"),
        );
        assert!(matches!(
            selector.replace(&tampered),
            Err(ManifestError::InvalidSchema)
        ));
    }
}
