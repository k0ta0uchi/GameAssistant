use gameassistant_lib::lance_memory::{self, MemoryItem};
use gameassistant_lib::logger::LogManager;
use gameassistant_lib::session::{normalize_kana, SessionEvent, SessionManager};
use gameassistant_lib::settings::{load_settings_file, save_setting_key, scan_skills};
use gameassistant_lib::tts::TtsManager;
use gameassistant_lib::twitch::parse_irc_privmsg;
use gameassistant_lib::web_search::SearchResultItem;
use gameassistant_lib::window_capture;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn create_unique_temp_dir() -> Result<PathBuf, String> {
    let p = std::env::temp_dir().join(format!("ga_test_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    Ok(p)
}

#[tokio::main]
async fn main() {
    println!("============================================================");
    println!("  GameAssistant Rust Native Integration Test Suite");
    println!("============================================================");

    let start_all = Instant::now();
    let mut passed = 0;
    let mut failed = 0;

    // Test 1: Settings
    print!("[TEST 1/8] Settings Load & Save ... ");
    match test_settings().await {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 2: Skills Scan
    print!("[TEST 2/8] Skills Scan & Extraction ... ");
    match test_skills().await {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 3: LanceDB Memory CRUD
    print!("[TEST 3/8] LanceDB Vector Memory CRUD ... ");
    match test_lance_memory().await {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 4: Kana Normalization
    print!("[TEST 4/8] Kana / Text Normalization ... ");
    match test_kana() {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 5: Session Manager Lifecycle & Trimming
    print!("[TEST 5/8] Session Manager Event Lifecycle ... ");
    match test_session().await {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 6: Twitch IRC PRIVMSG Parser
    print!("[TEST 6/8] Twitch IRC Message Parsing ... ");
    match test_twitch() {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 7: Web Search Response Formatter
    print!("[TEST 7/8] Web Search Summary Formatting ... ");
    match test_search() {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    // Test 8: Logger & Window Capture
    print!("[TEST 8/8] Logger Lifecycle & Window Capture ... ");
    match test_logger_and_capture() {
        Ok(_) => { println!("PASS"); passed += 1; }
        Err(e) => { println!("FAIL: {}", e); failed += 1; }
    }

    println!("------------------------------------------------------------");
    println!("Test Summary: {} passed, {} failed in {:.2?}", passed, failed, start_all.elapsed());
    println!("============================================================");

    if failed > 0 {
        std::process::exit(1);
    }
}

async fn test_settings() -> Result<(), String> {
    let temp_dir = create_unique_temp_dir()?;
    let root = &temp_dir;

    let initial = load_settings_file(root);
    if initial != serde_json::json!({}) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Initial settings must be empty".to_string());
    }

    let key = "test_key";
    let val = serde_json::json!("test_value_123");
    let saved = save_setting_key(root, key, val.clone())?;
    if saved.get(key) != Some(&val) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Saved value mismatch".to_string());
    }

    let reloaded = load_settings_file(root);
    if reloaded.get(key) != Some(&val) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Reloaded value mismatch".to_string());
    }

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

async fn test_skills() -> Result<(), String> {
    let temp_dir = create_unique_temp_dir()?;
    let root = &temp_dir;
    let skills_dir = root.join("skills");
    fs::create_dir_all(&skills_dir).map_err(|e| e.to_string())?;

    let skill_content = "---\nname: \"関西弁ツッコミスキル\"\ndescription: \"関西弁でテンポ良くツッコミを行うスキルです。\"\n---\n# キャラ設定\n";
    fs::write(skills_dir.join("kansai-style.md"), skill_content).map_err(|e| e.to_string())?;

    let response = scan_skills(root);
    if response.skills.len() != 1 {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Expected 1 skill, got {}", response.skills.len()));
    }
    let s = &response.skills[0];
    if s.id != "kansai-style" || s.name != "関西弁ツッコミスキル" {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Skill metadata mismatch: {:?}", s));
    }

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

async fn test_lance_memory() -> Result<(), String> {
    let temp_dir = create_unique_temp_dir()?;
    let root = &temp_dir;

    let list_empty = lance_memory::list_memories(root, Some(10), Some(0)).await?;
    if list_empty.total != 0 || !list_empty.memories.is_empty() {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Empty DB must return 0 memories".to_string());
    }

    let item1 = MemoryItem {
        id: "mem_001".to_string(),
        document: "ユーザーはエルデンリングが好きです。".to_string(),
        memory_type: "preference".to_string(),
        source: "Chat".to_string(),
        timestamp: "2026-08-31T12:00:00Z".to_string(),
        user_id: Some("user_a".to_string()),
    };
    let item2 = MemoryItem {
        id: "mem_002".to_string(),
        document: "配信者はよくボス戦で叫びます。".to_string(),
        memory_type: "observation".to_string(),
        source: "Assistant".to_string(),
        timestamp: "2026-08-31T12:01:00Z".to_string(),
        user_id: None,
    };

    let count = lance_memory::insert_memory_batch(root, vec![item1.clone(), item2.clone()], None).await?;
    if count != 2 {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Expected 2 inserted, got {}", count));
    }

    let list_res = lance_memory::list_memories(root, Some(10), Some(0)).await?;
    if list_res.total != 2 {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Expected 2 total, got {}", list_res.total));
    }

    let deleted = lance_memory::delete_memory(root, "mem_001").await?;
    if !deleted {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Delete returned false".to_string());
    }

    let list_after = lance_memory::list_memories(root, Some(10), Some(0)).await?;
    if list_after.total != 1 || list_after.memories[0].id != "mem_002" {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Remaining memory mismatch after delete".to_string());
    }

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

fn test_kana() -> Result<(), String> {
    if normalize_kana("コンニチハ") != "こんにちは" {
        return Err("Katakana conversion failed".to_string());
    }
    if normalize_kana("ゲームアシスタント") != "げーむあしすたんと" {
        return Err("Long word conversion failed".to_string());
    }
    if normalize_kana("HELLO WORLD") != "hello world" {
        return Err("Lowercase conversion failed".to_string());
    }
    if normalize_kana("あ〜〜〜") != "あーーー" {
        return Err("Wave dash conversion failed".to_string());
    }
    Ok(())
}

async fn test_session() -> Result<(), String> {
    let temp_dir = create_unique_temp_dir()?;
    let root = temp_dir.clone();
    let log_mgr = Arc::new(LogManager::new());
    let tts_mgr = Arc::new(TtsManager::new());
    let session = SessionManager::new(root, tts_mgr, log_mgr);

    session.start_session();
    if !session.is_active() {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Session should be active".to_string());
    }

    for i in 0..120 {
        session.add_event(SessionEvent {
            id: format!("ev_{}", i),
            r#type: "user_speech".to_string(),
            author: "User".to_string(),
            content: format!("メッセージ {}", i),
            timestamp: "2026-08-31T12:00:00Z".to_string(),
        });
    }

    let events = session.get_events();
    if events.len() != 100 {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Expected 100 events limit, got {}", events.len()));
    }
    if events.first().unwrap().id != "ev_20" {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(format!("Expected first event ev_20, got {}", events.first().unwrap().id));
    }

    session.stop_session();
    if session.is_active() {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err("Session should be inactive".to_string());
    }

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

fn test_twitch() -> Result<(), String> {
    let raw_mod = "@badge-info=;badges=moderator/1;display-name=ModUser;mod=1;subscriber=0 :moduser!moduser@moduser.tmi.twitch.tv PRIVMSG #streamer_channel :こんにちは！ナイスプレイ！";
    let parsed_mod = parse_irc_privmsg(raw_mod, "default_channel").ok_or("Failed to parse mod message")?;
    if parsed_mod.author != "ModUser" || parsed_mod.channel != "streamer_channel" || !parsed_mod.is_mod {
        return Err("Mod parsed mismatch".to_string());
    }

    let raw_sub = "@badge-info=subscriber/6;badges=subscriber/6;display-name=SubUser;mod=0;subscriber=1 :subuser!subuser@subuser.tmi.twitch.tv PRIVMSG #streamer_channel :いつも応援してます！";
    let parsed_sub = parse_irc_privmsg(raw_sub, "default_channel").ok_or("Failed to parse sub message")?;
    if parsed_sub.author != "SubUser" || !parsed_sub.is_subscriber {
        return Err("Sub parsed mismatch".to_string());
    }
    Ok(())
}

fn test_search() -> Result<(), String> {
    let items = vec![
        SearchResultItem {
            title: "エルデンリング攻略".to_string(),
            url: "https://example.com/elden".to_string(),
            description: "ボス攻略まとめ".to_string(),
        },
    ];
    let mut lines = Vec::new();
    for (i, r) in items.iter().enumerate() {
        lines.push(format!("{}. 【{}】\n   {}\n   URL: {}", i + 1, r.title, r.description, r.url));
    }
    let summary = format!("### Web検索結果: エルデンリング\n\n{}", lines.join("\n\n"));
    if !summary.contains("【エルデンリング攻略】") || !summary.contains("URL: https://example.com/elden") {
        return Err("Search format mismatch".to_string());
    }
    Ok(())
}

fn test_logger_and_capture() -> Result<(), String> {
    let log_mgr = LogManager::new();
    log_mgr.info("TestLogger", "テスト情報ログ");
    log_mgr.warn("TestLogger", "テスト警告ログ");
    log_mgr.error("TestLogger", "テストエラーログ");

    let logs = log_mgr.get_logs();
    if logs.len() != 3 || logs[0].level != "INFO" || logs[1].level != "WARNING" || logs[2].level != "ERROR" {
        return Err("Log levels mismatch".to_string());
    }

    log_mgr.clear();
    if !log_mgr.get_logs().is_empty() {
        return Err("Log clear failed".to_string());
    }

    let windows = window_capture::list_windows();
    for win in &windows {
        if win.is_empty() || win == "Program Manager" {
            return Err("Window filter violation".to_string());
        }
    }
    Ok(())
}
