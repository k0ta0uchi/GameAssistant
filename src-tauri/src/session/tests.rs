use std::sync::Arc;

use super::ai_pipeline::*;
use super::blog::*;
use super::commentary::*;
use super::input_pipeline::*;
use super::lifecycle::*;
use super::persistence::*;
use super::types::*;
use super::SessionManager;

use crate::lance_memory::{self, MemoryItem, StoredMemory, SummaryBackfillApplyResult, SummaryExclusionDetail};
use crate::logger::LogManager;
use crate::memory_v2::repository::{MemoryRepository, SummaryStatusRecord};
use crate::tts::TtsManager;

    fn unique_session_test_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ga-session-{}-{}-{}",
            name,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create session test root");
        dir
    }

    fn test_session_manager(name: &str) -> SessionManager {
        let root = unique_session_test_root(name);
        SessionManager::new(
            root,
            Arc::new(TtsManager::new()),
            Arc::new(LogManager::new(unique_session_test_root(&format!(
                "{}-logs",
                name
            )))),
        )
    }

    #[tokio::test]
    async fn storage_session_callback_arms_partial_once_and_promotes_final_once() {
        let session = test_session_manager("callback-wake-red");
        session.start_session();
        session
            .asr_engine
            .configure_wake_word(crate::asr::WakeWordConfig::new(
                vec!["ねえぐり".to_string()],
            ));
        session.asr_engine.begin_wake_word_session(1);
        let context = session
            .current_session_context()
            .expect("test session must be active");

        let first_partial =
            session.handle_partial_asr_result(&context, "mic", "ねえぐり", None, None);
        assert!(first_partial.should_acknowledge);
        assert!(
            session
                .is_collecting_prompt
                .load(std::sync::atomic::Ordering::SeqCst),
            "the session callback must mirror partial wake arming into prompt collection"
        );

        let repeated_partial =
            session.handle_partial_asr_result(&context, "mic", "ねえぐり", None, None);
        assert!(!repeated_partial.should_acknowledge);

        let final_prompt = session
            .persist_final_asr_result(&context, "mic", "ねえぐり 今日の配信をまとめて", None, None)
            .await
            .expect("final callback must persist one raw event");
        assert_eq!(
            final_prompt.decision.action,
            crate::asr::WakeWordAction::PromptDetected
        );
        assert!(final_prompt.decision.is_prompt);
        assert!(
            !session
                .is_collecting_prompt
                .load(std::sync::atomic::Ordering::SeqCst),
            "final prompt handling must clear the session collection state"
        );

        let duplicate_final =
            session
                .asr_engine
                .handle_wake_word("mic", "ねえぐり 今日の配信をまとめて", true, 900);
        assert_eq!(
            duplicate_final.action,
            crate::asr::WakeWordAction::DuplicateSuppressed
        );
    }

    #[test]
    fn storage_stale_asr_callback_gate_accepts_final_but_rejects_partial() {
        let session = test_session_manager("stale-asr-callback-gate");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.stop_session();

        assert!(session.admit_asr_callback(&context, true));
        assert!(!session.admit_asr_callback(&context, false));

        let logs = session.log_mgr.get_logs();
        assert!(logs
            .iter()
            .any(|entry| entry.message.contains("stale_asr_final_accepted_for_raw")));
        assert!(logs
            .iter()
            .any(|entry| entry.message.contains("stale_asr_callback_dropped")));
        assert!(logs.iter().any(|entry| {
            entry
                .message
                .contains("status=nod_not_requested phase=partial reason=stale_callback")
        }));
    }

    #[tokio::test]
    async fn storage_stale_final_is_persisted_raw_but_not_queued_for_live_summary() {
        let session = test_session_manager("stale-final-summary-boundary");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.stop_session();

        let persisted = session
            .persist_final_asr_result(&context, "mic", "ねえぐり 停止後の遅延確定発話", None, None)
            .await
            .expect("stale final must still reach the raw persistence boundary");

        let repository = MemoryRepository::open(&session.root_dir)
            .await
            .expect("open memory repository for stale final verification");
        assert!(repository
            .read_raw_events()
            .await
            .unwrap()
            .iter()
            .any(|event| event.event_id().to_string()
                == MemoryRepository::canonical_event_id(&persisted.event.id)));
        let logs = session.log_mgr.get_logs();
        assert!(logs
            .iter()
            .any(|entry| entry.message.contains("stale_summary_dropped")));
        assert!(!logs
            .iter()
            .any(|entry| entry.message.contains("summary_queued")));
    }

    #[tokio::test]
    async fn storage_empty_final_asr_is_classified_without_logging_transcript() {
        let session = test_session_manager("empty-final-admission-diagnostic");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");

        assert!(session
            .persist_final_asr_result(&context, "mic", "   ", None, None)
            .await
            .is_none());

        let logs = session.log_mgr.get_logs();
        let diagnostic = logs
            .iter()
            .find(|entry| entry.message.contains("raw_admission_dropped"))
            .expect("an empty final must produce a classified diagnostic");
        assert!(diagnostic
            .message
            .contains(&format!("session_id={}", context.session_id)));
        assert!(diagnostic
            .message
            .contains(&format!("generation={}", context.generation)));
        assert!(diagnostic.message.contains("reason=empty_transcript"));
        assert!(!diagnostic.message.contains("   "));
    }

    #[tokio::test]
    async fn storage_unknown_asr_stream_is_classified_without_exposing_stream_payload() {
        let session = test_session_manager("unknown-stream-admission-diagnostic");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");

        assert!(session
            .persist_final_asr_result(
                &context,
                "future-private-stream",
                "recognized text",
                None,
                None
            )
            .await
            .is_none());

        let logs = session.log_mgr.get_logs();
        let diagnostic = logs
            .iter()
            .find(|entry| entry.message.contains("unknown_stream_dropped"))
            .expect("an unknown final stream must produce a classified diagnostic");
        assert!(diagnostic
            .message
            .contains(&format!("session_id={}", context.session_id)));
        assert!(diagnostic
            .message
            .contains(&format!("generation={}", context.generation)));
        assert!(diagnostic.message.contains("reason=unknown_stream"));
        assert!(!diagnostic.message.contains("future-private-stream"));
        assert!(!diagnostic.message.contains("recognized text"));
    }

    #[tokio::test]
    async fn storage_stop_word_final_still_persists_raw_event() {
        let session = test_session_manager("stop-word-raw-red");
        session.start_session();
        let event = super::SessionEvent {
            id: "stop-word-raw-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "ストップ、直前の発話も保存して".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        // This is the finalized callback's durable boundary: stop-word
        // handling must not replace or discard the finalized raw payload.
        session.add_event(event.clone());
        session.save_event_to_memory(&event).await;

        let persisted = crate::lance_memory::get_memory_by_event_id(&session.root_dir, &event.id)
            .await
            .expect("raw event lookup must succeed")
            .expect("stop-word final must remain durable");
        assert_eq!(persisted.document, event.content);
        assert_eq!(persisted.id, event.id);
    }

    #[tokio::test]
    async fn storage_stop_resets_wake_state_before_blog_drain_boundary() {
        let session = test_session_manager("stop-blog-boundary-red");
        session.start_session();
        session.asr_engine.begin_wake_word_session(1);
        let partial = session
            .asr_engine
            .handle_wake_word("mic", "ねえぐり", false, 0);
        assert!(partial.should_acknowledge);

        let guard = session.begin_event_task();
        session.stop_session();
        assert!(!session.is_active());
        let next_session_partial =
            session
                .asr_engine
                .handle_wake_word("mic", "ねえぐり", false, 100);
        assert_eq!(
            next_session_partial.action,
            crate::asr::WakeWordAction::PartialWakeDetected,
        );
        assert!(
            next_session_partial.should_acknowledge,
            "Stop Session must release the old cooldown before its bounded drain"
        );
        drop(guard);
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );
    }

    #[test]
    fn storage_stopped_session_archive_is_reclaimed_when_blog_is_disabled() {
        let session = test_session_manager("archive-reclaim");
        std::fs::write(
            session.root_dir.join("settings.json"),
            r#"{"create_blog_post":false}"#,
        )
        .expect("write settings.json for archive reclaim test");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.add_event(super::SessionEvent {
            id: "archive-reclaim-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "archive remains durable".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });

        session.stop_session_with_services(None, None);

        assert!(
            !session
                .session_archives
                .lock()
                .contains_key(&context.session_id),
            "disabled blog generation must not retain an unbounded session archive"
        );
    }

    #[test]
    fn effective_wake_words_prefers_custom_list_and_dedupes() {
        let settings =
            serde_json::json!({ "custom_wake_words": "こっちむいて、 こっちむいて ,テスト" });
        assert_eq!(
            effective_wake_words(&settings),
            vec!["こっちむいて".to_string(), "テスト".to_string()]
        );
    }

    #[test]
    fn effective_wake_words_missing_key_uses_default_vocabulary() {
        let words = effective_wake_words(&serde_json::json!({}));
        assert_eq!(words.len(), crate::asr::DEFAULT_WAKE_WORDS.len());
        assert!(words.iter().any(|word| word == "ねえぐり"));
    }

    #[test]
    fn effective_wake_words_explicit_empty_admits_nothing() {
        for empty in [
            serde_json::json!({ "custom_wake_words": "" }),
            serde_json::json!({ "custom_wake_words": [] }),
            serde_json::json!({ "custom_wake_words": "、 ," }),
        ] {
            assert!(
                effective_wake_words(&empty).is_empty(),
                "explicit empty wake words must admit nothing: {empty}"
            );
        }
    }

    #[test]
    fn twitch_admission_matches_body_and_strips_wake_word() {
        let words = vec!["こっちむいて".to_string()];
        // 本文の途中に語が含まれても一致し、呼びかけを除いた本文を返す。
        assert_eq!(
            admit_twitch_comment(&words, "こっちむいて今日の天気は？"),
            Some("今日の天気は？".to_string())
        );
        // 一致なしは抑止 (C10)。
        assert_eq!(admit_twitch_comment(&words, "普通のコメント"), None);
        // ウェイクワードのみは空プロンプト扱いで起動しない (C14)。
        assert_eq!(admit_twitch_comment(&words, "こっちむいて"), None);
        // カスタム設定があれば既定語でも反応しない (C13)。
        assert_eq!(admit_twitch_comment(&words, "ねえぐりゲーム見せて"), None);
    }

    #[test]
    fn auto_commentary_is_off_unless_explicitly_enabled() {
        assert!(!auto_commentary_enabled(&serde_json::json!({})));
        assert!(!auto_commentary_enabled(
            &serde_json::json!({ "enable_auto_commentary": false })
        ));
        // 型が不正な場合は既定 (OFF) と同じ解釈にする。
        assert!(!auto_commentary_enabled(
            &serde_json::json!({ "enable_auto_commentary": "yes" })
        ));
        assert!(auto_commentary_enabled(
            &serde_json::json!({ "enable_auto_commentary": true })
        ));
    }

    #[tokio::test]
    async fn storage_selected_blog_sources_resolve_dedupe_and_sort_chronologically() {
        let session = test_session_manager("selected-blog");
        let row = |id: &str, timestamp: &str, doc: &str| MemoryItem {
            id: id.to_string(),
            document: doc.to_string(),
            memory_type: "user_speech".to_string(),
            source: "User".to_string(),
            timestamp: timestamp.to_string(),
            user_id: Some("User".to_string()),
        };
        lance_memory::insert_memory_batch_nullable_authoritative(
            &session.root_dir,
            vec![
                row("raw-newer", "2026-01-02T10:00:00Z", "newer"),
                row("raw-older", "2026-01-01T10:00:00Z", "older"),
            ],
            Some(vec![None, None]),
        )
        .await
        .expect("insert selected blog sources");

        let canonical = MemoryRepository::canonical_event_id("raw-newer");
        let aborted = session
            .collect_selected_blog_sources(&["raw-newer".to_string(), "missing-id".to_string()])
            .await
            .expect_err("missing selection must abort before generation");
        assert!(aborted.contains("1件が取得できません"), "{aborted}");
        assert!(aborted.contains("missing-id"), "{aborted}");

        let sources = session
            .collect_selected_blog_sources(&[
                "raw-newer".to_string(),
                canonical,
                "raw-older".to_string(),
            ])
            .await
            .expect("canonical/legacy duplicates must resolve to one source");
        let docs: Vec<&str> = sources.iter().map(|row| row.document.as_str()).collect();
        assert_eq!(docs, vec!["older", "newer"]);
    }

    #[test]
    fn selected_blog_source_text_rejects_oversize_selection() {
        let row = |id: &str, doc: String| StoredMemory {
            id: id.to_string(),
            document: doc,
            memory_type: "user_speech".into(),
            source: "User".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: None,
        };
        let rows = vec![
            // 1件目はプレフィックス (タイムスタンプ/発話者) を含めて上限内、
            // 2件目を足すと上限超過するサイズ。
            row("big", "x".repeat(BLOG_MAX_SOURCE_BYTES - 64)),
            row("small", "small".to_string()),
        ];
        let err = build_blog_source_text(&rows)
            .expect_err("oversize selection must be reported, not truncated");
        assert!(err.contains("入力上限"), "{err}");
        let ok = build_blog_source_text(&rows[..1]).expect("single row within limit");
        assert!(ok.contains("xxx"));
    }

    #[test]
    fn storage_stale_summary_can_emit_after_stop_but_not_after_replacement_start() {
        let session = test_session_manager("fact-generation-boundary");
        session.start_session();
        let first = session
            .current_session_context()
            .expect("first session must be active");
        assert!(session.allows_fact_ui_emit(&first));

        session.stop_session();
        assert!(
            session.allows_fact_ui_emit(&first),
            "a completed summary may still update the dashboard before replacement"
        );

        session.start_session();
        let second = session
            .current_session_context()
            .expect("replacement session must be active");
        assert!(!session.allows_fact_ui_emit(&first));
        assert!(session.allows_fact_ui_emit(&second));
    }

    #[test]
    fn stop_word_detection_covers_each_configured_fragment() {
        assert!(contains_stop_word("ストップってば！"));
        assert!(contains_stop_word("ちょっとだまってて"));
        assert!(contains_stop_word("静かにして"));
        assert!(!contains_stop_word("今日はゲームをしよう"));
        assert!(!contains_stop_word(""));
    }

    #[test]
    fn stop_word_detection_does_not_match_unrelated_word_fragments() {
        assert!(!contains_stop_word("ストップウォッチの使い方を教えて"));
    }

    #[test]
    fn prompt_boundary_normalizes_full_width_whitespace_and_preserves_asr_minimum() {
        assert_eq!(normalize_prompt_text("　A　"), "A");
        assert_eq!(normalize_prompt_text("　　"), "");
        assert!(prompt_is_sendable("A"));
        assert!(!asr_prompt_is_sendable("A"));
        assert!(asr_prompt_is_sendable("AB"));
        assert!(!prompt_is_sendable(""));
    }

    #[test]
    fn wake_word_required_drop_is_logged_without_a_user_toast() {
        assert!(!should_emit_input_drop_toast(
            super::InputDropReason::WakeWordRequired
        ));
        assert!(should_emit_input_drop_toast(
            super::InputDropReason::WakeOnly
        ));
        assert!(should_emit_input_drop_toast(
            super::InputDropReason::EmptyResponse
        ));
    }

    #[test]
    fn chat_history_is_bounded_and_retains_latest_event() {
        let events = (0..100)
            .map(|index| super::SessionEvent {
                id: format!("event-{index}"),
                r#type: "user_speech".to_string(),
                author: "User".to_string(),
                content: "x".repeat(1000),
                timestamp: chrono::Utc::now().to_rfc3339(),
            })
            .collect::<Vec<_>>();

        let messages = build_chat_messages(&events);
        assert!(messages.len() <= MAX_CHAT_HISTORY_MESSAGES);
        assert!(
            messages
                .iter()
                .map(|message| message.content.chars().count())
                .sum::<usize>()
                <= MAX_CHAT_HISTORY_CHARS
        );
        assert!(messages
            .last()
            .is_some_and(|message| message.content.contains(&"x".repeat(1000))));
    }

    #[tokio::test]
    async fn storage_empty_manual_input_is_not_reported_as_success() {
        let session = test_session_manager("empty-manual-input-outcome-red");

        let result = session
            .process_user_input(
                "User",
                "　\n\t",
                "manual_input",
                "",
                "",
                "gemini-2.0-flash",
                "",
                &crate::tts::TtsSettings::default(),
                None,
            )
            .await;

        assert!(result.is_err(), "empty input must be observable as a drop");
    }

    #[tokio::test]
    async fn storage_stale_session_input_is_not_reported_as_success() {
        let session = test_session_manager("stale-input-outcome-red");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.stop_session();
        let event = super::SessionEvent {
            id: "stale-input-outcome-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "質問".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let result = session
            .process_user_input_for_session(
                &context,
                &event,
                "質問",
                "",
                "",
                "gemini-2.0-flash",
                "",
                &crate::tts::TtsSettings::default(),
                None,
            )
            .await;

        assert!(matches!(
            result,
            Ok(super::InputProcessingOutcome::Dropped {
                reason: super::InputDropReason::StaleAtEntry
            })
        ));
    }

    #[tokio::test]
    async fn storage_missing_gemini_key_is_classified_before_generation_started() {
        let session = test_session_manager("missing-gemini-key-preflight");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        let event = super::SessionEvent {
            id: "missing-key-preflight-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "質問".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let result = session
            .process_user_input_for_session(
                &context,
                &event,
                "質問",
                " \t\n",
                "",
                "gemini-2.0-flash",
                "",
                &crate::tts::TtsSettings::default(),
                None,
            )
            .await;

        assert_eq!(
            result.unwrap_err(),
            "Gemini API key is not set",
            "missing credentials must fail before any transport attempt"
        );
        let logs = session.log_mgr.get_logs();
        assert!(logs.iter().any(|entry| {
            entry
                .message
                .contains("status=generation_blocked stage=preflight reason=api_key_missing")
        }));
        assert!(!logs
            .iter()
            .any(|entry| entry.message.contains("status=generation_started")));
    }

    #[tokio::test]
    async fn storage_prompt_received_schedules_nod_without_waiting_for_audio_completion() {
        let session = test_session_manager("prompt-received-nod-background");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");

        let started = std::time::Instant::now();
        session.schedule_nod_ack(
            &context,
            "prompt-received-nod-background-event",
            "final",
            crate::asr::WakeWordAction::PromptReceived,
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "nod scheduling must not wait for audio completion"
        );

        let mut logs = session.log_mgr.get_logs();
        for _ in 0..20 {
            if logs.iter().any(|entry| {
                entry.message.contains("status=nod_failed")
                    && entry.message.contains("reason=nod_asset_missing")
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            logs = session.log_mgr.get_logs();
        }
        assert!(logs.iter().any(|entry| {
            entry.message.contains("status=nod_scheduled")
                && entry.message.contains("phase=final")
                && entry.message.contains("action=PromptReceived")
        }));
        assert!(logs.iter().any(|entry| {
            entry.message.contains("status=nod_failed")
                && entry.message.contains("reason=nod_asset_missing")
        }));
    }

    #[tokio::test]
    async fn storage_stale_nod_schedule_is_rejected_after_generation_boundary() {
        let session = test_session_manager("stale-nod-generation-boundary");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        session.stop_session();

        session.schedule_nod_ack(
            &context,
            "stale-nod-event",
            "final",
            crate::asr::WakeWordAction::PromptReceived,
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let logs = session.log_mgr.get_logs();
        assert!(logs.iter().any(|entry| {
            entry
                .message
                .contains("status=nod_not_requested phase=final")
                && entry.message.contains("reason=stale_before_schedule")
        }));
        assert!(!logs.iter().any(|entry| {
            entry.message.contains("event_id=stale-nod-event")
                && entry.message.contains("status=nod_started")
        }));
    }

    #[tokio::test]
    async fn storage_non_prompt_final_is_classified_without_calling_gemini() {
        let session = test_session_manager("non-prompt-final-outcome");
        session.start_session();
        let context = session
            .current_session_context()
            .expect("test session must be active");
        let event = super::SessionEvent {
            id: "non-prompt-final-event".to_string(),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: "通常の発話".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let result = session
            .process_asr_followup(
                &context,
                super::PersistedAsrEvent {
                    event,
                    decision: crate::asr::WakeWordDecision {
                        stream: "mic".to_string(),
                        engine: crate::asr::WAKE_WORD_ENGINE,
                        session_generation: context.generation,
                        is_final: true,
                        phase: crate::asr::WakeWordPhase::Idle,
                        wake_word_checked: true,
                        wake_word_detected: false,
                        clean_prompt: "通常の発話".to_string(),
                        action: crate::asr::WakeWordAction::FinalSpeech,
                        should_acknowledge: false,
                        is_prompt: false,
                        cooldown_active: false,
                        duplicate_suppressed: false,
                    },
                    stop_word_detected: false,
                },
                "通常の発話",
                None,
            )
            .await;

        assert!(matches!(
            result,
            Ok(super::InputProcessingOutcome::Dropped {
                reason: super::InputDropReason::WakeWordRequired
            })
        ));
        assert!(!session
            .log_mgr
            .get_logs()
            .iter()
            .any(|entry| entry.message.contains("status=generation_started")));
    }

    #[test]
    fn storage_blog_article_paths_never_collide_within_one_second() {
        let dir = unique_session_test_root("blog-names");
        let first = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(first, dir.join("2026-09-07_12-00-00.md"));

        std::fs::write(&first, "first").expect("write first blog article fixture");
        let second = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(second, dir.join("2026-09-07_12-00-00_2.md"));

        std::fs::write(&second, "second").expect("write second blog article fixture");
        let third = unique_blog_path(&dir, "2026-09-07_12-00-00");
        assert_eq!(third, dir.join("2026-09-07_12-00-00_3.md"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn storage_stop_drain_reports_outstanding_raw_save_tasks_after_bounded_wait() {
        let session = test_session_manager("stop-drain");

        // No in-flight raw-save tasks: the drain completes immediately.
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );

        // A held guard is reported as outstanding once the bound elapses
        // instead of blocking blog generation forever.
        let guard = session.begin_event_task();
        let outstanding = session
            .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
            .await;
        assert_eq!(
            outstanding, 1,
            "a held raw-save guard must be reported, never silently ignored"
        );

        // Guards are RAII: releasing it lets the next drain finish cleanly.
        drop(guard);
        assert_eq!(
            session
                .wait_for_event_tasks_with_timeout(std::time::Duration::from_millis(50))
                .await,
            0
        );
    }

    #[tokio::test]
    async fn storage_stop_drain_wakes_as_soon_as_the_last_guard_drops() {
        let session = test_session_manager("stop-drain-wake");
        let guard = session.begin_event_task();
        let waiter = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .wait_for_event_tasks_with_timeout(std::time::Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        drop(guard);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("drain must wake on guard release")
                .unwrap(),
            0
        );
    }

    #[test]
    fn memory_admission_redacts_secrets_before_persistence() {
        let admitted = admit_redacted_memory_text("  token=super-secret-value  ");

        assert_eq!(admitted.as_deref(), Some("token=[REDACTED:credentials]"));
        assert!(!admitted
            .as_deref()
            .unwrap_or_default()
            .contains("super-secret-value"));
    }

    #[test]
    fn memory_admission_is_idempotent_and_rejects_empty_text() {
        let first = admit_redacted_memory_text("email alice@example.com")
            .expect("admission of non-empty text must succeed");
        let second = admit_redacted_memory_text(&first).expect("re-admission must succeed");

        assert_eq!(first, second);
        assert_eq!(admit_redacted_memory_text(" \n\t "), None);
    }

    #[test]
    fn blog_generation_defaults_on_for_portable_installs() {
        assert!(automatic_blog_post_enabled(&serde_json::json!({})));
        assert!(automatic_blog_post_enabled(
            &serde_json::json!({"create_blog_post": true})
        ));
        assert!(!automatic_blog_post_enabled(
            &serde_json::json!({"create_blog_post": false})
        ));
    }

    #[test]
    fn blog_fallback_is_session_scoped_and_bounded() {
        let started_at = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
            .expect("parse test session start timestamp")
            .with_timezone(&chrono::Utc);
        let row = |id: &str, timestamp: &str| StoredMemory {
            id: id.into(),
            document: format!("raw-{id}"),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: timestamp.into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: None,
        };

        let rows = vec![
            row("old", "2026-09-07T11:59:59Z"),
            row("new", "2026-09-07T12:00:01+00:00"),
            row("malformed", "not-a-timestamp"),
        ];
        let events = persisted_blog_fallback_events(rows, Some(&started_at));
        assert_eq!(
            events
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["new"]
        );

        let many = (0..250)
            .map(|index| row(&format!("event-{index}"), "2026-09-07T12:00:01Z"))
            .collect();
        assert_eq!(
            persisted_blog_fallback_events(many, Some(&started_at)).len(),
            200
        );
    }

    #[test]
    fn live_summary_processing_accepts_only_asr_stream_events() {
        assert!(live_asr_summary_event("user_speech"));
        assert!(live_asr_summary_event("discord_speech"));
        assert!(!live_asr_summary_event("twitch_chat"));
        assert!(!live_asr_summary_event("ai_response"));
        assert!(!live_asr_summary_event("auto_commentary"));
    }

    #[test]
    fn completed_projection_without_durable_summary_fact_is_reprocessed() {
        let row = StoredMemory {
            id: "legacy-row".into(),
            document: "raw event".into(),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: Some("legacy summary".into()),
            summary_status: Some(lance_memory::SUMMARY_STATUS_COMPLETED.into()),
            summary_model: Some("old-model".into()),
            summary_prompt_version: Some("old".into()),
            vector_source: Some(lance_memory::VECTOR_SOURCE_SUMMARY.into()),
        };
        assert!(should_backfill_row(&row, &std::collections::HashSet::new()));
    }

    #[test]
    fn backfill_partition_aggregates_already_processed_rows() {
        let row = |id: &str| StoredMemory {
            id: id.into(),
            document: "raw event".into(),
            memory_type: "user_speech".into(),
            source: "microphone".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: Some(lance_memory::SUMMARY_STATUS_COMPLETED.into()),
            summary_model: Some("model".into()),
            summary_prompt_version: Some(lance_memory::SUMMARY_PROMPT_VERSION.into()),
            vector_source: Some(lance_memory::VECTOR_SOURCE_SUMMARY.into()),
        };
        let rows = vec![row("done-1"), row("done-2"), row("new-1")];
        let durable = rows[..2]
            .iter()
            .map(|item| MemoryRepository::canonical_event_id(&item.id))
            .collect();
        let durable_statuses = rows[..2]
            .iter()
            .map(|item| {
                (
                    MemoryRepository::canonical_event_id(&item.id),
                    SummaryStatusRecord {
                        entity_id: item.id.clone(),
                        status: "completed".into(),
                        prompt_version: Some(lance_memory::SUMMARY_PROMPT_VERSION.into()),
                        ..Default::default()
                    },
                )
            })
            .collect();

        let partition = partition_backfill_rows(
            &rows,
            &durable,
            &std::collections::HashSet::new(),
            &durable_statuses,
        );

        assert_eq!(
            partition
                .candidates
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new-1"]
        );
        assert_eq!(partition.skipped_reasons.len(), 1);
        assert_eq!(partition.skipped_reasons.values().next().copied(), Some(2));
    }

    #[test]
    fn backfill_partition_admits_only_shared_summary_candidates() {
        let row = |id: &str, memory_type: &str, document: &str| StoredMemory {
            id: id.into(),
            document: document.into(),
            memory_type: memory_type.into(),
            source: "source".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            user_id: None,
            summary: None,
            summary_status: None,
            summary_model: None,
            summary_prompt_version: None,
            vector_source: Some(lance_memory::VECTOR_SOURCE_DOCUMENT.into()),
        };
        let rows = vec![
            row("human", "user_speech", "永久に保持する設定"),
            row("ai", "ai_response", "assistant output"),
            row("manual", "manual", "manual note"),
            row("empty", "user_speech", "  \n\t"),
            row("unknown", "unknown", "unknown event"),
        ];

        let partition = partition_backfill_rows(
            &rows,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
        );

        assert_eq!(
            partition
                .candidates
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["human"]
        );
        assert!(
            partition.skipped_reasons.is_empty(),
            "non-candidates are progress-only"
        );
        assert_eq!(partition.non_candidates, 4);
    }

    #[test]
    fn row_warnings_do_not_set_fatal_state_but_later_fatal_is_preserved() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 1,
            total: 3,
            queued: 2,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 1,
            attempted: 1,
            retry_count: 0,
            remaining: 2,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };

        increment_reason(&mut progress, "invalid_model_output");
        assert_eq!(progress.state, "running");
        assert_eq!(progress.error, None);
        assert_eq!(progress.fatal_error, None);

        set_backfill_fatal(&mut progress, "journal_commit_failed");
        assert_eq!(progress.state, "error");
        assert_eq!(
            progress.fatal_error.as_deref(),
            Some("journal_commit_failed")
        );
        assert_eq!(progress.error.as_deref(), Some("journal_commit_failed"));
        assert_eq!(progress.remaining, 2);
        assert_eq!(progress.reason_counts["invalid_model_output"], 1);
        assert!(!progress.reason_counts.contains_key("journal_commit_failed"));
    }

    #[test]
    fn durable_chunk_commit_preserves_terminal_counter_invariant() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 1,
            total: 4,
            queued: 3,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 1,
            attempted: 3,
            retry_count: 0,
            remaining: 3,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        let result = SummaryBackfillApplyResult {
            persisted: 1,
            policy_excluded: 1,
            exclusions: vec![SummaryExclusionDetail {
                entity_id: "excluded".into(),
                reason: "policy_excluded".into(),
            }],
            terminal_failed: 1,
            terminal_skipped: 0,
            projection_error: Some("projection unavailable".into()),
        };

        record_backfill_commit(&mut progress, 3, 1, 0, &result);

        assert_eq!(progress.processed, 4);
        assert_eq!(progress.persisted, 1);
        assert_eq!(progress.skipped, 1);
        assert_eq!(progress.failed, 1);
        assert_eq!(progress.remaining, 0);
        assert_eq!(
            progress.processed - progress.excluded,
            progress.persisted + progress.skipped + progress.failed
        );
        assert_eq!(progress.fatal_error, None);
        assert_eq!(progress.reason_counts["policy_excluded"], 1);
    }

    #[test]
    fn backfill_logs_are_structured_redacted_and_severity_is_explicit() {
        let message = backfill_log_message(
            "run-1",
            "inference",
            "event-1",
            2,
            4,
            3,
            "fallback",
            "invalid_model_output",
        );

        assert!(message.contains("run_id=run-1"));
        assert!(message.contains("phase=inference"));
        assert!(message.contains("event_id=event-1"));
        assert!(message.contains("chunk=2"));
        assert!(message.contains("reason=invalid_model_output"));
        assert!(!message.contains("secret source text"));
        assert_eq!(
            backfill_log_severity("fallback"),
            BackfillLogSeverity::Warning
        );
        assert_eq!(
            backfill_log_severity("projection_warning"),
            BackfillLogSeverity::Warning
        );
        assert_eq!(backfill_log_severity("fatal"), BackfillLogSeverity::Error);
        assert_eq!(
            backfill_log_severity("completed"),
            BackfillLogSeverity::Info
        );
        assert_eq!(
            runtime_failure_reason("summary queue full"),
            Some("summary_queue_failed")
        );
        assert_eq!(
            runtime_failure_reason("contract violation: invalid_model_output"),
            None
        );
    }

    #[test]
    fn stable_classifier_preserves_contract_reason_codes() {
        assert_eq!(
            inference_failure_reason("contract violation: metadata_echo"),
            "metadata_echo"
        );
        assert_eq!(
            inference_failure_reason("contract violation: ungrounded_summary"),
            "ungrounded_summary"
        );
        assert_eq!(
            inference_failure_reason("summary response is not JSON"),
            "invalid_model_output"
        );
    }

    #[test]
    fn stable_classifier_keeps_runtime_timeout_out_of_generic_inference_failure() {
        assert_eq!(
            inference_failure_reason("summary_runtime_timeout: request deadline exceeded"),
            "summary_runtime_timeout"
        );
        assert_eq!(
            runtime_failure_reason("summary_runtime_timeout: request deadline exceeded"),
            Some("summary_runtime_timeout")
        );

        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 0,
            total: 1,
            queued: 1,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 0,
            attempted: 0,
            retry_count: 1,
            remaining: 1,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        set_backfill_fatal(
            &mut progress,
            runtime_failure_reason("summary_runtime_timeout: request deadline exceeded")
                .expect("timeout is a fatal runtime reason"),
        );
        assert_eq!(progress.state, "error");
        assert_eq!(
            progress.fatal_error.as_deref(),
            Some("summary_runtime_timeout")
        );
        assert_eq!(progress.failed, 0);
        assert_eq!(progress.reason_counts.len(), 0);
    }

    #[test]
    fn retrying_the_same_failed_row_does_not_double_count_final_failure() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 0,
            total: 1,
            queued: 1,
            skipped: 0,
            failed: 0,
            persisted: 0,
            excluded: 0,
            attempted: 1,
            retry_count: 1,
            remaining: 1,
            reason_counts: std::collections::BTreeMap::new(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };
        let result = SummaryBackfillApplyResult::default();

        // The first attempt and its corrective retry represent one final row.
        record_backfill_commit(&mut progress, 1, 0, 1, &result);
        record_backfill_commit(&mut progress, 1, 0, 1, &result);

        assert_eq!(progress.failed, 1);
        assert_eq!(progress.retry_count, 1);
        assert_eq!(progress.processed, 1);
    }

    #[test]
    fn completed_with_row_warnings_is_not_a_run_fatal_error() {
        let mut progress = MemoryBackfillProgress {
            state: "running".into(),
            processed: 2,
            total: 2,
            queued: 2,
            skipped: 0,
            failed: 1,
            persisted: 1,
            excluded: 0,
            attempted: 2,
            retry_count: 1,
            remaining: 0,
            reason_counts: [("metadata_echo".to_string(), 1)].into_iter().collect(),
            fatal_error: None,
            message: "running".into(),
            error: None,
        };

        finalize_backfill_progress(&mut progress, 2, false);

        assert_eq!(progress.state, "completed");
        assert_eq!(progress.fatal_error, None);
        assert_eq!(progress.error, None);
        assert_eq!(progress.remaining, 0);
        assert!(progress.message.contains("warnings"));
    }

    #[test]
    fn backfill_log_contains_only_safe_identity_and_counter_fields() {
        let message = backfill_log_message_with_counters(
            "run-7",
            "inference",
            "event-7",
            3,
            2,
            4,
            1,
            2,
            0,
            1,
            1,
            "fallback",
            "metadata_echo: private source text",
        );

        assert!(message.contains("run_id=run-7"));
        assert!(message.contains("phase=inference"));
        assert!(message.contains("event_id=event-7"));
        assert!(message.contains("chunk=3"));
        assert!(message.contains("attempt=2"));
        assert!(message.contains("status=fallback"));
        assert!(message.contains("reason=metadata_echo"));
        assert!(message.contains("counters="));
        assert!(!message.contains("private source text"));
        assert!(!message.contains("model response"));
    }

    #[test]
    fn test_resolve_effective_twitch_channel() {
        // Case 1: explicit twitch_channel with '#' prefix and padding
        let s1 = serde_json::json!({
            "twitch_channel": " #my_channel ",
            "user_name": "fallback_user"
        });
        assert_eq!(resolve_effective_twitch_channel(&s1), "my_channel");

        // Case 2: explicit twitch_channel without '#'
        let s2 = serde_json::json!({
            "twitch_channel": "streamer123"
        });
        assert_eq!(resolve_effective_twitch_channel(&s2), "streamer123");

        // Case 3: fallback to valid ASCII alphanumeric user_name
        let s3 = serde_json::json!({
            "twitch_channel": "",
            "user_name": "valid_user_99"
        });
        assert_eq!(resolve_effective_twitch_channel(&s3), "valid_user_99");

        // Case 4: non-ASCII user_name (e.g. Japanese) is rejected as fallback
        let s4 = serde_json::json!({
            "twitch_channel": "",
            "user_name": "こうた"
        });
        assert_eq!(resolve_effective_twitch_channel(&s4), "");

        // Case 5: legacy twitch_bot_channel fallback
        let s5 = serde_json::json!({
            "twitch_bot_channel": "#legacy_channel"
        });
        assert_eq!(resolve_effective_twitch_channel(&s5), "legacy_channel");

        // Case 6: all empty
        let s6 = serde_json::json!({});
        assert_eq!(resolve_effective_twitch_channel(&s6), "");
    }
