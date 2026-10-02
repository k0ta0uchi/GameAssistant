use std::path::PathBuf;
use chrono::{DateTime, Local, Utc};

use crate::ai_client::{AiGenerateOptions, ChatMessage};
use crate::lance_memory::{self, StoredMemory};
use crate::memory_v2::repository::MemoryRepository;

use super::types::*;
use super::SessionManager;

/// Build a collision-safe Markdown path for one session's article.  The stamp
/// keeps articles sortable per session; when two stops land within the same
/// second (regeneration, double stop), a numeric suffix keeps both articles
/// instead of silently overwriting the first one.
pub(crate) fn unique_blog_path(blogs_dir: &std::path::Path, stamp: &str) -> PathBuf {
    let mut candidate = blogs_dir.join(format!("{}.md", stamp));
    let mut counter = 2;
    while candidate.exists() {
        candidate = blogs_dir.join(format!("{}_{}.md", stamp, counter));
        counter += 1;
    }
    candidate
}

/// Convert persisted rows into a bounded blog fallback.  Stop-time callers
/// pass the session's captured UTC boundary; rows with malformed timestamps
/// are excluded rather than risking unrelated historical content in the
/// prompt.  The no-boundary case is retained for the manual command but is
/// still capped by [`BLOG_FALLBACK_MAX_EVENTS`].
pub(crate) fn persisted_blog_fallback_events(
    memories: Vec<StoredMemory>,
    session_started_at: Option<&DateTime<Utc>>,
) -> Vec<SessionEvent> {
    let mut events = Vec::with_capacity(memories.len().min(BLOG_FALLBACK_MAX_EVENTS));
    for memory in memories {
        if let Some(start) = session_started_at {
            let Ok(occurred_at) = DateTime::parse_from_rfc3339(&memory.timestamp) else {
                continue;
            };
            if occurred_at.with_timezone(&Utc) < *start {
                continue;
            }
        }
        events.push(SessionEvent {
            id: memory.id,
            r#type: memory.memory_type,
            author: memory.source,
            content: memory.document,
            timestamp: memory.timestamp,
        });
        if events.len() >= BLOG_FALLBACK_MAX_EVENTS {
            break;
        }
    }
    events
}

pub(crate) fn merge_persisted_blog_events(
    events: &mut Vec<SessionEvent>,
    persisted: impl IntoIterator<Item = SessionEvent>,
) {
    for persisted_event in persisted {
        let canonical = MemoryRepository::canonical_event_id(&persisted_event.id);
        if events.iter().any(|event| {
            event.id == persisted_event.id
                || MemoryRepository::canonical_event_id(&event.id) == canonical
        }) {
            continue;
        }
        events.push(persisted_event);
    }
}

/// 収集済みの選択メモリーからブログ材料テキストを組み立てる。
/// 上限超過は黙って切り捨てず、件数を減らすようエラーで通知する。
pub(crate) fn build_blog_source_text(rows: &[StoredMemory]) -> Result<String, String> {
    let mut logs = String::new();
    for row in rows {
        let line = format!("[{}] {}: {}\n", row.timestamp, row.source, row.document);
        if logs.len().saturating_add(line.len()) > BLOG_MAX_SOURCE_BYTES {
            return Err(format!(
                "選択されたメモリーの合計が入力上限 ({BLOG_MAX_SOURCE_BYTES}バイト) を超えています。選択件数を減らして再実行してください。"
            ));
        }
        logs.push_str(&line);
    }
    Ok(logs)
}

impl SessionManager {
    /// note ブログ記事の自動執筆 (5,000文字規模 & スキル注入)
    pub async fn generate_blog_article(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        blog_system_prompt: &str,
    ) -> Result<String, String> {
        // Clone the boundary before entering the async call.  Holding a
        // parking_lot MutexGuard across `.await` makes the Tauri command
        // future !Send and prevents the library from compiling.
        let session_started_at = *self.session_started_at.lock();
        let session_id = {
            let id = self.session_id.lock().clone();
            (!id.is_empty()).then_some(id)
        };
        self.generate_blog_article_for_session(
            gemini_api_key,
            gemini_model,
            blog_system_prompt,
            session_started_at,
            session_id,
        )
        .await
    }

    /// Generate a blog article using a caller-supplied session boundary for
    /// persisted fallback rows.  Stop-time generation passes the boundary it
    /// captured before spawning its detached task; the public command keeps
    /// the existing API and uses the current manager boundary when available.
    pub(crate) async fn generate_blog_article_for_session(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        blog_system_prompt: &str,
        session_started_at: Option<DateTime<Utc>>,
        session_id: Option<String>,
    ) -> Result<String, String> {
        // The public/manual command can race a detached ASR raw-save task just
        // like Stop Session can.  Apply the same bounded drain at this
        // boundary so a direct blog request cannot silently omit its final
        // utterance; a timeout remains recoverable because raw persistence is
        // authoritative and the snapshot below is still bounded.
        let outstanding = self.wait_for_event_tasks_bounded().await;
        if outstanding > 0 {
            self.log_mgr.warn(
                "Blog",
                &format!(
                    "手動ブログ生成の保存待機がタイムアウトしました (未完了タスク {}件)。保存済みイベントで続行します。",
                    outstanding
                ),
            );
        }
        let mut events = session_id
            .as_ref()
            .and_then(|id| self.session_archives.lock().get(id).cloned())
            .unwrap_or_else(|| self.get_events());
        if session_id.is_some() || events.is_empty() {
            // A portable app can be restarted between session stop and blog
            // generation. Recover authoritative raw history whenever a
            // stopped-session boundary is known, even when the archive is
            // non-empty: a delayed final callback may have reached the
            // journal after the in-memory snapshot was taken, and the legacy
            // compatibility projection may still be lagging behind it.
            if let Ok(memories) = lance_memory::list_stored_memories(&self.root_dir).await {
                merge_persisted_blog_events(
                    &mut events,
                    persisted_blog_fallback_events(memories, session_started_at.as_ref()),
                );
            }
        }
        if let Some(id) = session_id {
            self.session_archives.lock().remove(&id);
        }
        if events.is_empty() {
            return Err("会話履歴がありません。".to_string());
        }

        self.log_mgr.info(
            "Blog",
            "Generating note blog article from session history...",
        );

        let mut logs = String::new();
        for ev in &events {
            let line = format!("[{}] {}: {}\n", ev.timestamp, ev.author, ev.content);
            if logs.len().saturating_add(line.len()) > BLOG_MAX_SOURCE_BYTES {
                break;
            }
            logs.push_str(&line);
        }

        // 記事生成は収集済みの材料から共通経路で行う (選択メモリー経路と共有)。
        self.generate_blog_article_from_history(
            gemini_api_key,
            gemini_model,
            blog_system_prompt,
            logs,
        )
        .await
    }

    /// 収集済みのブログ材料 (logs) から記事本文を生成する共有経路。
    /// 入力収集 (セッション履歴/選択メモリー) と保存・通知は呼び出し側の責務。
    /// blog_system_prompt が空の場合は既定の blog_writer_system_prompt を使う。
    pub(crate) async fn generate_blog_article_from_history(
        &self,
        gemini_api_key: &str,
        gemini_model: &str,
        blog_system_prompt: &str,
        logs: String,
    ) -> Result<String, String> {
        let st = crate::settings::load_settings_file(&self.root_dir);

        // 1. スキル適用の判定と読み込み (enable_blog_skills & enabled_blog_skills)
        let enable_skills = st
            .get("enable_blog_skills")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut skill_instructions = String::new();

        if enable_skills {
            let enabled_skills: Vec<String> = st
                .get("enabled_blog_skills")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| vec!["k0ta-writing-style".to_string()]);

            if !enabled_skills.is_empty() {
                let skills_text =
                    crate::settings::load_enabled_skills_text(&self.root_dir, &enabled_skills);
                if !skills_text.is_empty() {
                    self.log_mgr.info(
                        "Blog",
                        &format!("ブログ記事生成にスキルを適用します: {:?}", enabled_skills),
                    );
                    skill_instructions = format!(
                        "\n\n# 適用スキル・執筆ガイドライン\n以下のスキルの指示・文体・トーン＆マナー・構成パターンを最優先で適用して記事を作成してください。\n\n{}",
                        skills_text
                    );
                }
            }
        }

        let base_blog_prompt = if blog_system_prompt.trim().is_empty() {
            crate::prompts::get_prompt(&self.root_dir, "blog_writer_system_prompt")
        } else {
            blog_system_prompt.to_string()
        };

        let full_blog_prompt = format!("{}{}", base_blog_prompt, skill_instructions);

        let prompt_text = format!(
            "# 会話履歴・配信ログ\n{}\n\n上記の会話履歴を元に、指示に従ってnote用の魅力的なプレイ日誌ブログ記事を作成してください。",
            logs
        );

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt_text,
        }];

        // 2. ブログ Thinking モードの制御 (blog_use_thinking: true/false)
        let blog_use_thinking = st
            .get("blog_use_thinking")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let thinking_budget = if blog_use_thinking {
            Some(2048)
        } else {
            Some(0)
        };

        self.log_mgr.info(
            "Blog",
            &format!(
                "ブログ記事生成パラメータ (model: {}, thinking: {})",
                gemini_model, blog_use_thinking
            ),
        );

        let options = AiGenerateOptions {
            system_instruction: Some(full_blog_prompt),
            temperature: Some(0.7),
            max_output_tokens: Some(4000),
            image_base64: None,
            thinking_budget,
        };

        let blog_article = self
            .ai_client
            .generate_gemini(gemini_api_key, gemini_model, &messages, &options)
            .await?;

        self.log_mgr.info(
            "Blog",
            &format!(
                "✅ note ブログ記事の生成に成功しました (文字数: {})",
                blog_article.chars().count()
            ),
        );
        Ok(blog_article)
    }

    /// 選択メモリーからブログ記事を生成・保存する (trigger=selected_memories_blog)。
    /// 会話応答のセッション境界条件は適用しない: 非稼働中でも、自動実況と
    /// 終了時ブログの両設定がOFFでも、明示操作なら生成できる。
    pub async fn generate_blog_from_memories(
        &self,
        ids: &[String],
    ) -> Result<(String, String), String> {
        let sources = self.collect_selected_blog_sources(ids).await?;
        let logs = build_blog_source_text(&sources)?;

        let gemini_key = self.get_effective_gemini_key();
        if gemini_key.trim().is_empty() {
            return Err(
                "Gemini API キーが未設定のため選択メモリーのブログを生成できません。設定画面でAPIキーを保存してください。"
                    .to_string(),
            );
        }
        let st = crate::settings::load_settings_file(&self.root_dir);
        let model = st
            .get("gemini_model")
            .and_then(|value| value.as_str())
            .unwrap_or("latest")
            .to_string();

        self.log_mgr.info(
            "Blog",
            &format!(
                "trigger=selected_memories_blog status=generation_started requested={} resolved={}",
                ids.len(),
                sources.len()
            ),
        );
        let article = self
            .generate_blog_article_from_history(&gemini_key, &model, "", logs)
            .await
            .map_err(|err| format!("generation_failed: {err}"))?;

        // 保存は既存方針 (blogs/ + unique_blog_path) に従い、終了時生成と
        // 同時に走っても上書きしない。成功応答は保存成功後に返す。
        let blogs_dir = self.root_dir.join("blogs");
        let _ = std::fs::create_dir_all(&blogs_dir);
        let stamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let filepath = unique_blog_path(&blogs_dir, &stamp);
        let filename = filepath
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{stamp}.md"));
        std::fs::write(&filepath, &article).map_err(|err| {
            // 生成成功・保存失敗: 保存済みと誤認させないため理由を区別する。
            format!("save_failed: {err}")
        })?;
        self.log_mgr.info(
            "Blog",
            &format!(
                "trigger=selected_memories_blog status=completed file={filename} chars={}",
                article.chars().count()
            ),
        );
        Ok((filename, article))
    }

    /// 選択IDを既存のID解決経路 (canonical・旧ID・投影遅延フォールバック) で
    /// 解決し、重複を排除して時系列に整列する。欠落があれば生成前に中止する。
    pub(crate) async fn collect_selected_blog_sources(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredMemory>, String> {
        if ids.is_empty() {
            return Err("ブログ生成対象のメモリーが選択されていません。".to_string());
        }
        let mut resolved: Vec<StoredMemory> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut missing: Vec<String> = Vec::new();
        for id in ids {
            let id_trim = id.trim();
            if id_trim.is_empty() {
                return Err("空のメモリーIDが含まれています。".to_string());
            }
            let Some(row) = lance_memory::get_memory_by_event_id(&self.root_dir, id_trim).await?
            else {
                missing.push(id_trim.to_string());
                continue;
            };
            // canonical ID と旧 ID の両方で同じデータを指した場合は 1 件にまとめる
            // (解決結果の ID 表記ゆれは canonical 正規化して比較する)。
            let canonical =
                crate::memory_v2::repository::MemoryRepository::canonical_event_id(&row.id);
            if seen.insert(canonical) {
                resolved.push(row);
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "選択されたメモリーのうち {}件が取得できませんでした ({})。選択を確認して再実行してください。",
                missing.len(),
                missing.join(", ")
            ));
        }
        // 時系列で安定ソート (同時刻は ID 順)。
        resolved.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
        Ok(resolved)
    }
}
