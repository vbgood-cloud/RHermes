//! 搜索引擎抽象层
//!
//! 提供 SearchEngine trait + 搜索缓存 + 多引擎降级

pub mod duckduckgo;
pub mod serper;

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

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
// SearchCache — LRU + TTL 缓存
// ---------------------------------------------------------------------------

struct CacheEntry {
    formatted: String,
    timestamp: Instant,
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

    pub fn get(&self, query: &str) -> Option<String> {
        let mut cache = self.entries.lock().ok()?;
        if let Some(entry) = cache.get(&query.to_lowercase()) {
            if entry.timestamp.elapsed() < self.ttl {
                return Some(entry.formatted.clone());
            }
            cache.pop(&query.to_lowercase());
        }
        None
    }

    pub fn put(&self, query: &str, formatted: String) {
        if let Ok(mut cache) = self.entries.lock() {
            cache.put(
                query.to_lowercase(),
                CacheEntry {
                    formatted,
                    timestamp: Instant::now(),
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// MultiEngineSearcher — 多引擎降级
// ---------------------------------------------------------------------------

pub struct MultiEngineSearcher {
    engines: Vec<Box<dyn SearchEngine>>,
    cache: SearchCache,
    timeout: Duration,
}

impl MultiEngineSearcher {
    pub fn new(engines: Vec<Box<dyn SearchEngine>>, cache: SearchCache, timeout: Duration) -> Self {
        Self {
            engines,
            cache,
            timeout,
        }
    }

    /// 带缓存和降级的搜索
    pub async fn search(&self, query: &str, max_results: usize) -> String {
        // 1. 检查缓存
        if let Some(cached) = self.cache.get(query) {
            tracing::debug!("搜索缓存命中: query={}", query);
            return cached;
        }

        // 2. 依次尝试引擎
        for engine in &self.engines {
            match tokio::time::timeout(self.timeout, engine.search(query, max_results)).await {
                Ok(Ok(results)) if !results.is_empty() => {
                    let formatted = format_results(query, &results);
                    self.cache.put(query, formatted.clone());
                    tracing::info!(
                        "搜索成功: engine={}, results={}",
                        engine.name(),
                        results.len()
                    );
                    return formatted;
                }
                Ok(Ok(_)) => {
                    tracing::warn!("搜索返回空结果: engine={}", engine.name());
                    continue;
                }
                Ok(Err(e)) => {
                    tracing::warn!("搜索失败: engine={}, error={}", engine.name(), e);
                    continue;
                }
                Err(_) => {
                    tracing::warn!("搜索超时: engine={}, timeout={}s", engine.name(), self.timeout.as_secs());
                    continue;
                }
            }
        }

        format!(
            "<untrusted>\n未找到与「{query}」相关的搜索结果（已尝试所有搜索引擎）\n</untrusted>"
        )
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
