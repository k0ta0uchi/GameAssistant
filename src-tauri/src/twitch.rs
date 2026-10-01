use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

pub const OAUTH_STATE_TTL_SECS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchChatMessage {
    pub channel: String,
    pub author: String,
    pub content: String,
    pub is_mod: bool,
    pub is_subscriber: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TwitchBotSettings {
    pub channel: String,
    pub bot_nick: String,
    pub oauth_token: String, // "oauth:xxxx"
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TwitchAuthStatus {
    pub success: bool,
    pub user_id: String,
    pub login: String,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
}

pub fn persist_validated_tokens(
    root: &std::path::Path,
    token: &TwitchTokenResponse,
    account: &TwitchValidateResponse,
) -> Result<TwitchAuthStatus, String> {
    let outcome = persist_refreshed_tokens(Some(root), token);
    if !outcome.is_success() {
        return Err(outcome.errors.join("; "));
    }
    crate::settings::save_setting_key(root, "twitch_bot_id", serde_json::json!(account.user_id))?;
    crate::settings::save_setting_key(
        root,
        "twitch_bot_username",
        serde_json::json!(account.login),
    )?;
    Ok(TwitchAuthStatus {
        success: true,
        user_id: account.user_id.clone(),
        login: account.login.clone(),
        has_access_token: crate::credentials::is_secret_configured(root, "twitch_access_token"),
        has_refresh_token: crate::credentials::is_secret_configured(root, "twitch_refresh_token"),
    })
}

pub fn resolve_connection_settings(
    root: &std::path::Path,
    mut input: TwitchBotSettings,
) -> Result<TwitchBotSettings, String> {
    let saved = crate::settings::load_frontend_settings(root)?;
    if input.channel.trim().is_empty() {
        input.channel = crate::session::resolve_effective_twitch_channel(&saved);
    }
    if input.bot_nick.trim().is_empty() {
        input.bot_nick = saved["twitch_bot_username"]
            .as_str()
            .unwrap_or_default()
            .to_string();
    }
    if input.oauth_token.trim().is_empty() {
        input.oauth_token =
            crate::credentials::get_secret(root, "twitch_access_token").unwrap_or_default();
    }
    if input
        .client_id
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        input.client_id = saved["twitch_client_id"].as_str().map(str::to_string);
    }
    if input
        .client_secret
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        input.client_secret = crate::credentials::get_secret(root, "twitch_client_secret");
    }
    if input
        .refresh_token
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        input.refresh_token = crate::credentials::get_secret(root, "twitch_refresh_token");
    }
    Ok(input)
}

/// リフレッシュトークン永続化の結果を表す構造体
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPersistOutcome {
    pub access_token_saved: bool,
    pub refresh_token_saved: bool,
    pub errors: Vec<String>,
}

impl TokenPersistOutcome {
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

/// リフレッシュされた Twitch トークン（access / refresh）を設定ディレクトリの資格情報ストアへ永続化する。
/// エラーを握りつぶさず、各トークン保存の成否（部分成功を含む）とエラー一覧を記録して返却する。
pub fn persist_refreshed_tokens(
    root_dir: Option<&std::path::Path>,
    new_tok: &TwitchTokenResponse,
) -> TokenPersistOutcome {
    let mut outcome = TokenPersistOutcome {
        access_token_saved: false,
        refresh_token_saved: false,
        errors: Vec::new(),
    };

    let Some(rdir) = root_dir else {
        outcome
            .errors
            .push("Root directory is not configured for TwitchService".to_string());
        return outcome;
    };

    match crate::settings::save_setting_key(
        rdir,
        "twitch_access_token",
        serde_json::Value::String(new_tok.access_token.clone()),
    ) {
        Ok(_) => outcome.access_token_saved = true,
        Err(e) => outcome
            .errors
            .push(format!("Failed to persist twitch_access_token: {}", e)),
    }

    if let Some(ref new_r) = new_tok.refresh_token {
        match crate::settings::save_setting_key(
            rdir,
            "twitch_refresh_token",
            serde_json::Value::String(new_r.clone()),
        ) {
            Ok(_) => outcome.refresh_token_saved = true,
            Err(e) => outcome
                .errors
                .push(format!("Failed to persist twitch_refresh_token: {}", e)),
        }
    }

    outcome
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchValidateResponse {
    pub client_id: String,
    pub login: String,
    pub user_id: String,
    pub expires_in: u64,
}

pub struct TwitchService {
    is_connected: Arc<AtomicBool>,
    sender: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    http_client: reqwest::Client,
    app_handle: Arc<Mutex<Option<AppHandle>>>,
    log_mgr: Arc<Mutex<Option<Arc<crate::logger::LogManager>>>>,
    root_dir: Arc<Mutex<Option<std::path::PathBuf>>>,
    pending_auth_states: Arc<Mutex<HashMap<String, SystemTime>>>,
}

impl Default for TwitchService {
    fn default() -> Self {
        Self::new()
    }
}

impl TwitchService {
    pub fn new() -> Self {
        Self {
            is_connected: Arc::new(AtomicBool::new(false)),
            sender: Arc::new(Mutex::new(None)),
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            app_handle: Arc::new(Mutex::new(None)),
            log_mgr: Arc::new(Mutex::new(None)),
            root_dir: Arc::new(Mutex::new(None)),
            pending_auth_states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_log_manager(&self, log_mgr: Arc<crate::logger::LogManager>) {
        *self.log_mgr.lock() = Some(log_mgr);
    }

    pub fn set_root_dir(&self, root_dir: std::path::PathBuf) {
        *self.root_dir.lock() = Some(root_dir);
    }

    /// 認証開始ごとに暗号論的に安全なランダム state を生成・一時保存
    pub fn generate_auth_state(&self) -> String {
        let state: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();

        let mut states = self.pending_auth_states.lock();
        let now = SystemTime::now();
        // 期限切れのエントリをパージ
        states.retain(|_, created_at| {
            now.duration_since(*created_at)
                .map(|d| d.as_secs() < OAUTH_STATE_TTL_SECS)
                .unwrap_or(false)
        });
        states.insert(state.clone(), now);
        state
    }

    /// Twitch OAuth 認可 URL を生成 (Authorization Code フロー, 新規 state 付与)
    pub fn get_auth_url(&self, client_id: &str, redirect_uri: &str) -> String {
        let state = self.generate_auth_state();
        Self::format_auth_url(client_id, redirect_uri, &state)
    }

    /// Twitch OAuth 認可 URL をフォーマット
    pub fn format_auth_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
        let scopes = "chat:read chat:edit moderator:read:followers user:read:chat user:write:chat user:bot channel:bot";
        format!(
            "https://id.twitch.tv/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
            client_id,
            urlencoding::encode(redirect_uri),
            urlencoding::encode(scopes),
            urlencoding::encode(state)
        )
    }

    /// OAuth state を検証して消費（ワンタイム使用・再利用不可）
    pub fn verify_and_consume_state(&self, state: Option<&str>) -> Result<(), String> {
        let state_val = match state {
            Some(s) if !s.trim().is_empty() => s.trim(),
            _ => return Err("OAuth state is missing".to_string()),
        };

        let mut states = self.pending_auth_states.lock();
        let now = SystemTime::now();

        // 期限切れのエントリをパージ
        states.retain(|_, created_at| {
            now.duration_since(*created_at)
                .map(|d| d.as_secs() < OAUTH_STATE_TTL_SECS)
                .unwrap_or(false)
        });

        if let Some(created_at) = states.remove(state_val) {
            let is_valid = now
                .duration_since(created_at)
                .map(|d| d.as_secs() < OAUTH_STATE_TTL_SECS)
                .unwrap_or(false);
            if !is_valid {
                return Err("OAuth state has expired. Please restart authorization.".to_string());
            }
            Ok(())
        } else {
            Err("OAuth state is invalid, expired, or already used".to_string())
        }
    }

    #[cfg(test)]
    pub fn insert_pending_state_for_test(&self, state: String, created_at: SystemTime) {
        let mut states = self.pending_auth_states.lock();
        states.insert(state, created_at);
    }
}

/// 入力文字列 (code または URL、または code#state / code:state) と明示的な state オプションから
/// 有効な code と state を抽出する
pub fn parse_code_and_state(
    code_input: &str,
    explicit_state: Option<String>,
) -> (String, Option<String>) {
    let trimmed = code_input.trim();

    // 1. explicit_state が指定されている場合はそれを優先
    if let Some(s) = explicit_state {
        let s_trimmed = s.trim();
        if !s_trimmed.is_empty() {
            let clean_code = if let Some((c, _)) = trimmed.split_once('#') {
                c.trim().to_string()
            } else if let Some((c, _)) = trimmed.split_once(':') {
                c.trim().to_string()
            } else {
                trimmed.to_string()
            };
            return (clean_code, Some(s_trimmed.to_string()));
        }
    }

    // 2. URL 形式の場合 (https://... または ?code=...)
    if (trimmed.starts_with("http://") || trimmed.starts_with("https://")) && trimmed.contains('?')
    {
        if let Some((_, query)) = trimmed.split_once('?') {
            let mut extracted_code = String::new();
            let mut extracted_state = None;
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "code" {
                        extracted_code = urlencoding::decode(v)
                            .unwrap_or_else(|_| v.into())
                            .to_string();
                    } else if k == "state" {
                        extracted_state = Some(
                            urlencoding::decode(v)
                                .unwrap_or_else(|_| v.into())
                                .to_string(),
                        );
                    }
                }
            }
            if !extracted_code.is_empty() {
                return (extracted_code, extracted_state);
            }
        }
    }

    // 3. code#state 形式
    if let Some((c, s)) = trimmed.split_once('#') {
        let clean_c = c.trim();
        let clean_s = s.trim();
        if !clean_c.is_empty() && !clean_s.is_empty() {
            return (clean_c.to_string(), Some(clean_s.to_string()));
        }
    }

    // 4. code:state 形式
    if let Some((c, s)) = trimmed.split_once(':') {
        let clean_c = c.trim();
        let clean_s = s.trim();
        if !clean_c.is_empty() && !clean_s.is_empty() {
            return (clean_c.to_string(), Some(clean_s.to_string()));
        }
    }

    // 5. 単一コード
    (trimmed.to_string(), None)
}

impl TwitchService {
    /// アクセストークンの検証
    pub async fn validate_token(
        &self,
        access_token: &str,
    ) -> Result<TwitchValidateResponse, String> {
        let clean_token = access_token.trim().trim_start_matches("oauth:");
        let resp = self
            .http_client
            .get("https://id.twitch.tv/oauth2/validate")
            .header("Authorization", format!("OAuth {}", clean_token))
            .send()
            .await
            .map_err(|e| format!("Token validation request error: {}", e))?;

        if resp.status().is_success() {
            let val_res: TwitchValidateResponse = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse validate response: {}", e))?;
            Ok(val_res)
        } else {
            Err(format!("Token invalid (status: {})", resp.status()))
        }
    }

    /// リフレッシュトークンによるアクセストークン再取得
    pub async fn refresh_token(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<TwitchTokenResponse, String> {
        let params = [
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];

        let resp = self
            .http_client
            .post("https://id.twitch.tv/oauth2/token")
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("Token refresh request error: {}", e))?;

        if resp.status().is_success() {
            let tok_res: TwitchTokenResponse = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse refresh response: {}", e))?;
            Ok(tok_res)
        } else {
            let err_txt = resp.text().await.unwrap_or_default();
            Err(format!("Failed to refresh token: {}", err_txt))
        }
    }

    /// 認可コード (code) からアクセストークンを取得 (OAuth state 検証付き)
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        state: Option<&str>,
        redirect_uri: &str,
    ) -> Result<TwitchTokenResponse, String> {
        // 先に state 検証を実行。不一致・欠落・期限切れ時は HTTP リクエストを送信せずに即座に拒否
        self.verify_and_consume_state(state)?;

        let params = [
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
        ];

        let resp = self
            .http_client
            .post("https://id.twitch.tv/oauth2/token")
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("Token exchange request error: {}", e))?;

        if resp.status().is_success() {
            let tok_res: TwitchTokenResponse = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse token response: {}", e))?;
            Ok(tok_res)
        } else {
            let err_txt = resp.text().await.unwrap_or_default();
            Err(format!("Failed to exchange token: {}", err_txt))
        }
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::SeqCst)
    }

    pub fn send_chat(&self, channel: &str, message: &str) -> Result<(), String> {
        if !self.is_connected() {
            return Err("Twitch bot is not connected".to_string());
        }

        let chan = if channel.starts_with('#') {
            channel.to_string()
        } else {
            format!("#{}", channel)
        };

        let raw = format!("PRIVMSG {} :{}\r\n", chan, message);
        if let Some(tx) = self.sender.lock().as_ref() {
            tx.send(raw)
                .map_err(|e| format!("Failed to send chat: {}", e))?;
            Ok(())
        } else {
            Err("No sender channel available".to_string())
        }
    }

    pub fn disconnect(&self) {
        self.is_connected.store(false, Ordering::SeqCst);
        if let Some(tx) = self.sender.lock().take() {
            let _ = tx.send("QUIT\r\n".to_string());
        }
        if let Some(ref handle) = *self.app_handle.lock() {
            let _ = handle.emit("twitch_status", serde_json::json!({ "connected": false }));
        }
    }

    /// WebSocket IRC 接続を開始する非同期タスク
    pub async fn connect(
        &self,
        settings: TwitchBotSettings,
        app_handle: Option<AppHandle>,
        on_message: Option<Arc<dyn Fn(TwitchChatMessage) + Send + Sync>>,
    ) -> Result<(), String> {
        // 既存の接続があれば安全に切断
        if self.is_connected() {
            self.disconnect();
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }

        if let Some(ref h) = app_handle {
            *self.app_handle.lock() = Some(h.clone());
        }

        let channel = settings
            .channel
            .trim()
            .trim_start_matches('#')
            .to_lowercase();
        let chan_with_hash = format!("#{}", channel);

        let mut token = settings.oauth_token.trim().to_string();
        if !token.starts_with("oauth:") && !token.is_empty() {
            token = format!("oauth:{}", token);
        }

        // トークンの検証 & 自動リフレッシュ & ニックネーム整合
        let mut active_token = token.clone();
        let mut resolved_nick = settings.bot_nick.trim().to_lowercase();
        let mut is_anonymous = active_token.is_empty();

        if !is_anonymous {
            let clean = active_token.trim_start_matches("oauth:").trim();
            match self.validate_token(clean).await {
                Ok(val_res) => {
                    // 重要: Twitch IRC では PASS oauth:<token> を使う際、NICK は必ずトークン所有者のログインIDでなければならない
                    resolved_nick = val_res.login.to_lowercase();
                    if let Some(log) = self.log_mgr.lock().as_ref() {
                        log.info(
                            "Twitch",
                            &format!(
                                "Twitch token validated successfully for user '{}' (token owner)",
                                resolved_nick
                            ),
                        );
                    }
                }
                Err(val_err) => {
                    if let Some(log) = self.log_mgr.lock().as_ref() {
                        log.warn(
                            "Twitch",
                            &format!(
                                "Twitch token validation failed ({}). Attempting refresh...",
                                val_err
                            ),
                        );
                    }

                    let mut refreshed = false;
                    if let (Some(cid), Some(csec), Some(rtok)) = (
                        settings.client_id.as_deref(),
                        settings.client_secret.as_deref(),
                        settings.refresh_token.as_deref(),
                    ) {
                        if !cid.is_empty() && !csec.is_empty() && !rtok.is_empty() {
                            match self.refresh_token(cid, csec, rtok).await {
                                Ok(new_tok) => {
                                    active_token = format!("oauth:{}", new_tok.access_token);
                                    if let Ok(v2) = self.validate_token(&new_tok.access_token).await
                                    {
                                        resolved_nick = v2.login.to_lowercase();
                                    }
                                    refreshed = true;

                                    // settings.json / credential store に最新トークンを自動永続化
                                    let outcome = persist_refreshed_tokens(
                                        self.root_dir.lock().as_deref(),
                                        &new_tok,
                                    );

                                    if outcome.is_success() {
                                        if let Some(log) = self.log_mgr.lock().as_ref() {
                                            log.info(
                                                "Twitch",
                                                &format!(
                                                    "Twitch token refreshed and persisted to credential store successfully! Authenticated as '{}'",
                                                    resolved_nick
                                                ),
                                            );
                                        }
                                    } else {
                                        let summary = if outcome.access_token_saved
                                            && !outcome.refresh_token_saved
                                        {
                                            "partial persistence: access token saved, but refresh token failed"
                                        } else if !outcome.access_token_saved
                                            && outcome.refresh_token_saved
                                        {
                                            "partial persistence: refresh token saved, but access token failed"
                                        } else {
                                            "all token persistence failed"
                                        };
                                        if let Some(log) = self.log_mgr.lock().as_ref() {
                                            log.error(
                                                "Twitch",
                                                &format!(
                                                    "Twitch token refreshed via API but credential persistence failed ({}) [{}]. Connection continuing for current session.",
                                                    summary,
                                                    outcome.errors.join("; ")
                                                ),
                                            );
                                        }
                                    }

                                    if let Some(ref handle) = app_handle {
                                        let _ = handle.emit("twitch_token_refreshed", serde_json::json!({ "success": outcome.is_success(), "login": resolved_nick, "access_token_saved": outcome.access_token_saved, "refresh_token_saved": outcome.refresh_token_saved }));
                                        if !outcome.is_success() {
                                            let _ = handle.emit(
                                                "twitch_token_persist_error",
                                                serde_json::json!({
                                                    "access_token_saved": outcome.access_token_saved,
                                                    "refresh_token_saved": outcome.refresh_token_saved,
                                                    "errors": outcome.errors,
                                                }),
                                            );
                                        }
                                    }
                                }
                                Err(ref_err) => {
                                    if let Some(log) = self.log_mgr.lock().as_ref() {
                                        log.warn(
                                            "Twitch",
                                            &format!("Twitch token refresh failed: {}", ref_err),
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if !refreshed {
                        // トークン無効かつリフレッシュ失敗時は、安全に匿名モードにフォールバックしてチャットの確実な受信を維持！
                        let random_digits: u32 = rand::random::<u32>() % 90000 + 10000;
                        resolved_nick = format!("justinfan{}", random_digits);
                        active_token = "".to_string();
                        is_anonymous = true;
                        if let Some(log) = self.log_mgr.lock().as_ref() {
                            log.warn(
                                "Twitch",
                                &format!(
                                    "OAuth token invalid. Falling back to anonymous reader mode as '{}' to guarantee comment logging.",
                                    resolved_nick
                                ),
                            );
                        }
                    }
                }
            }
        } else {
            let random_digits: u32 = rand::random::<u32>() % 90000 + 10000;
            resolved_nick = format!("justinfan{}", random_digits);
        }

        let pass_cmd = if is_anonymous {
            "PASS SCHMOOPIE\r\n".to_string()
        } else {
            format!("PASS {}\r\n", active_token)
        };

        if let Some(log) = self.log_mgr.lock().as_ref() {
            if is_anonymous {
                log.info(
                    "Twitch",
                    &format!(
                        "Connecting to Twitch IRC anonymously as '{}' for channel '{}' (read-only mode)",
                        resolved_nick, chan_with_hash
                    ),
                );
            } else {
                log.info(
                    "Twitch",
                    &format!(
                        "Connecting to Twitch IRC with authenticated nick='{}' for channel '{}'",
                        resolved_nick, chan_with_hash
                    ),
                );
            }
        }

        let ws_url = "wss://irc-ws.chat.twitch.tv:443";
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| format!("Twitch WebSocket connection failed: {}", e))?;

        let (mut write, mut read) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *self.sender.lock() = Some(tx);
        self.is_connected.store(true, Ordering::SeqCst);

        if let Some(ref handle) = app_handle {
            let _ = handle.emit("twitch_status", serde_json::json!({ "connected": true }));
        }

        // 認証コマンド送信シーケンス: CAP REQ -> PASS -> NICK -> JOIN
        write
            .send(Message::Text(
                "CAP REQ :twitch.tv/tags twitch.tv/commands\r\n".to_string(),
            ))
            .await
            .map_err(|e| e.to_string())?;
        write
            .send(Message::Text(pass_cmd))
            .await
            .map_err(|e| e.to_string())?;
        write
            .send(Message::Text(format!("NICK {}\r\n", resolved_nick)))
            .await
            .map_err(|e| e.to_string())?;
        write
            .send(Message::Text(format!("JOIN {}\r\n", chan_with_hash)))
            .await
            .map_err(|e| e.to_string())?;

        let is_connected_sender = self.is_connected.clone();
        let is_connected_reader = self.is_connected.clone();
        let log_mgr_clone = self.log_mgr.clone();

        // 送信タスク
        let write_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if msg == "QUIT\r\n" {
                    let _ = write.close().await;
                    break;
                }
                if let Err(e) = write.send(Message::Text(msg)).await {
                    eprintln!("[Twitch] Send error: {}", e);
                    break;
                }
            }
            is_connected_sender.store(false, Ordering::SeqCst);
        });

        // 受信タスク
        let sender_clone = self.sender.clone();
        let chan_name = channel.clone();
        let nick_clone = resolved_nick.clone();
        tokio::spawn(async move {
            while let Some(msg_res) = read.next().await {
                match msg_res {
                    Ok(Message::Text(text)) => {
                        for line in text.lines() {
                            // PING / PONG
                            if line.starts_with("PING") {
                                if let Some(tx) = sender_clone.lock().as_ref() {
                                    let pong = line.replace("PING", "PONG");
                                    let _ = tx.send(format!("{}\r\n", pong));
                                }
                                continue;
                            }

                            // NOTICE (Twitch サーバーからの通知・エラー)
                            if line.contains(" NOTICE ") {
                                if let Some(log) = log_mgr_clone.lock().as_ref() {
                                    log.warn("Twitch", &format!("IRC Notice: {}", line));
                                }
                                eprintln!("[Twitch] IRC Notice: {}", line);
                            }

                            // 001 RPL_WELCOME (ログイン成功)
                            if line.contains(" 001 ") {
                                if let Some(log) = log_mgr_clone.lock().as_ref() {
                                    log.info(
                                        "Twitch",
                                        &format!(
                                            "Logged in to Twitch IRC successfully as '{}'",
                                            nick_clone
                                        ),
                                    );
                                }
                            }

                            // JOIN (チャンネル参加完了)
                            if line.contains(" JOIN ") {
                                if let Some(log) = log_mgr_clone.lock().as_ref() {
                                    log.info(
                                        "Twitch",
                                        &format!(
                                            "Joined Twitch channel '#{}' successfully",
                                            chan_name
                                        ),
                                    );
                                }
                            }

                            // PRIVMSG パース
                            if line.contains("PRIVMSG") {
                                if let Some(chat_msg) = parse_irc_privmsg(line, &chan_name) {
                                    if let Some(log) = log_mgr_clone.lock().as_ref() {
                                        log.info(
                                            "Twitch",
                                            &format!(
                                                "[Chat] {}: {}",
                                                chat_msg.author, chat_msg.content
                                            ),
                                        );
                                    }
                                    if let Some(ref handle) = app_handle {
                                        let _ = handle.emit("twitch-chat", &chat_msg);
                                    }
                                    if let Some(ref cb) = on_message {
                                        cb(chat_msg);
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        if is_connected_reader.load(Ordering::SeqCst) {
                            if let Some(log) = log_mgr_clone.lock().as_ref() {
                                log.warn(
                                    "Twitch",
                                    &format!("Twitch IRC connection closed by server: {:?}", frame),
                                );
                            }
                        } else if let Some(log) = log_mgr_clone.lock().as_ref() {
                            log.info("Twitch", "Twitch IRC connection closed cleanly.");
                        }
                        break;
                    }
                    Err(e) => {
                        if is_connected_reader.load(Ordering::SeqCst) {
                            if let Some(log) = log_mgr_clone.lock().as_ref() {
                                log.warn(
                                    "Twitch",
                                    &format!("Twitch IRC connection disconnected: {}", e),
                                );
                            }
                        } else if let Some(log) = log_mgr_clone.lock().as_ref() {
                            log.info("Twitch", "Twitch IRC session ended cleanly.");
                        }
                        break;
                    }
                    _ => {}
                }
            }
            is_connected_reader.store(false, Ordering::SeqCst);
            write_task.abort();
        });

        Ok(())
    }
}

/// Twitch IRC 行をパースして構造体に変換する
pub fn parse_irc_privmsg(raw: &str, default_channel: &str) -> Option<TwitchChatMessage> {
    // 例: @badge-info=;badges=... :username!username@username.tmi.twitch.tv PRIVMSG #channel :message
    let mut author = "User".to_string();
    let mut is_mod = false;
    let mut is_subscriber = false;

    if raw.starts_with('@') {
        if let Some(space_idx) = raw.find(' ') {
            let tags_part = &raw[1..space_idx];
            for tag in tags_part.split(';') {
                if let Some((k, v)) = tag.split_once('=') {
                    match k {
                        "display-name" if !v.is_empty() => author = v.to_string(),
                        "mod" if v == "1" => is_mod = true,
                        "subscriber" if v == "1" => is_subscriber = true,
                        _ => {}
                    }
                }
            }
        }
    }

    // display-name タグが無い場合、プレフィックス (:nick!user@...) から作者名をフォールバック取得
    if author == "User" {
        let prefix = if raw.starts_with(':') {
            raw.split_whitespace().next()
        } else if raw.starts_with('@') {
            raw.split_whitespace().nth(1)
        } else {
            None
        };
        if let Some(p) = prefix {
            if let Some(stripped) = p.strip_prefix(':') {
                if let Some(nick) = stripped.split('!').next() {
                    if !nick.is_empty() {
                        author = nick.to_string();
                    }
                }
            }
        }
    }

    let privmsg_idx = raw.find("PRIVMSG")?;
    let after_privmsg = &raw[privmsg_idx + 7..].trim_start();
    let (chan_part, msg_part) = after_privmsg.split_once(" :")?;

    let channel = chan_part.trim_start_matches('#').to_string();
    let content = msg_part.to_string();

    let timestamp = chrono::Local::now().to_rfc3339();

    Some(TwitchChatMessage {
        channel: if channel.is_empty() {
            default_channel.to_string()
        } else {
            channel
        },
        author,
        content,
        is_mod,
        is_subscriber,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registered_tokens_stay_in_backend_and_resolve_for_connection() {
        let root = std::env::temp_dir().join(format!("ga_auth_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let token = TwitchTokenResponse {
            access_token: "test-access-secret".into(),
            refresh_token: Some("test-refresh-secret".into()),
            expires_in: None,
            token_type: None,
        };
        let account = TwitchValidateResponse {
            client_id: "client".into(),
            login: "bot".into(),
            user_id: "123".into(),
            expires_in: 3600,
        };
        let status = persist_validated_tokens(&root, &token, &account).unwrap();
        let payload = serde_json::to_string(&status).unwrap();
        assert!(!payload.contains("test-access-secret"));
        assert!(!payload.contains("test-refresh-secret"));
        assert!(status.has_access_token && status.has_refresh_token);
        let resolved = resolve_connection_settings(&root, TwitchBotSettings::default()).unwrap();
        assert_eq!(resolved.oauth_token, token.access_token);
        assert_eq!(resolved.refresh_token, token.refresh_token);
        assert_eq!(resolved.bot_nick, "bot");
        let override_settings = TwitchBotSettings {
            oauth_token: "explicit".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_connection_settings(&root, override_settings)
                .unwrap()
                .oauth_token,
            "explicit"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_registration_reports_persistence_failure() {
        let root = std::env::temp_dir().join(format!("ga_auth_fail_{}", uuid::Uuid::new_v4()));
        std::fs::write(&root, b"not a directory").unwrap();
        let token = TwitchTokenResponse {
            access_token: "test-secret".into(),
            refresh_token: None,
            expires_in: None,
            token_type: None,
        };
        let account = TwitchValidateResponse {
            client_id: "client".into(),
            login: "bot".into(),
            user_id: "123".into(),
            expires_in: 3600,
        };
        let error = persist_validated_tokens(&root, &token, &account).unwrap_err();
        assert!(!error.contains("test-secret"));
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn test_parse_irc_privmsg_with_tags() {
        let raw = "@badge-info=;badges=broadcaster/1;display-name=StreamerKota;mod=1;subscriber=1 :streamerkota!streamerkota@streamerkota.tmi.twitch.tv PRIVMSG #k0ta0uchi :Hello chat!";
        let msg = parse_irc_privmsg(raw, "k0ta0uchi").expect("should parse");
        assert_eq!(msg.channel, "k0ta0uchi");
        assert_eq!(msg.author, "StreamerKota");
        assert_eq!(msg.content, "Hello chat!");
        assert!(msg.is_mod);
        assert!(msg.is_subscriber);
    }

    #[test]
    fn test_parse_irc_privmsg_prefix_fallback() {
        let raw = ":viewer99!viewer99@viewer99.tmi.twitch.tv PRIVMSG #k0ta0uchi :Nice play!";
        let msg = parse_irc_privmsg(raw, "k0ta0uchi").expect("should parse");
        assert_eq!(msg.channel, "k0ta0uchi");
        assert_eq!(msg.author, "viewer99");
        assert_eq!(msg.content, "Nice play!");
        assert!(!msg.is_mod);
        assert!(!msg.is_subscriber);
    }

    #[test]
    fn test_parse_irc_privmsg_empty_display_name() {
        let raw = "@display-name=;mod=0;subscriber=0 :anon_user!anon_user@anon_user.tmi.twitch.tv PRIVMSG #k0ta0uchi :test message";
        let msg = parse_irc_privmsg(raw, "k0ta0uchi").expect("should parse");
        assert_eq!(msg.channel, "k0ta0uchi");
        assert_eq!(msg.author, "anon_user");
        assert_eq!(msg.content, "test message");
    }

    #[tokio::test]
    async fn platform_test_live_twitch_connect() {
        let svc = TwitchService::new();
        let settings = TwitchBotSettings {
            channel: "k0ta0uchi".to_string(),
            bot_nick: "guri_bot".to_string(),
            oauth_token: "".to_string(),
            ..Default::default()
        };
        let res = svc.connect(settings, None, None).await;
        assert!(res.is_ok());
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        println!("is_connected: {}", svc.is_connected());
        svc.disconnect();
    }

    #[test]
    fn test_generate_auth_state_is_unique_and_embedded_in_auth_url() {
        let svc = TwitchService::new();
        let url1 = svc.get_auth_url("my_client_id", "https://localhost/auth");
        let url2 = svc.get_auth_url("my_client_id", "https://localhost/auth");

        assert!(url1.contains("state="));
        assert!(url2.contains("state="));
        assert_ne!(url1, url2, "Each auth URL must contain a unique state");
    }

    #[test]
    fn test_oauth_state_success_verification() {
        let svc = TwitchService::new();
        let state = svc.generate_auth_state();

        // 正常一致
        let res = svc.verify_and_consume_state(Some(&state));
        assert!(res.is_ok());
    }

    #[test]
    fn test_oauth_state_missing_rejected() {
        let svc = TwitchService::new();
        let _ = svc.generate_auth_state();

        // None
        let res_none = svc.verify_and_consume_state(None);
        assert!(res_none.is_err());
        assert!(res_none.unwrap_err().contains("missing"));

        // 空文字列
        let res_empty = svc.verify_and_consume_state(Some("   "));
        assert!(res_empty.is_err());
        assert!(res_empty.unwrap_err().contains("missing"));
    }

    #[test]
    fn test_oauth_state_mismatch_rejected() {
        let svc = TwitchService::new();
        let _valid_state = svc.generate_auth_state();

        // 不一致の state
        let res = svc.verify_and_consume_state(Some("attacker_forged_state"));
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("invalid"));
    }

    #[test]
    fn test_oauth_state_single_use_only() {
        let svc = TwitchService::new();
        let state = svc.generate_auth_state();

        // 1回目の検証は成功
        let first_res = svc.verify_and_consume_state(Some(&state));
        assert!(first_res.is_ok());

        // 同じ state での2回目の検証は失敗（再利用不可）
        let second_res = svc.verify_and_consume_state(Some(&state));
        assert!(second_res.is_err());
    }

    #[test]
    fn test_oauth_state_expired_rejected() {
        let svc = TwitchService::new();
        let expired_state = "expired_test_state".to_string();

        // TTL(600s) より古いタイムスタンプで挿入
        let past_time =
            SystemTime::now() - std::time::Duration::from_secs(OAUTH_STATE_TTL_SECS + 60);
        svc.insert_pending_state_for_test(expired_state.clone(), past_time);

        let res = svc.verify_and_consume_state(Some(&expired_state));
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_exchange_code_fails_without_http_request_on_invalid_state() {
        let svc = TwitchService::new();
        // 存在しない / 不正な state を渡した場合、無効なダミーエンドポイントでも即座に弾かれる
        let res = svc
            .exchange_code(
                "dummy_id",
                "dummy_secret",
                "dummy_code",
                Some("wrong_state"),
                "https://localhost/auth",
            )
            .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().contains("invalid"));
    }

    #[test]
    fn test_parse_code_and_state_variations() {
        // Case 1: explicit state provided
        let (c1, s1) = parse_code_and_state("mycode123", Some("mystate456".to_string()));
        assert_eq!(c1, "mycode123");
        assert_eq!(s1, Some("mystate456".to_string()));

        // Case 2: URL with code and state query params
        let (c2, s2) = parse_code_and_state(
            "https://k0ta0uchi.github.io/GameAssistant/auth.html?code=abcxyz&state=state789",
            None,
        );
        assert_eq!(c2, "abcxyz");
        assert_eq!(s2, Some("state789".to_string()));

        // Case 3: code#state format
        let (c3, s3) = parse_code_and_state("abcxyz#state789", None);
        assert_eq!(c3, "abcxyz");
        assert_eq!(s3, Some("state789".to_string()));

        // Case 4: code:state format
        let (c4, s4) = parse_code_and_state("abcxyz:state789", None);
        assert_eq!(c4, "abcxyz");
        assert_eq!(s4, Some("state789".to_string()));

        // Case 5: code only without state
        let (c5, s5) = parse_code_and_state("abcxyz_only", None);
        assert_eq!(c5, "abcxyz_only");
        assert_eq!(s5, None);
    }

    #[test]
    fn test_persist_refreshed_tokens_success() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_persist_token_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let new_tok = TwitchTokenResponse {
            access_token: "dummy_test_refreshed_access_token_abc".to_string(),
            refresh_token: Some("dummy_test_refreshed_refresh_token_xyz".to_string()),
            expires_in: Some(3600),
            token_type: Some("bearer".to_string()),
        };

        let outcome = persist_refreshed_tokens(Some(&temp_dir), &new_tok);
        assert!(outcome.is_success());
        assert!(outcome.access_token_saved);
        assert!(outcome.refresh_token_saved);
        assert!(outcome.errors.is_empty());

        // credential store に保存されていることを確認
        let saved_access = crate::credentials::get_secret(&temp_dir, "twitch_access_token");
        assert_eq!(
            saved_access.as_deref(),
            Some("dummy_test_refreshed_access_token_abc")
        );

        let saved_refresh = crate::credentials::get_secret(&temp_dir, "twitch_refresh_token");
        assert_eq!(
            saved_refresh.as_deref(),
            Some("dummy_test_refreshed_refresh_token_xyz")
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_persist_refreshed_tokens_without_refresh_token_succeeds() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_persist_token_no_refresh_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let new_tok = TwitchTokenResponse {
            access_token: "dummy_test_refreshed_access_token_only".to_string(),
            refresh_token: None,
            expires_in: Some(3600),
            token_type: Some("bearer".to_string()),
        };

        let outcome = persist_refreshed_tokens(Some(&temp_dir), &new_tok);
        assert!(outcome.is_success());
        assert!(outcome.access_token_saved);
        assert!(!outcome.refresh_token_saved);
        assert!(outcome.errors.is_empty());

        let saved_access = crate::credentials::get_secret(&temp_dir, "twitch_access_token");
        assert_eq!(
            saved_access.as_deref(),
            Some("dummy_test_refreshed_access_token_only")
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_persist_refreshed_tokens_missing_root_dir_reports_error() {
        let new_tok = TwitchTokenResponse {
            access_token: "dummy_test_refreshed_access_token_abc".to_string(),
            refresh_token: Some("dummy_test_refreshed_refresh_token_xyz".to_string()),
            expires_in: Some(3600),
            token_type: Some("bearer".to_string()),
        };

        let outcome = persist_refreshed_tokens(None, &new_tok);
        assert!(!outcome.is_success());
        assert!(!outcome.access_token_saved);
        assert!(!outcome.refresh_token_saved);
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].contains("Root directory is not configured"));
    }
}
