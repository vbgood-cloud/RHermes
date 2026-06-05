# 多 Provider 接入方案

1. **扩展配置 — config.toml 新增 [providers] 段**
   - 保留 [api] 向前兼容
   - 新 [providers.openai]、[providers.siliconflow] 等
   - 新增 ProviderConfig 结构：api_key, base_url, model（可选）, api_type（默认 openai-compatible）
   - 新增 [agent] 字段：default_provider、evolution_provider

2. **实现 ProviderFactory**
   - 根据 config + model 名自动匹配 provider
   - model 前缀推断：deepseek→deepseek, gpt-→openai, glm-→zhipu, qwen→qwen 等
   - 自动补充默认 base_url

3. **更新 ProviderPool 支持按 model 路由**
   - 注册 model → transport 映射
   - select(model) 前缀匹配路由
   - 熔断器保持不变

4. **向前兼容 + 配置更新**
   - init 向导支持多 provider 配置
