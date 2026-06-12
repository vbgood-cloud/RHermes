//! MCP (Model Context Protocol) 客户端模块
//!
//! 将远程 MCP Server 的工具包装为 Tool trait 实现，
//! 无缝融入 rhermes 的 ToolRegistry + ToolDispatcher 体系。

pub mod adapter;
pub mod config;
pub mod import;
pub mod setup;
pub mod sse_transport;
pub mod tool_wrapper;
pub mod transport;

pub use adapter::McpAdapter;
pub use adapter::McpAdapterManager;
pub use tool_wrapper::McpRemoteTool;
