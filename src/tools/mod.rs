//! RHermes 工具系统
//!
//! 工具注册表 + 元数据模型 + 5 个内置工具实现 + 并行调度器。

pub use registry::*;
pub use builtin::*;
pub use dispatcher::*;

/// WASM 插件系统
pub mod wasm_plugin;

mod registry;
mod builtin;
mod dispatcher;
pub mod search;
pub mod office;
