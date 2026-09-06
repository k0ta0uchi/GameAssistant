use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

pub const GEMMA_MODEL_ID: &str = "gemma-3-1b-it-Q4_K_S.gguf";
pub const GEMMA_DOWNLOAD_URL: &str = "https://huggingface.co/unsloth/gemma-3-1b-it-GGUF/resolve/477b13ec8f37fac41688958d088232adcd6e0836/gemma-3-1b-it-Q4_K_S.gguf?download=true";
pub const GEMMA_EXPECTED_SIZE_BYTES: u64 = 780_993_056;
pub const GEMMA_EXPECTED_SHA256: &str =
    "f1536b0b60e53ffd98c945a3295f51154db3ffdd95329d86971c83c86c56899f";
pub const GEMMA_TERMS_VERSION: &str = "gemma-terms-v1";
pub const GEMMA_TERMS_SOURCE: &str = "https://ai.google.dev/gemma/terms";
const INSTALL_MANIFEST: &str = ".gameassistant-install.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hf_repo: String,
    pub category: String, // "ASR" | "Embedding" | "LLM" | "Other"
    pub required: bool,
    pub estimated_size_bytes: u64,
    pub check_files: Vec<String>, // このファイル群が存在すればインストール済みと判定
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub expected_size_bytes: Option<u64>,
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hf_repo: String,
    pub category: String,
    pub required: bool,
    pub estimated_size_bytes: u64,
    pub is_installed: bool,
    pub actual_size_bytes: u64,
    pub local_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgressEvent {
    pub model_id: String,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub speed_mbps: f64,
    pub percent: f64,
    pub status: String, // "downloading" | "completed" | "error" | "cancelled"
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HfTreeItem {
    r#type: String,
    path: String,
    size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallManifest {
    files: Vec<InstallManifestFile>,
    #[serde(default)]
    complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallManifestFile {
    path: String,
    size: u64,
}

pub fn get_defined_models() -> Vec<ModelDef> {
    vec![
        ModelDef {
            id: "kotoba-whisper-v2.0-faster".to_string(),
            name: "Kotoba-Whisper v2.0 (CUDA INT8)".to_string(),
            description:
                "日本語特化の超高速・高精度リアルタイム音声認識モデル (Faster-Whisper CTranslate2)"
                    .to_string(),
            hf_repo: "kotoba-tech/kotoba-whisper-v2.0-faster".to_string(),
            category: "ASR".to_string(),
            required: true,
            estimated_size_bytes: 1_515_000_000,
            check_files: vec![
                "model.bin".to_string(),
                "config.json".to_string(),
                "preprocessor_config.json".to_string(),
                "tokenizer.json".to_string(),
                "vocabulary.json".to_string(),
            ],
            download_url: None,
            expected_size_bytes: None,
            expected_sha256: None,
        },
        ModelDef {
            id: "GLuCoSE-base-ja".to_string(),
            name: "GLuCoSE-base-ja (Embedding)".to_string(),
            description:
                "日本語セマンティック長期記憶・ベクトル検索用の高精度埋め込みモデル (768次元)"
                    .to_string(),
            hf_repo: "pkshatech/GLuCoSE-base-ja".to_string(),
            category: "Embedding".to_string(),
            required: true,
            estimated_size_bytes: 535_000_000,
            check_files: vec![
                "1_Pooling/config.json".to_string(),
                "added_tokens.json".to_string(),
                "config_sentence_transformers.json".to_string(),
                "entity_vocab.json".to_string(),
                "modules.json".to_string(),
                "pytorch_model.bin".to_string(),
                "config.json".to_string(),
                "sentence_bert_config.json".to_string(),
                "sentencepiece.bpe.model".to_string(),
                "special_tokens_map.json".to_string(),
                "tokenizer_config.json".to_string(),
            ],
            download_url: None,
            expected_size_bytes: None,
            expected_sha256: None,
        },
        ModelDef {
            id: GEMMA_MODEL_ID.to_string(),
            name: "Gemma 3 1B IT (GGUF Q4_K_S)".to_string(),
            description:
                "ローカル長期記憶の要約・事実抽出に使う必須CPUモデル (llama-server / GGUF)"
                    .to_string(),
            hf_repo: "unsloth/gemma-3-1b-it-GGUF".to_string(),
            category: "LLM".to_string(),
            required: true,
            estimated_size_bytes: GEMMA_EXPECTED_SIZE_BYTES,
            check_files: vec!["gemma-3-1b-it-Q4_K_S.gguf".to_string()],
            download_url: Some(GEMMA_DOWNLOAD_URL.to_string()),
            expected_size_bytes: Some(GEMMA_EXPECTED_SIZE_BYTES),
            expected_sha256: Some(GEMMA_EXPECTED_SHA256.to_string()),
        },
    ]
}

pub fn gemma_terms_accepted(root_dir: &Path) -> bool {
    let settings = crate::settings::load_settings_file(root_dir);
    settings
        .get("gemma_terms_accepted")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        && settings
            .get("gemma_terms_version")
            .and_then(|value| value.as_str())
            == Some(GEMMA_TERMS_VERSION)
        && settings
            .get("gemma_terms_model_sha256")
            .and_then(|value| value.as_str())
            == Some(GEMMA_EXPECTED_SHA256)
        && settings
            .get("gemma_terms_source")
            .and_then(|value| value.as_str())
            == Some(GEMMA_TERMS_SOURCE)
}

pub fn portable_models_dir(root_dir: &Path) -> PathBuf {
    root_dir.join("models")
}

fn validate_portable_root(root_dir: &Path) -> Result<(), String> {
    if !root_dir.is_absolute()
        || root_dir
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("runtime root must be an absolute traversal-free path".to_string());
    }
    reject_link_components(root_dir)
}

fn validate_portable_path(root_dir: &Path, candidate: &Path) -> Result<(), String> {
    validate_portable_root(root_dir)?;
    if !candidate.is_absolute()
        || !candidate.starts_with(root_dir)
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("model path escapes the portable runtime root".to_string());
    }
    reject_link_components(candidate)
}

fn reject_link_components(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("inspect portable path: {}", error)),
        };
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(format!(
                "portable model path contains a symlink or reparse point: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{get_defined_models, GEMMA_MODEL_ID};

    #[test]
    fn retired_sup_simcse_is_not_a_supported_model() {
        assert!(get_defined_models()
            .iter()
            .all(|model| !model.id.to_ascii_lowercase().contains("sup-simcse")));
    }

    #[test]
    fn gemma_is_required_and_uses_the_pinned_size() {
        let gemma = get_defined_models()
            .into_iter()
            .find(|model| model.id.ends_with(".gguf"))
            .expect("Gemma model definition");
        assert!(gemma.required);
        assert_eq!(gemma.estimated_size_bytes, 780_993_056);
        assert_eq!(
            gemma.download_url.as_deref(),
            Some(super::GEMMA_DOWNLOAD_URL)
        );
        assert_eq!(
            gemma.expected_sha256.as_deref(),
            Some(super::GEMMA_EXPECTED_SHA256)
        );
        assert!(gemma.description.contains("要約"));
    }

    #[test]
    fn gemma_terms_require_version_and_model_hash_metadata() {
        let root = tempfile_root("terms");
        crate::settings::save_setting_key(&root, "gemma_terms_accepted", serde_json::json!(true))
            .unwrap();
        assert!(!super::gemma_terms_accepted(&root));
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_version",
            serde_json::json!(super::GEMMA_TERMS_VERSION),
        )
        .unwrap();
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_model_sha256",
            serde_json::json!(super::GEMMA_EXPECTED_SHA256),
        )
        .unwrap();
        crate::settings::save_setting_key(
            &root,
            "gemma_terms_source",
            serde_json::json!(super::GEMMA_TERMS_SOURCE),
        )
        .unwrap();
        assert!(super::gemma_terms_accepted(&root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_gemma_file_is_not_considered_installed() {
        let root = tempfile_root("invalid-gemma");
        let path = root.join(GEMMA_MODEL_ID);
        std::fs::write(&path, b"not-a-valid-model").unwrap();
        let error = super::validate_gemma_file(&path).expect_err("short file must be rejected");
        assert!(error.contains("size mismatch"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_model_consumer_uses_the_portable_models_root() {
        let root = tempfile_root("portable-root");
        crate::settings::save_setting_key(
            &root,
            "models_dir",
            serde_json::json!(r"C:\\legacy\\models"),
        )
        .unwrap();
        assert_eq!(
            super::ModelManager::get_effective_models_dir(
                &root,
                Some(r"D:\\custom\\models".to_string())
            ),
            super::portable_models_dir(&root)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_download_registration_is_rejected_without_replacing_owner() {
        let manager = super::ModelManager::new();
        let root = tempfile_root("registration");
        let first = manager
            .begin_download(&root, "model-a")
            .expect("first owner");
        assert!(manager.begin_download(&root, "model-a").is_err());
        drop(first);
        assert!(manager.begin_download(&root, "model-a").is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn content_range_must_match_resume_offset_and_pinned_total() {
        let valid = format!(
            "bytes 10-{}/{}",
            super::GEMMA_EXPECTED_SIZE_BYTES - 1,
            super::GEMMA_EXPECTED_SIZE_BYTES
        );
        assert_eq!(
            super::parse_content_range("bytes 10-99/100"),
            Some((10, 99, 100))
        );
        assert!(super::valid_resume_content_range(&valid, 10));
        assert!(!super::valid_resume_content_range(
            &valid.replace("bytes 10", "bytes 11"),
            10
        ));
        let expected_body = super::GEMMA_EXPECTED_SIZE_BYTES - 10;
        let strict = format!(
            "bytes 10-{}/{}",
            super::GEMMA_EXPECTED_SIZE_BYTES - 1,
            super::GEMMA_EXPECTED_SIZE_BYTES
        );
        assert!(super::valid_resume_response(
            &strict,
            Some(expected_body),
            expected_body,
            10
        ));
        assert!(!super::valid_resume_response(
            &strict,
            Some(expected_body - 1),
            expected_body,
            10
        ));
        assert!(!super::valid_resume_content_range("bytes 10-99/101", 10));
        let out_of_bounds = format!(
            "bytes 10-{}/{}",
            super::GEMMA_EXPECTED_SIZE_BYTES + 1,
            super::GEMMA_EXPECTED_SIZE_BYTES
        );
        assert!(!super::valid_resume_content_range(&out_of_bounds, 10));
        assert!(!super::valid_resume_content_range(
            &format!(
                "bytes 10-{}/{}",
                super::GEMMA_EXPECTED_SIZE_BYTES - 2,
                super::GEMMA_EXPECTED_SIZE_BYTES
            ),
            10
        ));
    }

    #[test]
    fn truncated_required_hugging_face_files_are_not_installed() {
        let root = tempfile_root("truncated-tree");
        let model_dir = super::portable_models_dir(&root).join("kotoba-whisper-v2.0-faster");
        std::fs::create_dir_all(&model_dir).unwrap();
        for name in [
            "model.bin",
            "config.json",
            "tokenizer.json",
            "vocabulary.json",
        ] {
            std::fs::write(model_dir.join(name), []).unwrap();
        }
        let status = super::ModelManager::scan_models_status(&root, None)
            .into_iter()
            .find(|model| model.id == "kotoba-whisper-v2.0-faster")
            .unwrap();
        assert!(
            !status.is_installed,
            "zero-byte advertised files must not install"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manually_placed_runtime_files_are_accepted_without_manifest() {
        let root = tempfile_root("manual-model");
        let model_dir = super::portable_models_dir(&root).join("kotoba-whisper-v2.0-faster");
        std::fs::create_dir_all(&model_dir).unwrap();
        for name in [
            "model.bin",
            "config.json",
            "preprocessor_config.json",
            "tokenizer.json",
            "vocabulary.json",
        ] {
            std::fs::write(model_dir.join(name), b"manual fixture").unwrap();
        }
        let status = super::ModelManager::scan_models_status(&root, None)
            .into_iter()
            .find(|model| model.id == "kotoba-whisper-v2.0-faster")
            .unwrap();
        assert!(
            status.is_installed,
            "complete manually placed runtime files should be recognized"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn install_manifest_rejects_truncated_and_duplicate_files() {
        let root = tempfile_root("manifest-validation");
        let model_dir = super::portable_models_dir(&root).join("GLuCoSE-base-ja");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("config.json"), b"ok").unwrap();
        let manifest = serde_json::json!({
            "files": [
                {"path": "config.json", "size": 3},
                {"path": "CONFIG.JSON", "size": 3}
            ]
        });
        std::fs::write(
            model_dir.join(super::INSTALL_MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(!super::validate_install_manifest(
            &model_dir,
            &["config.json".to_string()]
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_download_lock_is_shared_by_distinct_managers() {
        let root = tempfile_root("cross-manager");
        let first = super::ModelManager::new();
        let second = super::ModelManager::new();
        let _owner = first.begin_download(&root, "model-a").expect("first owner");
        assert!(second.begin_download(&root, "model-a").is_err());
        drop(_owner);
        assert!(
            root.join("models").join(".model-download.lock").exists(),
            "stable lock path must remain after releasing the OS lock"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lock_helper_process_cannot_bypass_held_lock() {
        if let Ok(root) = std::env::var("GAMEASSISTANT_LOCK_TEST_ROOT") {
            assert!(
                super::acquire_download_lock(&std::path::Path::new(&root).join("models")).is_err()
            );
            return;
        }
        let root = tempfile_root("cross-process-lock");
        let _owner = super::acquire_download_lock(&root.join("models")).expect("first owner");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "model_manager::tests::lock_helper_process_cannot_bypass_held_lock",
                "--nocapture",
            ])
            .env("GAMEASSISTANT_LOCK_TEST_ROOT", &root)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "child process bypassed the held model lock"
        );
        assert!(root.join("models").join(".model-download.lock").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_lock_denies_delete_and_stable_path_replacement_while_held() {
        let root = tempfile_root("windows-lock-share");
        let models = root.join("models");
        let lock_path = models.join(".model-download.lock");
        let _owner = super::acquire_download_lock(&models).expect("first owner");
        let second = super::ModelManager::new();
        assert!(
            second.begin_download(&root, "model-a").is_err(),
            "a second manager must observe LockFileEx contention"
        );

        assert!(
            std::fs::remove_file(&lock_path).is_err(),
            "Windows lock must deny unlink while held"
        );

        let replacement = models.join("replacement.lock");
        std::fs::write(&replacement, b"replacement").unwrap();
        assert!(
            std::fs::rename(&replacement, &lock_path).is_err(),
            "Windows lock must deny replacement of the stable lock path"
        );
        assert!(lock_path.is_file());
        drop(_owner);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn gemma_hash_is_recomputed_after_same_size_content_mutation() {
        let root = tempfile_root("gemma-mutation");
        let path = root.join(GEMMA_MODEL_ID);
        let mut file = std::fs::File::create(&path).unwrap();
        file.set_len(super::GEMMA_EXPECTED_SIZE_BYTES).unwrap();
        use std::io::{Seek, SeekFrom, Write};
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"GGUF").unwrap();
        file.flush().unwrap();
        set_test_mtime(&path);
        let first = super::validate_gemma_file(&path).expect_err("test content is not pinned");
        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(&[1]).unwrap();
        file.flush().unwrap();
        set_test_mtime(&path);
        let second = super::validate_gemma_file(&path).expect_err("mutated content is not pinned");
        assert_ne!(
            first, second,
            "same-size content mutation must not reuse a stale hash"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_and_final_install_have_one_deterministic_winner() {
        let root = tempfile_root("cancel-linearization");
        let part = root.join("model.part");
        let target = root.join("model");
        std::fs::write(&part, b"new").unwrap();
        let control = super::DownloadControl::new();
        assert!(control.cancel());
        assert!(control.commit(&part, &target).is_err());
        assert!(!target.exists());

        std::fs::write(&part, b"new").unwrap();
        let control = super::DownloadControl::new();
        control.commit(&part, &target).unwrap();
        assert!(!control.cancel());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");

        let part2 = root.join("second.part");
        let target2 = root.join("second");
        std::fs::write(&part2, b"second").unwrap();
        let control = super::DownloadControl::new();
        control.install_file(&part2, &target2).unwrap();
        assert!(
            control.cancel(),
            "cancellation remains accepted until final file"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn set_test_mtime(path: &std::path::Path) {
        let path = path.to_string_lossy().replace('\'', "''");
        let command = format!(
            "(Get-Item -LiteralPath '{path}').LastWriteTimeUtc = [DateTime]::Parse('2000-01-01T00:00:00Z')"
        );
        assert!(std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn non_gemma_tree_paths_reject_absolute_parent_and_prefix_components() {
        assert!(super::validate_hf_tree_path("safe/config.json").is_ok());
        for path in ["/etc/passwd", "../escape", r"..\escape", r"C:\\escape"] {
            assert!(super::validate_hf_tree_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn repository_documentation_is_not_required_for_model_install() {
        assert!(super::is_hf_metadata_file(".gitattributes"));
        assert!(super::is_hf_metadata_file("README.md"));
        assert!(super::is_hf_metadata_file("README_JA.md"));
        assert!(super::is_hf_metadata_file("docs/LICENSE"));
        assert!(!super::is_hf_metadata_file("config.json"));
        assert!(!super::is_hf_metadata_file("tokenizer.json"));
        assert!(!super::is_hf_metadata_file("model.bin"));
    }

    #[cfg(unix)]
    #[test]
    fn model_download_paths_reject_nested_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile_root("nested-link");
        let outside = tempfile_root("nested-link-outside");
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        symlink(&outside, models.join("escape")).unwrap();
        assert!(
            super::validate_portable_path(&root, &models.join("escape").join("model.bin")).is_err()
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[tokio::test]
    async fn tree_api_pagination_downloads_every_advertised_file_and_writes_manifest() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap();
                let path = String::from_utf8_lossy(&request[..count]);
                let path = path.split_whitespace().nth(1).unwrap_or_default();
                let (status, headers, body) = if path.starts_with("/api/") {
                    (
                        "200 OK",
                        format!(
                            "Content-Type: application/json\r\nLink: <http://{}/page-2>; rel=\"next\"\r\n",
                            addr
                        ),
                        r#"[{"type":"file","path":"nested/a.txt","size":5}]"#.to_string(),
                    )
                } else if path.starts_with("/page-2") {
                    (
                        "200 OK",
                        "Content-Type: application/json\r\n".to_string(),
                        r#"[{"type":"file","path":"b.txt","size":4}]"#.to_string(),
                    )
                } else if path.ends_with("/nested/a.txt") {
                    ("200 OK", "".to_string(), "alpha".to_string())
                } else {
                    ("200 OK", "".to_string(), "beta".to_string())
                };
                let response = format!(
                    "HTTP/1.1 {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    headers,
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let client = reqwest::Client::new();
        let tree = super::fetch_hf_tree(
            &client,
            &format!("http://{}/api/models/test/tree/main", addr),
        )
        .await
        .expect("tree API should consume all pages");
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].0, "nested/a.txt");
        assert_eq!(tree[1].0, "b.txt");

        let root = tempfile_root("tree-download");
        let control = std::sync::Arc::new(super::DownloadControl::new());
        for (path, size) in &tree {
            let target = root.join(path);
            let part = root.join(format!("{}.part", path));
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            super::download_file_chunked(
                &client,
                &format!("http://{}/resolve/main/{}", addr, path),
                &part,
                &target,
                &control,
                *size,
                |_| {},
            )
            .await
            .expect("advertised file should download completely");
        }
        let manifest = tree
            .iter()
            .map(|(path, size)| super::InstallManifestFile {
                path: path.clone(),
                size: size.unwrap(),
            })
            .collect::<Vec<_>>();
        let manifest_temp = super::prepare_install_manifest(&root, &manifest).unwrap();
        std::fs::rename(&manifest_temp, root.join(super::INSTALL_MANIFEST)).unwrap();
        assert!(super::validate_install_manifest(
            &root,
            &["nested/a.txt".to_string(), "b.txt".to_string()]
        ));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn download_rejects_truncated_http_body_and_honors_late_cancellation() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                if index == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabc",
                        )
                        .unwrap();
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nab",
                        )
                        .unwrap();
                    thread::sleep(std::time::Duration::from_millis(300));
                    let _ = stream.write_all(b"cdef");
                }
            }
        });
        let root = tempfile_root("http-integrity");
        let client = reqwest::Client::new();
        let control = std::sync::Arc::new(super::DownloadControl::new());
        let truncated = super::download_file_chunked(
            &client,
            &format!("http://{}/truncated", addr),
            &root.join("truncated.part"),
            &root.join("truncated"),
            &control,
            Some(5),
            |_| {},
        )
        .await;
        assert!(truncated.is_err());
        assert!(!root.join("truncated").exists());

        let cancellation_control = std::sync::Arc::new(super::DownloadControl::new());
        let cancellation_trigger = cancellation_control.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(cancellation_trigger.cancel());
        });
        let cancelled = super::download_file_chunked(
            &client,
            &format!("http://{}/cancel", addr),
            &root.join("cancel.part"),
            &root.join("cancel"),
            &cancellation_control,
            Some(6),
            |_| {},
        )
        .await;
        assert!(cancelled.is_err());
        assert!(!root.join("cancel").exists());
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn download_accepts_chunked_body_without_content_length() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            // Hugging Face may return small files using chunked transfer, so
            // there is no Content-Length header even though the body is valid.
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
                )
                .unwrap();
        });

        let root = tempfile_root("chunked-download");
        let target = root.join("chunked.txt");
        let part = root.join("chunked.txt.part");
        let client = reqwest::Client::new();
        let control = std::sync::Arc::new(super::DownloadControl::new());
        let downloaded = super::download_file_chunked(
            &client,
            &format!("http://{}/chunked", addr),
            &part,
            &target,
            &control,
            Some(5),
            |_| {},
        )
        .await
        .expect("chunked body should be accepted when its bytes match the advertised size");
        assert_eq!(downloaded, 5);
        assert_eq!(std::fs::read(&target).unwrap(), b"abcde");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn gemma_resume_rejects_a_short_206_body_after_valid_start_end_and_total() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            assert!(request.contains("range: bytes=10-"));
            let response = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 10-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\na",
                super::GEMMA_EXPECTED_SIZE_BYTES - 1,
                super::GEMMA_EXPECTED_SIZE_BYTES,
                super::GEMMA_EXPECTED_SIZE_BYTES - 10
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let root = tempfile_root("gemma-wire-integrity");
        let part = root.join("model.part");
        let target = root.join("model.gguf");
        std::fs::write(&part, [0_u8; 10]).unwrap();
        let client = reqwest::Client::new();
        let control = std::sync::Arc::new(super::DownloadControl::new());
        let result = super::download_gemma_file(
            &client,
            &format!("http://{}/gemma", addr),
            &part,
            &target,
            &control,
            &|_, _, _, _, _, _| {},
        )
        .await;
        assert!(result.is_err());
        assert!(!target.exists());
        assert!(root
            .read_dir()
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains("invalid-")));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delivery_urls_are_https_and_allowlisted() {
        assert!(super::is_allowed_hf_url(
            "https://huggingface.co/repo/resolve/main/file"
        ));
        assert!(super::is_allowed_hf_url(
            "https://us.aws.cdn.hf.co/repo/file?x=1"
        ));
        assert!(!super::is_allowed_hf_url(
            "http://huggingface.co/repo/resolve/main/file"
        ));
        assert!(!super::is_allowed_hf_url(
            "https://user:password@us.aws.cdn.hf.co/repo/file"
        ));
        assert!(!super::is_allowed_hf_url(
            "https://@us.aws.cdn.hf.co/repo/file"
        ));
        assert!(!super::is_allowed_hf_url(
            "https://us.aws.cdn.hf.co.evil.example/repo/file"
        ));
        assert!(!super::is_allowed_hf_url(
            "https://evil.example/repo/resolve/main/file"
        ));
        assert!(super::is_allowed_hf_redirect(
            4,
            "https://us.aws.cdn.hf.co/repo/file"
        ));
        assert!(!super::is_allowed_hf_redirect(
            5,
            "https://us.aws.cdn.hf.co/repo/file"
        ));
    }

    fn tempfile_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-model-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}

pub struct ModelManager {
    cancels: Arc<Mutex<HashMap<String, Arc<DownloadControl>>>>,
}

struct DownloadRegistration {
    cancels: Arc<Mutex<HashMap<String, Arc<DownloadControl>>>>,
    key: String,
    control: Arc<DownloadControl>,
    _lock: DownloadLock,
}

impl Drop for DownloadRegistration {
    fn drop(&mut self) {
        let mut cancels = self.cancels.lock();
        if cancels
            .get(&self.key)
            .map(|current| Arc::ptr_eq(current, &self.control))
            .unwrap_or(false)
        {
            cancels.remove(&self.key);
        }
    }
}

struct AttemptState {
    cancelled: bool,
    committed: bool,
}

struct DownloadControl {
    state: Mutex<AttemptState>,
}

impl DownloadControl {
    fn new() -> Self {
        Self {
            state: Mutex::new(AttemptState {
                cancelled: false,
                committed: false,
            }),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.state.lock().cancelled
    }

    fn cancel(&self) -> bool {
        let mut state = self.state.lock();
        if state.committed {
            return false;
        }
        state.cancelled = true;
        true
    }

    /// Cancellation and final installation are linearized by the same mutex.
    /// Once the rename wins, cancellation is no longer accepted and the caller
    /// may safely emit the completed event.
    fn commit(&self, part_path: &Path, target_path: &Path) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.cancelled {
            return Err("download cancelled".to_string());
        }
        install_file_atomic(part_path, target_path)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(target_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("flush installed file: {}", error))?;
        state.committed = true;
        Ok(())
    }

    fn install_file(&self, part_path: &Path, target_path: &Path) -> Result<(), String> {
        let state = self.state.lock();
        if state.cancelled {
            return Err("download cancelled".to_string());
        }
        install_file_atomic(part_path, target_path)
    }
}

struct DownloadLock {
    file: File,
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::Storage::FileSystem::UnlockFile;
            let _ = UnlockFile(
                HANDLE(self.file.as_raw_handle() as _),
                0,
                0,
                u32::MAX,
                u32::MAX,
            );
        }
    }
}

fn acquire_download_lock(models_dir: &Path) -> Result<DownloadLock, String> {
    reject_link_components(models_dir)?;
    fs::create_dir_all(models_dir)
        .map_err(|error| format!("create model lock directory: {}", error))?;
    reject_link_components(models_dir)?;
    let path = models_dir.join(".model-download.lock");
    reject_link_components(&path)?;

    #[cfg(windows)]
    let file = {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        };

        let wide_path: Vec<u16> = OsStr::new(&path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                0xC0000000, // GENERIC_READ | GENERIC_WRITE
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE(std::ptr::null_mut()),
            )
        }
        .map_err(|error| format!("open model download lock: {}", error))?;
        unsafe { File::from_raw_handle(handle.0 as _) }
    };

    #[cfg(not(windows))]
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open model download lock: {}", error))?;
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        };
        let mut overlapped = std::mem::zeroed();
        let flags = LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY;
        if LockFileEx(
            HANDLE(file.as_raw_handle() as _),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
        .is_err()
        {
            return Err(format!(
                "model download already locked: {}",
                models_dir.display()
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Keep the stable path forever and lock the inode, so contention cannot
        // unlink it and let a racing process replace it underneath us.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(format!(
                "model download already locked: {}",
                models_dir.display()
            ));
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        return Err("model download locking is unsupported on this platform".to_string());
    }
    Ok(DownloadLock { file })
}

impl Default for ModelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelManager {
    pub fn new() -> Self {
        Self {
            cancels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn begin_download(
        &self,
        root_dir: &Path,
        model_id: &str,
    ) -> Result<DownloadRegistration, String> {
        validate_portable_root(root_dir)?;
        let models_dir = portable_models_dir(root_dir);
        validate_portable_path(root_dir, &models_dir)?;
        let download_lock = acquire_download_lock(&models_dir)?;
        let key = format!("{}::{}", models_dir.to_string_lossy(), model_id);
        let mut cancels = self.cancels.lock();
        if cancels.contains_key(&key) {
            return Err(format!("model download already in progress: {}", model_id));
        }
        let control = Arc::new(DownloadControl::new());
        cancels.insert(key.clone(), control.clone());
        Ok(DownloadRegistration {
            cancels: self.cancels.clone(),
            key,
            control,
            _lock: download_lock,
        })
    }

    /// モデル保存先ディレクトリの取得
    pub fn get_effective_models_dir(root_dir: &Path, _custom_dir: Option<String>) -> PathBuf {
        // Legacy `models_dir` is intentionally ignored by every model consumer.
        portable_models_dir(root_dir)
    }

    /// 全モデルのインストール状態をスキャン
    pub fn scan_models_status(root_dir: &Path, custom_dir: Option<String>) -> Vec<ModelStatus> {
        let models_dir = Self::get_effective_models_dir(root_dir, custom_dir);
        let defs = get_defined_models();
        let mut results = Vec::new();

        for def in defs {
            let model_target_path = if def.id.ends_with(".gguf") {
                models_dir.join(&def.id)
            } else {
                models_dir.join(&def.id)
            };

            let mut is_installed = false;
            let mut actual_size_bytes = 0;

            if validate_portable_path(root_dir, &model_target_path).is_err() {
                results.push(ModelStatus {
                    id: def.id,
                    name: def.name,
                    description: def.description,
                    hf_repo: def.hf_repo,
                    category: def.category,
                    required: def.required,
                    estimated_size_bytes: def.estimated_size_bytes,
                    is_installed: false,
                    actual_size_bytes: 0,
                    local_path: model_target_path.to_string_lossy().to_string(),
                });
                continue;
            }

            if def.id.ends_with(".gguf") {
                if is_regular_file(&model_target_path) {
                    if let Ok(meta) = fs::metadata(&model_target_path) {
                        actual_size_bytes = meta.len();
                        is_installed = validate_gemma_file(&model_target_path).is_ok();
                    }
                }
            } else if is_real_directory(&model_target_path) {
                is_installed = validate_install_manifest(&model_target_path, &def.check_files);

                // ディレクトリの合計サイズを計算
                if let Ok(entries) = fs::read_dir(&model_target_path) {
                    for entry in entries.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_file() {
                                actual_size_bytes += meta.len();
                            }
                        }
                    }
                }
            }

            results.push(ModelStatus {
                id: def.id,
                name: def.name,
                description: def.description,
                hf_repo: def.hf_repo,
                category: def.category,
                required: def.required,
                estimated_size_bytes: def.estimated_size_bytes,
                is_installed,
                actual_size_bytes,
                local_path: model_target_path.to_string_lossy().to_string(),
            });
        }

        results
    }

    /// ダウンロードのキャンセル
    pub fn cancel_download(&self, model_id: &str) -> bool {
        let mut accepted = false;
        for (key, control) in self.cancels.lock().iter() {
            if key.ends_with(&format!("::{}", model_id)) {
                accepted |= control.cancel();
            }
        }
        accepted
    }

    /// Hugging Face からのモデルダウンロード実行 (マルチエンドポイント・ストリーミング)
    pub async fn download_model(
        &self,
        app: AppHandle,
        root_dir: PathBuf,
        model_id: String,
        custom_dir: Option<String>,
    ) -> Result<(), String> {
        validate_portable_root(&root_dir)?;
        if model_id == GEMMA_MODEL_ID && !gemma_terms_accepted(&root_dir) {
            return Err(
                "Gemma Terms must be acknowledged with current model metadata before downloading the model".to_string(),
            );
        }
        let defs = get_defined_models();
        let def = defs
            .into_iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("Unknown model id: {}", model_id))?;

        let models_dir = Self::get_effective_models_dir(&root_dir, custom_dir);
        validate_portable_path(&root_dir, &models_dir)?;
        if models_dir.exists() && !is_real_directory(&models_dir) {
            return Err("portable models path is not a real directory".to_string());
        }
        fs::create_dir_all(&models_dir)
            .map_err(|e| format!("Failed to create models dir: {}", e))?;
        let registration = self.begin_download(&root_dir, &model_id)?;
        let cancel_control = registration.control.clone();

        let emit_progress =
            |status: &str, cur: u64, tot: u64, speed: f64, pct: f64, err: Option<String>| {
                let _ = app.emit(
                    "download_progress",
                    DownloadProgressEvent {
                        model_id: model_id.clone(),
                        current_bytes: cur,
                        total_bytes: tot,
                        speed_mbps: speed,
                        percent: pct,
                        status: status.to_string(),
                        error_message: err,
                    },
                );
            };

        emit_progress("downloading", 0, def.estimated_size_bytes, 0.0, 0.0, None);

        let target_dir = if def.id.ends_with(".gguf") {
            models_dir.clone()
        } else {
            let td = models_dir.join(&def.id);
            validate_portable_path(&root_dir, &td)?;
            if td.exists() && !is_real_directory(&td) {
                return Err("model target path is not a real directory".to_string());
            }
            fs::create_dir_all(&td).map_err(|e| format!("Failed to create target dir: {}", e))?;
            td
        };

        // ダウンロード対象のファイルリストを取得
        // 大きなモデル (780MB+) も想定するため総タイムアウトは長め、接続失敗だけ短く検知する
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(3600))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if is_allowed_hf_redirect(attempt.previous().len(), attempt.url().as_str()) {
                    attempt.follow()
                } else if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else {
                    attempt.error("redirect target is not an allowlisted HTTPS Hugging Face host")
                }
            }))
            .user_agent("GameAssistant/0.2")
            .build()
            .map_err(|e| e.to_string())?;

        // 1. GGUF 単体ファイルの場合. Gemma uses one immutable URL and is
        // installed only after size, SHA-256, and GGUF magic validation.
        if def.id.ends_with(".gguf") {
            let filename = &def.id;
            let target_file = target_dir.join(filename);
            let part_file = target_dir.join(format!("{}.part", filename));
            validate_portable_path(&root_dir, &target_file)?;
            validate_portable_path(&root_dir, &part_file)?;
            if target_file.exists() && !is_regular_file(&target_file) {
                return Err("Gemma target path is not a regular file".to_string());
            }
            if target_file.is_file() {
                if validate_gemma_file(&target_file).is_ok() {
                    emit_progress(
                        "completed",
                        GEMMA_EXPECTED_SIZE_BYTES,
                        GEMMA_EXPECTED_SIZE_BYTES,
                        0.0,
                        100.0,
                        None,
                    );
                    return Ok(());
                }
                quarantine_file(&target_file)?;
            }

            let gemma_result = download_gemma_file(
                &client,
                GEMMA_DOWNLOAD_URL,
                &part_file,
                &target_file,
                &cancel_control,
                &emit_progress,
            )
            .await;
            let gemma_result = match gemma_result {
                Err(error) if error.contains("malformed 206 body") => {
                    // The malformed partial was quarantined by the transfer
                    // seam; retry once without any resumable bytes.
                    download_gemma_file(
                        &client,
                        GEMMA_DOWNLOAD_URL,
                        &part_file,
                        &target_file,
                        &cancel_control,
                        &emit_progress,
                    )
                    .await
                }
                other => other,
            };
            if let Err(error) = gemma_result {
                let status = if cancel_control.is_cancelled() {
                    "cancelled"
                } else {
                    "error"
                };
                emit_progress(
                    status,
                    0,
                    def.estimated_size_bytes,
                    0.0,
                    0.0,
                    if status == "cancelled" {
                        None
                    } else {
                        Some(error.clone())
                    },
                );
                if status == "cancelled" {
                    return Ok(());
                }
                return Err(error);
            }

            emit_progress(
                "completed",
                def.estimated_size_bytes,
                def.estimated_size_bytes,
                0.0,
                100.0,
                None,
            );
            return Ok(());
        }

        // 2. ディレクトリモデル (Hugging Face Tree API から取得)
        let tree_urls = vec![
            format!(
                "https://huggingface.co/api/models/{}/tree/main",
                def.hf_repo
            ),
            format!("https://hf-mirror.com/api/models/{}/tree/main", def.hf_repo),
        ];

        let mut files_to_download: Vec<(String, Option<u64>)> = Vec::new();
        let mut tree_error = None;
        for tree_url in tree_urls {
            match fetch_hf_tree(&client, &tree_url).await {
                Ok(files) if !files.is_empty() => {
                    files_to_download = files;
                    break;
                }
                Ok(_) => tree_error = Some("Hugging Face tree API returned no files".to_string()),
                Err(error) => tree_error = Some(error),
            }
        }
        if files_to_download.is_empty() {
            return Err(tree_error.unwrap_or_else(|| {
                "Hugging Face tree API did not return a complete file listing".to_string()
            }));
        }
        // Repository metadata is useful on the model page but is not part of
        // the runtime artifact.  A transient failure or a renamed README must
        // never make an otherwise complete model impossible to install.
        files_to_download.retain(|(path, _)| !is_hf_metadata_file(path));
        if files_to_download.is_empty() {
            return Err("Hugging Face tree contained no model files".to_string());
        }
        let seen_paths: HashSet<String> = files_to_download
            .iter()
            .map(|(path, _)| path.replace('\\', "/").to_ascii_lowercase())
            .collect();

        let total_est_bytes: u64 = if files_to_download.iter().any(|(_, s)| s.is_some()) {
            files_to_download.iter().map(|(_, s)| s.unwrap_or(0)).sum()
        } else {
            def.estimated_size_bytes
        };
        let mut manifest_files: Vec<InstallManifestFile> = Vec::new();

        let mut overall_downloaded: u64 = 0;
        let start_time = Instant::now();
        let mut failed_files: Vec<String> = Vec::new();

        for (rel_path, file_size) in files_to_download {
            validate_hf_tree_path(&rel_path)?;
            let file_target = target_dir.join(&rel_path);
            let file_part = target_dir.join(format!("{}.part", rel_path));
            validate_portable_path(&root_dir, &file_target)?;
            validate_portable_path(&root_dir, &file_part)?;
            if cancel_control.is_cancelled() {
                emit_progress(
                    "cancelled",
                    overall_downloaded,
                    total_est_bytes,
                    0.0,
                    0.0,
                    None,
                );
                return Ok(());
            }

            if file_target.exists() && !is_regular_file(&file_target) {
                return Err(format!("model target is not a regular file: {}", rel_path));
            }

            if let Some(parent) = file_target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create model subdirectory: {}", error))?;
                reject_link_components(parent)?;
            }

            let mirrors = vec![
                format!(
                    "https://huggingface.co/{}/resolve/main/{}",
                    def.hf_repo, rel_path
                ),
                format!(
                    "https://hf-mirror.com/{}/resolve/main/{}",
                    def.hf_repo, rel_path
                ),
            ];

            let mut file_ok = false;
            for url in mirrors {
                if cancel_control.is_cancelled() {
                    emit_progress(
                        "cancelled",
                        overall_downloaded,
                        total_est_bytes,
                        0.0,
                        0.0,
                        None,
                    );
                    return Ok(());
                }

                let cur_base = overall_downloaded;
                let on_chunk = |chunk_len: u64| {
                    let total_cur = cur_base + chunk_len;
                    let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
                    let speed = (total_cur as f64 / 1_048_576.0) / elapsed;
                    let pct = ((total_cur as f64 / total_est_bytes as f64) * 100.0).min(99.9);
                    emit_progress("downloading", total_cur, total_est_bytes, speed, pct, None);
                };

                match download_file_chunked(
                    &client,
                    &url,
                    &file_part,
                    &file_target,
                    &cancel_control,
                    file_size,
                    on_chunk,
                )
                .await
                {
                    Ok(bytes) => {
                        let actual_size = file_size.unwrap_or(bytes);
                        overall_downloaded += actual_size;
                        manifest_files.push(InstallManifestFile {
                            path: rel_path.clone(),
                            size: actual_size,
                        });
                        file_ok = true;
                        break;
                    }
                    Err(e) => {
                        if cancel_control.is_cancelled() {
                            emit_progress(
                                "cancelled",
                                overall_downloaded,
                                total_est_bytes,
                                0.0,
                                0.0,
                                None,
                            );
                            return Ok(());
                        }
                        println!(
                            "[WARN] Failed to download {} from {}: {}. Trying next mirror...",
                            rel_path, url, e
                        );
                    }
                }
            }

            if !file_ok {
                println!(
                    "[WARN] Optional/Required file {} could not be fetched",
                    rel_path
                );
                failed_files.push(rel_path);
            }
        }

        if !failed_files.is_empty() {
            let msg = format!(
                "Failed to download {} file(s): {}",
                failed_files.len(),
                failed_files.join(", ")
            );
            emit_progress(
                "error",
                overall_downloaded,
                total_est_bytes,
                0.0,
                0.0,
                Some(msg.clone()),
            );
            return Err(msg);
        }

        if manifest_files.len() != seen_paths.len() {
            return Err("cannot persist an install without advertised file sizes".to_string());
        }
        let manifest_temp = prepare_install_manifest(&target_dir, &manifest_files)?;
        if let Err(error) =
            cancel_control.commit(&manifest_temp, &target_dir.join(INSTALL_MANIFEST))
        {
            let _ = fs::remove_file(&manifest_temp);
            return Err(error);
        }

        emit_progress(
            "completed",
            total_est_bytes,
            total_est_bytes,
            0.0,
            100.0,
            None,
        );
        Ok(())
    }
}

fn validate_install_manifest(model_dir: &Path, required_checks: &[String]) -> bool {
    if reject_link_components(model_dir).is_err() {
        return false;
    }
    let manifest_path = model_dir.join(INSTALL_MANIFEST);
    if reject_link_components(&manifest_path).is_err() {
        return false;
    }
    // Manual installation is supported as a recovery path.  A user-provided
    // model tree has no generated manifest, so validate every runtime marker
    // directly and let the normal setup retry re-enter the managed path.
    let Ok(bytes) = fs::read(&manifest_path) else {
        return validate_manual_model_files(model_dir, required_checks);
    };
    let Ok(manifest) = serde_json::from_slice::<InstallManifest>(&bytes) else {
        // A present but malformed manifest is treated as corruption rather
        // than silently downgraded to the looser manual-file contract.
        return false;
    };
    if !manifest.complete {
        return false;
    }
    let mut seen = HashSet::new();
    for entry in &manifest.files {
        if validate_hf_tree_path(&entry.path).is_err()
            || !seen.insert(entry.path.replace('\\', "/").to_ascii_lowercase())
        {
            return false;
        }
        let path = model_dir.join(&entry.path);
        if reject_link_components(&path).is_err() {
            return false;
        }
        if !is_regular_file(&path) || fs::metadata(&path).map(|m| m.len()).ok() != Some(entry.size)
        {
            return false;
        }
    }
    required_checks.iter().all(|check| {
        let direct = model_dir.join(check);
        (is_regular_file(&direct) && seen.contains(&check.replace('\\', "/").to_ascii_lowercase()))
            || ((check == "model.bin" || check == "pytorch_model.bin")
                && is_regular_file(&model_dir.join("model.safetensors"))
                && seen.contains("model.safetensors"))
    })
}

fn validate_manual_model_files(model_dir: &Path, required_checks: &[String]) -> bool {
    required_checks.iter().all(|check| {
        let direct = model_dir.join(check);
        if reject_link_components(&direct).is_err() {
            return false;
        }
        if is_regular_file(&direct)
            && fs::metadata(&direct)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
        {
            return true;
        }
        (check == "model.bin" || check == "pytorch_model.bin")
            && is_regular_file(&model_dir.join("model.safetensors"))
            && fs::metadata(model_dir.join("model.safetensors"))
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
    })
}

fn prepare_install_manifest(
    model_dir: &Path,
    files: &[InstallManifestFile],
) -> Result<PathBuf, String> {
    reject_link_components(model_dir)?;
    for entry in files {
        validate_hf_tree_path(&entry.path)?;
        reject_link_components(&model_dir.join(&entry.path))?;
    }
    let bytes = serde_json::to_vec_pretty(&InstallManifest {
        files: files.to_vec(),
        complete: true,
    })
    .map_err(|error| format!("serialize model install manifest: {}", error))?;
    let temp = model_dir.join(format!("{}.tmp-{}", INSTALL_MANIFEST, std::process::id()));
    reject_link_components(model_dir)?;
    reject_link_components(&temp)?;
    fs::write(&temp, bytes).map_err(|error| format!("write model install manifest: {}", error))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&temp)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("flush model install manifest: {}", error))?;
    Ok(temp)
}

pub fn validate_gemma_file(path: &Path) -> Result<(), String> {
    validate_gemma_file_uncached(path)
}

pub fn validate_gemma_file_uncached(path: &Path) -> Result<(), String> {
    reject_link_components(path)?;
    if !is_regular_file(path) {
        return Err(format!(
            "Gemma model is not a regular file: {}",
            path.display()
        ));
    }
    let metadata = fs::metadata(path).map_err(|error| format!("read Gemma metadata: {}", error))?;
    if metadata.len() != GEMMA_EXPECTED_SIZE_BYTES {
        return Err(format!(
            "Gemma size mismatch: expected {}, got {}",
            GEMMA_EXPECTED_SIZE_BYTES,
            metadata.len()
        ));
    }
    let mut file = File::open(path).map_err(|error| format!("open Gemma model: {}", error))?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .map_err(|error| format!("read GGUF header: {}", error))?;
    if &magic != b"GGUF" {
        return Err("Gemma model is not a GGUF file".to_string());
    }
    let mut hasher = Sha256::new();
    file.rewind()
        .map_err(|error| format!("rewind Gemma model: {}", error))?;
    // Keep the hashing buffer on the heap.  A 1 MiB stack array overflows the
    // default Windows thread stack in the portable build while scanning Gemma
    // during startup, surfacing as STATUS_STACK_OVERFLOW (0xc00000fd).
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("hash Gemma model: {}", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != GEMMA_EXPECTED_SHA256 {
        return Err(format!(
            "Gemma SHA-256 mismatch: expected {}, got {}",
            GEMMA_EXPECTED_SHA256, actual
        ));
    }
    Ok(())
}

fn quarantine_file(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("model has no parent: {}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("model has no name: {}", path.display()))?
        .to_string_lossy();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut quarantined = parent.join(format!("{}.invalid-{}", name, stamp));
    let mut suffix = 0_u32;
    while quarantined.exists() {
        suffix += 1;
        quarantined = parent.join(format!("{}.invalid-{}-{}", name, stamp, suffix));
    }
    fs::rename(path, &quarantined)
        .map_err(|error| format!("quarantine invalid model {}: {}", path.display(), error))?;
    Ok(quarantined)
}

fn quarantine_path(path: &Path) -> Result<PathBuf, String> {
    quarantine_file(path)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file() && !is_reparse_point(&metadata))
        .unwrap_or(false)
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir() && !is_reparse_point(&metadata))
        .unwrap_or(false)
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

fn install_file_atomic(part_path: &Path, target_path: &Path) -> Result<(), String> {
    reject_link_components(part_path)?;
    reject_link_components(target_path)?;
    let parent = target_path
        .parent()
        .ok_or_else(|| format!("target has no parent: {}", target_path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("create install directory: {}", error))?;
    reject_link_components(parent)?;
    #[cfg(windows)]
    {
        if target_path.exists() {
            let backup = parent.join(format!(
                ".{}.backup-{}",
                target_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                std::process::id()
            ));
            if backup.exists() {
                let _ = fs::remove_file(&backup);
            }
            fs::rename(target_path, &backup)
                .map_err(|error| format!("backup existing model file: {}", error))?;
            if let Err(error) = fs::rename(part_path, target_path) {
                let _ = fs::rename(&backup, target_path);
                return Err(format!("install model file: {}", error));
            }
            let _ = fs::remove_file(&backup);
            return Ok(());
        }
    }
    fs::rename(part_path, target_path).map_err(|error| format!("install model file: {}", error))
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn valid_resume_content_range(value: &str, resume_from: u64) -> bool {
    parse_content_range(value)
        .map(|(start, end, total)| {
            start == resume_from
                && end >= start
                && end == total.saturating_sub(1)
                && total == GEMMA_EXPECTED_SIZE_BYTES
        })
        .unwrap_or(false)
}

fn valid_resume_response(
    content_range: &str,
    content_length: Option<u64>,
    body_count: u64,
    resume_from: u64,
) -> bool {
    valid_resume_content_range(content_range, resume_from)
        && content_length == Some(GEMMA_EXPECTED_SIZE_BYTES - resume_from)
        && body_count == GEMMA_EXPECTED_SIZE_BYTES - resume_from
}

fn validate_hf_tree_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return Err(format!("unsafe Hugging Face tree path: {}", path));
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Err(format!("unsafe Hugging Face tree path: {}", path));
    }
    for component in path.split(['/', '\\']) {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!("unsafe Hugging Face tree path: {}", path));
        }
    }
    Ok(())
}

/// Files maintained as repository documentation/metadata rather than model
/// inputs.  These are intentionally excluded from the install manifest so a
/// documentation-only change or unavailable README cannot block setup.
fn is_hf_metadata_file(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".gitattributes"
        || name.starts_with("readme")
        || name == "license"
        || name.starts_with("license.")
        || name == "copying"
        || name.starts_with("copying.")
}

fn is_allowed_hf_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() || has_userinfo_delimiter(raw) {
        return false;
    }
    matches!(
        url.host_str(),
        Some(
            "huggingface.co"
                | "www.huggingface.co"
                | "hf.co"
                | "hf-mirror.com"
                | "cdn-lfs.huggingface.co"
                | "cdn-lfs-us-1.hf.co"
                | "cas-bridge.xethub.hf.co"
                | "cdn-lfs.hf.co"
                | "us.aws.cdn.hf.co"
        )
    )
}

fn has_userinfo_delimiter(raw: &str) -> bool {
    let Some((_, authority_and_path)) = raw.split_once("://") else {
        return false;
    };
    authority_and_path
        .split(['/', '?', '#'])
        .next()
        .map(|authority| authority.contains('@'))
        .unwrap_or(false)
}

fn is_allowed_hf_redirect(previous_redirects: usize, raw: &str) -> bool {
    previous_redirects < 5 && is_allowed_hf_url(raw)
}

async fn download_gemma_file<F>(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    target_path: &Path,
    cancel_flag: &DownloadControl,
    emit_progress: &F,
) -> Result<(), String>
where
    F: Fn(&str, u64, u64, f64, f64, Option<String>),
{
    reject_link_components(part_path)?;
    reject_link_components(target_path)?;
    if part_path.exists() && !is_regular_file(part_path) {
        return Err("Gemma partial path is not a regular file".to_string());
    }
    if is_regular_file(part_path) && validate_gemma_file_uncached(part_path).is_ok() {
        if cancel_flag.is_cancelled() {
            return Err("Gemma download cancelled".to_string());
        }
        cancel_flag.commit(part_path, target_path)?;
        return Ok(());
    }
    let existing = fs::metadata(part_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if existing > GEMMA_EXPECTED_SIZE_BYTES {
        quarantine_file(part_path)?;
    } else if existing == GEMMA_EXPECTED_SIZE_BYTES {
        // A complete-sized partial that failed validation is not resumable;
        // preserve it before starting a fresh request.
        quarantine_file(part_path)?;
    }
    let mut resume_from = fs::metadata(part_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let response = loop {
        let mut request = client.get(url);
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={}-", resume_from));
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("download Gemma: {}", error))?;
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            let valid = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .map(|value| valid_resume_content_range(value, resume_from))
                .unwrap_or(false);
            let expected_body = GEMMA_EXPECTED_SIZE_BYTES.saturating_sub(resume_from);
            if !valid || response.content_length() != Some(expected_body) {
                let had_partial = part_path.exists();
                if had_partial {
                    quarantine_path(part_path)?;
                }
                if resume_from > 0 {
                    resume_from = 0;
                    continue;
                }
                return Err("Gemma response has invalid Content-Range".to_string());
            }
        }
        break response;
    };
    let append = resume_from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !response.status().is_success() {
        return Err(format!("Gemma download HTTP error: {}", response.status()));
    }
    let initial = if append { resume_from } else { 0 };
    let mut file = if append {
        OpenOptions::new().create(true).append(true).open(part_path)
    } else {
        File::create(part_path)
    }
    .map_err(|error| format!("open Gemma partial file: {}", error))?;
    let response_content_length = response.content_length();
    let response_content_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_default();
    let mut stream = response.bytes_stream();
    let start = Instant::now();
    let mut downloaded = initial;
    while let Some(chunk) = stream.next().await {
        if cancel_flag.is_cancelled() {
            let _ = file.flush();
            let _ = file.sync_all();
            return Err("Gemma download cancelled; partial file retained for resume".to_string());
        }
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if append {
                    let _ = quarantine_file(part_path);
                }
                return Err(format!("read Gemma download: {}", error));
            }
        };
        file.write_all(&chunk)
            .map_err(|error| format!("write Gemma partial file: {}", error))?;
        downloaded += chunk.len() as u64;
        let speed = (downloaded as f64 / 1_048_576.0) / start.elapsed().as_secs_f64().max(0.001);
        let percent = (downloaded as f64 / GEMMA_EXPECTED_SIZE_BYTES as f64 * 100.0).min(99.9);
        emit_progress(
            "downloading",
            downloaded,
            GEMMA_EXPECTED_SIZE_BYTES,
            speed,
            percent,
            None,
        );
    }
    file.flush()
        .map_err(|error| format!("flush Gemma partial file: {}", error))?;
    file.sync_all()
        .map_err(|error| format!("sync Gemma partial file: {}", error))?;
    drop(file);
    let body_count = downloaded.saturating_sub(initial);
    if append {
        if !valid_resume_response(
            &response_content_range,
            response_content_length,
            body_count,
            resume_from,
        ) {
            let _ = quarantine_file(part_path);
            return Err("malformed 206 body; partial file quarantined for restart".to_string());
        }
    } else if body_count != GEMMA_EXPECTED_SIZE_BYTES {
        let _ = quarantine_file(part_path);
        return Err(format!(
            "Gemma body size mismatch: expected {}, got {}",
            GEMMA_EXPECTED_SIZE_BYTES, downloaded
        ));
    }
    if cancel_flag.is_cancelled() {
        return Err("Gemma download cancelled; partial file retained for resume".to_string());
    }
    if let Err(error) = validate_gemma_file_uncached(part_path) {
        let _ = quarantine_file(part_path);
        return Err(error);
    }
    if cancel_flag.is_cancelled() {
        return Err("Gemma download cancelled; partial file retained for resume".to_string());
    }
    cancel_flag.commit(part_path, target_path)?;
    Ok(())
}

async fn download_file_chunked<F>(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    target_path: &Path,
    cancel_flag: &DownloadControl,
    expected_size: Option<u64>,
    on_chunk: F,
) -> Result<u64, String>
where
    F: Fn(u64),
{
    reject_link_components(part_path)?;
    reject_link_components(target_path)?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP error: {}", resp.status()));
    }

    if let Some(expected) = expected_size {
        // Hugging Face may use chunked transfer for small repository files,
        // in which case Content-Length is intentionally absent.  Reject an
        // explicitly advertised wrong length, but validate the actual byte
        // count below so both fixed-length and chunked responses are safe.
        if let Some(advertised) = resp.content_length() {
            if advertised != expected {
                return Err(format!(
                    "content length mismatch: expected {}, got {}",
                    expected, advertised
                ));
            }
        }
    }

    if part_path.exists() && !is_regular_file(part_path) {
        return Err("download partial path is not a regular file".to_string());
    }
    let mut file = File::create(part_path).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk_res) = stream.next().await {
        if cancel_flag.is_cancelled() {
            let _ = fs::remove_file(part_path);
            return Err("download cancelled".to_string());
        }

        let chunk = match chunk_res {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = fs::remove_file(part_path);
                return Err(error.to_string());
            }
        };
        if let Some(expected) = expected_size {
            let next_size = downloaded.saturating_add(chunk.len() as u64);
            if next_size > expected {
                let _ = fs::remove_file(part_path);
                return Err(format!(
                    "body length exceeds advertised size: expected {}, got at least {}",
                    expected, next_size
                ));
            }
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        on_chunk(downloaded);
    }

    file.flush().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    if let Some(expected) = expected_size {
        if downloaded != expected {
            let _ = fs::remove_file(part_path);
            return Err(format!(
                "body length mismatch: expected {}, got {}",
                expected, downloaded
            ));
        }
    }
    drop(file);

    if cancel_flag.is_cancelled() {
        return Err("download cancelled".to_string());
    }
    cancel_flag.commit(part_path, target_path)?;
    Ok(downloaded)
}

/// Enumerate the complete recursive Hugging Face tree. A successful first page
/// is not sufficient: every `Link: rel="next"` page must be consumed, and a
/// full-sized page without pagination metadata is rejected as potentially
/// truncated rather than silently falling back to check_files.
async fn fetch_hf_tree(
    client: &reqwest::Client,
    initial_url: &str,
) -> Result<Vec<(String, Option<u64>)>, String> {
    let mut next_url = Some(if initial_url.contains('?') {
        format!("{}&recursive=true&limit=1000", initial_url)
    } else {
        format!("{}?recursive=true&limit=1000", initial_url)
    });
    let mut visited = HashSet::new();
    let mut files = Vec::new();
    let mut seen_paths = HashSet::new();
    while let Some(url) = next_url.take() {
        if !is_allowed_tree_url(&url) {
            return Err(format!("Hugging Face tree URL is not allowlisted: {}", url));
        }
        if !visited.insert(url.clone()) {
            return Err("Hugging Face tree pagination loop detected".to_string());
        }
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| format!("request Hugging Face tree: {}", error))?;
        if !response.status().is_success() {
            return Err(format!(
                "Hugging Face tree HTTP error: {}",
                response.status()
            ));
        }
        let next = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_next_link)
            .or_else(|| {
                response
                    .headers()
                    .get("x-next-page")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
            });
        let items = response
            .json::<Vec<HfTreeItem>>()
            .await
            .map_err(|error| format!("parse Hugging Face tree: {}", error))?;
        for item in &items {
            if item.r#type != "file" {
                continue;
            }
            validate_hf_tree_path(&item.path)?;
            let size = item.size.ok_or_else(|| {
                format!(
                    "Hugging Face tree file has no advertised size: {}",
                    item.path
                )
            })?;
            let normalized = item.path.replace('\\', "/").to_ascii_lowercase();
            if !seen_paths.insert(normalized) {
                return Err(format!("duplicate Hugging Face tree path: {}", item.path));
            }
            files.push((item.path.clone(), Some(size)));
        }
        if next.is_none() && items.len() >= 1000 {
            return Err(
                "Hugging Face tree page is full but has no pagination link; refusing truncated listing"
                    .to_string(),
            );
        }
        next_url = next.and_then(|candidate| {
            if let Ok(resolved) = reqwest::Url::parse(&candidate) {
                Some(resolved.to_string())
            } else {
                let base = reqwest::Url::parse(&url).ok()?;
                base.join(&candidate)
                    .ok()
                    .map(|resolved| resolved.to_string())
            }
        });
    }
    Ok(files)
}

fn is_allowed_tree_url(raw: &str) -> bool {
    if is_allowed_hf_url(raw) {
        return true;
    }
    #[cfg(test)]
    {
        return reqwest::Url::parse(raw)
            .ok()
            .map(|url| {
                let host = url.host_str().unwrap_or_default();
                url.scheme() == "http" && (host == "127.0.0.1" || host == "localhost")
            })
            .unwrap_or(false);
    }
    #[cfg(not(test))]
    false
}

fn parse_next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let (target, params) = part.trim().split_once('>')?;
        if !params.contains("rel=\"next\"") && !params.contains("rel=next") {
            return None;
        }
        target.strip_prefix('<').map(str::to_owned)
    })
}
