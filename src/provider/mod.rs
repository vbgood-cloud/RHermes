//! Provider 层 —— Transport（协议适配）+ Provider Pool（熔断/加权）
//!
//! 将 API 调用抽象为 Transport trait，支持多 Provider 实例的
//! 加权轮询和熔断器健康检测。

mod factory;
mod pool;
mod transport;

pub use factory::{
    create_main_transport, create_transport, default_provider_base_url,
    infer_provider_from_model,
};
pub use pool::ProviderPool;
pub use transport::DeepSeekTransport;
pub use transport::Transport;
