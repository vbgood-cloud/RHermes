//! Bing HTML 搜索引擎（备用）
//!
//! 通过解析 www.bing.com/search 的搜索结果页面获取结果。
//! 免费、无需 API Key。

use async_trait::async_trait;
use reqwest::Proxy;
use scraper::{Html, Selector};

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct BingEngine {
    client: reqwest::Client,
}

impl BingEngine {
    pub fn new(proxy: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(10));
        if let Some(proxy_url) = proxy {
            if let Ok(p) = Proxy::all(proxy_url) {
                builder = builder.proxy(p);
            } else {
                tracing::warn!("BingEngine: 代理配置无效: {}", proxy_url);
            }
        }
        let client = builder.build().unwrap_or_default();
        Self { client }
    }
}

#[async_trait]
impl SearchEngine for BingEngine {
    fn name(&self) -> &str {
        "bing"
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!(
            "https://www.bing.com/search?q={}&count={}",
            url_encode(query),
            max_results.min(10)
        );

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .map_err(|e| SearchError::Network(e.to_string()))?;

        let html = resp
            .text()
            .await
            .map_err(|e| SearchError::Network(e.to_string()))?;

        parse_bing_html(&html, max_results)
    }
}

/// URL 编码
fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => result.push_str(&format!("%{:02X}", byte)),
        }
    }
    result
}

fn parse_bing_html(html: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
    let document = Html::parse_document(html);

    // Bing 结果结构：<li class="b_algo"> <h2><a href="...">标题</a></h2> <p>摘要</p>
    let algo_sel = match Selector::parse("li.b_algo, .b_algo") {
        Ok(s) => s,
        Err(_) => return Err(SearchError::Parse("选择器解析失败".into())),
    };
    let title_sel = match Selector::parse("h2 a") {
        Ok(s) => s,
        Err(_) => return Err(SearchError::Parse("选择器解析失败".into())),
    };
    let snippet_sel = match Selector::parse("p, .b_caption p") {
        Ok(s) => s,
        Err(_) => return Err(SearchError::Parse("选择器解析失败".into())),
    };

    let mut results = Vec::new();

    for element in document.select(&algo_sel) {
        if results.len() >= max_results {
            break;
        }

        let title_el = element.select(&title_sel).next();
        let title = title_el
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default()
            .trim()
            .to_string();

        let url = title_el
            .and_then(|e| e.value().attr("href"))
            .unwrap_or("")
            .to_string();

        let snippet = element
            .select(&snippet_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default()
            .trim()
            .to_string();

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
                source: "bing".to_string(),
            });
        }
    }

    if results.is_empty() {
        // 备用：直接找所有 h2 > a
        let fallback_sel = match Selector::parse("h2 a") {
            Ok(s) => s,
            Err(_) => return Err(SearchError::NoResults),
        };
        for link in document.select(&fallback_sel) {
            if results.len() >= max_results {
                break;
            }
            let href = link.value().attr("href").unwrap_or("");
            if href.is_empty() || href.contains("bing.com") {
                continue;
            }
            let title = link.text().collect::<String>().trim().to_string();
            if title.is_empty() {
                continue;
            }
            results.push(SearchResult {
                title,
                url: href.to_string(),
                snippet: String::new(),
                source: "bing".to_string(),
            });
        }
    }

    if results.is_empty() {
        return Err(SearchError::NoResults);
    }

    Ok(results)
}
