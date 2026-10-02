use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub file_path: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsResponse {
    pub skills: Vec<SkillItem>,
    pub enabled_skills: Vec<String>,
    pub master_enabled: bool,
}

pub fn load_settings_file(root_dir: &Path) -> Value {
    let settings_path = root_dir.join("settings.json");
    if settings_path.exists() {
        if let Ok(content) = fs::read_to_string(&settings_path) {
            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                return json;
            }
        }
    }
    serde_json::json!({})
}

/// settings.json から whisper_device を取得する（デフォルトは "cuda"）。
pub fn get_whisper_device(root_dir: &Path) -> String {
    let settings = load_settings_file(root_dir);
    if let Some(dev) = settings.get("whisper_device").and_then(|v| v.as_str()) {
        if dev.trim().eq_ignore_ascii_case("cpu") {
            return "cpu".to_string();
        }
    }
    "cuda".to_string()
}

/// フロントエンドに返却するための安全化された設定 snapshot をロードする。
/// - 初回や未マイグレーション時は自動的に平文クレデンシャルを OS credential store (DPAPI) へ移行
/// - 平文シークレットは完全に除去
/// - 設定済みフラグ（`has_gemini_api_key`, `has_twitch_client_secret` 等）を付与
pub fn load_frontend_settings(root_dir: &Path) -> Result<Value, String> {
    let mut current = load_settings_file(root_dir);
    let store = crate::credentials::DpapiCredentialStore::new(root_dir);
    crate::credentials::sanitize_settings_for_frontend(root_dir, &mut current, &store)?;
    Ok(current)
}

pub fn save_setting_key(root_dir: &Path, key: &str, value: Value) -> Result<Value, String> {
    if crate::credentials::is_secret_key(key) {
        match value {
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    crate::credentials::delete_secret(root_dir, key)?;
                } else {
                    crate::credentials::set_secret(root_dir, key, trimmed)?;
                }
            }
            Value::Null => {
                crate::credentials::delete_secret(root_dir, key)?;
            }
            _ => {
                return Err(format!("Secret setting '{}' must be a string or null", key));
            }
        }
        return load_frontend_settings(root_dir);
    }

    let settings_path = root_dir.join("settings.json");
    let mut current_json = load_settings_file(root_dir);

    if let Value::Object(ref mut map) = current_json {
        map.insert(key.to_string(), value);
    } else {
        let mut map = serde_json::Map::new();
        map.insert(key.to_string(), value);
        current_json = Value::Object(map);
    }

    let pretty_str = serde_json::to_string_pretty(&current_json).map_err(|e| e.to_string())?;

    fs::write(&settings_path, pretty_str).map_err(|e| e.to_string())?;
    load_frontend_settings(root_dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveSettingResponse {
    pub settings: Value,
    pub warning: Option<String>,
}

/// 設定を永続化し、永続化が成功した場合にのみ worker_sync を実行する。
/// - 永続化失敗時: worker_sync は呼ばれず、即時 Err を返す（状態分岐を防止）
/// - 永続化成功 + worker反映失敗時: 設定は保持され、warning を返す
/// - 永続化成功 + worker反映成功時: warning は None
pub fn save_setting_with_worker_sync<F>(
    root_dir: &Path,
    key: &str,
    value: &Value,
    worker_sync: F,
) -> Result<SaveSettingResponse, String>
where
    F: FnOnce(&str, &Value) -> Result<(), String>,
{
    // 1. Rust 永続化を先に実行（canonical settings を確定）
    let settings = save_setting_key(root_dir, key, value.clone())?;

    // 2. 永続化が成功した場合のみ、worker への動的反映を試行
    let mut warning = None;
    if let Err(e) = worker_sync(key, value) {
        warning = Some(e);
    }

    Ok(SaveSettingResponse { settings, warning })
}

pub fn scan_skills(root_dir: &Path) -> SkillsResponse {
    let skills_dir = root_dir.join("skills");
    let mut skills = Vec::new();

    if skills_dir.exists() && skills_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // 1. サブディレクトリ形式: skills/{skill_dir}/SKILL.md
                    let skill_md = path.join("SKILL.md");
                    let target_path = if skill_md.exists() {
                        Some(skill_md)
                    } else {
                        let lower_md = path.join("skill.md");
                        if lower_md.exists() {
                            Some(lower_md)
                        } else {
                            None
                        }
                    };

                    if let Some(target) = target_path {
                        let dir_stem = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let raw_content = fs::read_to_string(&target).unwrap_or_default();
                        let (name, desc, body) = parse_frontmatter(&raw_content, &dir_stem);

                        skills.push(SkillItem {
                            id: dir_stem,
                            name,
                            description: desc,
                            file_path: Some(target.to_string_lossy().into_owned()),
                            content: Some(body),
                        });
                    }
                } else if path.is_file() {
                    // 2. 単一ファイル形式: skills/{skill_name}.md
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if ext == "md" || ext == "yaml" || ext == "yml" {
                        let stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let raw_content = fs::read_to_string(&path).unwrap_or_default();
                        let (name, desc, body) = parse_frontmatter(&raw_content, &stem);

                        skills.push(SkillItem {
                            id: stem,
                            name,
                            description: desc,
                            file_path: Some(path.to_string_lossy().into_owned()),
                            content: Some(body),
                        });
                    }
                }
            }
        }
    }

    let settings = load_settings_file(root_dir);
    let enabled_skills: Vec<String> = settings
        .get("enabled_blog_skills")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| vec!["k0ta-writing-style".to_string()]);

    let master_enabled = settings
        .get("enable_blog_skills")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    SkillsResponse {
        skills,
        enabled_skills,
        master_enabled,
    }
}

pub fn get_skill_content(root_dir: &Path, id: &str) -> Result<String, String> {
    let skills_dir = root_dir.join("skills");
    let dir_skill = skills_dir.join(id).join("SKILL.md");
    if dir_skill.exists() {
        return fs::read_to_string(&dir_skill).map_err(|e| e.to_string());
    }
    let file_skill = skills_dir.join(format!("{}.md", id));
    if file_skill.exists() {
        return fs::read_to_string(&file_skill).map_err(|e| e.to_string());
    }
    Err(format!("Skill '{}' not found", id))
}

pub fn save_skill_content(root_dir: &Path, id: &str, content: &str) -> Result<(), String> {
    let skills_dir = root_dir.join("skills");
    let dir_skill = skills_dir.join(id).join("SKILL.md");
    if dir_skill.exists() {
        return fs::write(&dir_skill, content).map_err(|e| e.to_string());
    }
    let file_skill = skills_dir.join(format!("{}.md", id));
    if file_skill.exists() {
        return fs::write(&file_skill, content).map_err(|e| e.to_string());
    }

    // 新規スキルの場合はディレクトリ構造を作成
    let target_dir = skills_dir.join(id);
    let _ = fs::create_dir_all(&target_dir);
    fs::write(target_dir.join("SKILL.md"), content).map_err(|e| e.to_string())
}

pub fn load_enabled_skills_text(root_dir: &Path, enabled_ids: &[String]) -> String {
    if enabled_ids.is_empty() {
        return String::new();
    }
    let res = scan_skills(root_dir);
    let mut sections = Vec::new();

    for id in enabled_ids {
        if let Some(item) = res.skills.iter().find(|s| &s.id == id) {
            let mut sec = format!("## スキル: {} ({})\n", item.name, item.id);
            if !item.description.is_empty() {
                sec.push_str(&format!("> 説明: {}\n\n", item.description));
            }
            if let Some(ref c) = item.content {
                sec.push_str(c);
            }
            sections.push(sec);
        }
    }

    sections.join("\n\n---\n\n")
}

fn parse_frontmatter(raw: &str, default_id: &str) -> (String, String, String) {
    let mut name = default_id.to_string();
    let mut description = "ゲームアシスタント用執筆スキル".to_string();
    let mut body = raw.to_string();

    let trimmed = raw.trim();
    if trimmed.starts_with("---") {
        if let Some(end_idx) = trimmed[3..].find("---") {
            let front_matter = &trimmed[3..3 + end_idx];
            body = trimmed[3 + end_idx + 3..].trim().to_string();

            for line in front_matter.lines() {
                let l = line.trim();
                if let Some(pos) = l.find(':') {
                    let key = l[..pos].trim().to_lowercase();
                    let val = l[pos + 1..]
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string();
                    if key == "name" && !val.is_empty() {
                        name = val;
                    } else if key == "description" && !val.is_empty() {
                        description = val;
                    }
                }
            }
        }
    } else {
        // 通常の見出しなどから抽出
        for line in raw.lines() {
            let l = line.trim();
            if l.starts_with("# ") {
                name = l.trim_start_matches("# ").trim().to_string();
                break;
            }
        }
    }

    (name, description, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_save_setting_with_worker_sync_does_not_call_worker_on_persistence_failure() {
        // 存在しない不正なパスを指定して永続化を意図的に失敗させる
        let invalid_root = Path::new("Z:\\non_existent_directory_for_test_12345");
        let worker_called = AtomicBool::new(false);

        let res = save_setting_with_worker_sync(
            invalid_root,
            "preallocate_vram",
            &serde_json::json!(true),
            |_k, _v| {
                worker_called.store(true, Ordering::SeqCst);
                Ok(())
            },
        );

        assert!(res.is_err(), "Persistence failure must return Err");
        assert!(
            !worker_called.load(Ordering::SeqCst),
            "Worker sync must NOT be called when persistence fails"
        );
    }

    #[test]
    fn test_save_setting_with_worker_sync_returns_warning_on_worker_failure() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_settings_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::create_dir_all(&temp_dir);

        let worker_called = AtomicBool::new(false);
        let res = save_setting_with_worker_sync(
            &temp_dir,
            "preallocate_vram",
            &serde_json::json!(true),
            |_k, _v| {
                worker_called.store(true, Ordering::SeqCst);
                Err("WebSocket connection not active".to_string())
            },
        );

        assert!(
            res.is_ok(),
            "Persistence succeeded, so overall result should be Ok"
        );
        let response = res.unwrap();
        assert!(
            worker_called.load(Ordering::SeqCst),
            "Worker sync was attempted"
        );
        assert_eq!(
            response.warning,
            Some("WebSocket connection not active".to_string()),
            "Worker failure must be reported as a warning"
        );
        assert_eq!(
            response.settings.get("preallocate_vram"),
            Some(&serde_json::json!(true)),
            "Settings must be persisted despite worker warning"
        );

        // クリーンアップ
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_save_setting_with_worker_sync_success() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_settings_test_success_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::create_dir_all(&temp_dir);

        let worker_called = AtomicBool::new(false);
        let res = save_setting_with_worker_sync(
            &temp_dir,
            "preallocate_vram",
            &serde_json::json!(false),
            |_k, _v| {
                worker_called.store(true, Ordering::SeqCst);
                Ok(())
            },
        );

        assert!(res.is_ok());
        let response = res.unwrap();
        assert!(worker_called.load(Ordering::SeqCst));
        assert_eq!(
            response.warning, None,
            "Warning must be None on clean success"
        );
        assert_eq!(
            response.settings.get("preallocate_vram"),
            Some(&serde_json::json!(false))
        );

        // クリーンアップ
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_save_setting_key_for_secret_does_not_persist_in_plain_settings_json() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_settings_secret_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::create_dir_all(&temp_dir);

        let secret_val = "super-secret-twitch-key-999";
        let res = save_setting_key(
            &temp_dir,
            "twitch_client_secret",
            serde_json::Value::String(secret_val.to_string()),
        )
        .expect("save_setting_key for secret should succeed");

        // 1. settings.json ファイルそのものを生テキストとして読み込み、平文 secret が含まれないことを検証
        let settings_path = temp_dir.join("settings.json");
        if settings_path.exists() {
            let content = fs::read_to_string(&settings_path).unwrap();
            assert!(
                !content.contains(secret_val),
                "settings.json must NOT contain plaintext secret value"
            );
        }

        // 2. 返却された settings オブジェクトに平文がなく、設定済みフラグが含まれることを検証
        let res_str = serde_json::to_string(&res).unwrap();
        assert!(
            !res_str.contains(secret_val),
            "Frontend settings snapshot must NOT contain plaintext secret value"
        );
        assert_eq!(
            res.get("has_twitch_client_secret"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(res.get("has_client_secret"), Some(&serde_json::json!(true)));

        // 3. credential store から復号取得できることを検証
        let loaded_secret = crate::credentials::get_secret(&temp_dir, "twitch_client_secret");
        assert_eq!(loaded_secret.as_deref(), Some(secret_val));

        // 4. 空文字列で更新した場合は安全に削除されることを検証
        let res2 = save_setting_key(
            &temp_dir,
            "twitch_client_secret",
            serde_json::Value::String("   ".to_string()),
        )
        .expect("empty secret should delete");
        assert_eq!(
            res2.get("has_twitch_client_secret"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            crate::credentials::get_secret(&temp_dir, "twitch_client_secret"),
            None
        );

        // クリーンアップ
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_get_whisper_device() {
        let temp_dir =
            std::env::temp_dir().join(format!("ga_test_whisper_dev_{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&temp_dir);

        // 未設定時は "cuda"
        assert_eq!(get_whisper_device(&temp_dir), "cuda");

        // "cpu" 設定時
        let _ = save_setting_key(
            &temp_dir,
            "whisper_device",
            serde_json::Value::String("cpu".to_string()),
        );
        assert_eq!(get_whisper_device(&temp_dir), "cpu");

        // 大文字 "CPU" 設定時
        let _ = save_setting_key(
            &temp_dir,
            "whisper_device",
            serde_json::Value::String("CPU".to_string()),
        );
        assert_eq!(get_whisper_device(&temp_dir), "cpu");

        // "cuda" 設定時
        let _ = save_setting_key(
            &temp_dir,
            "whisper_device",
            serde_json::Value::String("cuda".to_string()),
        );
        assert_eq!(get_whisper_device(&temp_dir), "cuda");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
