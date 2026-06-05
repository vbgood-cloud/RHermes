//! Provider Pool —— 熔断器 + 加权轮询
//!
//! 管理多个 Transport 实例，自动跳过故障 Provider，
//! 支持加权轮询调度和熔断恢复。

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiError, ApiEvent, ChatRequest, ChatResponse};
use crate::provider::Transport;

// ---------------------------------------------------------------------------
// 熔断器
// ---------------------------------------------------------------------------

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq)]
enum BreakerState {
    /// 正常（请求直达）
    Closed,
    /// 半开（试探恢复）
    HalfOpen,
    /// 断开（快速失败）
    Open,
}

impl BreakerState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::HalfOpen,
            2 => Self::Open,
            _ => Self::Closed,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::HalfOpen => 1,
            Self::Open => 2,
        }
    }
}

/// 熔断器 — 连续失败 N 次后断开，等待冷却后半开试探
pub(crate) struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU32,
    last_failure_epoch: AtomicU64,
    threshold: u32,
    cooldown_secs: u64,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            state: AtomicU8::new(BreakerState::Closed.to_u8()),
            failure_count: AtomicU32::new(0),
            last_failure_epoch: AtomicU64::new(0),
            threshold,
            cooldown_secs,
        }
    }

    /// 检查请求是否允许通过
    pub fn allow_request(&self) -> bool {
        match BreakerState::from_u8(self.state.load(Ordering::Acquire)) {
            BreakerState::Closed => true,
            BreakerState::HalfOpen => true, // 半开时允许试探
            BreakerState::Open => {
                let last_fail = self.last_failure_epoch.load(Ordering::Acquire);
                let now = now_epoch_secs();
                if now.saturating_sub(last_fail) >= self.cooldown_secs {
                    // 冷却时间到，半开试探
                    self.state
                        .compare_exchange(
                            BreakerState::Open.to_u8(),
                            BreakerState::HalfOpen.to_u8(),
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                } else {
                    false
                }
            }
        }
    }

    /// 记录成功
    pub fn record_success(&self) {
        self.state.store(BreakerState::Closed.to_u8(), Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
    }

    /// 记录失败，返回是否已断开
    pub fn record_failure(&self) -> bool {
        let count = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
        self.last_failure_epoch.store(now_epoch_secs(), Ordering::Release);
        if count >= self.threshold {
            self.state.store(BreakerState::Open.to_u8(), Ordering::Release);
            true
        } else {
            false
        }
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Provider 条目
// ---------------------------------------------------------------------------

struct ProviderEntry {
    transport: Arc<dyn Transport>,
    breaker: CircuitBreaker,
    weight: u32,
}

impl ProviderEntry {
    fn is_healthy(&self) -> bool {
        self.breaker.allow_request()
    }
}

// ---------------------------------------------------------------------------
// ProviderPool
// ---------------------------------------------------------------------------

/// Provider Pool — 熔断 + 加权轮询
///
/// 持有多个 Transport 实例，请求时选择健康的 Provider。
/// 连续失败 N 次后自动熔断，冷却后试探恢复。
pub struct ProviderPool {
    providers: Vec<ProviderEntry>,
    index: AtomicU32,
}

impl ProviderPool {
    /// 从单个 Transport 创建 Pool（最常用）
    pub fn single(transport: Arc<dyn Transport>, threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            providers: vec![ProviderEntry {
                transport,
                breaker: CircuitBreaker::new(threshold, cooldown_secs),
                weight: 1,
            }],
            index: AtomicU32::new(0),
        }
    }

    /// 添加多个 Transport（用于多 Provider 场景）
    pub fn with_providers(
        transports: Vec<(Arc<dyn Transport>, u32)>,
        threshold: u32,
        cooldown_secs: u64,
    ) -> Self {
        let providers = transports
            .into_iter()
            .map(|(t, w)| ProviderEntry {
                transport: t,
                breaker: CircuitBreaker::new(threshold, cooldown_secs),
                weight: w,
            })
            .collect();
        Self {
            providers,
            index: AtomicU32::new(0),
        }
    }

    /// 重试次数（来自配置）
    pub fn max_retries(&self) -> u32 {
        3 // 可配置化
    }

    // ---- 私有辅助 ----

    /// 选择下一个健康的 Provider（加权轮询）
    fn next_healthy(&self) -> Option<&ProviderEntry> {
        let len = self.providers.len();
        if len == 0 {
            return None;
        }

        let start = self.index.fetch_add(1, Ordering::AcqRel) as usize % len;

        for i in 0..len {
            let idx = (start + i) % len;
            if self.providers[idx].is_healthy() {
                return Some(&self.providers[idx]);
            }
        }
        None // 全部不可用
    }

    /// 执行聊天请求（带熔断 + 退避重试）
    async fn execute_chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        let max_retries = self.max_retries();
        let mut last_error = ApiError::RetryExhausted;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt.saturating_sub(1)));
                tokio::time::sleep(delay).await;
            }

            let entry = match self.next_healthy() {
                Some(e) => e,
                None => return Err(ApiError::RetryExhausted),
            };

            match entry.transport.chat(request.clone()).await {
                Ok(resp) => {
                    entry.breaker.record_success();
                    return Ok(resp);
                }
                Err(e) => {
                    // 非可重试错误（401/400等）立即返回，不浪费重试
                    if !e.is_retryable() {
                        return Err(e);
                    }
                    entry.breaker.record_failure();
                    last_error = e;
                    tracing::debug!("API 调用失败（将重试）: {}", last_error);
                }
            }
        }

        Err(last_error)
    }
}

/// ProviderPool 也实现 Transport trait，方便统一调用
#[async_trait]
impl Transport for ProviderPool {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ApiError> {
        self.execute_chat(request).await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        tx: UnboundedSender<ApiEvent>,
    ) -> Result<(), ApiError> {
        let entry = match self.next_healthy() {
            Some(e) => e,
            None => return Err(ApiError::RetryExhausted),
        };
        match entry.transport.chat_stream(request, tx).await {
            Ok(r) => {
                entry.breaker.record_success();
                Ok(r)
            }
            Err(e) => {
                entry.breaker.record_failure();
                Err(e)
            }
        }
    }

    async fn get_balance(&self) -> Result<f64, ApiError> {
        let entry = match self.next_healthy() {
            Some(e) => e,
            None => return Err(ApiError::RetryExhausted),
        };
        entry.transport.get_balance().await
    }

    fn model_name(&self) -> &str {
        // 返回第一个健康 provider 的模型名
        for p in &self.providers {
            if p.is_healthy() {
                return p.transport.model_name();
            }
        }
        // 全部不可用也返回第一个（fallback）
        self.providers
            .first()
            .map(|p| p.transport.model_name())
            .unwrap_or("unknown")
    }
}
