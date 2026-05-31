//! RHermes 成本控制体系
//!
//! 五个互补机制控制 Token 花费：
//!
//! 1. **Flash-First 分级** — auto / flash / pro 三级 preset
//! 2. **NEEDS_PRO 自动升级** — 模型自报告，flash 自动切 pro
//! 3. **辅助调用强制 Flash** — 摘要/压缩等强制走 flash
//! 4. **轮次自动压缩** — 工具结果 >3000 token 自动摘要
//! 5. **成本仪表盘** — 每轮/累计成本实时显示



// ---------------------------------------------------------------------------
// 模型级别
// ---------------------------------------------------------------------------

/// 模型分级
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelTier {
    /// 日常开发，成本最低
    Flash,
    /// 复杂任务，更强推理
    Pro,
}

impl ModelTier {
    /// 模型名称（DeepSeek API 使用）
    pub fn model_name(&self) -> &'static str {
        match self {
            Self::Flash => "deepseek-v4-flash",
            Self::Pro => "deepseek-v4-pro",
        }
    }

    /// 成本倍率（相对于 Flash）
    pub fn cost_multiplier(&self) -> f64 {
        match self {
            Self::Flash => 1.0,
            Self::Pro => 12.0, // Pro 约 12x Flash
        }
    }

    /// 每百万 input token 的价格（美元）
    pub fn input_price_per_m(&self) -> f64 {
        match self {
            Self::Flash => 0.15,
            Self::Pro => 1.80,
        }
    }

    /// 每百万 output token 的价格（美元）
    pub fn output_price_per_m(&self) -> f64 {
        match self {
            Self::Flash => 0.60,
            Self::Pro => 7.20,
        }
    }
}

// ---------------------------------------------------------------------------
// 使用预设
// ---------------------------------------------------------------------------

/// 成本预设
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CostPreset {
    /// 全部走 Flash（最省钱）
    Flash,
    /// Flash → 遇到 NEEDS_PRO 自动切 Pro
    Auto,
    /// 全部走 Pro（最强推理）
    Pro,
}

impl CostPreset {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Flash => "flash",
            Self::Auto => "auto",
            Self::Pro => "pro",
        }
    }

    /// 当前预设下，主模型的初始级别
    pub fn initial_tier(&self) -> ModelTier {
        match self {
            Self::Flash | Self::Auto => ModelTier::Flash,
            Self::Pro => ModelTier::Pro,
        }
    }
}

// ---------------------------------------------------------------------------
// 成本计算
// ---------------------------------------------------------------------------

/// 单轮成本明细
#[derive(Debug, Clone)]
pub struct CostBreakdown {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub tier: ModelTier,
    pub cost: f64,
}

impl CostBreakdown {
    /// 根据 Token 用量和模型级别计算成本
    pub fn calculate(input: u32, output: u32, cache_hit: u32, cache_miss: u32, tier: ModelTier) -> Self {
        // 缓存命中的 input token 按 10% 计费
        let effective_input = cache_miss as f64 + cache_hit as f64 * 0.1;
        let input_cost = effective_input / 1_000_000.0 * tier.input_price_per_m();
        let output_cost = output as f64 / 1_000_000.0 * tier.output_price_per_m();

        Self {
            input_tokens: input,
            output_tokens: output,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: cache_miss,
            tier,
            cost: input_cost + output_cost,
        }
    }

    /// 缓存命中率 (0-100)
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hit_tokens + self.cache_miss_tokens;
        if total > 0 {
            self.cache_hit_tokens as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// NEEDS_PRO 检测
// ---------------------------------------------------------------------------

/// NEEDS_PRO 标记检测
///
/// 模型在 Flash 级别遇到超出能力范围的任务时，
/// 在响应第一行输出 `<<<NEEDS_PRO>>>` 请求升级。
pub struct NeedsProDetector;

impl NeedsProDetector {
    /// 检测响应中是否包含 NEEDS_PRO 标记
    ///
    /// 返回 (去除标记后的内容, 是否请求升级)
    pub fn detect(content: &str) -> (&str, bool) {
        let trimmed = content.trim();
        if trimmed.starts_with("<<<NEEDS_PRO>>>") {
            let rest = trimmed.trim_start_matches("<<<NEEDS_PRO>>>").trim();
            (rest, true)
        } else if trimmed.starts_with("<<<NEEDS_PRO:") {
            // 带原因的格式: <<<NEEDS_PRO: 原因>>>
            if let Some(end) = trimmed.find(">>>") {
                let rest = trimmed[end + 3..].trim();
                (rest, true)
            } else {
                (trimmed, true)
            }
        } else {
            (content, false)
        }
    }
}

// ---------------------------------------------------------------------------
// 轮次自动压缩
// ---------------------------------------------------------------------------

/// 工具结果压缩器
///
/// 超过阈值的内容被压缩（保留开头和结尾的关键信息）。
pub struct ResultCompressor {
    /// 压缩阈值（字符数）
    threshold: usize,
    /// 压缩后保留的开头字符数
    head_chars: usize,
    /// 压缩后保留的结尾字符数
    tail_chars: usize,
}

impl Default for ResultCompressor {
    fn default() -> Self {
        Self {
            threshold: 3000,
            head_chars: 1000,
            tail_chars: 500,
        }
    }
}

impl ResultCompressor {
    pub fn new(threshold: usize, head_chars: usize, tail_chars: usize) -> Self {
        Self {
            threshold,
            head_chars,
            tail_chars,
        }
    }

    /// 压缩工具结果
    ///
    /// 如果内容长度超过阈值，保留 head + "…[省略 N 字符]…" + tail
    pub fn compress(&self, content: &str) -> String {
        if content.len() <= self.threshold {
            return content.to_string();
        }

        let total = content.len();
        let head = &content[..self.head_chars.min(total)];
        let tail = &content[total.saturating_sub(self.tail_chars)..];
        let omitted = total - self.head_chars - self.tail_chars;

        format!(
            "{}\n…[省略 {} 字符，总计 {}]…\n{}",
            head, omitted, total, tail
        )
    }

    /// 是否需要压缩
    pub fn needs_compress(&self, content: &str) -> bool {
        content.len() > self.threshold
    }
}

// ---------------------------------------------------------------------------
// 成本控制器（主入口）
// ---------------------------------------------------------------------------

/// 成本控制器
///
/// 管理模型分级、成本计算、NEEDS_PRO 升级、自动压缩。
pub struct CostController {
    /// 当前预设
    preset: CostPreset,
    /// 当前使用的模型级别
    current_tier: ModelTier,
    /// 累计成本
    total_cost: f64,
    /// 本轮成本
    round_cost: f64,
    /// 结果压缩器
    compressor: ResultCompressor,
    /// 是否显示升级提示
    show_escalation: bool,
}

impl CostController {
    /// 创建新的成本控制器
    pub fn new(preset: CostPreset) -> Self {
        let tier = preset.initial_tier();
        Self {
            preset,
            current_tier: tier,
            total_cost: 0.0,
            round_cost: 0.0,
            compressor: ResultCompressor::default(),
            show_escalation: true,
        }
    }

    // ---- 预设管理 ----

    /// 获取当前预设
    pub fn preset(&self) -> &CostPreset {
        &self.preset
    }

    /// 切换预设
    pub fn set_preset(&mut self, preset: CostPreset) {
        self.preset = preset;
        self.current_tier = preset.initial_tier();
    }

    /// 获取当前模型级别
    pub fn current_tier(&self) -> ModelTier {
        self.current_tier
    }

    /// 获取当前模型名称
    pub fn current_model(&self) -> &'static str {
        self.current_tier.model_name()
    }

    // ---- NEEDS_PRO 处理 ----

    /// 处理模型响应，检测 NEEDS_PRO
    ///
    /// 返回 (处理后的内容, 是否发生了升级)
    pub fn process_response<'a>(&mut self, content: &'a str) -> (&'a str, bool) {
        let (cleaned, needs_pro) = NeedsProDetector::detect(content);

        if needs_pro && self.current_tier == ModelTier::Flash && self.preset == CostPreset::Auto {
            // Auto 模式下自动升级
            self.current_tier = ModelTier::Pro;
            (cleaned, true)
        } else if needs_pro {
            // 已经是 Pro 或 Flash 模式不升级
            (cleaned, false)
        } else {
            (content, false)
        }
    }

    /// 升级后的一轮结束后，如果预设是 Auto，降级回 Flash
    pub fn end_round(&mut self) {
        // 记录本轮成本
        self.total_cost += self.round_cost;
        self.round_cost = 0.0;

        // Auto 模式下降级回 Flash
        if self.preset == CostPreset::Auto && self.current_tier == ModelTier::Pro {
            self.current_tier = ModelTier::Flash;
        }
    }

    // ---- 成本记录 ----

    /// 记录一轮的成本
    pub fn record_cost(&mut self, breakdown: &CostBreakdown) {
        self.round_cost = breakdown.cost;
    }

    /// 获取本轮成本
    pub fn round_cost(&self) -> f64 {
        self.round_cost
    }

    /// 获取累计成本
    pub fn total_cost(&self) -> f64 {
        self.total_cost
    }

    /// 重置累计成本
    pub fn reset_total(&mut self) {
        self.total_cost = 0.0;
        self.round_cost = 0.0;
    }

    // ---- 自动压缩 ----

    /// 压缩工具结果（如果超过阈值）
    pub fn compress_result(&self, content: &str) -> String {
        self.compressor.compress(content)
    }

    /// 是否需要压缩
    pub fn needs_compress(&self, content: &str) -> bool {
        self.compressor.needs_compress(content)
    }

    // ---- 辅助调用 ----

    /// 辅助调用（摘要/压缩等）始终使用 Flash
    pub fn auxiliary_tier() -> ModelTier {
        ModelTier::Flash
    }

    /// 是否为辅助调用估算成本
    pub fn estimate_auxiliary_cost(input_tokens: u32, output_tokens: u32) -> f64 {
        let tier = Self::auxiliary_tier();
        let input_cost = input_tokens as f64 / 1_000_000.0 * tier.input_price_per_m();
        let output_cost = output_tokens as f64 / 1_000_000.0 * tier.output_price_per_m();
        input_cost + output_cost
    }
}

impl Default for CostController {
    fn default() -> Self {
        Self::new(CostPreset::Auto)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ModelTier ----

    #[test]
    fn test_model_tier_names() {
        assert_eq!(ModelTier::Flash.model_name(), "deepseek-v4-flash");
        assert_eq!(ModelTier::Pro.model_name(), "deepseek-v4-pro");
    }

    #[test]
    fn test_flash_cheaper_than_pro() {
        assert!(ModelTier::Flash.input_price_per_m() < ModelTier::Pro.input_price_per_m());
        assert!(ModelTier::Flash.output_price_per_m() < ModelTier::Pro.output_price_per_m());
    }

    // ---- CostPreset ----

    #[test]
    fn test_preset_initial_tier() {
        assert_eq!(CostPreset::Flash.initial_tier(), ModelTier::Flash);
        assert_eq!(CostPreset::Auto.initial_tier(), ModelTier::Flash);
        assert_eq!(CostPreset::Pro.initial_tier(), ModelTier::Pro);
    }

    // ---- CostBreakdown ----

    #[test]
    fn test_cost_breakdown_calculation() {
        // 1000 input, 100 output, 全部缓存命中
        let breakdown = CostBreakdown::calculate(1000, 100, 800, 200, ModelTier::Flash);
        assert!(breakdown.cost > 0.0);
        assert!(breakdown.cost < 1.0);
        assert!((breakdown.cache_hit_rate() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_cache_hit_reduces_cost() {
        let full_miss = CostBreakdown::calculate(1000, 100, 0, 1000, ModelTier::Flash);
        let full_hit = CostBreakdown::calculate(1000, 100, 1000, 0, ModelTier::Flash);
        // 全缓存命中的成本应该低得多（输入按 10% 计费）
        assert!(full_hit.cost < full_miss.cost);
    }

    #[test]
    fn test_cache_hit_rate_zero_when_no_tokens() {
        let breakdown = CostBreakdown::calculate(0, 0, 0, 0, ModelTier::Flash);
        assert_eq!(breakdown.cache_hit_rate(), 0.0);
    }

    // ---- NEEDS_PRO ----

    #[test]
    fn test_needs_pro_detection() {
        let (rest, needs) = NeedsProDetector::detect("<<<NEEDS_PRO>>> 这个问题需要更强推理");
        assert!(needs);
        assert!(rest.starts_with("这个问题"));
    }

    #[test]
    fn test_needs_pro_with_reason() {
        let (rest, needs) = NeedsProDetector::detect("<<<NEEDS_PRO: 复杂数学推导>>>请计算...");
        assert!(needs);
        assert!(rest.starts_with("请计算"));
    }

    #[test]
    fn test_needs_pro_no_false_positive() {
        let (_, needs) = NeedsProDetector::detect("正常回复内容 <<<NEEDS_PRO>>> 不触发");
        assert!(!needs); // 不在开头
    }

    // ---- CostController ----

    #[test]
    fn test_controller_initial_state() {
        let ctrl = CostController::new(CostPreset::Auto);
        assert_eq!(ctrl.current_tier(), ModelTier::Flash);
        assert_eq!(ctrl.total_cost(), 0.0);
        assert_eq!(ctrl.round_cost(), 0.0);
    }

    #[test]
    fn test_controller_auto_escalation() {
        let mut ctrl = CostController::new(CostPreset::Auto);
        assert_eq!(ctrl.current_tier(), ModelTier::Flash);

        // NEEDS_PRO 触发升级
        let (_, escalated) = ctrl.process_response("<<<NEEDS_PRO>>>");
        assert!(escalated);
        assert_eq!(ctrl.current_tier(), ModelTier::Pro);

        // 轮次结束后降级
        ctrl.end_round();
        assert_eq!(ctrl.current_tier(), ModelTier::Flash);
    }

    #[test]
    fn test_controller_flash_preset_no_escalation() {
        let mut ctrl = CostController::new(CostPreset::Flash);
        let (_, escalated) = ctrl.process_response("<<<NEEDS_PRO>>>");
        assert!(!escalated); // Flash preset 不升级
        assert_eq!(ctrl.current_tier(), ModelTier::Flash);
    }

    #[test]
    fn test_controller_pro_preset_stays_pro() {
        let mut ctrl = CostController::new(CostPreset::Pro);
        assert_eq!(ctrl.current_tier(), ModelTier::Pro);
        ctrl.end_round();
        assert_eq!(ctrl.current_tier(), ModelTier::Pro); // 不降级
    }

    #[test]
    fn test_controller_cost_accumulation() {
        let mut ctrl = CostController::new(CostPreset::Flash);
        let breakdown = CostBreakdown::calculate(1000, 100, 800, 200, ModelTier::Flash);
        ctrl.record_cost(&breakdown);
        let round_cost = ctrl.round_cost();
        assert!(round_cost > 0.0);

        ctrl.end_round();
        assert_eq!(ctrl.total_cost(), round_cost);
        assert_eq!(ctrl.round_cost(), 0.0); // 轮次结束后清零
    }

    #[test]
    fn test_controller_switch_preset() {
        let mut ctrl = CostController::new(CostPreset::Auto);
        ctrl.set_preset(CostPreset::Pro);
        assert_eq!(ctrl.current_tier(), ModelTier::Pro);
    }

    // ---- ResultCompressor ----

    #[test]
    fn test_compressor_below_threshold() {
        let comp = ResultCompressor::default();
        let text = "short text";
        assert_eq!(comp.compress(text), text);
        assert!(!comp.needs_compress(text));
    }

    #[test]
    fn test_compressor_above_threshold() {
        let comp = ResultCompressor::new(10, 5, 3);
        let text = "1234567890ABCDE";
        let compressed = comp.compress(text);
        assert!(comp.needs_compress(text));
        assert_ne!(compressed, text); // 内容被改变了
        assert!(compressed.contains("[省略"));
    }

    #[test]
    fn test_compressor_preserves_head_and_tail() {
        let comp = ResultCompressor::new(10, 5, 3);
        let text = "12345____CDE";
        let compressed = comp.compress(text);
        assert!(compressed.starts_with("12345")); // head 5 chars
        assert!(compressed.ends_with("CDE"));     // tail 3 chars
    }

    // ---- 辅助调用 ----

    #[test]
    fn test_auxiliary_always_flash() {
        assert_eq!(CostController::auxiliary_tier(), ModelTier::Flash);
    }

    #[test]
    fn test_estimate_auxiliary_cost() {
        let cost = CostController::estimate_auxiliary_cost(1000, 100);
        assert!(cost > 0.0);
        assert!(cost < 0.01); // 辅助调用应该很便宜
    }
}
