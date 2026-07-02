//! RHermes 并行工具调度器
//!
//! 核心职责：按 parallel_safe 标志分组执行工具调用。
//!
//! ## 调度策略
//!
//! ```text
//! 输入: [read(a), read(b), write(c), search(d), write(e)]
//!                                         
//!  并行组1 (JoinSet)    串行队列
//!  ┌──────────────┐    ┌─────────┐
//!  │ read(a)      │    │ write(c)│  ← 等并行完成
//!  │ read(b)      │    └─────────┘
//!  │ search(d)    │         ↓
//!  └──────┬───────┘    ┌─────────┐
//!         ↓             │ write(e)│
//!     (全部完成)        └─────────┘
//!         ↓                  ↓
//!     合并结果 → 返回 Vec<ToolResult>
//! ```

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Semaphore;

use crate::tools::{ToolRegistry, ToolResult};
use crate::tools::ToolCall;

// ---------------------------------------------------------------------------
// 调度器
// ---------------------------------------------------------------------------

/// 并行工具调度器
#[derive(Clone)]
pub struct ToolDispatcher {
    registry: ToolRegistry,
    /// 最大并行数
    max_concurrency: usize,
}

impl ToolDispatcher {
    /// 创建调度器
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            max_concurrency: 10,
        }
    }

    /// 设置最大并行数
    pub fn with_max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = n;
        self
    }

    /// 获取工具注册表引用
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// 调度一批工具调用
    ///
    /// 保持原始调用顺序：
    /// - 连续的 parallel_safe 调用并行执行（JoinSet）
    /// - 非 parallel_safe 的调用单独执行（等当前并行组完成）
    ///
    /// 示例: [read(a), read(b), write(c), search(d), write(e)]
    ///   → 并行组 [read(a), read(b)]  → write(c)  → 并行组 [search(d)]  → write(e)
    pub async fn dispatch(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        if calls.is_empty() {
            return vec![];
        }

        let mut results = Vec::new();
        let mut parallel_batch: Vec<ToolCall> = Vec::new();

        for call in calls {
            match self.registry.get(&call.name) {
                Some(tool) if tool.parallel_safe() => {
                    parallel_batch.push(call);
                }
                _ => {
                    // 串行或未知工具：先等当前并行组完成
                    if !parallel_batch.is_empty() {
                        let batch = std::mem::take(&mut parallel_batch);
                        let batch_results = self.execute_parallel(batch).await;
                        results.extend(batch_results);
                    }
                    // 然后执行单个串行调用
                    let result = self.execute_single(call).await;
                    results.push(result);
                }
            }
        }

        // 处理最后一组并行调用
        if !parallel_batch.is_empty() {
            let batch_results = self.execute_parallel(parallel_batch).await;
            results.extend(batch_results);
        }

        results
    }

    /// 并行执行一组工具调用
    async fn execute_parallel(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));
        let registry = Arc::new(self.registry.clone());

        let mut join_set = tokio::task::JoinSet::new();

        for call in calls {
            let reg = Arc::clone(&registry);
            let sem = Arc::clone(&semaphore);

            join_set.spawn(async move {
                let _permit = sem.acquire().await.expect("信号量获取失败");
                execute_tool_call(&reg, call).await
            });
        }

        let mut results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => results.push(tool_result),
                Err(e) => {
                    // task panic
                    results.push(ToolResult::error(
                        "unknown".into(),
                        "unknown".into(),
                        format!("task panic: {e}"),
                    ));
                }
            }
        }

        results
    }

    /// 串行执行单个工具调用
    async fn execute_single(&self, call: ToolCall) -> ToolResult {
        execute_tool_call(&self.registry, call).await
    }
}

// ---------------------------------------------------------------------------
// 内部函数
// ---------------------------------------------------------------------------

async fn execute_tool_call(registry: &ToolRegistry, call: ToolCall) -> ToolResult {
    let start = Instant::now();
    let name = call.name.clone();
    let call_id = call.id.clone();

    let tool = match registry.get(&name) {
        Some(t) => t,
        None => {
            return ToolResult::error(
                call_id,
                name.clone(),
                format!("未知工具: {name}"),
            );
        }
    };

    let result = tool.execute(call.arguments).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(output) => ToolResult::success(call_id, name, output, duration_ms),
        Err(e) => ToolResult::error(call_id, name, format!("{e}")),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Config;
    use crate::tools::builtin_registry;
    use crate::tools::builtin::GLOBAL_WORKSPACE;
    use serde_json::json;

    /// 确保 GLOBAL_WORKSPACE 已初始化。get_or_init 是线程安全的，多次调用无副作用。
    fn ensure_workspace() {
        GLOBAL_WORKSPACE.get_or_init(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".to_string())
        });
    }

    /// 创建位于 workspace 下的临时目录，避免路径边界检查失败
    fn tempdir_in_workspace() -> tempfile::TempDir {
        ensure_workspace();
        tempfile::tempdir_in(std::env::current_dir().unwrap()).expect("创建临时目录失败")
    }

    fn make_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("call_{}", name),
            name: name.into(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn test_dispatch_empty() {
        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg);
        let results = dispatcher.dispatch(vec![]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_unknown_tool() {
        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg);
        let results = dispatcher
            .dispatch(vec![make_call("nonexistent", json!({}))])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0].output.contains("未知工具"));
    }

    #[tokio::test]
    async fn test_dispatch_parallel_read_files() {
        // 创建临时文件用于测试
        let tmp = tempdir_in_workspace();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        tokio::fs::write(&f1, "hello").await.unwrap();
        tokio::fs::write(&f2, "world").await.unwrap();

        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg);

        let calls = vec![
            make_call("read_file", json!({"path": f1.to_str().unwrap()})),
            make_call("read_file", json!({"path": f2.to_str().unwrap()})),
        ];

        let results = dispatcher.dispatch(calls).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert!(results[1].success);
        assert!(results[0].output.contains("hello"));
        assert!(results[1].output.contains("world"));
    }

    #[tokio::test]
    async fn test_dispatch_serial_write_then_read() {
        let tmp = tempdir_in_workspace();
        let file_path = tmp.path().join("test.txt").to_str().unwrap().to_string();

        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg);

        // write_file (serial) → read_file (parallel)
        let calls = vec![
            make_call("write_file", json!({"path": file_path, "content": "平行宇宙"})),
            make_call("read_file", json!({"path": file_path})),
        ];

        let results = dispatcher.dispatch(calls).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].success, "write should succeed: {}", results[0].output);
        assert!(results[1].success, "read should succeed: {}", results[1].output);
        assert!(results[1].output.contains("平行宇宙"));
    }

    #[tokio::test]
    async fn test_dispatch_mixed_parallel_and_serial() {
        let tmp = tempdir_in_workspace();
        let f1 = tmp.path().join("a.txt").to_str().unwrap().to_string();
        let f2 = tmp.path().join("b.txt").to_str().unwrap().to_string();
        let f3 = tmp.path().join("c.txt").to_str().unwrap().to_string();

        // 先创建所有文件
        tokio::fs::write(&f1, "alpha").await.unwrap();
        tokio::fs::write(&f2, "beta").await.unwrap();

        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg);

        let calls = vec![
            make_call("read_file", json!({"path": f1})),          // parallel
            make_call("read_file", json!({"path": f2})),          // parallel
            make_call("write_file", json!({"path": f3, "content": "gamma"})), // serial
            make_call("read_file", json!({"path": f3})),          // parallel (读刚写的文件)
        ];

        let results = dispatcher.dispatch(calls).await;
        assert_eq!(results.len(), 4);
        assert!(results[0].success, "read a: {}", results[0].output);
        assert!(results[1].success, "read b: {}", results[1].output);
        assert!(results[2].success, "write c: {}", results[2].output);
        assert!(results[3].success, "read c: {}", results[3].output);
        assert!(results[3].output.contains("gamma"), "read c got: {}", results[3].output);
    }

    #[tokio::test]
    async fn test_dispatch_concurrency_limit() {
        let tmp = tempdir_in_workspace();
        let reg = builtin_registry(&Config::default());
        let dispatcher = ToolDispatcher::new(reg).with_max_concurrency(2);

        // 创建 5 个文件并行读
        let mut calls = Vec::new();
        for i in 0..5 {
            let f = tmp.path().join(format!("{i}.txt"));
            tokio::fs::write(&f, format!("data_{i}")).await.unwrap();
            calls.push(make_call("read_file", json!({"path": f.to_str().unwrap()})));
        }

        let results = dispatcher.dispatch(calls).await;
        assert_eq!(results.len(), 5);
        for r in &results {
            assert!(r.success, "{}", r.output);
        }
    }
}
