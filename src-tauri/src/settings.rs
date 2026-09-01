use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

pub fn save_setting_key(root_dir: &Path, key: &str, value: Value) -> Result<Value, String> {
    let settings_path = root_dir.join("settings.json");
    let mut current_json = load_settings_file(root_dir);

    if let Value::Object(ref mut map) = current_json {
        map.insert(key.to_string(), value);
    } else {
        let mut map = serde_json::Map::new();
        map.insert(key.to_string(), value);
        current_json = Value::Object(map);
    }

    let pretty_str = serde_json::to_string_pretty(&current_json)
        .map_err(|e| e.to_string())?;

    fs::write(&settings_path, pretty_str).map_err(|e| e.to_string())?;
    Ok(current_json)
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
                        let dir_stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
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
                        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
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
                    let val = l[pos + 1..].trim().trim_matches('"').trim_matches('\'').to_string();
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

