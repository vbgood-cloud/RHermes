# D18: Jev 决策模型集成方案（判断层）

> 决策编号: D18
> 日期: 2026-09-27
> 状态: 设计完成（方案已选定，待 P0 中文验证后实施）
> 前置: D17（分层网络代理 — Jev API 需走代理/网关）

## 一、现状分析

### 1.1 RHermes 中的"语义判断"需求清单

| 判断点 | 源码位置 | 现行做法 | 痛点 |
|--------|---------|---------|------|
| 记忆是否值得保存 | `src/tui/mod.rs:469`（提示词软约束）+ `src/agent/memory.rs` | 提示词约束 LLM 自行决定 | 软约束不可靠，垃圾记忆污染 SQLite+FTS5 检索 |
| 技能生命周期 active→stale→archived | `src/agent/curator.rs` | 时间规则为主 | 纯时间规则不懂语义，该归档的没归档 |
| Tool-Call 修复策略选择 | `src/agent/repair.rs` | 流水线串行逐个尝试 | 每个策略一轮重试，浪费轮次与 token |
| 学习反思评分 (overall/depth 0-1) | `src/edu/reflection.rs` | LLM 生成文本 + 解析 | 解析脆弱；连续分数语义对 LLM 本就勉强 |
| 搜索结果相关性过滤 | `src/tools/search/` | 不过滤，全量返回 | 低相关结果白白消耗 Context 预算 |

共同特征：**都是高频、小上下文、答案离散的判断**——恰好是通用 LLM 最不划算的用法（贵、慢、输出需解析）。

### 1.2 Jev 是什么（2026-09 官方文档核实）

TypeSafe AI 的 System One 模型：输入 `state` + 带类型的问题，返回**类型化结构答案**（概率/选项/档位），不做文本生成。

```http
POST https://api.typesafe.ai/v1/systemone
Authorization: Bearer $TYPESAFE_API_KEY
```

| 原语 | 返回 | 约束 |
|------|------|------|
| `noul` | 0–1 概率（"是"的概率，无 confidence） | criteria 可选 |
| `choice` | 选项 + 每项概率 + confidence | criteria 必填，≤255 项 |
| `score` | 概率加权档位 + confidence | criteria 必填，2–10 个有序档 |

- 计费：**仅输入 token**，约 $0.042/M（DeepSeek 输入价 ~6 倍差距；输出免费）
- 延迟 70–500ms，32K 上下文
- 一次请求的多个 `questions` **并行独立评估**——官方实测 13 问合一比 13 次独立调用快 ~9.6×、便宜 ~11.5×

### 1.3 官方工程约束（必须遵守）

1. 问题 ID 不发给模型 → 完整问题必须写进 `instructions`
2. `confidence` 是分布集中度，**不是正确性**，仅用于回退门控
3. `score` 弱数值校准 → **禁止档位间插值**，只做阈值判断
4. `jev-latest` 别名会漂移 → 阈值调优后**锁定版本**（如 `jev-1.13.0`）
5. 数学/计数/日期留在应用代码，Jev 只做语义判断

---

## 二、决策

### 2.1 定位：判断层（Judge），与生成层互补

```text
生成层  DeepSeek (provider/)     ── Agent Loop、内容生成   ── 不变
判断层  Jev      (judge/)        ── 高频结构化决策         ── 新增
         ↳ 失败/未配置 → 回退现有行为（提示词/规则），Jev 是纯增益，非硬依赖
```

**不进 `provider/` 熔断体系**：Jev 不是 chat-completion 协议，无法实现 `Transport` trait；它调用廉价、可重试一次、失败即回退，不需要加权轮询与熔断。

### 2.2 分阶段集成（每阶段独立可回滚）

| 阶段 | 集成点 | 原语 | 回退路径 |
|------|--------|------|---------|
| **P0** | 中文判断质量验证（Playground，不写代码） | 全部 | 不通过则中止后续阶段 |
| **P1** | 记忆保存判断（替代提示词软约束） | Noul | Jev 失败 → 保持提示词约束行为 |
| **P2** | 技能生命周期批量评估（curator） | Choice | 失败 → 现有时间规则 |
| **P3** | 修复策略预选（repair）/ 反思档位化（reflection）/ 搜索预筛（search） | Choice / Score / Score | 失败 → 现有流水线 |

### 2.3 网络路径（依赖 D17）

`api.typesafe.ai` 国内可达性未验证 → 统一走 D17 工厂：`create_proxied_client(&config.proxy, "jev", ...)`；`proxy.rules.jev` 默认 `true`。备选网关：OpenRouter `https://openrouter.ai/api/v1/systemone`（改 `base_url` 即可，请求体同构）。

---

## 三、架构设计

### 3.1 新模块 `src/judge/`

```text
src/judge/
├── mod.rs      门面：JudgeHandle（Arc 共享）+ 全局 set/get（对齐 tools::set_global_* 惯例）
├── client.rs   reqwest 客户端：Bearer 认证、timeout 5s、失败重试 1 次
└── types.rs    serde 请求/响应类型（三原语）
```

### 3.2 核心类型（`types.rs`）

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul { instructions: String, #[serde(skip_serializing_if = "Option::is_none")] criteria: Option<HashMap<String, String>> },
    Choice { instructions: String, criteria: HashMap<String, String> },
    Score { instructions: String, criteria: Vec<String> },
}

#[derive(Serialize)]
pub struct JudgeRequest {
    pub state: serde_json::Value,          // 字符串或结构化 JSON
    pub model: String,
    pub questions: HashMap<String, Question>,  // key = 调用方自定 ID
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TypedAnswer {
    Noul { noul: f64 },
    Choice { choice: String, confidence: f64, probabilities: HashMap<String, f64> },
    Score { score: f64, confidence: f64, probabilities: HashMap<String, f64> },
}

#[derive(Deserialize)]
pub struct JudgeResponse {
    pub model: String,
    pub answers: HashMap<String, TypedAnswer>,
    pub usage: JudgeUsage,
}
```

### 3.3 门面 API（`mod.rs`）

```rust
pub struct Judge { client: JudgeClient, cfg: JevConfig }

impl Judge {
    /// 批量判断：一次请求多问题（利用官方并行评估，延迟几乎不增）
    pub async fn ask(
        &self,
        state: &str,
        questions: HashMap<String, Question>,
    ) -> Result<HashMap<String, TypedAnswer>, JudgeError>;

    /// 便捷方法：单个 Noul；None = 未启用/失败，调用方走回退
    pub async fn noul(&self, state: &str, instructions: &str) -> Option<f64>;

    /// 便捷方法：单个 Choice + confidence 门控
    /// 返回 None 当 confidence < min_confidence（视为"拿不准"，回退）
    pub async fn choice_gated(&self, state: &str, instructions: &str,
        criteria: HashMap<String, String>) -> Option<String>;
}
```

错误处理对齐全库惯例：`JudgeError` 枚举 + `String`，`Judge` 内部吞掉网络错误返回 `None`（判断层绝不阻塞主流程）。

### 3.4 P1 集成示例：记忆保存判断

```text
现状: TUI 系统提示词软约束 → Memory 工具直接落库
目标: Memory 工具 execute() 内先问一次 Jev（Noul）:
      state   = 拟保存的记忆内容 + 当前会话主题摘要
      问题    = "该内容是跨会话有长期价值的知识/偏好/事实吗？
                 （排除：任务进度、临时日志、已完成工作、TODO）"
      noul ≥ 0.7 → 落库；< 0.7 → 返回"已评估为临时信息，未保存"
      Jev 失败(None) → 落库（保持现状，宁可多存不可丢）
```

同步落库改为 `tokio::spawn` 内联判断，不阻塞工具返回；单次判断 ~500 token ≈ **$0.00002**，日均 1000 次判断 ≈ **$0.02/天**。

### 3.5 P2 集成示例：技能生命周期批量评估

```text
curator 巡检时（N 技能一次调用，fan-out 模式）:
  state   = 技能清单（名称+描述+最近使用时间摘要）
  questions = 每技能一个 Choice:
    { "skill_{id}": Choice {
        instructions: "技能 {name}：{desc}，最近使用 {days} 天前。应处于哪个生命周期状态？",
        criteria: { "active": "仍被频繁需要", "stale": "疑似过时待观察",
                    "archived": "应归档" } } }
  confidence < min_confidence 的技能 → 保留现状态（时间规则兜底）
```

### 3.6 reflection 语义重设计（P3 注意事项）

`ReflectionRecord.overall_score: f64` 连续分语义与 Jev `score` 弱校准冲突 → 改为**三档位**（`未达标/合格/优秀`）+ 档位概率，落库字段同步迁移（`overall_score` 保留、由档位中点映射，`grade_probabilities` 新增）。

---

## 四、配置文件格式

`config.toml`（非敏感）：

```toml
[jev]
enabled = true                       # false 或缺省 → 全部走回退路径
model = "jev-1.13.0"                 # P0/P1 期间可用 jev-latest，调优后锁定
base_url = "https://api.typesafe.ai" # 备选: https://openrouter.ai
timeout_secs = 5
min_confidence = 0.6                 # choice 门控阈值，P1 实测后调
```

`.env`（敏感）：

```text
TYPESAFE_API_KEY=ts-xxx
```

`[proxy.rules]` 增加一行（D17 体系）：

```toml
[proxy.rules]
jev = true    # api.typesafe.ai 国内默认不可达，默认走代理
```

---

## 五、验证方案

### P0 门槛（中文质量，Playground 人工评估，不写代码）

1. 取 20 条真实中文对话 state（含口语/错别字/混合术语），对 P1/P2 的 3 类问题各评一遍
2. 通过标准：Noul 方向正确率 ≥ 85%、Choice 首选正确率 ≥ 80%
3. **不达标 → 冻结 P1-P3，本文档归档为"已否决"，零代码成本**

### 集成回归

4. 无 `[jev]` 配置 / 无 key → 所有集成点走回退，行为与现状完全一致
5. `enabled = false` 显式关闭 → 同上
6. 拔网线模拟 API 失败 → 工具调用不报错、不延迟超过 timeout 上限，回退路径生效
7. `min_confidence = 0.99` 强制门控 → choice_gated 恒返回 None（验证回退分支可达）

### 单元测试（对齐 `#[cfg(test)]` 内联惯例）

8. `types.rs`：官方 quickstart 请求/响应 JSON 样例 round-trip 反序列化
9. `client.rs`：mock HTTP server 覆盖 400 校验错误 / 401 / 超时 / 成功四路径
10. P1 判断门控：noul 边界值 0.7/0.699 的落库分支

### 效果指标（P1 上线两周后评估）

11. 记忆误保存率（人工抽样 50 条）：对比启用前后
12. 日均判断成本（`usage.input_tokens` 累计）：应 < $0.05/天

---

## 六、风险与对策

| 风险 | 对策 |
|------|------|
| 中文判断质量未验证 | P0 人工门槛，不达标即冻结，零代码成本 |
| api.typesafe.ai 国内不可达 | D17 代理（rules.jev 默认 true）+ OpenRouter 网关备选（同构请求体） |
| Jev API 演进变动的可能性 | client 隔离在 `judge/` 单模块；版本锁定；网关同构可切换 |
| score 被误用为连续分 | 便捷 API 不暴露裸 score；reflection 显式改档位语义（§3.6） |
| 判断层故障拖垮主流程 | 全部调用吞错回退；异步 fire；timeout 5s 硬上限 |
| 无官方 Rust SDK | 自写 ~200 行 client（reqwest+serde），无第三方依赖增量 |
