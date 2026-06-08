//! Serper (Google Search API) 搜索引擎
//!
//! 需要配置 search.serper_api_key
//! API: https://google.serper.dev/search

use async_trait::async_trait;
use reqwest::Proxy;

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct SerperEngine {
    client: reqwest::Client,
    api_key: String,
}

impl SerperEngine {
    pub fn new(api_key: String, proxy: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10));
        if let Some(proxy_url) = proxy {
            if let Ok(p) = Proxy::all(proxy_url) {
                builder = builder.proxy(p);
            } else {
                tracing::warn!("SerperEngine: 代理配置无效: {}", proxy_url);
            }
        }
        let client = builder.build().unwrap_or_default();
        Self { client, api_key }
    }
}

#[async_trait]
impl SearchEngine for SerperEngine {
    fn name(&self) -> &str {
        "serper"
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
        let body_json = serde_json::json!({
            "q": query,
            "num": max_results.min(20),
        });

        let resp = self
            .client
            .post("https://google.serper.dev/search")
            .header("X-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&body_json).unwrap_or_default())
            .send()
            .await
            .map_err(|e| SearchError::Network(e.to_string()))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SearchError::Parse(e.to_string()))?;

        let mut results = Vec::new();

        // Knowledge Graph（如果有）
        if let Some(kg) = body["knowledgeGraph"].as_object() {
            if let Some(desc) = kg.get("description").and_then(|d| d.as_str()) {
                results.push(SearchResult {
                    title: kg.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                    url: kg
                        .get("descriptionLink")
                        .and_then(|l| l.as_str())
                        .unwrap_or("")
                        .to_string(),
                    snippet: desc.to_string(),
                    source: "serper-kg".to_string(),
                });
            }
        }

        // Organic results
        if let Some(organic) = body["organic"].as_array() {
            for item in organic.iter() {
                if results.len() >= max_results {
                    break;
                }
                results.push(SearchResult {
                    title: item["title"].as_str().unwrap_or_default().to_string(),
                    url: item["link"].as_str().unwrap_or_default().to_string(),
                    snippet: item["snippet"].as_str().unwrap_or_default().to_string(),
                    source: "serper".to_string(),
                });
            }
        }

        if results.is_empty() {
            return Err(SearchError::NoResults);
        }

        Ok(results)
    }
}
