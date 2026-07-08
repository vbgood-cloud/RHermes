//! 搜索引擎抽象层
//!
//! 提供 SearchEngine trait + 搜索缓存 + 多引擎降级

pub mod bing;
pub mod baidu;
pub mod duckduckgo;
pub mod searxng;
pub mod serper;

use std::num::NonZeroUsize;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// SearchResult — 统一的搜索结果
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
}

// ---------------------------------------------------------------------------
// SearchEngine trait
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SearchError {
    Network(String),
    Parse(String),
    Timeout,
    NoResults,
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Network(e) => write!(f, "网络错误: {e}"),
            SearchError::Parse(e) => write!(f, "解析错误: {e}"),
            SearchError::Timeout => write!(f, "搜索超时"),
            SearchError::NoResults => write!(f, "无结果"),
        }
    }
}

#[async_trait]
pub trait SearchEngine: Send + Sync {
    fn name(&self) -> &str;
    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, SearchError>;
}

// ---------------------------------------------------------------------------
// 查询词归一化（缓存 key 使用）
// ---------------------------------------------------------------------------

fn normalize_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len());
    let mut prev_was_space = false;
    for ch in query.trim().chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            for c in ch.to_lowercase() {
                result.push(c);
            }
            prev_was_space = false;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// SearchCache — LRU + TTL 缓存
// ---------------------------------------------------------------------------

struct CacheEntry {
    formatted: String,
    timestamp: std::time::Instant,
}

pub struct SearchCache {
    entries: Mutex<lru::LruCache<String, CacheEntry>>,
    ttl: Duration,
}

impl SearchCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(lru::LruCache::new(NonZeroUsize::new(capacity).unwrap())),
            ttl,
        }
    }

    pub async fn get(&self, query: &str) -> Option<String> {
        let key = normalize_query(query);
        let mut cache = self.entries.lock().await;
        if let Some(entry) = cache.get(&key) {
            if entry.timestamp.elapsed() < self.ttl {
                return Some(entry.formatted.clone());
            }
            cache.pop(&key);
        }
        None
    }

    pub async fn put(&self, query: &str, formatted: String) {
        let key = normalize_query(query);
        let mut cache = self.entries.lock().await;
        cache.put(
            key,
            CacheEntry {
                formatted,
                timestamp: std::time::Instant::now(),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// MultiEngineSearcher — 多引擎降级 + 重试 + 速率限制
// ---------------------------------------------------------------------------

pub struct MultiEngineSearcher {
    engines: Vec<Box<dyn SearchEngine>>,
    cache: SearchCache,
    timeout: Duration,
    last_request: tokio::sync::Mutex<std::time::Instant>,
    min_interval: Duration,
}

impl MultiEngineSearcher {
    pub fn new(engines: Vec<Box<dyn SearchEngine>>, cache: SearchCache, timeout: Duration) -> Self {
        Self {
            engines,
            cache,
            timeout,
            last_request: tokio::sync::Mutex::new(std::time::Instant::now()),
            min_interval: Duration::from_secs(1),
        }
    }

    /// 带缓存、速率限制、重试和降级的搜索
    pub async fn search(&self, query: &str, max_results: usize) -> String {
        // 1. 检查缓存（用归一化后的 key）
        let cache_key = normalize_query(query);
        if let Some(cached) = self.cache.get(&cache_key).await {
            tracing::debug!("搜索缓存命中: query={}", query);
            // 如果缓存结果没有引擎标注，补一个
            if !cached.starts_with("🔍 搜索引擎") {
                return format!("🔍 搜索引擎: 缓存\n{}", cached);
            }
            return cached;
        }

        // 2. 速率限制：确保两次请求之间至少间隔 min_interval
        {
            let mut last = self.last_request.lock().await;
            let elapsed = last.elapsed();
            if elapsed < self.min_interval {
                let sleep_time = self.min_interval - elapsed;
                tracing::debug!("搜索速率限制: 等待 {}ms", sleep_time.as_millis());
                tokio::time::sleep(sleep_time).await;
            }
            *last = std::time::Instant::now();
        }

        // 3. 依次尝试引擎（每个引擎最多重试 2 次）
        for engine in &self.engines {
            if let Some(results) = self.try_engine_with_retry(engine.as_ref(), query, max_results).await {
                let engine_name = engine.name().to_string();
                let formatted = format!("🔍 搜索引擎: {}\n{}", engine_name, format_results(query, &results));
                self.cache.put(&cache_key, formatted.clone()).await;
                tracing::info!(
                    "搜索成功: engine={}, results={}",
                    engine.name(),
                    results.len()
                );
                return formatted;
            }
        }

        format!(
            "<untrusted>\n未找到与「{query}」相关的搜索结果（已尝试所有搜索引擎）\n</untrusted>"
        )
    }

    /// 尝试单个引擎，最多重试 2 次
    async fn try_engine_with_retry(
        &self,
        engine: &dyn SearchEngine,
        query: &str,
        max_results: usize,
    ) -> Option<Vec<SearchResult>> {
        let mut last_error = None;

        for attempt in 0..2 {
            match tokio::time::timeout(self.timeout, engine.search(query, max_results)).await {
                Ok(Ok(results)) if !results.is_empty() => return Some(results),
                Ok(Ok(_)) => {
                    // 空结果 — 不重试
                    tracing::warn!("搜索返回空结果: engine={}", engine.name());
                    return None;
                }
                Ok(Err(e)) => {
                    last_error = Some(e);
                    tracing::warn!(
                        "搜索失败: engine={}, attempt={}, error={:?}",
                        engine.name(),
                        attempt + 1,
                        last_error.as_ref().unwrap()
                    );
                    // 临时性错误才重试
                    if attempt == 0 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        "搜索超时: engine={}, attempt={}",
                        engine.name(),
                        attempt + 1
                    );
                    // 超时后直接重试（不 sleep，因为已经等了 timeout 秒）
                }
            }
        }

        if let Some(e) = last_error {
            tracing::warn!("搜索彻底失败: engine={}, error={}", engine.name(), e);
        }
        None
    }
}

/// 格式化搜索结果
fn format_results(query: &str, results: &[SearchResult]) -> String {
    let mut lines = vec![format!("搜索结果「{query}」：\n")];
    for (i, r) in results.iter().enumerate() {
        lines.push(format!("{}. {}", i + 1, r.title));
        if !r.snippet.is_empty() {
            lines.push(format!("   {}", r.snippet));
        }
        lines.push(format!("   链接: {}", r.url));
        if i < results.len() - 1 {
            lines.push(String::new());
        }
    }
    format!("<untrusted>\n{}\n</untrusted>", lines.join("\n"))
}
