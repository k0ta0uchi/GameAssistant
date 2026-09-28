use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

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
        }
    }

    pub fn set_log_manager(&self, log_mgr: Arc<crate::logger::LogManager>) {
        *self.log_mgr.lock() = Some(log_mgr);
    }

    pub fn set_root_dir(&self, root_dir: std::path::PathBuf) {
        *self.root_dir.lock() = Some(root_dir);
    }

    /// Twitch OAuth 認可 URL を生成 (Authorization Code フロー)
    pub fn get_auth_url(client_id: &str, redirect_uri: &str) -> String {
        let scopes = "chat:read chat:edit moderator:read:followers user:read:chat user:write:chat user:bot channel:bot";
        format!(
            "https://id.twitch.tv/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}",
            client_id,
            urlencoding::encode(redirect_uri),
            urlencoding::encode(scopes)
        )
    }

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

    /// 認可コード (code) からアクセストークンを取得
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TwitchTokenResponse, String> {
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
                                    if let Some(log) = self.log_mgr.lock().as_ref() {
                                        log.info(
                                            "Twitch",
                                            &format!(
                                                "Twitch token refreshed successfully! Authenticated as '{}'",
                                                resolved_nick
                                            ),
                                        );
                                    }
                                    // settings.json に最新アクセストークンを自動永続化
                                    if let Some(ref rdir) = *self.root_dir.lock() {
                                        let _ = crate::settings::save_setting_key(
                                            rdir,
                                            "twitch_access_token",
                                            serde_json::Value::String(new_tok.access_token.clone()),
                                        );
                                        if let Some(ref new_r) = new_tok.refresh_token {
                                            let _ = crate::settings::save_setting_key(
                                                rdir,
                                                "twitch_refresh_token",
                                                serde_json::Value::String(new_r.clone()),
                                            );
                                        }
                                    }
                                    if let Some(ref handle) = app_handle {
                                        let _ = handle.emit("twitch_token_refreshed", &new_tok);
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
    async fn test_live_twitch_connect() {
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
}
