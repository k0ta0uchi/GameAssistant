use gameassistant_lib::lance_memory;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Import the historical LanceDB table into the memory-v2 table.
///
/// The source table is scanned by the library in bounded Arrow batches and is
/// never overwritten, deleted, or replaced. Re-running this command resumes a
/// partial import by ID and is therefore safe after interruption.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root_dir = resolve_runtime_root()?;
    println!(
        "Importing legacy LanceDB memories from {:?} ...",
        root_dir.join(lance_memory::LANCE_DB_DIR)
    );

    let stats = lance_memory::import_legacy_memories(&root_dir)
        .await
        .map_err(|error| format!("Legacy import failed: {}", error))?;
    println!(
        "Imported {} legacy records: {}",
        stats.imported_count, stats.message
    );
    Ok(())
}

fn resolve_runtime_root() -> Result<PathBuf, String> {
    let mut args = std::env::args_os().skip(1);
    let explicit = match (args.next(), args.next()) {
        (None, None) => None,
        (Some(flag), Some(root)) if flag == "--root" => Some(PathBuf::from(root)),
        _ => return Err("usage: migrate_to_lance [--root <absolute-runtime-root>]".into()),
    };
    let root = explicit
        .or_else(|| std::env::var_os("GAMEASSISTANT_RUNTIME_ROOT").map(PathBuf::from))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        })
        .ok_or_else(|| "unable to resolve executable runtime root".to_string())?;
    if !root.is_absolute()
        || root
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("migration root must be an absolute traversal-free path".into());
    }
    reject_links(&root)?;
    Ok(root)
}

fn reject_links(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err("migration root contains a symlink".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err("migration root contains a reparse point".into());
            }
        }
    }
    Ok(())
}
