# Jev 判断层（`src/judge/`）— D18

> 3 个文件 · 702 行。定位：与**生成层**（`provider/`，DeepSeek 对话）互补的**高频结构化判断层**。
> 代码：`judge/mod.rs`（门面 + 全局单例）· `judge/client.rs`（HTTP）· `judge/types.rs`（serde 类型）
> 配置：`[jev]`（`core/config.rs::JevConfig`）· 设计依据：`docs/decisions/D18-jev-judge.md`

## 铁律（实现必须遵守）

1. **纯增益、非硬依赖**：未启用 / 未配 key / 网络失败 / 超时 / confidence 不足 → **一律返回 `None`**，调用方走原启发式（**回退路径**）。
2. **绝不阻塞主流程**：所有错误在门面内吞掉，判断层**无 panic 路径**。
3. **便捷方法只有「有答案 / 无答案」两种出口**，调用方不感知失败原因。

> 由此可推出：**关掉判断层，功能应当完全照旧**（只是质量/精度下降），不应有任何功能不可用。

## 配置

```toml
[jev]
enabled = false                  # 默认关；false → 全部集成点走回退
model = "jev-latest"             # 默认；⚠️ 阈值调优后应锁定版本（如 jev-1.13.0），latest 会漂移
base_url = "https://api.typesafe.ai"
timeout_secs = 5                 # 默认 5；判断层硬上限，超时即回退
min_confidence = 0.6             # 默认 0.6；choice 门控阈值
# api_key 只从 .env 的 TYPESAFE_API_KEY 读取，永不写入 config.toml（serde skip）
```

`Judge::from_config()` 在 `enabled == false` **或** `api_key` 为空时返回 `None`。

## 三种问题原语（`Question`）

| 原语 | 语义 | 返回 |
|------|------|------|
| `Noul` | 是非判断 | 「是」的概率 `noul ∈ [0,1]`，**无 confidence** |
| `Choice` | 多选一 | 选中项 + `confidence` + 各选项概率（criteria ≤ 255 项） |
| `Score` | 有序档位评分（2–10 档） | 概率加权档位 + `confidence` |

> ⚠️ **两条官方工程约束**（`types.rs` 顶部注释）：
> - **问题 ID 不发给模型** → 完整问题必须写进 `instructions`
> - **score 是弱数值校准** → **禁止档位间插值**，只能做阈值判断

## 门面 API（`Judge`）

| 方法 | 用途 |
|------|------|
| `ask(state, HashMap<String, Question>)` | 官方 **fan-out**：一次请求多问题，问题间并行独立评估（几乎不增加延迟）。**失败返回 `Err`**，仅供 P0 评估脚本等需要显式感知错误的调用方 |
| `noul(state, instructions, threshold)` | 单个是非判断。`noul ≥ threshold → Some(true)`；`noul ≤ 1-threshold → Some(false)`；**中间「真模糊区」→ None（宁可回退）** |
| `choice_gated(state, instructions, criteria)` | 单个多选 + confidence 门控：`confidence < min_confidence` 视为「拿不准」，与失败同待遇 → `None` |
| `min_confidence()` | 供自定义 fan-out 调用方自行比对 |

**业务集成点请用 `noul` / `choice_gated`**（自动吞错回退），不要用 `ask`。

## 全局单例

```rust
static GLOBAL_JUDGE: OnceLock<Option<Judge>>;
judge::set_global_judge(Judge::from_config(&config));   // 启动时一次
judge::global_judge() -> Option<&'static Judge>          // None = 未初始化或未启用
```

初始化时机：`lib.rs:807`（TUI 模式）、`gateway/mod.rs:112`（Gateway 模式）。

## 五处集成点

| # | 位置 | 原语 | 用途 |
|---|------|------|------|
| P1 | `tools/builtin.rs:1769` | `noul(.., 0.7)` | **记忆保存门控**：判断该内容是否值得写入长期记忆 |
| P2 | `agent/curator.rs:107-150` | `choice`（active/stale/archived） | **技能库生命周期巡检**：`Curator::run_with_judge()` 一次 fan-out 评估全部技能，**confidence 达标的结论覆盖时间规则**，未达标/失败 → 全走时间规则 |
| P3a | `agent/session.rs:560` | `noul(.., 0.75)` | **修复后 tool_calls 可信度**：修复管线产出的调用是否可信 |
| P3b | `agent/session.rs:257-280` | `score` + `min_confidence` 门控 | **学习反思三档**：档位中点映射 `0→0.25 / 1→0.6 / 2→0.95`（**禁插值**）；失败或置信不足 → 保留启发式分数 |
| P3c | `tools/search/mod.rs:250` | `noul` | **搜索结果预筛**：`p < 0.5` 滤除低价值结果 |

> 注：P2 / P3b 直接用 `ask()` + 自行比对 `min_confidence()`（fan-out 一次多问的场景），P1 / P3a / P3c 用便捷方法 `noul()`。

## 测试

`judge/mod.rs` 与 `judge/types.rs` 的单测用**本地 axum mock**（`/v1/systemone`）覆盖：
门控语义（`noul` 三区 + `choice` 低置信回退）、官方 quickstart 请求/响应 schema round-trip、缺 key 时不 panic。

## 已知边界

- **`ask()` 之外无重试**：失败即回退，不重试（符合「不阻塞」原则）。
- **`Score` 档位顺序依赖 `criteria` 数组下标**（index 0 = 第一档），响应里的 `legend` 字段**未被解析**（`TypedAnswer::Score` 只取 `score`/`confidence`/`probabilities`）。
- **单例不可重置**：`OnceLock` 语义，同进程内只能初始化一次。
- **默认 `model = "jev-latest"` 会漂移**：生产环境建议锁定具体版本号。
