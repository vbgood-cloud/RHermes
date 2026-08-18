# D35 实现提示词 — LiteParse 文档解析工具

> 决策编号: D35
> 前置: D8 (Office 工具), D24 (统一插件), D14 (安全加固)
> 依赖: liteparse crate v2.6+

## 概述

新增 `src/tools/liteparse.rs`，封装 LiteParse Rust crate，提供 3 个内置工具：`parse_document`、`screenshot_document`、`check_document_complexity`。支持 PDF/DOCX/XLSX/PPTX/图片的统一只读解析。

## 涉及文件

- `Cargo.toml` — 添加 liteparse 依赖
- `src/tools/liteparse.rs` — 新建，核心实现
- `src/tools/builtin.rs` — 注册新工具到 builtin_registry
- `src/core/config.rs` — 新增 LiteParseSettings 配置段
- `config.template.toml` — 新增 [liteparse] 配置段（如果有动态模板则跳过）

---

## 任务 1: Cargo.toml 添加依赖

**文件**: `Cargo.toml`

**目标**: 添加 liteparse crate 依赖，不启用 tesseract feature（避免 C 库编译依赖）

**改动要求**:
- 在 [dependencies] 中添加 `liteparse`，指定 `version = "2.6"`，设置 `default-features = false`
- 不启用 tesseract feature（OCR 通过配置项控制，默认关闭）
- 如果 reqwest 版本冲突（rhermes 0.12 vs liteparse 0.13），升级 rhermes 的 reqwest 到 0.13

**注意**: liteparse 的 pdfium 依赖（liteparse-pdfium-sys）会自动下载预编译库，不需要手动安装 pdfium。但 Windows 上首次编译可能需要联网下载。

---

## 任务 2: Config 新增 LiteParseSettings

**文件**: `src/core/config.rs`

**目标**: 新增 LiteParseSettings 结构体，作为 Config 的一个字段

**改动要求**:
- 新建结构体 `LiteParseSettings`（注意不要叫 LiteParseConfig，会与 liteparse crate 的类型冲突）
- 字段（全部带 `#[serde(default)]`）：
  - `enabled: bool`（默认 true）— 是否启用文档解析工具
  - `ocr_language: String`（默认 "eng"）— OCR 语言
  - `default_format: String`（默认 "markdown"）— 默认输出格式
  - `max_pages: usize`（默认 50）— 最大页数限制
  - `dpi: f32`（默认 150.0）— 渲染 DPI
  - `ocr_enabled: bool`（默认 false）— 是否默认启用 OCR
  - `tessdata_path: Option<String>`（默认 None）— tessdata 目录
  - `ocr_server_url: Option<String>`（默认 None）— 远程 OCR 服务 URL
- 为 LiteParseSettings 实现 Default trait，所有默认值如上
- 在 Config 结构体中添加 `liteparse: LiteParseSettings` 字段，带 `#[serde(default)]`
- 在 generate_annotated_template() 中添加 [liteparse] 配置段，每个字段带注释说明用途和可选值

**注意**: ocr_enabled 默认 false 因为 tesseract feature 未启用，需要用户显式配置 ocr_server_url 或重新编译启用 feature 才能使用 OCR。

---

## 任务 3: liteparse.rs 核心实现

**文件**: `src/tools/liteparse.rs`（新建）

**目标**: 封装 LiteParse API 为 3 个 Tool trait 实现

**改动要求**:

### 3.1 模块结构

新建 `src/tools/liteparse.rs`，导出 3 个结构体：`ParseDocument`、`ScreenshotDocument`、`CheckDocumentComplexity`。每个实现 `Tool` trait。

### 3.2 全局 LiteParse 实例

用一个 `OnceLock<Arc<liteparse::LiteParse>>` 全局持有 LiteParse 实例（lazy init），避免每次调用重建。实例从 Config 的 LiteParseSettings 初始化：
- ocr_enabled 从配置读取
- ocr_language 从配置读取
- output_format 设为 Markdown（parse_document 内部根据参数动态转换）
- dpi 从配置读取
- max_pages 从配置读取
- quiet 设为 true
- tessdata_path 和 ocr_server_url 从配置读取

初始化函数 `init_liteparse(settings: &LiteParseSettings) -> Result<()>`，在 main.rs 启动时调用一次。如果初始化失败（如 pdfium 加载失败），记录 warn 日志但不 panic，3 个工具的 execute 返回友好错误信息。

### 3.3 ParseDocument 工具

结构体 `ParseDocument`，impl Tool trait：

- `name()` 返回 "parse_document"
- `description()` 返回简短描述："Parse a document (PDF, DOCX, XLSX, PPTX, image) and extract text as markdown, plain text, or structured JSON. Supports OCR for scanned documents."
- `parallel_safe()` 返回 false（PDFium 有全局锁）
- `parameters()` 返回 JSON Schema：
  - file_path（string, required）— 文档文件路径
  - format（string, optional, enum: markdown/text/json, default: markdown）
  - ocr_language（string, optional, default: from config）
  - page_range（string, optional）— 页码范围如 "1-5,10,15-20"
  - max_pages（integer, optional, default: from config）— 最大页数
  - no_ocr（boolean, optional, default: false）— 跳过 OCR
  - password（string, optional）— 加密文档密码
- `execute()` 逻辑：
  1. 从参数提取 file_path，检查文件存在性
  2. 路径经 is_protected_path() 检查（复用 D14 安全机制）
  3. 从全局实例获取 LiteParse，如果未初始化则返回错误提示
  4. 如果传入的参数与全局配置不同（如 ocr_language、max_pages），需要创建临时 LiteParseConfig 和 LiteParse 实例（因为 LiteParseConfig 是构造时确定的）。优化：如果参数与全局一致则复用全局实例，否则创建临时实例
  5. 调用 `parser.parse(file_path).await`
  6. 根据 format 参数选择输出：
     - markdown：拼接所有页的 markdown 字段
     - text：使用 result.text 全文
     - json：序列化 pages 数组（只含 page_number + text + text_items 数量，不含完整 text_items 避免 token 爆炸）
  7. 内容截断保护：如果输出超过 50000 字符，截断并标记 truncated=true
  8. 返回 JSON 结果：{ num_pages, format, content, truncated }

### 3.4 ScreenshotDocument 工具

结构体 `ScreenshotDocument`，impl Tool trait：

- `name()` 返回 "screenshot_document"
- `description()` 返回："Generate PNG screenshots of document pages. Useful for visual analysis of diagrams, charts, or scanned content."
- `parallel_safe()` 返回 false
- `parameters()` 返回 JSON Schema：
  - file_path（string, required）
  - page_range（string, optional）
  - dpi（integer, optional, default: from config）
  - output_dir（string, optional, default: 系统临时目录下的 rhermes-screenshots/ 子目录）
  - max_pages（integer, optional, default: 10）
- `execute()` 逻辑：
  1. 检查文件存在性和 protected_path
  2. 如果 output_dir 为空，创建临时目录 {std::env::temp_dir()}/rhermes-screenshots/{timestamp}/
  3. 调用 parser.screenshot(file_path).await
  4. 收集结果：每页返回 ScreenshotResult { page_num, width, height, image_bytes }
  5. 将 image_bytes 写入 output_dir/page_{n}.png
  6. 返回 JSON：{ screenshots: [{ page_num, path, width, height, size_bytes }], output_dir }

### 3.5 CheckDocumentComplexity 工具

结构体 `CheckDocumentComplexity`，impl Tool trait：

- `name()` 返回 "check_document_complexity"
- `description()` 返回："Quickly check if a document needs OCR or has complex layouts. Lightweight - does not extract text."
- `parallel_safe()` 返回 false
- `parameters()` 返回 JSON Schema：
  - file_path（string, required）
- `execute()` 逻辑：
  1. 检查文件存在性
  2. 调用 parser.is_complex(file_path).await
  3. 收集每页的 needs_ocr、reasons、text_coverage
  4. 汇总判断：全部 needs_ocr=false → "all_simple"；全部 true → "needs_ocr"；混合 → "mixed"
  5. 返回 JSON：{ pages: [...], summary: "all_simple"|"needs_ocr"|"mixed" }

### 3.6 错误处理

所有工具的 execute 方法中：
- 文件不存在 → `ToolError::ExecutionFailed("File not found: {path}")`
- 路径被保护 → `ToolError::ExecutionFailed("Access denied: protected path")`
- LiteParse 未初始化 → `ToolError::ExecutionFailed("LiteParse not initialized. Set [liteparse] enabled = true in config.")`
- 解析失败 → `ToolError::ExecutionFailed(e.to_string())`
- 输出内容用 `<untrusted>` 标签包裹（parse_document 的 content 字段）

---

## 任务 4: builtin.rs 注册工具

**文件**: `src/tools/builtin.rs`

**目标**: 将 3 个新工具注册到 builtin_registry

**改动要求**:
- 在 builtin_registry() 函数中（或 full_registry() 中），根据 config.liteparse.enabled 条件注册：
  - 如果 enabled 为 true，添加 ParseDocument、ScreenshotDocument、CheckDocumentComplexity 到 registry
  - 如果 enabled 为 false，跳过注册（工具不存在于系统中）
- 注意 builtin_registry 的签名，如果它不接受 config 参数，需要在 full_registry() 中注册（full_registry 已接受 config）

**注意**: 确保 3 个工具的 name() 不与现有工具冲突。parse_document / screenshot_document / check_document_complexity 都是全新名称。

---

## 任务 5: main.rs 初始化

**文件**: `src/main.rs`

**目标**: 在启动时初始化全局 LiteParse 实例

**改动要求**:
- 在 main() 函数中，Config 加载后、AgentSession 创建前，调用 `liteparse::init_liteparse(&config.liteparse)`
- 如果初始化失败，打印 warn 日志但不退出（degraded mode，工具调用时返回错误）
- 用条件判断：如果 config.liteparse.enabled 为 false，跳过初始化

---

## 任务 6: config.template.toml 新增配置段

**文件**: `config.template.toml` 或 `generate_annotated_template()` 函数

**目标**: 添加 [liteparse] 配置段

**改动要求**:
如果项目使用 D26 的动态模板生成，在 generate_annotated_template() 函数中添加 [liteparse] 段输出。内容：

```toml
[liteparse]
# 文档解析工具（PDF/DOCX/XLSX/PPTX/图片）
# 需要 pdfium 动态库（首次运行自动下载）
enabled = true
# OCR 语言（Tesseract 格式）
# 中文: chi_sim, 英文: eng, 中英混输: chi_sim+eng
ocr_language = "eng"
# 默认输出格式: markdown / text / json
default_format = "markdown"
# 最大页数（防止超大文档消耗过多 token）
max_pages = 50
# 渲染 DPI（截图和 OCR）
dpi = 150
# 是否启用 OCR（需要 tesseract 或远程 OCR 服务）
# 关闭则只提取原生文本，速度更快，90% 的 PDF 足够
ocr_enabled = false
# tessdata 目录路径（可选，默认从 TESSDATA_PREFIX 环境变量读取）
# tessdata_path = "/usr/share/tesseract-ocr/4.00/tessdata"
# 远程 OCR 服务 URL（可选，用于替代本地 tesseract）
# ocr_server_url = "http://localhost:8080/ocr"
```

---

## 实施顺序

1. **任务 1**（Cargo.toml）→ `cargo check` 验证依赖解析无冲突
2. **任务 2**（config.rs）→ `cargo check` 验证编译
3. **任务 3**（liteparse.rs）→ `cargo check` 验证编译
4. **任务 4**（builtin.rs）→ `cargo check` 验证注册
5. **任务 5**（main.rs）→ `cargo check` 验证初始化
6. **任务 6**（config template）→ `cargo run -- config init` 验证模板生成

每个任务完成后 `cargo check` 确认无编译错误。

## 验收标准

1. `cargo build --release` 成功
2. `cargo test` 全部通过
3. `rhermes` 启动后，Agent 能调用 `parse_document` 解析 PDF 文件并返回 Markdown
4. `rhermes` 启动后，Agent 能调用 `screenshot_document` 生成页面截图
5. `rhermes` 启动后，Agent 能调用 `check_document_complexity` 判断文档是否需要 OCR
6. `config init` 生成的模板包含 [liteparse] 段
7. 设置 `enabled = false` 后，3 个工具不注册到系统

## 注意事项

- LiteParse 的 `parse()` 是 async fn，Tool trait 的 execute 也是 async，直接 .await 即可
- PDFium 有进程全局锁（不是线程安全的），并发 parse 调用安全但实际串行执行。所以 parallel_safe 必须为 false
- LiteParseConfig 的 output_format 字段只在 LiteParse::new() 时生效。如果 parse_document 参数中的 format 与全局不同，需要创建临时 LiteParse 实例。优化建议：全局实例固定 output_format=Markdown，text 格式直接取 result.text，json 格式手动序列化 result.pages
- 不要在 Tool::execute 中做文件格式白名单检查——LiteParse 内部已做格式检测，不支持的格式会返回错误
- ScreenshotResult 的图片数据是 Vec<u8>（PNG bytes），需要写入文件后返回路径，不要尝试直接返回 base64（太大）
