//! DuckDuckGo HTML 搜索引擎
//!
//! 通过解析 html.duckduckgo.com 的搜索结果页面获取结果。
//! 免费、无需 API Key、支持中英文。
//!
//! 使用三级降级解析策略，适应 DDG HTML 结构的变化。

use async_trait::async_trait;
use reqwest::Proxy;
use scraper::{Html, Selector};

use crate::tools::search::{SearchEngine, SearchError, SearchResult};

pub struct DuckDuckGoEngine {
    client: reqwest::Client,
}

impl DuckDuckGoEngine {
    pub fn new(proxy: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(15));
        if let Some(proxy_url) = proxy {
            if let Ok(p) = Proxy::all(proxy_url) {
                builder = builder.proxy(p);
            } else {
                tracing::warn!("DuckDuckGoEngine: 代理配置无效: {}", proxy_url);
            }
        }
        let client = builder.build().unwrap_or_default();
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

/// 三级降级解析 DDG HTML
fn parse_ddg_html(html: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
    // 反爬检测：先检查是否被拦截
    if is_captcha_page(html) {
        return Err(SearchError::Network(
            "DuckDuckGo 反爬拦截，请切换搜索引擎或稍后重试".into(),
        ));
    }

    // 策略1: class="result"（旧版稳定结构）
    let mut results = parse_class_result(html, max_results);
    if !results.is_empty() {
        return Ok(results);
    }

    // 策略2: 新结构 — <a> 在 article / div 内
    results = parse_generic_links(html, max_results);
    if !results.is_empty() {
        return Ok(results);
    }

    // 策略3: 纯文本回退 — 提取所有 http/https URL + 周围文本
    results = parse_plain_text_fallback(html, max_results);
    if !results.is_empty() {
        return Ok(results);
    }

    // 如果 HTML 异常短且无结果，也判为反爬
    if html.len() < 500 {
        return Err(SearchError::Network(
            "DuckDuckGo 返回空页面（可能被反爬拦截）".into(),
        ));
    }

    Err(SearchError::NoResults)
}

/// 检测 DDG 是否返回了反爬页面
fn is_captcha_page(html: &str) -> bool {
    let lower = html.to_lowercase();
    lower.contains("captcha")
        || lower.contains("challenge")
        || lower.contains("blocked")
        || lower.contains("rate limit")
        || lower.contains("automated query")
        || lower.contains("automated request")
        || lower.contains("please try again later")
        || lower.contains("your request has been blocked")
}

/// 策略1: 解析 class="result"（旧版稳定结构）
fn parse_class_result(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);

    let result_sel = match Selector::parse(".result") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let title_sel = match Selector::parse(".result__title a, .result__a, a.result__a") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let snippet_sel = match Selector::parse(".result__snippet, .snippet") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

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

    results
}

/// 策略2: 通用链接解析 — 找所有含 uddg= 的 a 标签
fn parse_generic_links(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);

    // 找所有内含 uddg= 的链接，这是 DDG 重定向特征
    let link_sel = match Selector::parse("a[href*='uddg='], a[rel='nofollow'], a.result__a, h2 a") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    for link in document.select(&link_sel) {
        if results.len() >= max_results {
            break;
        }
        let href = link.value().attr("href").unwrap_or("");
        if href.is_empty() || href.contains("duckduckgo.com") {
            continue;
        }
        let title = link.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }
        let url = extract_real_url(href);
        if url.is_empty() {
            continue;
        }
        results.push(SearchResult {
            title,
            url,
            snippet: String::new(),
            source: "duckduckgo".to_string(),
        });
    }

    results
}

/// 策略3: 纯文本回退 — 从 HTML 正文中提取 URL + 周围文本
fn parse_plain_text_fallback(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);
    let body_sel = match Selector::parse("body") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let a_sel = match Selector::parse("a") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let body = match document.select(&body_sel).next() {
        Some(b) => b,
        None => return Vec::new(),
    };

    let mut results = Vec::new();
    for link in body.select(&a_sel) {
        if results.len() >= max_results {
            break;
        }
        let href = link.value().attr("href").unwrap_or("");
        let url = extract_real_url(href);
        if url.is_empty() || url.contains("duckduckgo.com") {
            continue;
        }
        let title = link.text().collect::<String>().trim().to_string();
        if title.is_empty() || title.len() < 3 {
            continue;
        }
        // 提取父元素文本作为 snippet
        let snippet = link
            .parent()
            .and_then(|p| {
                let mut text = String::new();
                for child in p.children() {
                    if let Some(t) = child.value().as_text() {
                        text.push_str(t);
                    }
                }
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text.trim().to_string())
                }
            })
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url,
            snippet,
            source: "duckduckgo".to_string(),
        });
    }

    results
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
