//! DuckDuckGo HTML 搜索引擎
//!
//! 通过解析 html.duckduckgo.com 的搜索结果页面获取结果。
//! 免费、无需 API Key、支持中英文。

use async_trait::async_trait;
use scraper::{Html, Selector};

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct DuckDuckGoEngine {
    client: reqwest::Client,
}

impl DuckDuckGoEngine {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

#[async_trait]
impl SearchEngine for DuckDuckGoEngine {
    fn name(&self) -> &str {
        "duckduckgo"
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            url_encode(query)
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| SearchError::Network(e.to_string()))?;

        let html = resp
            .text()
            .await
            .map_err(|e| SearchError::Network(e.to_string()))?;

        parse_ddg_html(&html, max_results)
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

/// URL 解码
fn url_decode(s: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    Some(result)
}

fn parse_ddg_html(html: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
    let document = Html::parse_document(html);

    // DDG HTML 结果结构：
    // <div class="result">
    //   <a class="result__a" href="//duckduckgo.com/l/?uddg=编码URL">标题</a>
    //   <a class="result__snippet">摘要</a>
    // </div>
    let result_sel = Selector::parse(".result").unwrap();
    let title_sel = Selector::parse(".result__a").unwrap();
    let snippet_sel = Selector::parse(".result__snippet").unwrap();

    let mut results = Vec::new();

    for element in document.select(&result_sel) {
        if results.len() >= max_results {
            break;
        }

        let title = element
            .select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default()
            .trim()
            .to_string();

        let raw_href = element
            .select(&title_sel)
            .next()
            .and_then(|e| e.value().attr("href"))
            .unwrap_or("");

        let url = extract_real_url(raw_href);

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
                source: "duckduckgo".to_string(),
            });
        }
    }

    if results.is_empty() {
        // 备用解析：直接找所有 .result__a 链接
        let link_sel = Selector::parse("a.result__a").unwrap();
        for link in document.select(&link_sel) {
            if results.len() >= max_results {
                break;
            }
            let title = link.text().collect::<String>().trim().to_string();
            let raw_href = link.value().attr("href").unwrap_or("");
            let url = extract_real_url(raw_href);
            if !title.is_empty() && !url.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet: String::new(),
                    source: "duckduckgo".to_string(),
                });
            }
        }
    }

    if results.is_empty() {
        return Err(SearchError::NoResults);
    }

    Ok(results)
}

/// 从 DDG 的重定向 URL 中提取真实 URL
fn extract_real_url(href: &str) -> String {
    // 格式1: //duckduckgo.com/l/?uddg=编码URL&rut=xxx
    if let Some(start) = href.find("uddg=") {
        let encoded = &href[start + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        if let Some(decoded) = url_decode(&encoded[..end]) {
            return decoded;
        }
    }
    // 格式2: 直接是 URL
    if href.starts_with("http") {
        return href.to_string();
    }
    // 格式3: //开头
    if href.starts_with("//") {
        return format!("https:{}", href);
    }
    String::new()
}
