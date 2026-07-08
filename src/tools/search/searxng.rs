//! SearXNG 搜索引擎
//!
//! 优先使用 JSON API（format=json），失败时回退到 HTML 抓取。
//! 用户可自部署或使用公共实例（如 https://searx.be）。

use std::time::Duration;

use async_trait::async_trait;

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct SearXngEngine {
    client: reqwest::Client,
    /// SearXNG 实例地址（如 https://searx.be）
    base_url: String,
}

impl SearXngEngine {
    pub fn new(base_url: &str, proxy: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/125.0");

        if let Some(p) = proxy {
            if let Ok(proxy) = reqwest::Proxy::all(p) {
                builder = builder.proxy(proxy);
            }
        }

        Self {
            client: builder.build().unwrap_or_else(|_| reqwest::Client::new()),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// 尝试 JSON API
    async fn search_json(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!(
            "{}/search?q={}&format=json&categories=general",
            self.base_url,
            urlencoding(query)
        );

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| SearchError::Network(format!("{e}")))?;

        if !resp.status().is_success() {
            return Err(SearchError::Network(format!(
                "SearXNG JSON 返回 {}",
                resp.status()
            )));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SearchError::Parse(format!("{e}")))?;

        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .take(max_results)
                    .filter_map(|item| {
                        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                        let url = item.get("url").and_then(|u| u.as_str()).unwrap_or_default().to_string();
                        let snippet = item.get("content").and_then(|c| c.as_str()).unwrap_or_default().to_string();
                        if url.is_empty() && title.is_empty() { None } else {
                            Some(SearchResult { title, url, snippet, source: "searxng".to_string() })
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if results.is_empty() {
            Err(SearchError::NoResults)
        } else {
            Ok(results)
        }
    }

    /// HTML 抓取 fallback
    async fn search_html(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!(
            "{}/search?q={}&categories=general",
            self.base_url,
            urlencoding(query)
        );

        let resp = self
            .client
            .get(&url)
            .header("Accept", "text/html")
            .send()
            .await
            .map_err(|e| SearchError::Network(format!("{e}")))?;

        if !resp.status().is_success() {
            return Err(SearchError::Network(format!(
                "SearXNG HTML 返回 {}",
                resp.status()
            )));
        }

        let html = resp
            .text()
            .await
            .map_err(|e| SearchError::Parse(format!("{e}")))?;

        if html.len() < 500 {
            return Err(SearchError::NoResults);
        }

        let results = parse_searxng_html(&html, max_results);

        if results.is_empty() {
            Err(SearchError::NoResults)
        } else {
            Ok(results)
        }
    }
}

const NAME: &str = "searxng";

#[async_trait]
impl SearchEngine for SearXngEngine {
    fn name(&self) -> &str { NAME }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        // 优先尝试 JSON API
        match self.search_json(query, max_results).await {
            Ok(results) => Ok(results),
            Err(json_err) => {
                tracing::debug!("SearXNG JSON 失败 ({}), 尝试 HTML 抓取", json_err);
                // JSON 失败 → 回退 HTML 抓取
                self.search_html(query, max_results).await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTML 解析
// ---------------------------------------------------------------------------

/// 解析 SearXNG HTML 搜索结果
///
/// SearXNG 的 HTML 结构：
/// <article class="result">
///   <h3><a href="https://..." class="url_header">标题</a></h3>
///   <p class="content">摘要</p>
/// </article>
fn parse_searxng_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // 策略 1: 解析 <article> 容器
    for block in html.split("<article").skip(1) {
        if results.len() >= max_results {
            break;
        }

        let (title, url) = extract_link(block);
        if url.is_empty() || title.is_empty() {
            continue;
        }

        let snippet = extract_text_by_class(block, &["content", "snippet"]);

        results.push(SearchResult {
            title,
            url,
            snippet,
            source: "searxng".to_string(),
        });
    }

    // 策略 2: 泛用 <h3> + <a> 解析
    if results.is_empty() {
        for segment in html.split("<h3").skip(1) {
            if results.len() >= max_results {
                break;
            }
            let (title, url) = extract_link(segment);
            if !url.is_empty() && !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet: String::new(),
                    source: "searxng".to_string(),
                });
            }
        }
    }

    // 策略 3: 最泛用 — 所有带 href 的 <a> 标签
    if results.is_empty() {
        for segment in html.split("<a ").skip(1) {
            if results.len() >= max_results {
                break;
            }
            let href = extract_attr(segment, "href");
            if href.is_empty() || href.starts_with('#') || href.starts_with('/') {
                continue;
            }
            let title = extract_text_after_tag(segment, 200);
            if !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url: href,
                    snippet: String::new(),
                    source: "searxng".to_string(),
                });
            }
        }
    }

    results
}

/// 从 HTML 片段中提取第一个 <a href="..."> 的链接和文本
fn extract_link(html: &str) -> (String, String) {
    let url = extract_attr(html, "href");
    let title = extract_text_after_tag(html, 200);
    (title, url)
}

/// 提取 HTML 属性值（如 href="..."）
fn extract_attr(html: &str, attr: &str) -> String {
    let pattern = format!("{attr}=\"");
    html.find(&pattern)
        .and_then(|pos| {
            let rest = &html[pos + pattern.len()..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .unwrap_or_default()
}

/// 提取标签后的纯文本（截取前 max_chars 字符）
fn extract_text_after_tag(html: &str, max_chars: usize) -> String {
    html.find('>')
        .and_then(|gt| {
            let after = &html[gt + 1..];
            if let Some(end) = after.find("</a>") {
                Some(strip_html_tags(&after[..end]))
            } else {
                Some(strip_html_tags(&after[..after.len().min(max_chars)]))
            }
        })
        .unwrap_or_default()
}

/// 按 CSS class 提取文本（在 div/p/span 中查找）
fn extract_text_by_class(html: &str, classes: &[&str]) -> String {
    for class_name in classes {
        let pattern = format!("class=\"{}\"", class_name);
        if let Some(pos) = html.find(&pattern) {
            let rest = &html[pos..];
            // 找下一个 > 开始读取内容
            if let Some(gt) = rest.find('>') {
                let after = &rest[gt + 1..];
                // 找匹配的闭合标签
                if let Some(end) = after.find("</div>").or_else(|| after.find("</p>")).or_else(|| after.find("</span>")) {
                    let text = strip_html_tags(&after[..end]);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// 去除 HTML 标签，保留纯文本
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn urlencoding(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}
