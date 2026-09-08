use serde::{Deserialize, Serialize};
use std::time::Duration;

const GEMINI_DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";
const GEMINI_MAX_ATTEMPTS: usize = 3;
const GEMINI_RETRY_DELAY: Duration = Duration::from_millis(100);
const GEMINI_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiKeyError {
    Missing,
    Invalid,
}

impl GeminiKeyError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Missing => "api_key_missing",
            Self::Invalid => "api_key_invalid",
        }
    }
}

fn parse_gemini_api_keys(api_key_str: &str) -> Result<Vec<String>, GeminiKeyError> {
    if api_key_str.trim().is_empty() {
        return Err(GeminiKeyError::Missing);
    }

    let pieces: Vec<&str> = api_key_str.split(',').map(str::trim).collect();
    if pieces.iter().all(|piece| piece.is_empty()) {
        return Err(GeminiKeyError::Missing);
    }

    let mut keys = Vec::with_capacity(pieces.len());
    for piece in pieces {
        if piece.is_empty() {
            return Err(GeminiKeyError::Invalid);
        }
        let quoted = piece
            .chars()
            .next()
            .is_some_and(|character| matches!(character, '\'' | '"'))
            || piece
                .chars()
                .last()
                .is_some_and(|character| matches!(character, '\'' | '"'));
        let key = if quoted {
            let first = piece.chars().next();
            let last = piece.chars().last();
            if first != last || !matches!(first, Some('\'' | '"')) || piece.len() < 2 {
                return Err(GeminiKeyError::Invalid);
            }
            &piece[1..piece.len() - 1]
        } else {
            piece
        };
        if key.is_empty()
            || key.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '\'' | '"')
            })
            || key.eq_ignore_ascii_case("null")
            || key.eq_ignore_ascii_case("undefined")
        {
            return Err(GeminiKeyError::Invalid);
        }
        keys.push(key.to_string());
    }
    if keys.is_empty() {
        Err(GeminiKeyError::Missing)
    } else {
        Ok(keys)
    }
}

pub fn validate_gemini_api_key(api_key_str: &str) -> Result<(), GeminiKeyError> {
    parse_gemini_api_keys(api_key_str).map(|_| ())
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "model" | "assistant" | "system"
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiGenerateOptions {
    pub system_instruction: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<i32>,
    pub image_base64: Option<String>, // スクリーンショットや画像解析用
    pub thinking_budget: Option<i32>, // 思考モードトークン上限 (0 = オフ)
}

/// 思考プロセスのタグ・残骸を除去するヘルパー関数
fn strip_thought_artifacts(text: &str) -> String {
    let mut cleaned = text.to_string();

    // 1. <thought>...</thought> タグ除去
    while let Some(start) = cleaned.find("<thought>") {
        if let Some(end) = cleaned[start..].find("</thought>") {
            cleaned.replace_range(start..start + end + 10, "");
        } else {
            cleaned.truncate(start);
            break;
        }
    }

    // 2. *thought*...*thought* 除去
    while let Some(start) = cleaned.find("*thought*") {
        if let Some(end) = cleaned[start + 9..].find("*thought*") {
            cleaned.replace_range(start..start + 9 + end + 9, "");
        } else {
            cleaned.truncate(start);
            break;
        }
    }

    // 3. ```thought ... ``` 除去
    while let Some(start) = cleaned.find("```thought") {
        if let Some(end) = cleaned[start + 10..].find("```") {
            cleaned.replace_range(start..start + 10 + end + 3, "");
        } else {
            cleaned.truncate(start);
            break;
        }
    }

    cleaned.trim().to_string()
}

/// Remove API-key query values from transport errors before they reach logs
/// or user-facing error strings.  The Gemini client now authenticates via a
/// header, but keeping this guard makes the error path safe if a proxy or a
/// future endpoint reintroduces a `?key=` URL.
fn sanitize_transport_error(message: &str) -> String {
    let mut sanitized = message.to_string();
    for marker in ["?key=", "&key="] {
        let mut search_from = 0usize;
        while let Some(relative_start) = sanitized[search_from..].find(marker) {
            let value_start = search_from + relative_start + marker.len();
            let value_end = sanitized[value_start..]
                .char_indices()
                .find(|(_, character)| {
                    matches!(*character, '&' | ')' | ' ' | '\"' | '\'' | '\n' | '\r')
                })
                .map(|(offset, _)| value_start + offset)
                .unwrap_or(sanitized.len());
            sanitized.replace_range(value_start..value_end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        }
    }
    sanitized
}

pub struct AiClient {
    client: reqwest::Client,
    endpoint_base: String,
}

impl Default for AiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AiClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(GEMINI_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            client,
            endpoint_base: GEMINI_DEFAULT_ENDPOINT.to_string(),
        }
    }

    #[cfg(test)]
    fn with_endpoint(endpoint_base: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default();
        Self {
            client,
            endpoint_base,
        }
    }

    /// Gemini API 呼び出し (REST & 複数キーローテーション & thought除外)
    pub async fn generate_gemini(
        &self,
        api_key_str: &str,
        model: &str,
        messages: &[ChatMessage],
        options: &AiGenerateOptions,
    ) -> Result<String, String> {
        let raw_keys = parse_gemini_api_keys(api_key_str).map_err(|error| match error {
            GeminiKeyError::Missing => "Gemini API key is not set".to_string(),
            GeminiKeyError::Invalid => "Gemini API key configuration is invalid".to_string(),
        })?;

        let model_name = if model.trim().is_empty() {
            "gemini-3.7-flash"
        } else {
            model.trim()
        };

        // Contents 構築
        let mut contents = Vec::new();
        for msg in messages {
            let role = if msg.role == "assistant" || msg.role == "model" {
                "model"
            } else {
                "user"
            };

            let parts = vec![serde_json::json!({
                "text": msg.content
            })];

            contents.push(serde_json::json!({
                "role": role,
                "parts": parts
            }));
        }

        // 画像が指定されている場合
        if let Some(ref b64) = options.image_base64 {
            let clean_b64 = if let Some(idx) = b64.find("base64,") {
                &b64[idx + 7..]
            } else {
                b64.as_str()
            };

            let mime_type = if b64.starts_with("data:image/png") {
                "image/png"
            } else {
                "image/jpeg"
            };

            let img_part = serde_json::json!({
                "inline_data": {
                    "mime_type": mime_type,
                    "data": clean_b64
                }
            });

            if let Some(last) = contents.last_mut() {
                if let Some(parts_arr) = last.get_mut("parts").and_then(|p| p.as_array_mut()) {
                    parts_arr.push(img_part);
                }
            } else {
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [img_part]
                }));
            }
        }

        let mut body = serde_json::json!({
            "contents": contents
        });

        if let Some(ref sys) = options.system_instruction {
            if !sys.is_empty() {
                body["system_instruction"] = serde_json::json!({
                    "parts": [{ "text": sys }]
                });
            }
        }

        let mut gen_config = serde_json::Map::new();
        if let Some(temp) = options.temperature {
            gen_config.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(max_tokens) = options.max_output_tokens {
            gen_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
        }
        if let Some(budget) = options.thinking_budget {
            gen_config.insert(
                "thinkingConfig".to_string(),
                serde_json::json!({
                    "thinkingBudget": budget
                }),
            );
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(gen_config);
        }

        // モデルフォールバックリスト（指定モデルを最優先、次に2.0-flash、1.5-flash、2.5-flash）
        let mut model_candidates = vec![model_name];
        if !model_candidates.contains(&"gemini-2.0-flash") {
            model_candidates.push("gemini-2.0-flash");
        }
        if !model_candidates.contains(&"gemini-2.5-flash") {
            model_candidates.push("gemini-2.5-flash");
        }
        if !model_candidates.contains(&"gemini-1.5-flash") {
            model_candidates.push("gemini-1.5-flash");
        }

        let mut last_error = String::new();

        for m_name in &model_candidates {
            for (idx, key) in raw_keys.iter().enumerate() {
                // Keep credentials out of the request URL.  Besides being
                // safer for proxies and tracing middleware, this prevents a
                // reqwest transport error from echoing the API key via its
                // `Display` implementation.  Gemini accepts the
                // `x-goog-api-key` header for API-key authentication.
                let url = format!(
                    "{}/models/{}:generateContent",
                    self.endpoint_base.trim_end_matches('/'),
                    m_name
                );

                let budget_info = match options.thinking_budget {
                    Some(0) => "Thinking=OFF (budget: 0)".to_string(),
                    Some(b) => format!("Thinking=ON (budget: {})", b),
                    None => "Thinking=DEFAULT".to_string(),
                };

                crate::logger::global_info(
                    "Gemini",
                    &format!(
                        "Requesting model='{}', key_index={}, {}...",
                        m_name, idx, budget_info
                    ),
                );

                for attempt in 0..GEMINI_MAX_ATTEMPTS {
                    let should_retry;
                    let res = self
                        .client
                        .post(&url)
                        .header("x-goog-api-key", key)
                        .json(&body)
                        .send()
                        .await;
                    match res {
                        Ok(resp) => {
                            let status_code = resp.status();
                            if status_code.is_success() {
                                if let Ok(resp_json) = resp.json::<serde_json::Value>().await {
                                    let mut text_result = String::new();
                                    if let Some(candidates) =
                                        resp_json.get("candidates").and_then(|c| c.as_array())
                                    {
                                        if let Some(first) = candidates.first() {
                                            if let Some(parts) = first
                                                .get("content")
                                                .and_then(|c| c.get("parts"))
                                                .and_then(|p| p.as_array())
                                            {
                                                for part in parts {
                                                    // 1. 思考プロセス (thought: true) パーツを完全に除外
                                                    let is_thought = part
                                                        .get("thought")
                                                        .and_then(|t| t.as_bool())
                                                        .unwrap_or(false);
                                                    let has_thought_sig =
                                                        part.get("thought_signature").is_some();
                                                    if is_thought || has_thought_sig {
                                                        continue;
                                                    }

                                                    // 2. 通常の回答テキストのみを抽出
                                                    if let Some(txt) =
                                                        part.get("text").and_then(|t| t.as_str())
                                                    {
                                                        text_result.push_str(txt);
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // 3. テキスト内に混入した思考タグ (<thought>...</thought> 等) を完全除去
                                    let clean = strip_thought_artifacts(&text_result);
                                    if !clean.is_empty() {
                                        crate::logger::global_info(
                                            "Gemini",
                                            &format!(
                                                "Generation succeeded model='{}', key_index={}, attempt={}",
                                                m_name,
                                                idx,
                                                attempt + 1
                                            ),
                                        );
                                        return Ok(clean);
                                    }
                                    last_error = format!(
                                        "Gemini API returned an empty response ({}, {})",
                                        m_name, idx
                                    );
                                } else {
                                    last_error = format!(
                                        "Failed to parse Gemini response ({}, {})",
                                        m_name, idx
                                    );
                                }
                                should_retry = true;
                            } else {
                                crate::logger::global_warn(
                                    "Gemini",
                                    &format!(
                                        "Generation failed model='{}', key_index={}, attempt={}, status={}",
                                        m_name,
                                        idx,
                                        attempt + 1,
                                        status_code
                                    ),
                                );
                                last_error =
                                    format!("Gemini API error ({}, {})", m_name, status_code);
                                should_retry = retryable_status(status_code);
                            }
                        }
                        Err(error) => {
                            let safe_error = sanitize_transport_error(&error.to_string());
                            crate::logger::global_error(
                                "Gemini",
                                &format!(
                                    "HTTP request error on model='{}', key_index={}, attempt={}: {}",
                                    m_name,
                                    idx,
                                    attempt + 1,
                                    safe_error
                                ),
                            );
                            last_error =
                                format!("HTTP request error ({}, {}): {}", m_name, idx, safe_error);
                            should_retry = true;
                        }
                    }

                    if !should_retry || attempt + 1 >= GEMINI_MAX_ATTEMPTS {
                        break;
                    }
                    tokio::time::sleep(GEMINI_RETRY_DELAY * (attempt as u32 + 1)).await;
                }
            }
        }

        if last_error.is_empty() {
            last_error = "Gemini API request failed".to_string();
        }
        Err(sanitize_transport_error(&last_error))
    }

    /// llama.cpp / Local OpenAI-compatible REST API 呼び出し
    pub async fn generate_llama_cpp(
        &self,
        base_url: &str,
        messages: &[ChatMessage],
        options: &AiGenerateOptions,
    ) -> Result<String, String> {
        let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

        let mut req_messages = Vec::new();
        if let Some(ref sys) = options.system_instruction {
            if !sys.is_empty() {
                req_messages.push(serde_json::json!({
                    "role": "system",
                    "content": sys
                }));
            }
        }

        for msg in messages {
            req_messages.push(serde_json::json!({
                "role": msg.role,
                "content": msg.content
            }));
        }

        let body = serde_json::json!({
            "messages": req_messages,
            "temperature": options.temperature.unwrap_or(0.7),
            "max_tokens": options.max_output_tokens.unwrap_or(300),
            "stream": false
        });

        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("llama.cpp request error: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("llama.cpp HTTP error: {}", resp.status()));
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse llama.cpp response: {}", e))?;

        let text = resp_json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|msg| msg.get("content"))
            .and_then(|cnt| cnt.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        sanitize_transport_error, validate_gemini_api_key, AiClient, AiGenerateOptions,
        ChatMessage, GeminiKeyError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn transport_errors_redact_query_api_keys() {
        let error =
            "error sending request for url (https://example.test/path?key=secret-key&alt=1)";
        let sanitized = sanitize_transport_error(error);
        assert!(!sanitized.contains("secret-key"));
        assert!(sanitized.contains("key=[REDACTED]"));
    }

    #[test]
    fn transport_errors_without_query_keys_are_unchanged() {
        let error = "connection reset by peer";
        assert_eq!(sanitize_transport_error(error), error);
    }

    #[test]
    fn gemini_key_preflight_distinguishes_missing_and_invalid_configuration() {
        assert_eq!(
            validate_gemini_api_key(" \t\n"),
            Err(GeminiKeyError::Missing)
        );
        assert_eq!(
            validate_gemini_api_key("test key"),
            Err(GeminiKeyError::Invalid)
        );
        assert_eq!(
            validate_gemini_api_key("\"unterminated"),
            Err(GeminiKeyError::Invalid)
        );
        assert!(validate_gemini_api_key("\"test-key\"").is_ok());
    }

    #[tokio::test]
    async fn gemini_retries_bounded_transient_http_failure_without_logging_key() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_server = requests.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await;
                let request_number = requests_for_server.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if request_number == 0 {
                    ("503 Service Unavailable", String::new())
                } else {
                    (
                        "200 OK",
                        r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]}}]}"#.to_string(),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = AiClient::with_endpoint(format!("http://{}/v1beta", address));
        let result = client
            .generate_gemini(
                "test-secret-key",
                "gemini-test",
                &[ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                }],
                &AiGenerateOptions {
                    system_instruction: None,
                    temperature: None,
                    max_output_tokens: None,
                    image_base64: None,
                    thinking_budget: Some(0),
                },
            )
            .await;

        assert_eq!(result.unwrap(), "ok");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn gemini_empty_response_is_retried_and_never_returned_as_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_server = requests.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await;
                let request_number = requests_for_server.fetch_add(1, Ordering::SeqCst);
                let body = if request_number == 0 {
                    r#"{"candidates":[]}"#.to_string()
                } else {
                    r#"{"candidates":[{"content":{"parts":[{"text":"recovered"}]}}]}"#.to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = AiClient::with_endpoint(format!("http://{}/v1beta", address));
        let result = client
            .generate_gemini(
                "test-key",
                "gemini-test",
                &[ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                }],
                &AiGenerateOptions {
                    system_instruction: None,
                    temperature: None,
                    max_output_tokens: None,
                    image_base64: None,
                    thinking_budget: Some(0),
                },
            )
            .await;

        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
