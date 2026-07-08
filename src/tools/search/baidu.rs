//! 百度搜索引擎
//!
//! 通过抓取百度搜索 HTML 页面解析结果。

use std::time::Duration;

use async_trait::async_trait;

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct BaiduEngine {
    client: reqwest::Client,
}

impl BaiduEngine {
    pub fn new(proxy: Option<&str>) -> Self {
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
        }
    }
}

const NAME: &str = "baidu";

#[async_trait]
impl SearchEngine for BaiduEngine {
    fn name(&self) -> &str { NAME }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!(
            "https://www.baidu.com/s?wd={}&rn={}",
            urlencoding(query),
            max_results.min(10)
        );

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .map_err(|e| SearchError::Network(format!("{e}")))?;

        if !resp.status().is_success() {
            return Err(SearchError::Network(format!(
                "百度返回 {}",
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

        let results = parse_baidu_html(&html, max_results);

        if results.is_empty() {
            Err(SearchError::NoResults)
        } else {
            Ok(results)
        }
    }
}

/// 解析百度搜索结果 HTML
fn parse_baidu_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // 策略 1: 解析 class="result" 的容器
    for block in html.split("class=\"result").skip(1) {
        if results.len() >= max_results {
            break;
        }

        let (title, url) = extract_title_and_url(block);
        if url.is_empty() || title.is_empty() {
            continue;
        }

        let snippet = extract_snippet(block);

        results.push(SearchResult {
            title,
            url,
            snippet,
            source: "baidu".to_string(),
        });
    }

    // 策略 2: 泛用 <h3> 标签解析
    if results.is_empty() {
        for segment in html.split("<h3").skip(1) {
            if results.len() >= max_results {
                break;
            }
            let (title, url) = extract_title_and_url(segment);
            if !url.is_empty() && !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet: String::new(),
                    source: "baidu".to_string(),
                });
            }
        }
    }

    results
}

/// 从 HTML 片段中提取标题和 URL
fn extract_title_and_url(html: &str) -> (String, String) {
    let url = html
        .find("href=\"")
        .and_then(|pos| {
            let rest = &html[pos + 6..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .unwrap_or_default();

    let title = html
        .find("<a ")
        .and_then(|pos| {
            let rest = &html[pos..];
            rest.find('>').map(|gt| {
                let after = &rest[gt + 1..];
                if let Some(end) = after.find("</a>") {
                    strip_html_tags(&after[..end])
                } else {
                    strip_html_tags(&after[..after.len().min(200)])
                }
            })
        })
        .unwrap_or_default();

    (title, url)
}

/// 提取摘要文本
fn extract_snippet(html: &str) -> String {
    let markers = ["c-abstract", "content-right", "c-span-last"];
    for marker in &markers {
        if let Some(pos) = html.find(marker) {
            let rest = &html[pos..];
            if let Some(end) = rest.find("</div>").or_else(|| rest.find("</span>")) {
                let text = strip_html_tags(&rest[..end]);
                if !text.trim().is_empty() {
                    return text.trim().to_string();
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
