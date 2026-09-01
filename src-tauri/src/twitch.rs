use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchBotSettings {
    pub channel: String,
    pub bot_nick: String,
    pub oauth_token: String, // "oauth:xxxx"
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
        }
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
    pub async fn validate_token(&self, access_token: &str) -> Result<TwitchValidateResponse, String> {
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
            tx.send(raw).map_err(|e| format!("Failed to send chat: {}", e))?;
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
        if let Some(ref h) = app_handle {
            *self.app_handle.lock() = Some(h.clone());
        }

        let channel = settings.channel.to_lowercase();
        let chan_with_hash = if channel.starts_with('#') {
            channel.clone()
        } else {
            format!("#{}", channel)
        };

        let mut token = settings.oauth_token.trim().to_string();
        if !token.starts_with("oauth:") && !token.is_empty() {
            token = format!("oauth:{}", token);
        }

        let nick = if settings.bot_nick.trim().is_empty() {
            "justinfan12345".to_string() // 読み取り専用デフォルト
        } else {
            settings.bot_nick.trim().to_string()
        };

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

        // 認証コマンド送信
        let pass_cmd = if token.is_empty() {
            "PASS SCHMOOPIE\r\n".to_string()
        } else {
            format!("PASS {}\r\n", token)
        };
        write.send(Message::Text(pass_cmd)).await.map_err(|e| e.to_string())?;
        write.send(Message::Text(format!("NICK {}\r\n", nick))).await.map_err(|e| e.to_string())?;
        write.send(Message::Text("CAP REQ :twitch.tv/tags twitch.tv/commands\r\n".to_string())).await.map_err(|e| e.to_string())?;
        write.send(Message::Text(format!("JOIN {}\r\n", chan_with_hash))).await.map_err(|e| e.to_string())?;

        let is_connected = self.is_connected.clone();

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
        });

        // 受信タスク
        let sender_clone = self.sender.clone();
        let chan_name = channel.clone();
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

                            // PRIVMSG パース
                            if line.contains("PRIVMSG") {
                                if let Some(chat_msg) = parse_irc_privmsg(line, &chan_name) {
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
                    Ok(Message::Close(_)) | Err(_) => {
                        break;
                    }
                    _ => {}
                }
            }
            is_connected.store(false, Ordering::SeqCst);
            let _ = write_task.abort();
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

    let privmsg_idx = raw.find("PRIVMSG")?;
    let after_privmsg = &raw[privmsg_idx + 7..].trim_start();
    let (chan_part, msg_part) = after_privmsg.split_once(" :")?;

    let channel = chan_part.trim_start_matches('#').to_string();
    let content = msg_part.to_string();

    let timestamp = chrono::Local::now().to_rfc3339();

    Some(TwitchChatMessage {
        channel: if channel.is_empty() { default_channel.to_string() } else { channel },
        author,
        content,
        is_mod,
        is_subscriber,
        timestamp,
    })
}

