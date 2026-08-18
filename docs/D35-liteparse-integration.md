# D35: LiteParse 文档解析工具 — Rust 原生集成

> 决策编号: D35
> 日期: 2026-07-17
> 状态: 设计完成
> 前置: D8 (Office 文档工具), D24 (统一插件系统)

## 背景

rhermes 已有 Office 文档工具（excel.rs/word.rs/pptx.rs，D8 读写分离策略），但**缺少 PDF 解析能力**。PDF 是最常见的文档格式——毕业论文、实验报告、教材课件、科研论文——Agent 无法读取 PDF 内容是重大能力缺口。

LiteParse（LlamaIndex 团队，Rust 原生，v2.6.0）是最佳选择：
- Rust 核心，与 rhermes 同语言，直接 Cargo 依赖
- 内置 PDFium 文本提取 + Tesseract OCR（零外部依赖）
- Markdown 输出（含表格/标题/列表识别），天然适配 LLM 上下文
- 空间文本提取（bounding box + 字体元数据），为 D28/D29 教学系统提供结构化数据
- 异步 API（async fn），与 rhermes tokio 运行时一致
- 支持 PDF / DOCX / XLSX / PPTX / 图片（与 D8 Office 工具部分重叠，但 LiteParse 更适合"只读提取"场景）

## 决策

新增 3 个内置工具，封装 LiteParse Rust API，注册到 builtin_registry。

### 与 D8 Office 工具的关系

| 工具 | 来源 | 定位 | 场景 |
|------|------|------|------|
| D8 excel.rs/word.rs/pptx.rs | Rust 原生 + Python 子进程 | **读写分离**，精确操作单元格/段落 | 生成/修改文档 |
| D35 liteparse.rs | LiteParse crate | **统一只读解析**，多格式输入 | Agent 需要理解文档内容时 |

D35 是 D8 的"读"侧补充——当 Agent 只需要提取文档内容（不需要修改）时，走 liteparse 更统一（一个工具处理所有格式）。

## LiteParse 核心 API（Rust）

```
LiteParse::new(config: LiteParseConfig) -> LiteParse
LiteParse::parse(&self, input: &str) -> Result<ParseResult, LiteParseError>
LiteParse::is_complex(&self, input: &str) -> Result<Vec<PageComplexityStats>, LiteParseError>
LiteParse::screenshot(&self, input: &str) -> Result<Vec<ScreenshotResult>, LiteParseError>

ParseResult {
    pages: Vec<ParsedPage>,       // 每页内容
    text: String,                 // 全文拼接
    outline: Vec<OutlineTarget>,  // PDF 书签/目录
    images: Vec<ExtractedImage>,  // 嵌入图片（image_mode=Embed 时）
}

ParsedPage {
    page_number: usize,
    text: String,
    markdown: String,
    text_items: Vec<TextItem>,    // 带 bounding box 的文本块
}

TextItem {
    text: String, x: f32, y: f32, width: f32, height: f32,
    font_name: Option<String>, font_size: Option<f32>,
    confidence: Option<f32>,      // OCR 置信度
    link: Option<String>,         // 超链接
    words: Vec<WordBox>,          // 词级子框
}
```

## 三个工具定义

### 工具1: `parse_document`

**用途**：解析文档，提取文本/Markdown/JSON

**输入参数**：
- `file_path`（必填）：文件路径，支持 PDF/DOCX/XLSX/PPTX/PNG/JPEG
- `format`（可选，默认 markdown）：输出格式，markdown / text / json
- `ocr_language`（可选，默认 eng）：OCR 语言，chi_sim / eng / eng+chi_sim
- `page_range`（可选）：指定页码，如 "1-5,10,15-20"
- `max_pages`（可选，默认 50）：最大页数限制（防止超大文档消耗过多 token）
- `no_ocr`（可选，默认 false）：跳过 OCR，仅提取原生文本
- `password`（可选）：加密文档密码

**输出**（JSON）：
- `num_pages`：总页数
- `format`：实际输出格式
- `content`：解析内容（markdown/text 字符串 或 json 对象）
- `truncated`：是否因超过 max_pages 或 token 限制被截断

**安全**：
- `parallel_safe = false`（PDF 解析有全局锁，并行无意义）
- 文件路径经 D14 `is_protected_path()` 检查
- 输出内容经 `<untrusted>` 包裹（D14 #4）
- max_pages 防止 Agent 被超大 PDF 撑爆上下文

### 工具2: `screenshot_document`

**用途**：生成文档页面截图（PNG），供 LLM 视觉理解或保存

**输入参数**：
- `file_path`（必填）：文件路径
- `page_range`（可选）：指定页码
- `dpi`（可选，默认 150）：渲染分辨率
- `output_dir`（可选，默认临时目录）：截图保存目录
- `max_pages`（可选，默认 10）：最大截图页数

**输出**（JSON）：
- `screenshots`：数组，每项含 `page_num`、`path`、`width`、`height`、`size_bytes`
- `output_dir`：实际保存目录

**安全**：
- `parallel_safe = false`
- 截图文件写入临时目录或用户指定目录，不走 `is_protected_path()`
- `max_pages` 限制防止生成过多文件

### 工具3: `check_document_complexity`

**用途**：快速判断文档是否需要 OCR（轻量，不实际解析）

**输入参数**：
- `file_path`（必填）：文件路径

**输出**（JSON）：
- `pages`：数组，每页含 `page_number`、`needs_ocr`、`reasons`、`text_coverage`
- `summary`：`all_simple`（全部可直提）/ `needs_ocr`（需要 OCR）/ `mixed`

**安全**：
- `parallel_safe = false`
- 轻量操作，无输出内容注入风险

## 配置扩展

config.toml 新增 `[liteparse]` 段：

```toml
[liteparse]
# 是否启用 LiteParse 文档解析工具（需要 pdfium 动态库）
enabled = true
# 默认 OCR 语言（Tesseract 格式）
# 中文: chi_sim, 英文: eng, 中英混: eng+chi_sim
ocr_language = "eng"
# 默认输出格式: markdown / text / json
default_format = "markdown"
# 默认最大页数（防止超大文档撑爆上下文）
max_pages = 50
# 默认 DPI（截图和 OCR 渲染）
dpi = 150
# 是否默认启用 OCR（关闭则只提取原生文本，速度更快）
ocr_enabled = true
# tessdata 目录路径（可选，默认从 TESSDATA_PREFIX 环境变量读取）
tessdata_path = ""
```

对应 Config 结构体新增 `LiteParseConfig` 字段（重命名为 `LiteParseSettings` 避免与 liteparse crate 的 `LiteParseConfig` 冲突）。

## 依赖分析

### Cargo.toml 新增

```toml
[dependencies]
liteparse = { version = "2.6", default-features = false }
```

**不启用 `tesseract` feature**（默认启用，但编译需要 leptonica/tesseract C 库，在 Windows 上编译困难）。

替代方案：
- rhermes 启动时检测系统是否有 `tesseract` 二进制 → 有则启用 OCR
- 或者通过 LiteParse 的 `ocr_server_url` 配置远程 OCR 服务
- 或者 `default-features = false` 后设置 `ocr_enabled = false`，仅用原生文本提取（90% 的 PDF 足够）

**推荐策略**：
1. `default-features = false`（不带 tesseract 编译）
2. Config 中 `ocr_enabled` 默认 `false`
3. 用户如需 OCR，设置 `ocr_server_url` 指向远程 OCR 服务，或安装系统 tesseract 后通过 feature 重新编译

### 依赖冲突风险

| 依赖 | rhermes 当前版本 | liteparse 版本 | 冲突风险 |
|------|-----------------|---------------|---------|
| reqwest | 0.12 | 0.13.3 | ⚠️ 可能（semver 不兼容） |
| tokio | 1.x | 1.52.3 | ✅ 兼容 |
| serde | 1.x | 1.0.228 | ✅ 兼容 |
| serde_json | 1.x | 1.0.149 | ✅ 兼容 |
| image | 不使用 | 0.25 | ✅ 新增依赖 |
| blake3 | 不使用 | 1.x | ✅ 新增依赖 |

**reqwest 冲突处理**：如果 rhermes 用 reqwest 0.12 而 liteparse 要求 0.13.3，Cargo 会报错。解决方案：
- 方案 A：升级 rhermes 的 reqwest 到 0.13（推荐，0.13 是最新稳定版）
- 方案 B：等 liteparse 降级（不太可能）
- 方案 C：fork liteparse 降级 reqwest（最后手段）

**pdfium 动态库**：liteparse-pdfium-sys 会编译/下载 pdfium C 库。Windows 上可能需要 MSVC 或 vcpkg。Linux 上通常通过 pkg-config 或自动下载预编译库。

## 与教学四部曲的关系

| 设计 | liteparse 角色 |
|------|---------------|
| **D28 TelemetrySink** | 解析教材 PDF → 提取知识点构建课程知识库 |
| **D29 实验评估** | 解析学生 PDF 实验报告 → LLM 评分（报告题三维度评分的"理解深度"维度需要全文） |
| **D31 双模式** | 学生端可用 parse_document 工具读取课件辅助学习 |
| **D8 Office 工具** | 互补：D8 精确读写 Office 文档，D35 统一只读解析所有格式（含 PDF/图片） |

## 不做什么

- **不做 PDF 生成**（用 D8 的子进程 python/reportlab 方案）
- **不做 PDF 编辑/合并/拆分**（超出 Agent 核心场景）
- **不做 OCR 服务端**（liteparse 只是客户端，不提供 OCR HTTP 服务）
- **不替代 D8 的 word.rs/pptx.rs**（精确写操作仍走 D8）
- **不集成 search_items**（Python 绑定有 bug，Rust API 可用但 Agent 不需要文档内搜索——直接提取全文让 LLM 处理）

## 改动量估算

| 文件 | 操作 | 行数 |
|------|------|------|
| `src/tools/liteparse.rs` | 新建 | ~280 行 |
| `src/tools/builtin.rs` | 修改（注册3个工具） | ~15 行 |
| `src/core/config.rs` | 修改（新增 LiteParseSettings） | ~40 行 |
| `Cargo.toml` | 修改（添加依赖） | ~3 行 |
| `config.template.toml` | 修改（新增 [liteparse] 段） | ~20 行 |
| **总计** | | **~358 行** |

无新 crate 依赖冲突（除 reqwest 版本需验证），不改动 Agent Loop。
