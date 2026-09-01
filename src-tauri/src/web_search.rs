use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResponse {
    pub query: String,
    pub results: Vec<SearchResultItem>,
    pub summary_text: String,
}

pub struct WebSearchClient {
    client: reqwest::Client,
}

impl WebSearchClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Brave Search API による Web 検索
    pub async fn search_brave(&self, query: &str, api_key: &str, count: usize) -> Result<Vec<SearchResultItem>, String> {
        if api_key.trim().is_empty() {
            return Err("BRAVE_API_KEY is not set".to_string());
        }

        let url = "https://api.search.brave.com/res/v1/web/search";
        let res = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
            .query(&[("q", query), ("count", &count.to_string())])
            .send()
            .await
            .map_err(|e| format!("Brave search request failed: {}", e))?;

        if !res.status().is_success() {
            return Err(format!("Brave search API returned status: {}", res.status()));
        }

        let data: serde_json::Value = res
            .json()
            .await
            .map_err(|e| format!("Failed to parse Brave search response: {}", e))?;

        let mut items = Vec::new();
        if let Some(results) = data.get("web").and_then(|w| w.get("results")).and_then(|r| r.as_array()) {
            for item in results {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let description = item.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();

                if !title.is_empty() && !url.is_empty() {
                    items.push(SearchResultItem {
                        title,
                        url,
                        description,
                    });
                }
            }
        }

        Ok(items)
    }

    /// DuckDuckGo Instant Answer API フォールバック
    pub async fn search_duckduckgo(&self, query: &str) -> Result<Vec<SearchResultItem>, String> {
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(query)
        );

        let res = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("DuckDuckGo request failed: {}", e))?;

        let data: serde_json::Value = res
            .json()
            .await
            .map_err(|e| format!("Failed to parse DuckDuckGo response: {}", e))?;

        let mut items = Vec::new();
        if let Some(abstract_text) = data.get("AbstractText").and_then(|v| v.as_str()) {
            if !abstract_text.is_empty() {
                let title = data.get("Heading").and_then(|v| v.as_str()).unwrap_or(query).to_string();
                let url = data.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or("").to_string();
                items.push(SearchResultItem {
                    title,
                    url,
                    description: abstract_text.to_string(),
                });
            }
        }

        if let Some(related) = data.get("RelatedTopics").and_then(|r| r.as_array()) {
            for topic in related.iter().take(5) {
                if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                    let first_url = topic.get("FirstURL").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    items.push(SearchResultItem {
                        title: text.chars().take(40).collect(),
                        url: first_url,
                        description: text.to_string(),
                    });
                }
            }
        }

        Ok(items)
    }

    /// クエリに応じた統合検索と要約文字列の生成
    pub async fn search_and_format(&self, query: &str, brave_api_key: &str) -> WebSearchResponse {
        let clean_query = query.trim();
        let results = if !brave_api_key.is_empty() {
            self.search_brave(clean_query, brave_api_key, 5)
                .await
                .unwrap_or_else(|_| Vec::new())
        } else {
            self.search_duckduckgo(clean_query)
                .await
                .unwrap_or_else(|_| Vec::new())
        };

        let mut summary_lines = Vec::new();
        for (i, r) in results.iter().enumerate() {
            summary_lines.push(format!("{}. 【{}】\n   {}\n   URL: {}", i + 1, r.title, r.description, r.url));
        }

        let summary_text = if summary_lines.is_empty() {
            "関連するWeb検索結果は見つかりませんでした。".to_string()
        } else {
            format!("### Web検索結果: {}\n\n{}", clean_query, summary_lines.join("\n\n"))
        };

        WebSearchResponse {
            query: clean_query.to_string(),
            results,
            summary_text,
        }
    }
}

