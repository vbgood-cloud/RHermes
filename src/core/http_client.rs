//! HTTP 客户端工厂
//!
//! 根据代理配置统一创建 reqwest::Client，
//! 支持三模式（all/off/auto）+ no_proxy 域名排除。

use std::time::Duration;

use crate::core::config::ProxyConfig;

/// 根据代理配置创建 reqwest::Client
///
/// - `proxy_config`：全局代理配置
/// - `feature`：功能名（如 "llm", "web_search", "wechat"）
/// - `timeout`：请求超时
pub fn create_proxied_client(
    proxy_config: &ProxyConfig,
    feature: &str,
    timeout: Duration,
) -> reqwest::Client {
    let need_proxy = proxy_config.need_proxy(feature);

    let mut builder = reqwest::Client::builder().timeout(timeout);

    if need_proxy {
        if let Some(ref url) = proxy_config.url {
            match reqwest::Proxy::all(url) {
                Ok(mut proxy) => {
                    // 注入 no_proxy 排除列表
                    if !proxy_config.no_proxy.is_empty() {
                        let no_proxy_str = proxy_config.no_proxy.join(",");
                        let no_proxy = reqwest::NoProxy::from_string(&no_proxy_str);
                        proxy = proxy.no_proxy(no_proxy);
                    }
                    tracing::info!(feature, proxy = url, "启用代理");
                    builder = builder.proxy(proxy);
                }
                Err(e) => {
                    tracing::warn!(feature, proxy = url, error = %e, "代理地址无效");
                }
            }
        }
    }

    builder.build().expect("创建 HTTP 客户端失败")
}
