//! ProviderFactory — 根据配置和模型名自动创建 Transport
//!
//! 参考 blockcell 的 factory.rs 设计。

use std::sync::Arc;

use crate::core::Config;
use crate::provider::{DeepSeekTransport, ProviderPool, Transport};

/// 默认的 API base URL 映射
pub fn default_provider_base_url(provider_name: &str) -> &'static str {
    match provider_name {
        "openai" | "openrouter" => "https://api.openai.com/v1",
        "deepseek" => "https://api.deepseek.com",
        "siliconflow" => "https://api.siliconflow.cn/v1",
        "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
        "moonshot" | "kimi" => "https://api.moonshot.cn/v1",
        "ollama" => "http://localhost:11434",
        "groq" => "https://api.groq.com/openai/v1",
        _ => "https://api.openai.com/v1",
    }
}

/// 从 model 字符串前缀推断 provider 名称
pub fn infer_provider_from_model(model: &str) -> Option<&'static str> {
    if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        Some("openai")
    } else if model.starts_with("deepseek") {
        Some("deepseek")
    } else if model.starts_with("glm-") || model.starts_with("glm") || model.starts_with("zhipu") {
        Some("zhipu")
    } else if model.starts_with("qwen") || model.starts_with("qwen-") {
        Some("qwen")
    } else if model.starts_with("moonshot") || model.starts_with("kimi") {
        Some("kimi")
    } else if model.starts_with("gemini-") {
        Some("gemini")
    } else if model.starts_with("claude-") {
        Some("anthropic")
    } else if model.starts_with("llama") || model.starts_with("mistral") {
        Some("ollama")
    } else {
        None
    }
}

/// 查找 fallback provider（第一个有 API Key 的）
fn fallback_provider_name(config: &Config) -> Option<String> {
    let priority = ["deepseek", "openai", "siliconflow", "zhipu", "moonshot", "kimi", "ollama"];
    for name in &priority {
        if let Some(p) = config.providers.get(*name) {
            if !p.api_key.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    // ollama 不需要 api_key
    if config.providers.contains_key("ollama") {
        return Some("ollama".into());
    }
    None
}

/// 从配置创建 Transport
///
/// 优先级：
/// 1. 显式 provider_name（来自 agent.default_provider 或显式指定）
/// 2. model 前缀推断
/// 3. config 中第一个有效 API Key 的 provider（fallback）
pub fn create_transport(
    config: &Config,
    model: &str,
    explicit_provider: Option<&str>,
    circuit_breaker_threshold: u32,
    circuit_breaker_cooldown_secs: u64,
) -> Result<Arc<dyn Transport>, String> {
    // 确定使用的 provider 名称
    let provider_name = if let Some(ep) = explicit_provider {
        if !ep.is_empty() { ep.to_string() } else {
            // 空字符串也尝试推断
            infer_provider_from_model(model)
                .map(|s| s.to_string())
                .or_else(|| fallback_provider_name(config))
                .unwrap_or_else(|| "deepseek".into())
        }
    } else {
        // 先尝试 model 前缀
        infer_provider_from_model(model)
            .map(|s| s.to_string())
            // 再尝试 config.agent.default_provider
            .or_else(|| {
                let dp = config.agent.default_provider.clone();
                if dp.is_empty() { None } else { Some(dp) }
            })
            // 最后 fallback
            .or_else(|| fallback_provider_name(config))
            .unwrap_or_else(|| "deepseek".into())
    };

    // 获取 provider 配置
    let provider_cfg = config.providers.get(&provider_name);

    // 构建 Transport
    let transport = match provider_name.as_str() {
        "ollama" => {
            // ollama 特殊处理
            let base_url = provider_cfg
                .and_then(|p| p.base_url.clone())
                .unwrap_or_else(|| default_provider_base_url("ollama").into());
            let api_key = provider_cfg.map(|p| p.api_key.clone()).unwrap_or_default();
            let model = provider_cfg
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| model.to_string());

            // 构建一个临时的 Config 用于 DeepSeekTransport
            let mut transport_config = config.clone();
            transport_config.api.base_url = base_url;
            transport_config.api.model = model;
            transport_config.api_key = api_key;
            Arc::new(DeepSeekTransport::new(&transport_config))
        }
        _ => {
            // OpenAI 兼容（deepseek, openai, siliconflow, zhipu 等）
            let base_url = provider_cfg
                .and_then(|p| p.base_url.clone())
                .unwrap_or_else(|| default_provider_base_url(&provider_name).into());
            let api_key = provider_cfg
                .map(|p| p.api_key.clone())
                .filter(|k| !k.is_empty())
                .unwrap_or_else(|| config.api_key.clone());
            let model = provider_cfg
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| model.to_string());

            let mut transport_config = config.clone();
            transport_config.api.base_url = base_url;
            transport_config.api.model = model;
            transport_config.api_key = api_key;
            Arc::new(DeepSeekTransport::new(&transport_config))
        }
    };

    // 包装到 ProviderPool（熔断）
    let pool = ProviderPool::single(transport, circuit_breaker_threshold, circuit_breaker_cooldown_secs);
    Ok(Arc::new(pool))
}

/// 创建主对话用的 Transport（根据 config 自动选择）
pub fn create_main_transport(
    config: &Config,
    circuit_breaker_threshold: u32,
    circuit_breaker_cooldown_secs: u64,
) -> Result<Arc<dyn Transport>, String> {
    let model = &config.api.model;
    let explicit = if config.agent.default_provider.is_empty() {
        None
    } else {
        Some(config.agent.default_provider.as_str())
    };
    create_transport(config, model, explicit, circuit_breaker_threshold, circuit_breaker_cooldown_secs)
}
