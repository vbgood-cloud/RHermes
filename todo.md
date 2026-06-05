# 架构重构 — 全部完成 ✅

1. **Provider 层 — Transport + Provider trait + Pool** ✅
   - `src/provider/mod.rs` — 模块入口
   - `src/provider/transport.rs` — `Transport` trait + `DeepSeekTransport` 实现
   - `src/provider/pool.rs` — `CircuitBreaker` + `ProviderPool`（加权轮询）
   - `src/core/config.rs` — 新增 `ProviderPoolConfig`
   - `src/api/mod.rs` — `DeepSeekClient` 实现 `Transport` trait
   - `src/main.rs` — 创建 `ProviderPool` 并注入
   - `src/tui/mod.rs` — 使用 `Arc<dyn Transport>` 替代 `DeepSeekClient`
   - `src/agent/task.rs` — 所有子 Agent 函数接收 `Arc<dyn Transport>`
   - `src/tools/builtin.rs` — 全局 Transport 设置/获取

2. **Prefix Cache 管理器** ✅
   - `src/core/prefix_cache.rs` — 三层 system prompt 管理
   - `src/core/context.rs` — Context 内部集成 PrefixCacheManager

3. **上下文压缩 + .jsonl 归档** ✅
   - `src/core/archive.rs` — 压缩事件归档到 `compressions/YYYY-MM-DD.jsonl`
   - `src/tui/mod.rs` — 压缩触发时自动归档

4. **会话持久化到 SQLite** ✅
   - `src/agent/memory.rs` — 新增 `session_messages` 表 + 读写方法
   - `src/tui/mod.rs` — resume/保存优先 SQLite，回退 JSON 文件
