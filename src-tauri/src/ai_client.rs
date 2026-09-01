use serde::{Deserialize, Serialize};
use std::time::Duration;

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

pub struct AiClient {
    client: reqwest::Client,
}

impl AiClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Gemini API 呼び出し (REST & 複数キーローテーション & thought除外)
    pub async fn generate_gemini(
        &self,
        api_key_str: &str,
        model: &str,
        messages: &[ChatMessage],
        options: &AiGenerateOptions,
    ) -> Result<String, String> {
        let raw_keys: Vec<&str> = api_key_str
            .split(',')
            .map(|k| k.trim().trim_matches('\'').trim_matches('"'))
            .filter(|k| !k.is_empty())
            .collect();

        if raw_keys.is_empty() {
            return Err("Gemini API key is not set".to_string());
        }

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
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                    m_name, key
                );

                let budget_info = match options.thinking_budget {
                    Some(0) => "Thinking=OFF (budget: 0)",
                    Some(b) => format!("Thinking=ON (budget: {})", b).leak(),
                    None => "Thinking=DEFAULT",
                };

                crate::logger::global_info("Gemini", &format!("Requesting model='{}', key_index={}, {}...", m_name, idx, budget_info));

                let res = self.client.post(&url).json(&body).send().await;
                match res {
                    Ok(resp) => {
                        let status_code = resp.status();
                        if status_code.is_success() {
                            let resp_json: serde_json::Value = resp
                                .json()
                                .await
                                .map_err(|e| format!("Failed to parse Gemini response: {}", e))?;

                            let mut text_result = String::new();
                            if let Some(candidates) = resp_json.get("candidates").and_then(|c| c.as_array()) {
                                if let Some(first) = candidates.first() {
                                    if let Some(parts) = first.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                                        for part in parts {
                                            // 1. 思考プロセス (thought: true) パーツを完全に除外
                                            let is_thought = part.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
                                            let has_thought_sig = part.get("thought_signature").is_some();
                                            if is_thought || has_thought_sig {
                                                continue;
                                            }

                                            // 2. 通常の回答テキストのみを抽出
                                            if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
                                                text_result.push_str(txt);
                                            }
                                        }
                                    }
                                }
                            }

                            // 3. テキスト内に混入した思考タグ (<thought>...</thought> 等) を完全除去
                            let clean = strip_thought_artifacts(&text_result);
                            if !clean.is_empty() {
                                crate::logger::global_info("Gemini", &format!("SUCCESS generation with model='{}', key_index={}: '{}'", m_name, idx, clean));
                                return Ok(clean);
                            }
                        } else {
                            let err_text = resp.text().await.unwrap_or_default();
                            crate::logger::global_warn("Gemini", &format!("Failed with status {} on model='{}', key_index={}: {}", status_code, m_name, idx, err_text));
                            last_error = format!("Gemini API Error ({}, {}): {}", m_name, status_code, err_text);
                        }
                    }
                    Err(e) => {
                        crate::logger::global_error("Gemini", &format!("HTTP request error on model='{}', key_index={}: {}", m_name, idx, e));
                        last_error = format!("HTTP request error ({}, {}): {}", m_name, idx, e);
                    }
                }
            }
        }

        Err(last_error)
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
            let err_text = resp.text().await.unwrap_or_default();
            return Err(format!("llama.cpp error: {}", err_text));
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
