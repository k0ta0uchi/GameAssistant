use super::error::{MemoryError, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// All memory-v2 paths derive from this injected absolute root. No environment
/// variable, current directory, or user profile is consulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryPaths {
    runtime_root: PathBuf,
    memory_root: PathBuf,
    raw_events: PathBuf,
    facts: PathBuf,
    operations: PathBuf,
    embeddings: PathBuf,
    journal: PathBuf,
    manifest: PathBuf,
    lock: PathBuf,
    staging: PathBuf,
    legacy: PathBuf,
}

impl MemoryPaths {
    pub fn from_runtime_root(root: impl AsRef<Path>) -> Result<Self> {
        let runtime_root = root.as_ref().to_path_buf();
        if !safe_root(&runtime_root) || has_link_escape(&runtime_root) {
            return Err(MemoryError::InvalidRuntimeRoot);
        }
        let memory_root = runtime_root.join("data").join("memory_v2");
        let paths = Self {
            raw_events: memory_root.join("raw_events"),
            facts: memory_root.join("facts"),
            operations: memory_root.join("operations"),
            embeddings: memory_root.join("embeddings"),
            journal: memory_root.join("journal.jsonl"),
            manifest: memory_root.join("manifest.json"),
            lock: memory_root.join("journal.lock"),
            staging: memory_root.join("staging"),
            legacy: memory_root.join("legacy"),
            runtime_root,
            memory_root,
        };
        for path in [
            &paths.memory_root,
            &paths.raw_events,
            &paths.facts,
            &paths.operations,
            &paths.embeddings,
            &paths.journal,
            &paths.manifest,
            &paths.lock,
            &paths.staging,
            &paths.legacy,
        ] {
            if !paths.contains(path) || has_link_escape(path) {
                return Err(MemoryError::InvalidRuntimeRoot);
            }
        }
        Ok(paths)
    }
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }
    pub fn memory_root(&self) -> &Path {
        &self.memory_root
    }
    pub fn raw_events(&self) -> &Path {
        &self.raw_events
    }
    pub fn facts(&self) -> &Path {
        &self.facts
    }
    pub fn operations(&self) -> &Path {
        &self.operations
    }
    pub fn embeddings(&self) -> &Path {
        &self.embeddings
    }
    pub fn journal(&self) -> &Path {
        &self.journal
    }
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }
    pub fn lock(&self) -> &Path {
        &self.lock
    }
    pub fn staging(&self) -> &Path {
        &self.staging
    }
    pub fn legacy(&self) -> &Path {
        &self.legacy
    }
    pub fn journal_path(&self) -> &Path {
        self.journal()
    }
    pub fn manifest_path(&self) -> &Path {
        self.manifest()
    }
    pub fn lock_path(&self) -> &Path {
        self.lock()
    }
    pub fn staging_path(&self) -> &Path {
        self.staging()
    }
    pub fn legacy_path(&self) -> &Path {
        self.legacy()
    }

    /// Validate a path supplied by a storage adapter before opening or replacing it.
    pub fn validate_contained_path(&self, candidate: impl AsRef<Path>) -> Result<()> {
        let candidate = candidate.as_ref();
        if !self.contains(candidate) || has_link_escape(candidate) {
            return Err(MemoryError::InvalidRuntimeRoot);
        }
        Ok(())
    }
    /// Safely append one controlled filename component below the runtime root.
    pub fn controlled_child(&self, parent: impl AsRef<Path>, component: &str) -> Result<PathBuf> {
        let mut parts = Path::new(component).components();
        if component.is_empty()
            || component.chars().any(|character| character.is_control())
            || component.contains('/')
            || component.contains('\\')
            || !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
        {
            return Err(MemoryError::InvalidRuntimeRoot);
        }
        let parent = parent.as_ref();
        self.validate_contained_path(parent)?;
        let child = parent.join(component);
        self.validate_contained_path(&child)?;
        Ok(child)
    }
    fn contains(&self, candidate: &Path) -> bool {
        safe_absolute(candidate) && candidate.starts_with(&self.runtime_root)
    }
}

fn safe_root(path: &Path) -> bool {
    safe_absolute(path)
}
fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}

fn has_link_escape(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return true;
            }
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn rejects_relative_parent_and_absolute_controlled_components() {
        assert!(MemoryPaths::from_runtime_root("relative").is_err());
        let root = std::env::temp_dir().join(format!("memory-v2-paths-{}", Uuid::new_v4()));
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        assert!(paths.controlled_child(paths.memory_root(), "..").is_err());
        assert!(paths
            .controlled_child(paths.memory_root(), "child/name")
            .is_err());
        assert!(paths
            .validate_contained_path(root.parent().unwrap().join("escape"))
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_nested_symlink_escape_before_path_use() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("memory-v2-link-{}", Uuid::new_v4()));
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        symlink(&outside, root.join("data").join("memory_v2")).unwrap();
        assert!(MemoryPaths::from_runtime_root(&root).is_err());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
