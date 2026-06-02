//! 核心基础设施模块
//!
//! 包含配置管理、三段式 Context 和路径管理器。

pub use config::ApiConfig;
pub use config::Config;
pub use config::MemoryConfig;
pub use config::RequestConfig;
pub use config::AgentConfig;
pub use context::Context;
pub use context::Message;
pub use path::PathManager;

mod config;
mod context;
mod path;
