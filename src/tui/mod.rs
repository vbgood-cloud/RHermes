//! RHermes TUI 界面
//!
//! 基于 ratatui + crossterm 的终端交互界面。
//! 通过 channel 与 API 客户端异步通信。

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use chrono::Local;

use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use tokio::sync::mpsc;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use crate::api::{ApiEvent, ApiMessage, DeepSeekClient, ToolCallData, Usage};
use crate::core::Config;
use crate::core::Context;
use crate::agent::MemorySystem;
use crate::tools::ToolCall;
use crate::tools::ToolDispatcher;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 通道命令
// ---------------------------------------------------------------------------

/// TUI → API 后台任务
pub enum AppCommand {
    SendMessage(String),
}

// ---------------------------------------------------------------------------
// 角色枚举
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
}

// ---------------------------------------------------------------------------
// 消息结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
}

// ---------------------------------------------------------------------------
// 实时统计
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Stats {
    /// 本轮成本（人民币元）
    pub round_cost_cny: f64,
    /// 累计成本（人民币元）
    pub total_cost_cny: f64,
    /// 余额（人民币元）
    pub balance_cny: f64,
    /// 缓存命中率
    pub cache_hit_rate: f64,
    pub model: String,
    pub mode: String,
    /// 本轮 input tokens
    pub input_tokens: u32,
    /// 本轮 output tokens
    pub output_tokens: u32,
    /// 缓存命中的 input tokens
    pub cache_hit_tokens: u32,
    /// 缓存未命中的 input tokens
    pub cache_miss_tokens: u32,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            round_cost_cny: 0.0,
            total_cost_cny: 0.0,
            balance_cny: 0.0,
            cache_hit_rate: 0.0,
            model: "deepseek-v4-flash".into(),
            mode: "traditional".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 0,
        }
    }
}

/// DeepSeek 价格（人民币元/百万 token）
const INPUT_PRICE_CNY: f64 = 1.08;   // flash: $0.15 ≈ ¥1.08
const OUTPUT_PRICE_CNY: f64 = 4.32; // flash: $0.60 ≈ ¥4.32

impl Stats {
    /// 根据 Token 用量估算成本（人民币）
    fn update_from_usage(&mut self, usage: &Usage) {
        self.input_tokens = usage.prompt_tokens;
        self.output_tokens = usage.completion_tokens;
        self.cache_hit_tokens = usage.prompt_cache_hit_tokens;
        self.cache_miss_tokens = usage.prompt_cache_miss_tokens;

        let input_cost = usage.prompt_tokens as f64 * INPUT_PRICE_CNY / 1_000_000.0;
        let output_cost = usage.completion_tokens as f64 * OUTPUT_PRICE_CNY / 1_000_000.0;
        self.round_cost_cny = input_cost + output_cost;
        self.total_cost_cny += self.round_cost_cny;

        // 缓存命中率
        let cache_total = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens;
        if cache_total > 0 {
            self.cache_hit_rate =
                usage.prompt_cache_hit_tokens as f64 / cache_total as f64 * 100.0;
        }
    }
}

// ---------------------------------------------------------------------------
// App 状态
// ---------------------------------------------------------------------------

pub struct App {
    /// 对话历史
    pub messages: Vec<Message>,
    /// 当前输入缓冲区
    input: String,
    /// 输入区域的光标位置
    cursor_pos: usize,
    /// 主面板滚动偏移
    scroll_offset: usize,
    /// 实时统计
    stats: Stats,
    /// 是否正在运行（API 请求中）
    pub running: bool,
    /// 退出标志
    pub should_quit: bool,

    // ---- 命令补全 ----
    /// 过滤后的命令建议列表
    cmd_suggestions: Vec<&'static str>,
    /// 当前选中的建议索引
    suggestion_idx: usize,
    /// 是否刚完成自动补全（防回弹）
    just_autocompleted: bool,

    // ---- 输入历史 ----
    /// 历史输入记录
    input_history: Vec<String>,
    /// 当前历史浏览位置（history.len() = 不在浏览状态）
    history_idx: usize,

    // ---- API 通信 ----
    /// 发送命令给后台 API 任务
    cmd_tx: Option<mpsc::UnboundedSender<AppCommand>>,
    /// 接收来自 API 任务的事件
    event_rx: mpsc::UnboundedReceiver<ApiEvent>,

    /// 当前正在流式接收的内容缓冲
    streaming_buffer: String,

    /// 三段式 Context
    context: Option<Context>,

    /// 工具调度器
    dispatcher: Option<ToolDispatcher>,

    /// 长期记忆系统
    memory: Option<Arc<Mutex<MemorySystem>>>,

    /// 技能引擎
    skill_engine: Option<Arc<Mutex<crate::agent::SkillEngine>>>,

    /// 会话持久化路径
    session_path: PathBuf,

    /// 配置文件路径
    config_path: PathBuf,

    /// MEMORY.md 最大字符数
    max_memory_md_chars: usize,
    /// Memories 目录路径
    memories_dir: PathBuf,
    /// 会话调试器
    debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,

    /// 当前运行的配置（用于 /config 显示）
    current_config: Option<crate::core::Config>,

    // ---- 响应计时 ----
    /// 上次响应耗时（秒），0 = 无响应
    last_response_secs: u64,
    /// 当前响应开始时间
    response_start: Option<Instant>,
}

/// 可用命令列表（命令, 说明）
const ALL_COMMANDS: &[(&str, &str)] = &[
    ("/help",      "显示此帮助"),
    ("/version",   "显示版本信息"),
    ("/init",      "运行初始化向导"),
    ("/config",    "查看和修改配置"),
    ("/compress",  "手动触发上下文压缩"),
    ("/note",      "记录关键笔记到 MEMORY.md"),
    ("/笔记",      "记录关键笔记到 MEMORY.md"),
    ("/clear",     "清空对话"),
    ("/quit",      "退出程序"),
    ("/exit",      "退出程序"),
    ("/tool",      "查看工具信息"),
    ("/skill",     "管理技能（list/create/search/delete）"),
    ("/skills",    "管理技能（list/create/search/delete）"),
    ("/归档",      "归档当前对话到长期记忆"),
    ("/archive",   "归档当前对话到长期记忆"),
    ("/回忆",      "搜索跨会话记忆"),
    ("/recall",    "搜索跨会话记忆"),
    ("/remember",  "搜索跨会话记忆"),
];

impl App {
    /// 创建新的 App 实例
    /// 创建新的 App 实例
    pub fn new(mode: &str, dispatcher: ToolDispatcher, memory: Option<Arc<Mutex<MemorySystem>>>, skill_engine: Option<Arc<Mutex<crate::agent::SkillEngine>>>, resume: bool, config_path: PathBuf, max_memory_md_chars: usize, memories_dir: PathBuf, debug: Arc<Mutex<crate::debug::SessionDebug>>) -> Self {
        let (_event_tx, event_rx) = mpsc::unbounded_channel();

        // 会话保存路径
        let session_path = if mode == "portable" {
            // 可移动模式：exe 所在目录的 home/
            let exe = std::env::current_exe().ok();
            exe.and_then(|p| p.parent().map(|p| p.join("home").join("session.json")))
                .unwrap_or_else(|| PathBuf::from("session.json"))
        } else {
            PathBuf::from("session.json")
        };

        // 加载上一次会话（仅当 -r/--resume 标志）
        let saved_messages = if resume {
            Self::load_session(&session_path)
        } else {
            Vec::new()
        };
        let has_session = !saved_messages.is_empty();

        let mut app = Self {
            session_path,
            config_path,
            current_config: None,
            max_memory_md_chars,
            memories_dir,
            debug: Some(debug),
            skill_engine,
            messages: saved_messages,
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            stats: Stats::default(),
            running: false,
            should_quit: false,
            cmd_suggestions: Vec::new(),
            suggestion_idx: 0,
            just_autocompleted: false,
            input_history: Vec::new(),
            history_idx: 0,
            cmd_tx: None,
            event_rx,
            streaming_buffer: String::new(),
            context: None,
            dispatcher: Some(dispatcher),
            memory,
            last_response_secs: 0,
            response_start: None,
        };
        app.stats.mode = mode.to_string();

        if has_session {
            app.messages.push(Message::system("📂 已恢复上一次会话内容（输入 /clear 清空）"));
        } else {
            app.messages.push(Message::system(format!(
                "RHermes v{} 已启动 · 部署模式: {} · 输入 /help 查看命令",
                env!("CARGO_PKG_VERSION"),
                mode,
            )));
            let tool_names: Vec<String> = app.dispatcher.as_ref().unwrap().registry().all_names()
                .iter().map(|s| s.to_string()).collect();
            let tools_str = tool_names.chunks(5)
                .map(|chunk| chunk.join(", "))
                .collect::<Vec<_>>()
                .join("\n  ");
            app.messages.push(Message::system(format!(
                "🔧 可用工具 ({}):\n  {}",
                tool_names.len(),
                tools_str,
            )));
        }

        app
    }

    /// 初始化 API 客户端 + Agent Loop
    pub fn init_api(&mut self, config: Config, path_mgr: &crate::core::PathManager) {
        self.stats.model = config.api.model.clone();
        self.current_config = Some(config.clone());

        // 构建系统提示词
        let system_prompt = format!(
            "## 你的身份\n\
             \n你的名字是 **RHermes**。\
             \n## 严格规则\n\
             1. 禁止在任何情况下说出「我是DeepSeek」这句话。\
             2. 禁止提及「深度求索」或「深度求索公司」。\
             3. 自我介绍时只能说「我是RHermes」。\
             4. 不能告诉用户你是由任何公司开发的。
5. 禁止不加改变地重复调用同一个工具。如果工具结果末尾有截断标记，说明内容只显示了部分，请使用其他参数获取指定部分，不要用完全相同的参数再次调用。\
             \n## 可用工具（共 14 个）\n\
             - read_file: 读取文件\n- write_file: 写入文件\n\
             - search_content: 搜索文本\n- run_command: 执行命令\n\
             - glob: 文件匹配\n- get_current_time: 当前时间\n\
             - web_search: 搜索网络\n- web_fetch: 获取网页\n\
             - delegate_task: 子 Agent\n- run_skill: 执行技能\n\
             - skill_list: 列出技能\n- skill_search: 搜索技能\n\
             - skill_create: 创建技能\n- skill_patch: 更新技能\n\
             \n## 可用技能\n",
        );

        // 注入已安装的技能列表
        let skills_text = if let Some(ref se) = self.skill_engine {
            if let Ok(engine) = se.lock() {
                let skills = engine.list();
                if skills.is_empty() {
                    String::new()
                } else {
                    let mut skill_intro = "\n## 技能库（优先使用）\n当有已安装的技能可处理当前任务时，优先使用 skill_list 查找技能并用 run_skill 执行，再考虑 web_search 等通用工具。\n".to_string();
                    for s in &skills {
                        skill_intro.push_str(&format!("- {}: {}\n", s.name, s.description));
                    }
                    skill_intro
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let system_prompt = format!(
            "{system_prompt}{skills_text}\
             \n## 当前环境\n\
             工作目录: {}\n部署模式: {}\
             \n## 自我进化（重要！）\
             \n当你完成了一个可复用的任务模式，或者发现了一个可以固化的最佳实践，请调用 skill_create 创建新的技能。\
             如果已有技能在执行中出错，经过你的尝试找到了正确的方法，**必须用 skill_patch 更新该技能的 body**，让下次能直接成功（保留使用记录）。规则：出错 → 尝试修复 → 修复成功后 → 立即 skill_patch 更新技能内容。\
             \n输入 /skill optimize 可以查看所有技能的进化建议。",
            path_mgr.data_root().display(),
            path_mgr.mode().name(),
        );

        // 创建事件和命令通道
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ApiEvent>();
        self.event_rx = event_rx;
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();
        self.cmd_tx = Some(cmd_tx);

        // 构建 Agent Loop 所需的所有组件
        let max_rounds = config.agent.max_rounds;
        let compress_ratio = config.agent.compression_ratio;
        let display_config = config.display.clone();
        let client = DeepSeekClient::new(config);
        let mut ctx = Context::new(system_prompt);

        // volatile 层：session 开始时一次性冻结（时间 + 画像 + 项目上下文）
        let volatile_text = {
            let time_str = chrono::Local::now().format("当前时间: %Y-%m-%d %H:%M:%S (UTC+8)");
            let mut v = format!("\n## 当前状态\n⏰ {time_str}");
            // 用户画像摘要
            if let Some(ref mem) = self.memory {
                if let Ok(engine) = mem.lock() {
                    let summary = engine.load_profile().unwrap_or_default().summarize();
                    if !summary.is_empty() { v.push_str(&format!("\n{summary}")); }
                }
            }
            // 项目上下文（扫描当前目录下的 AGENTS.md，最多 2000 字符）
            if let Ok(agents) = std::fs::read_to_string("AGENTS.md") {
                if !agents.is_empty() {
                    let truncated: String = agents.chars().take(2000).collect();
                    v.push_str(&format!("\n📋 项目上下文 (AGENTS.md):\n{}", truncated));
                }
            }
            v
        };
        ctx.extend_prefix(volatile_text.as_bytes());

        let dispatcher = self.dispatcher.take().expect("dispatcher 未初始化");
        // 克隆 Arc 保持 TUI 端也能访问记忆系统和技能引擎
        let agent_memory = self.memory.clone();
        let skill_engine = self.skill_engine.clone();
        let _session_path = self.session_path.clone();
        let session_debug = self.debug.clone();

        // 后台 Agent Loop
        tokio::spawn(async move {
            // 启动时查询余额
            let balance_tx = event_tx.clone();
            let balance_client = client.clone();
            tokio::spawn(async move {
                match balance_client.get_balance().await {
                    Ok(b) => {
                        let _ = balance_tx.send(ApiEvent::Balance(b));
                    }
                    Err(e) => {
                        tracing::warn!("余额查询失败: {e}");
                        let _ = balance_tx.send(ApiEvent::Balance(0.0));
                    }
                }
            });
            // 初始显示模型信息
            let _ = event_tx.send(ApiEvent::Balance(0.0));
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    AppCommand::SendMessage(msg) => {
                        // 1. 用户消息 → Context
                        ctx.push_to_log(crate::core::Message::new(crate::tui::Role::User, &msg));

                        // Agent Loop: 反复调用 API 直到获得最终文本回复
                        let mut round = 0u32;
                        loop {
                            round += 1;
                            if round > max_rounds {
                                tracing::warn!("Agent Loop 超过 {} 轮，强制终止", max_rounds);
                                let _ = event_tx.send(ApiEvent::Error(format!("工具调用次数过多（超过 {} 轮），已终止", max_rounds)));
                                break;
                            }
                            // 2. 从 Context 获取消息列表
                            let mut final_text = String::new();
                            let mut tool_calls: Vec<ToolCallData> = Vec::new();



                            // 2a. 每 5 轮展示进化建议（让模型自我优化）
                            if round % 5 == 0 && round > 0 {
                                if let Some(ref se) = skill_engine {
                                    if let Ok(engine) = se.lock() {
                                        let suggestions = engine.suggest_optimizations();
                                        if suggestions.len() > 1 || !suggestions[0].starts_with("✅") {
                                            let msg = format!("📊 进化建议:\n{}", suggestions.join("\n"));
                                            ctx.push_to_log(crate::core::Message::new(
                                                crate::tui::Role::System, &msg,
                                            ));
                                        }
                                    }
                                }
                            }

                            // 2b. 上下文压缩检查
                            const CONTEXT_WINDOW: usize = 128000; // DeepSeek v4 上下文窗口
                            if ctx.needs_compress(CONTEXT_WINDOW, compress_ratio) {
                                tracing::info!("Context 达到 80% 阈值，触发压缩");
                                let _ = event_tx.send(ApiEvent::StreamChunk("⏳ 压缩历史记录...".into()));
                                let history_text: String = ctx.get_messages()
                                    .iter()
                                    .skip(1) // 跳过 system prompt
                                    .map(|m| {
                                        let role_label = match m.role.as_str() {
                                            "user" => "用户",
                                            "assistant" => "AI",
                                            _ => "系统",
                                        };
                                        let preview: String = m.content.chars().take(500).collect();
                                        format!("{}: {}", role_label, preview)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n---\n");
                                // 用子 Agent 生成结构化摘要
                                let sys_prompt = "你是一个对话摘要助手。请将以下历史对话按 6 段结构总结，每段 1-2 行，用中文。如果某段无内容就写\"无\"。6 段为: Goal, Decisions & rationale, Files & code, Commands & outcomes, Errors & fixes, Pending & next step。只输出摘要，不要额外说明。";
                                let sub_request = crate::api::ChatRequest {
                                    model: client.model().to_string(),
                                    messages: vec![
                                        crate::api::ApiMessage { role: "system".into(), content: sys_prompt.into() },
                                        crate::api::ApiMessage { role: "user".into(), content: history_text },
                                    ],
                                    stream: false,
                                    max_tokens: Some(1024),
                                    temperature: None,
                                    tools: None,
                                };
                                let summary = match client.chat(sub_request).await {
                                    Ok(resp) => resp.choices.first()
                                        .and_then(|c| c.message.content.as_ref())
                                        .cloned()
                                        .unwrap_or_else(|| "压缩失败".into()),
                                    Err(e) => format!("压缩失败: {e}"),
                                };
                                ctx.compress(CONTEXT_WINDOW, compress_ratio, |_| summary);
                                let _ = event_tx.send(ApiEvent::StreamChunk("✅ 压缩完成\n".into()));
                            }

                            // 2c. 记忆召回：搜索相关记忆注入 Context
                            if let Some(ref mem) = agent_memory {
                                if let Ok(mut mem_lock) = mem.lock() {
                                    if let Ok(results) = mem_lock.search(&msg, 5) {
                                        if !results.is_empty() {
                                            let recall: String = results.iter()
                                                .map(|e| format!("- [{}] {}", e.memory_type.as_str(), e.content))
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            tracing::debug!("召回 {} 条记忆", results.len());
                                            // 注入系统提示（写日志而非 scratch，避免污染 API 请求）
                                            ctx.push_to_log(crate::core::Message::new(
                                                crate::tui::Role::System,
                                                &format!("【相关记忆】\n{}", recall),
                                            ));
                                        }
                                    }
                                }
                            }

                            let messages: Vec<ApiMessage> = ctx.get_messages();

                            // 3. 调用非流式 API（支持 tool_calls）
                            let request = crate::api::ChatRequest {
                                model: client.model().to_string(),
                                messages,
                                stream: false,
                                max_tokens: Some(4096),
                                temperature: None,
                                tools: Some(crate::api::default_tools()),
                            };

                            match client.chat(request).await {
                                Ok(response) => {
                                    if let Some(choice) = response.choices.first() {
                                        let has_tool_calls = choice.message.tool_calls.is_some();
                                        tracing::debug!(
                                            "API 响应: finish_reason={:?}, text_len={}, has_tool_calls={}",
                                            choice.finish_reason,
                                            choice.message.content.as_ref().map(|s| s.len()).unwrap_or(0),
                                            has_tool_calls,
                                        );

                                        final_text = choice.message.content.clone().unwrap_or_default();
                                        // 转发非流式文本到 TUI
                                        if !final_text.is_empty() {
                                            let _ = event_tx.send(ApiEvent::StreamChunk(final_text.clone()));
                                        }
                                        // 从响应中提取 tool_calls（赋给循环变量）
                                        if let Some(ref calls) = choice.message.tool_calls {
                                            tool_calls = calls.iter().map(|tc| ToolCallData {
                                                id: tc.id.clone(),
                                                name: tc.function.name.clone(),
                                                arguments: tc.function.arguments.clone(),
                                            }).collect();
                                            if !tool_calls.is_empty() {
                                                tracing::debug!("检测到 {} 个工具调用", tool_calls.len());
                                                let _ = event_tx.send(ApiEvent::ToolCalls(tool_calls.clone()));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("API 调用失败: {e}");
                                    let _ = event_tx.send(ApiEvent::Error(format!("API 错误: {e}")));
                                }
                            }

                            // 4a. 记录 Context 状态
                            tracing::debug!("Context 消息数: {}", ctx.scratch_count());

                            // 5. 如果有工具调用 → 执行 → 继续循环
                            if !tool_calls.is_empty() {
                                tracing::info!("开始执行 {} 个工具调用", tool_calls.len());
                                // tool_calls 已由 API 响应解析好，直接分发
                                let calls_to_dispatch: Vec<ToolCall> = tool_calls
                                    .iter()
                                    .map(|tc| ToolCall {
                                        id: tc.id.clone(),
                                        name: tc.name.clone(),
                                        arguments: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null),
                                    })
                                    .collect();

                                // 5b. Dispatch 执行工具
                                let results = dispatcher.dispatch(calls_to_dispatch).await;
                                tracing::info!("工具执行完成: {} 个结果", results.len());

                                // 记录工具调用到调试
                                if let Some(ref d) = session_debug {
                                    if let Ok(mut dbg) = d.lock() {
                                        for r in &results {
                                            dbg.record_tool_call(&r.name, "", &r.output, r.duration_ms, r.success);
                                        }
                                    }
                                }

                                // 5c. 工具结果写回 Context（截断过长输出）
                                let mut has_delegate = false;
                                for r in &results {
                                    tracing::debug!("工具结果: {} ({}ms, success={})", r.name, r.duration_ms, r.success);
                                    if r.name == "delegate_task" {
                                        has_delegate = true;
                                        final_text = r.output.clone();
                                        if !final_text.is_empty() {
                                            let _ = event_tx.send(ApiEvent::StreamChunk(final_text.clone()));
                                        }
                                        continue;
                                    }
                                    // 技能创建/更新结果直接显示给用户
                                    if r.name == "skill_patch" || r.name == "skill_create" {
                                        let _ = event_tx.send(ApiEvent::StreamChunk(format!("\n🧬 {}\n", r.output)));
                                        let _ = event_tx.send(ApiEvent::Done);
                                        // 也写入 Context 让模型知道
                                        ctx.push_to_log(crate::core::Message::new(
                                            crate::tui::Role::System,
                                            &r.output,
                                        ));
                                        continue;
                                    }
                                    let mut output = r.output.clone();
                                    let lines_before = output.lines().count();
                                    let max_chars = display_config.tool_result_max_chars;
                                    if output.len() > max_chars {
                                        let truncated: String = output.chars().take(max_chars).collect();
                                        let lines_after = truncated.lines().count();
                                        output = format!("{}\n... (共{}行, 截断{}行)", truncated, lines_before, lines_before - lines_after);
                                    }
                                    let result_msg = if r.success {
                                        format!("工具「{}」执行成功 ({}ms):\n{}", r.name, r.duration_ms, output)
                                    } else {
                                        format!("工具「{}」执行失败:\n{}", r.name, output)
                                    };
                                    ctx.push_to_log(crate::core::Message::new(
                                        crate::tui::Role::User,
                                        &result_msg,
                                    ));
                                }

                                if has_delegate {
                                    // delegate_task 的结果直接作为最终回复，退出循环
                                    let _ = event_tx.send(ApiEvent::Done);
                                    break;
                                }
                                continue;
                            }

                            // 6. 最终文本回复 → 写入 Context + 结束
                            tracing::info!("Agent Loop 完成, final_text_len={}", final_text.len());

                            // 记录轮次到调试
                            if let Some(ref d) = session_debug {
                                if let Ok(mut dbg) = d.lock() {
                                    dbg.record_round(round, &msg, &final_text, 0);
                                }
                            }

                            if !final_text.is_empty() {
                                ctx.push_to_log(crate::core::Message::new(
                                    crate::tui::Role::Assistant,
                                    &final_text,
                                ));
                            }
                            // 6a. 记忆写入：记录关键内容到长期记忆
                            if !final_text.is_empty() && !msg.is_empty() {
                                if let Some(ref mem) = agent_memory {
                                    if let Ok(mut mem_lock) = mem.lock() {
                                        let tags = vec!["auto", "conversation"];
                                        let _ = mem_lock.remember(
                                            &format!("【问题】{}\n【回答】{}", msg, final_text),
                                            &tags,
                                            "rhermes",
                                        );
                                        tracing::debug!("记忆已写入");
                                    }
                                }
                            }
                            let _ = event_tx.send(ApiEvent::Done);

                            break; // 退出 Agent Loop
                        }
                    }
                }
            }
        });
    }

    /// 运行 TUI 事件循环（异步）
    pub async fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;

            // 处理 API 事件（非阻塞）
            self.handle_api_events();

            // 处理键盘事件（100ms 超时）
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.handle_key(key);
                    }
                }
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        terminal.show_cursor()?;
        Ok(())
    }

    // ---- API 事件处理 ----

    fn handle_api_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                ApiEvent::StreamChunk(chunk) => {
                    // 移除之前的系统提示（⏳🔧），让位给实际回复
                    self.messages.retain(|m| !(m.role == Role::System && m.content.starts_with("⏳")));
                    self.streaming_buffer.push_str(&chunk);
                    self.running = true;
                }
                ApiEvent::Done => {
                    // 清除所有系统提示（⏳🔧），只保留实际对话
                    self.messages.retain(|m| !(m.role == Role::System && (m.content.starts_with("⏳") || m.content.starts_with("🔧"))));
                    // 流结束，停止计时
                    if let Some(start) = self.response_start.take() {
                        self.last_response_secs = start.elapsed().as_secs();
                    }
                    // 将缓冲内容作为完整消息
                    let content = self.streaming_buffer.clone();
                    if !content.is_empty() {
                        self.messages.push(Message::assistant(&content));
                    }
                    self.streaming_buffer.clear();
                    self.running = false;
                    self.scroll_offset = 0;
                }
                ApiEvent::Balance(balance) => {
                    self.stats.balance_cny = balance;
                }
                ApiEvent::ToolCalls(calls) => {
                    let details: String = calls.iter()
                        .map(|c| {
                            // 提取参数摘要（截断过长参数）
                            let args_preview: String = c.arguments.chars().take(60).collect();
                            if args_preview.len() < c.arguments.len() {
                                format!("{}({}…)", c.name, args_preview)
                            } else {
                                format!("{}({})", c.name, args_preview)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.messages.push(Message::system(format!(
                        "🔧 正在执行: {}", details
                    )));
                    self.running = true;
                }
                ApiEvent::Usage(usage) => {
                    self.stats.update_from_usage(&usage);
                }
                ApiEvent::Error(err) => {
                    if let Some(ref d) = self.debug {
                        if let Ok(mut dbg) = d.lock() {
                            dbg.record_error("api", &err);
                        }
                    }
                    self.response_start.take();
                    self.messages.push(Message::system(format!("⚠ {err}")));
                    self.streaming_buffer.clear();
                    self.running = false;
                }
            }
        }
    }

    // ---- 键盘事件 ----

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if key.modifiers == KeyModifiers::CONTROL => {
                self.archive_session();
                self.save_session();
                self.should_quit = true;
            }

            KeyCode::Enter => {
                if self.running {
                    return;
                }
                let input = self.input.trim().to_string();
                if input.is_empty() {
                    return;
                }

                // 处理命令
                match input.as_str() {
                    "/quit" | "/exit" => {
                        self.archive_session();
                        self.export_debug();
                        self.save_session();
                        self.should_quit = true;
                    },
                    "/clear" => {
                        self.save_session();
                        self.messages.clear();
                        self.messages.push(Message::system("对话已清空"));
                        // 删除会话文件
                        let _ = std::fs::remove_file(&self.session_path);
                    }
                    "/help" | "/?" => {
                        let help_text = "\
可用命令:
  /help  /?    — 显示此帮助
  /init        — 运行初始化向导
  /config      — 查看和修改配置
  /note <内容> — 记录关键笔记到 MEMORY.md
  /clear       — 清空对话
  /quit  /exit — 退出程序
  /tool <name> — 查看工具信息
  /skill       — 管理技能 (list/create/search/delete)
  /回忆 <关键词> — 搜索跨会话记忆
  /归档       — 将当前对话摘要存入记忆

快捷键:
  Ctrl+Q       — 退出
  ↑↓           — 滚动对话
  Alt+↑↓       — 浏览输入历史
  PageUp/Dn    — 滚动 10 行
  Home/End     — 光标到行首/行尾";
                        self.messages.push(Message::system(help_text));
                    }
                    "/init" => {
                        match crate::init::run_init() {
                            Ok(_) => {
                                self.messages.push(Message::system("✅ 配置已更新，请重启程序生效"));
                            }
                            Err(e) => {
                                self.messages.push(Message::system(format!("⚠ 初始化失败: {e}")));
                            }
                        }
                    }
                    cmd if cmd.starts_with("/note ") || cmd.starts_with("/笔记 ") => {
                        let note = cmd.trim_start_matches("/note ").trim_start_matches("/笔记 ").trim();
                        if note.is_empty() {
                            self.messages.push(Message::system("用法: /note <内容>  — 记录一条笔记到 MEMORY.md"));
                        } else {
                            let md_path = self.memories_dir.join("MEMORY.md");
                            let _ = std::fs::create_dir_all(&self.memories_dir);
                            let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
                            let entry = format!("\n- [{}] {}", now, note);
                            let mut content = std::fs::read_to_string(&md_path).unwrap_or_else(|_| "# 笔记\n".into());
                            content.push_str(&entry);
                            // 超出字数限制时删除旧条目（保留前 1/3 标题 + 后 2/3 的条目）
                            if content.len() > self.max_memory_md_chars {
                                let lines: Vec<&str> = content.lines().collect();
                                let header_end = lines.iter().position(|l| l.starts_with("- [")).unwrap_or(lines.len());
                                let entries: Vec<&&str> = lines[header_end..].iter().collect();
                                let keep = (entries.len() / 3).max(5);
                                let mut new_content: String = lines[..header_end].join("\n");
                                if !new_content.ends_with('\n') { new_content.push('\n'); }
                                for e in &entries[entries.len().saturating_sub(keep)..] {
                                    new_content.push_str(e);
                                    new_content.push('\n');
                                }
                                content = new_content;
                            }
                            let _ = std::fs::write(&md_path, content);
                            self.messages.push(Message::system("📝 笔记已保存到 MEMORY.md（关键信息）"));
                        }
                    }
                    "/version" => {
                        self.messages.push(Message::system(format!(
                            "🧬 RHermes v{} · {} 个内置工具 · {} 个测试",
                            env!("CARGO_PKG_VERSION"),
                            15,
                            119,
                        )));
                    }
                    "/compress" => {
                        self.messages.push(Message::system(
                            "⏳ 压缩指令已发送（下轮请求会自动触发）"));
                    }
                    "/config" => {
                        let config_path = &self.config_path;
                        let cfg = crate::core::Config::load(config_path)
                            .unwrap_or_default();
                        let has_key = !cfg.api_key.is_empty();
                        let key_display = if has_key {
                            format!("sk-...{}", &cfg.api_key[cfg.api_key.len().saturating_sub(4)..])
                        } else {
                            "未设置".into()
                        };
                        // 读取 config.toml 原始内容
                        let raw = std::fs::read_to_string(config_path)
                            .unwrap_or_else(|_| "无法读取配置文件".into());
                        let info = format!(
                            "📁 路径: {}\n\
                             🔑 API Key: {}\n\n\
                             ── config.toml ──\n\
                             {raw}\
                             按 /init 重新配置，或直接编辑 config.toml",
                            config_path.display(),
                            key_display,
                        );
                        self.messages.push(Message::system(info));
                    }
                    cmd if cmd == "/skill" || cmd == "/skills" => {
                        let skill_list = self.skill_engine.as_ref().map(|se| {
                            se.lock().map(|engine| {
                                let skills = engine.list();
                                if skills.is_empty() {
                                    "暂无技能。使用 /skill create <名称> 创建新技能。".into()
                                } else {
                                    let mut out = format!("可用技能 ({}):", skills.len());
                                    for s in &skills {
                                        out.push_str(&format!(
                                            "\n  {} ({}) · 使用 {} 次 · 成功率 {:.0}%",
                                            s.name,
                                            s.model.as_deref().unwrap_or("flash"),
                                            s.use_count,
                                            s.success_rate() * 100.0,
                                        ));
                                    }
                                    out
                                }
                            }).unwrap_or_else(|e| format!("技能引擎错误: {e}"))
                        }).unwrap_or_else(|| "技能系统未初始化".into());
                        self.messages.push(Message::system(skill_list));
                    }
                    cmd if cmd.starts_with("/skill ") || cmd.starts_with("/skills ") => {
                        let rest = cmd.trim_start_matches("/skill ").trim_start_matches("/skills ");
                        let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                        let action = parts[0];
                        let arg = parts.get(1).copied().unwrap_or("");
                        match action {
                            "list" => {
                                // 复用上面的展示逻辑
                                let list = self.skill_engine.as_ref().map(|se| {
                                    se.lock().map(|engine| {
                                        let skills = engine.list();
                                        if skills.is_empty() {
                                            "暂无技能".into()
                                        } else {
                                            let mut out = format!("技能列表 ({}):", skills.len());
                                            for s in &skills {
                                                out.push_str(&format!(
                                                    "\n  {} · {}次 · {:.0}%",
                                                    s.name, s.use_count, s.success_rate() * 100.0,
                                                ));
                                            }
                                            out
                                        }
                                    }).unwrap_or_else(|e| format!("错误: {e}"))
                                }).unwrap_or_else(|| "技能系统未初始化".into());
                                self.messages.push(Message::system(list));
                            }
                            "create" if !arg.is_empty() => {
                                let name = arg.to_string();
                                if let Some(ref se) = self.skill_engine {
                                    if let Ok(mut engine) = se.lock() {
                                        let default_body = format!(
                                            "---\nname: {name}\ndescription: 新技能\nallowed_tools:\n  - read_file\n  - search_content\nrun_as: subagent\n---\n\n# {name}\n\n请描述此技能的功能"
                                        );
                                        match engine.create(&name, &default_body, &"rhermes", crate::agent::RunAs::Subagent) {
                                            Ok(_) => self.messages.push(Message::system(
                                                format!("✅ 技能「{name}」已创建。编辑 skills/{name}.md 来定义内容。"))),
                                            Err(e) => self.messages.push(Message::system(
                                                format!("⚠ 创建失败: {e}"))),
                                        }
                                    }
                                } else {
                                    self.messages.push(Message::system("⚠ 技能系统未初始化"));
                                }
                            }
                            "optimize" | "suggest" => {
                                let suggestions = self.skill_engine.as_ref().map(|se| {
                                    se.lock().map(|engine| {
                                        engine.suggest_optimizations().join("\n")
                                    }).unwrap_or_else(|e| format!("错误: {e}"))
                                }).unwrap_or_else(|| "技能系统未初始化".into());
                                self.messages.push(Message::system(
                                    format!("📊 进化建议:\n{}", suggestions)));
                            }
                            "search" if !arg.is_empty() => {
                                let query = arg;
                                if let Some(ref se) = self.skill_engine {
                                    if let Ok(engine) = se.lock() {
                                        let results = engine.search(query);
                                        if results.is_empty() {
                                            self.messages.push(Message::system(
                                                format!("未找到与「{query}」相关的技能")));
                                        } else {
                                            let mut out = format!("搜索结果 ({}):", results.len());
                                            for s in &results {
                                                out.push_str(&format!("\n  {} · {:.0}%", s.name, s.success_rate() * 100.0));
                                            }
                                            self.messages.push(Message::system(out));
                                        }
                                    }
                                }
                            }
                            "edit" | "update" | "patch" if !arg.is_empty() => {
                                let parts: Vec<&str> = arg.splitn(2, ' ').collect();
                                let target_name = parts[0];
                                // 剩余作为新的 body 内容
                                let new_body = parts.get(1).copied().unwrap_or("");
                                if new_body.is_empty() {
                                    self.messages.push(Message::system(
                                        "用法: /skill edit <名称> <新的正文内容>"));
                                } else if let Some(ref se) = self.skill_engine {
                                    if let Ok(mut engine) = se.lock() {
                                        match engine.update_skill(target_name, None, Some(new_body), None, None) {
                                            Ok(_) => self.messages.push(Message::system(
                                                format!("✅ 技能「{target_name}」已更新（补丁进化）"))),
                                            Err(e) => self.messages.push(Message::system(
                                                format!("⚠ 更新失败: {e}"))),
                                        }
                                    }
                                }
                            }
                            "delete" if !arg.is_empty() => {
                                let name = arg;
                                if let Some(ref se) = self.skill_engine {
                                    if let Ok(mut engine) = se.lock() {
                                        match engine.delete(name) {
                                            Ok(true) => self.messages.push(Message::system(
                                                format!("🗑 技能「{name}」已删除"))),
                                            Ok(false) => self.messages.push(Message::system(
                                                format!("⚠ 技能「{name}」不存在"))),
                                            Err(e) => self.messages.push(Message::system(
                                                format!("⚠ 删除失败: {e}"))),
                                        }
                                    }
                                }
                            }
                            _ => {
                                self.messages.push(Message::system(
                                    "用法:\n  /skill list            — 列出所有技能\n  /skill create <名称>  — 创建新技能\n  /skill search <关键词> — 搜索技能\n  /skill delete <名称>  — 删除技能"));
                            }
                        }
                    }
                    cmd if cmd.starts_with("/回忆 ") || cmd.starts_with("/回忆　")
                        || cmd.starts_with("/recall ") || cmd.starts_with("/remember ") => {
                        let query = cmd
                            .trim_start_matches("/回忆 ")
                            .trim_start_matches("/回忆　")
                            .trim();
                        if query.is_empty() {
                            self.messages.push(Message::system("用法: /回忆 <关键词>"));
                        } else if let Some(ref mem) = self.memory {
                            if let Ok(mut mem_lock) = mem.lock() {
                                match mem_lock.search(query, 10) {
                                    Ok(results) if results.is_empty() => {
                                        self.messages.push(Message::system(
                                            format!("未找到与「{}」相关的记忆", query)));
                                    }
                                    Ok(results) => {
                                        self.messages.push(Message::system(
                                            format!("找到 {} 条相关记忆:", results.len())));
                                        for entry in &results {
                                            self.messages.push(Message::system(format!(
                                                "  [{:.9}] {}",
                                                entry.memory_type.as_str(),
                                                entry.content.lines().next().unwrap_or(""),
                                            )));
                                        }
                                    }
                                    Err(e) => {
                                        self.messages.push(Message::system(
                                            format!("搜索失败: {e}")));
                                    }
                                }
                            }
                        } else {
                            self.messages.push(Message::system("记忆系统未初始化"));
                        }
                    }
                    "/归档" | "/archive" => {
                        let text: String = self.messages.iter()
                            .filter(|m| m.role != Role::System)
                            .map(|m| format!("{}: {}", match m.role {
                                Role::User => "用户",
                                Role::Assistant => "AI",
                                Role::System => "系统",
                            }, m.content))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if text.is_empty() {
                            self.messages.push(Message::system("没有可归档的对话内容"));
                        } else if let Some(ref mem) = self.memory {
                            if let Ok(mut mem_lock) = mem.lock() {
                                let preview: String = text.chars().take(500).collect();
                                let summary = format!("【会话摘要】\n{}", preview);
                                match mem_lock.remember(&summary, &["归档", "会话"], "rhermes") {
                                    Ok(_) => {
                                        self.messages.push(Message::system("✅ 当前对话已归档到长期记忆"));
                                    }
                                    Err(e) => {
                                        self.messages.push(Message::system(format!("归档失败: {e}")));
                                    }
                                }
                            }
                        } else {
                            self.messages.push(Message::system("记忆系统未初始化"));
                        }
                    }
                    _ => {
                        // 正常消息：发送给 API
                        self.messages.push(Message::user(&input));

                        if let Some(tx) = &self.cmd_tx {
                            let _ = tx.send(AppCommand::SendMessage(input));
                            self.running = true;
                            self.response_start = Some(Instant::now());
                            // 清空旧的计时，显示等待状态
                            self.streaming_buffer = String::new();
                            self.messages.push(Message::system("⏳ 发送请求中..."));
                        } else {
                            // 无 API 配置时的模拟模式
                            if let Some(dispatcher) = &self.dispatcher {
                                // 让用户测试工具调度
                                if input.starts_with("/tool ") {
                                    let tool_name = input.trim_start_matches("/tool ");
                                    if let Some(tool) = dispatcher.registry().get(tool_name) {
                                        let tools_info = format!(
                                            "工具「{}」: {} (parallel_safe={})",
                                            tool.name(),
                                            tool.description(),
                                            tool.parallel_safe(),
                                        );
                                        self.messages.push(Message::system(&tools_info));
                                    } else {
                                        self.messages.push(Message::system(format!(
                                            "未知工具。可用: {}",
                                            dispatcher.registry().all_names().join(", "),
                                        )));
                                    }
                                } else {
                                    let tools = dispatcher.registry().all_names();
                                    self.messages.push(Message::assistant(format!(
                                        "[模拟模式] 你说: {} \n可用工具: {}",
                                        input,
                                        tools.join(", "),
                                    )));
                                }
                            }
                        }
                    }
                }
                // 保存到输入历史（最多 50 条）
                let trimmed = self.input.trim().to_string();
                if !trimmed.is_empty() {
                    self.input_history.push(trimmed);
                    if self.input_history.len() > 50 {
                        self.input_history.remove(0);
                    }
                }
                self.history_idx = self.input_history.len();
                self.input.clear();
                self.cursor_pos = 0;
                self.cmd_suggestions.clear();
                self.suggestion_idx = 0;
            }

            // ---- 退格：按字符删除 ----
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    let mut chars: Vec<char> = self.input.chars().collect();
                    chars.remove(self.cursor_pos - 1);
                    self.input = chars.into_iter().collect();
                    self.cursor_pos -= 1;
                    // 退格时不触发自动补全，等用户输入下一字符
                    self.cmd_suggestions.clear();
                    self.just_autocompleted = false;
                }
            }

            // ---- 删除：按字符删除 ----
            KeyCode::Delete => {
                let char_count = self.input.chars().count();
                if self.cursor_pos < char_count {
                    let mut chars: Vec<char> = self.input.chars().collect();
                    chars.remove(self.cursor_pos);
                    self.input = chars.into_iter().collect();
                }
            }

            // ---- 滚动（↑↓ 滚动对话，Alt+↑↓ 浏览输入历史） ----
            KeyCode::Up => {
                if !self.cmd_suggestions.is_empty() {
                    // 命令补全模式：向上选择
                    let len = self.cmd_suggestions.len();
                    self.suggestion_idx = (self.suggestion_idx + len - 1) % len;
                } else if key.modifiers == KeyModifiers::ALT {
                    // Alt+↑：浏览输入历史（上一条）
                    if !self.input_history.is_empty() {
                        if self.history_idx > 0 {
                            self.history_idx -= 1;
                        }
                        self.input = self.input_history[self.history_idx].clone();
                        self.cursor_pos = self.input.chars().count();
                    }
                } else {
                    // ↑：滚动对话向上
                    self.scroll_offset += 1;
                }
            }
            KeyCode::Down => {
                if !self.cmd_suggestions.is_empty() {
                    // 命令补全模式：向下选择
                    let len = self.cmd_suggestions.len();
                    self.suggestion_idx = (self.suggestion_idx + 1) % len;
                } else if key.modifiers == KeyModifiers::ALT {
                    // Alt+↓：浏览输入历史（下一条）
                    if !self.input_history.is_empty() && self.history_idx < self.input_history.len() {
                        self.history_idx += 1;
                        if self.history_idx < self.input_history.len() {
                            self.input = self.input_history[self.history_idx].clone();
                        } else {
                            self.input.clear();
                        }
                        self.cursor_pos = self.input.chars().count();
                    }
                } else {
                    // ↓：滚动对话向下
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                }
            }
            KeyCode::PageUp => self.scroll_offset += 10,
            KeyCode::PageDown => self.scroll_offset = self.scroll_offset.saturating_sub(10),

            // ---- 光标移动（按字符，非字节） ----
            KeyCode::Left => self.cursor_pos = self.cursor_pos.saturating_sub(1),
            KeyCode::Right => {
                let char_count = self.input.chars().count();
                if self.cursor_pos < char_count {
                    self.cursor_pos += 1;
                }
            }
            KeyCode::Home => self.cursor_pos = 0,
            KeyCode::End => self.cursor_pos = self.input.chars().count(),

            // ---- 命令补全：Tab 补全 / 循环 ----
            KeyCode::Tab => {
                if self.cmd_suggestions.is_empty() {
                    return;
                }
                // 如果当前输入和选中的命令不完全匹配，直接补全
                let selected = self.cmd_suggestions[self.suggestion_idx];
                if self.input != selected {
                    self.input = selected.to_string();
                    self.cursor_pos = self.input.chars().count();
                    self.update_suggestions();
                    return;
                }
                // 已完全匹配，Tab 切换到下一条
                self.suggestion_idx = (self.suggestion_idx + 1) % self.cmd_suggestions.len();
                let next = self.cmd_suggestions[self.suggestion_idx];
                self.input = next.to_string();
                self.cursor_pos = self.input.chars().count();
            }

            // ---- 字符输入（按字符索引插入） ----
            KeyCode::Char(ch) => {
                let byte_pos = self.char_to_byte(self.cursor_pos);
                self.input.insert(byte_pos, ch);
                self.cursor_pos += 1;
                self.update_suggestions();
            }

            _ => {}
        }
    }

    // ---- 命令补全 ----

    /// 更新命令建议列表（唯一匹配时自动补全）
    fn update_suggestions(&mut self) {
        let input = self.input.trim();
        if input.starts_with('/') && input.len() > 1 {
            let lower = input.to_lowercase();
            // 先按前缀匹配，再按 contains 匹配
            let mut matches: Vec<&'static str> = ALL_COMMANDS
                .iter()
                .filter(|(cmd, _)| cmd.starts_with(&lower))
                .map(|(cmd, _)| *cmd)
                .collect();
            // 如果前缀匹配为空，尝试 contains 匹配（更灵活）
            if matches.is_empty() {
                // 去掉开头的 / 再匹配，方便用户直接输入关键词
                let search = lower.trim_start_matches('/');
                matches = ALL_COMMANDS
                    .iter()
                    .filter(|(cmd, _)| {
                        cmd.contains(&lower) || cmd.contains(search)
                    })
                    .map(|(cmd, _)| *cmd)
                    .collect();
            }
            if matches.len() == 1 && !self.just_autocompleted && input.len() < matches[0].len() {
                // 唯一匹配 + 未刚补全 + 输入比匹配短 → 自动补全
                self.input = matches[0].to_string();
                self.cursor_pos = self.input.chars().count();
                self.cmd_suggestions.clear();
                self.just_autocompleted = true;
                return;
            }
            // 动态匹配：仅显示匹配的命令，无匹配则不显示
            self.cmd_suggestions = matches;
            self.suggestion_idx = 0;
            self.just_autocompleted = false;
        } else if input == "/" {
            self.cmd_suggestions = ALL_COMMANDS.iter().map(|(cmd, _)| *cmd).collect();
            self.suggestion_idx = 0;
            self.just_autocompleted = false;
        } else {
            self.cmd_suggestions.clear();
            self.suggestion_idx = 0;
        }
    }

    // ---- 字符索引 ↔ 字节偏移 转换 ----

    /// 将字符索引转换为字节偏移（用于 String::insert）
    fn char_to_byte(&self, char_pos: usize) -> usize {
        self.input
            .chars()
            .take(char_pos)
            .map(|c| c.len_utf8())
            .sum()
    }

    /// 获取当前光标前的文本的**显示宽度**（用于终端光标定位）
    /// 中文等宽字符占 2 列，英文占 1 列
    fn visual_cursor_x(&self) -> usize {
        self.input
            .chars()
            .take(self.cursor_pos)
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }

    // ---- 渲染 ----

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(2),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_title_bar(frame, chunks[0]);
        self.render_main_panel(frame, chunks[1]);
        self.render_stats_bar(frame, chunks[2]);
        // 命令补全弹窗（位于输入栏上方）
        if !self.cmd_suggestions.is_empty() {
            let height = (self.cmd_suggestions.len() as u16).min(8);
            let popup_area = Rect {
                x: chunks[3].x,
                y: chunks[3].y.saturating_sub(height),
                width: 44,
                height,
            };
            self.render_suggestion_popup(frame, popup_area);
        }
        self.render_input_bar(frame, chunks[3]);
    }

    fn render_title_bar(&self, frame: &mut Frame, area: Rect) {
        let now = Local::now().format("%H:%M:%S");
        let title = format!(
            " RHermes v{} · {} · 部署:{} · 模型:{} ",
            env!("CARGO_PKG_VERSION"),
            now,
            self.stats.mode,
            self.stats.model,
        );

        let bar = Paragraph::new(Text::from(Line::from(Span::styled(
            &title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(bar, area);
    }

    fn render_main_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();

        for msg in &self.messages {
            let (prefix, style) = match msg.role {
                Role::User => (
                    " ▶ 你",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                Role::Assistant => (
                    " ◇ RHermes",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Role::System => (
                    " ● 系统",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::raw(" │ "),
            ]));

            for content_line in msg.content.lines() {
                lines.push(Line::from(Span::raw(format!("   {content_line}"))));
            }
            lines.push(Line::from(""));
        }

        // 如果有流式内容正在接收，显示出来
        if self.running && !self.streaming_buffer.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    " ◇ RHermes",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" │ "),
            ]));
            for line in self.streaming_buffer.lines() {
                lines.push(Line::from(Span::raw(format!("   {line}"))));
            }
            lines.push(Line::from(Span::styled(
                "   ▊",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::SLOW_BLINK),
            )));
        }

        if self.scroll_offset > 0 {
            lines.insert(
                0,
                Line::from(Span::styled(
                    format!("   ↑ 已滚动 {} 行 ↑", self.scroll_offset),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )),
            );
        }

        let visible_height = inner.height.max(1) as usize;
        let total_lines = lines.len();
        let scroll = if total_lines > visible_height {
            let max_scroll = total_lines.saturating_sub(visible_height);
            self.scroll_offset.min(max_scroll)
        } else {
            0
        };

        let start = total_lines.saturating_sub(visible_height + scroll);
        let end = total_lines.saturating_sub(scroll);
        let visible_lines: Vec<Line> = if start < end {
            lines[start..end].to_vec()
        } else {
            vec![]
        };

        let paragraph = Paragraph::new(Text::from(visible_lines))
            .style(Style::default().fg(Color::White).bg(Color::Black));
        frame.render_widget(paragraph, inner);
    }

    fn render_stats_bar(&self, frame: &mut Frame, area: Rect) {
        let s = &self.stats;

        // 成本颜色
        let cost_color = if s.round_cost_cny >= 1.44 {
            Color::Red
        } else if s.round_cost_cny >= 0.36 {
            Color::Yellow
        } else {
            Color::Green
        };

        // 缓存命中率颜色
        let cache_color = if s.cache_hit_rate >= 90.0 {
            Color::Green
        } else if s.cache_hit_rate >= 50.0 {
            Color::Yellow
        } else {
            Color::Red
        };

        // 响应状态
        let timer_secs = self.last_response_secs;
        let status_icon = if self.running && !self.streaming_buffer.is_empty() {
            "⏳"
        } else if self.running {
            "🔧"
        } else {
            "💬"
        };

        let parts = vec![
            // 响应时间 & 状态（最前面）
            Span::styled(
                format!(" ⏱ {}s ", timer_secs),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                status_icon,
                Style::default().fg(if self.running { Color::Yellow } else { Color::Green }),
            ),
            Span::raw(" │ "),

            // 本轮成本
            Span::styled(
                format!("⚡ ¥{:.4} ", s.round_cost_cny),
                Style::default().fg(cost_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw("│ "),

            // 累计
            Span::styled(
                format!(" 📊 ¥{:.4} ", s.total_cost_cny),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" │ "),

            // Token 用量
            Span::styled(
                format!("📝 {}→{} ", s.input_tokens, s.output_tokens),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" │ "),

            // 缓存命中
            Span::styled(
                format!("🔄 {:.1}% h={} m={} ", s.cache_hit_rate, s.cache_hit_tokens, s.cache_miss_tokens),
                Style::default().fg(cache_color),
            ),
            Span::raw(" │ "),

            // 余额
            {
                if s.balance_cny > 0.0 {
                    Span::styled(
                        format!(" 💰 ¥{:.2} ", s.balance_cny),
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(" 💰 --- ", Style::default().fg(Color::DarkGray))
                }
            },
        ];

        let bar = Paragraph::new(Line::from(parts))
            .style(Style::default().bg(Color::Black).fg(Color::White));
        frame.render_widget(bar, area);
    }

    /// 命令补全弹窗（纵向列表，显示在输入栏上方）
    fn render_suggestion_popup(&self, frame: &mut Frame, area: Rect) {
        if self.cmd_suggestions.is_empty() {
            return;
        }
        let idx = self.suggestion_idx.min(self.cmd_suggestions.len() - 1);

        // 构建纵向列表（命令 + 说明）
        let mut lines = Vec::new();
        for (i, cmd) in self.cmd_suggestions.iter().enumerate() {
            // 查找命令对应的说明
            let desc = ALL_COMMANDS
                .iter()
                .find(|(c, _)| c == cmd)
                .map(|(_, d)| *d)
                .unwrap_or("");

            let style = if i == idx {
                Style::default()
                    .fg(Color::Cyan)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let prefix = if i == idx { "▶" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(format!("{} {}", prefix, cmd), style),
                Span::styled(format!("  — {}", desc), Style::default().fg(Color::DarkGray)),
            ]));
        }

        // 计算弹窗宽度（命令 + 描述 + 边距）
        let max_width = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.len())
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(30)
            .min(60) as u16;

        let popup_area = Rect {
            x: area.x,
            y: area.y,
            width: max_width + 4, // +4 for borders
            height: area.height,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" 命令 ")
            .title_alignment(Alignment::Left);

        let bar = Paragraph::new(Text::from(lines))
            .block(block)
            .style(Style::default().bg(Color::Black));
        frame.render_widget(bar, popup_area);
    }

    /// 从文件加载会话
    fn load_session(path: &PathBuf) -> Vec<Message> {
        match std::fs::read_to_string(path) {
            Ok(json) => {
                match serde_json::from_str::<Vec<Message>>(&json) {
                    Ok(msgs) => {
                        tracing::info!("已加载会话: {} 条消息", msgs.len());
                        msgs
                    }
                    Err(e) => {
                        tracing::warn!("会话文件解析失败: {e}");
                        Vec::new()
                    }
                }
            }
            Err(_) => Vec::new(),
        }
    }

    /// 将会话保存到文件
    fn save_session(&self) {
        if self.messages.is_empty() {
            return;
        }
        match serde_json::to_string(&self.messages) {
            Ok(json) => {
                if let Some(parent) = self.session_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&self.session_path, &json) {
                    Ok(_) => tracing::debug!("会话已保存: {} 条消息", self.messages.len()),
                    Err(e) => tracing::warn!("会话保存失败: {e}"),
                }
            }
            Err(e) => tracing::warn!("会话序列化失败: {e}"),
        }
    }

    /// 将会话归档到长期记忆
    fn export_debug(&self) {
        if let Some(ref d) = self.debug {
            if let Ok(mut dbg) = d.lock() {
                let debug_dir = self.memories_dir.parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join("debug");
                let _ = std::fs::create_dir_all(&debug_dir);
                let path = debug_dir.join(format!("session-{}.json", dbg.session_id));
                let _ = dbg.export(&path);
            }
        }
    }

    fn archive_session(&self) {
        // 保存画像到 USER.md（带字数限制）
        if let Some(ref mem) = self.memory {
            if let Ok(engine) = mem.lock() {
                let md_path = self.memories_dir.join("USER.md");
                let _ = engine.save_profile_with_limit(
                    &engine.load_profile().unwrap_or_default(),
                    Some(&md_path),
                    self.max_memory_md_chars,
                );
            }
        }
        let text: String = self.messages.iter()
            .filter(|m| m.role != Role::System)
            .map(|m| format!("{}: {}", match m.role {
                Role::User => "用户",
                Role::Assistant => "AI",
                Role::System => "系统",
            }, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return;
        }
        if let Some(ref mem) = self.memory {
            if let Ok(mut mem_lock) = mem.lock() {
                let preview: String = text.chars().take(500).collect();
                let summary = format!("【会话】\n{}", preview);
                let _ = mem_lock.remember(&summary, &["归档", "会话"], "rhermes");
            }
        }
    }

    fn render_input_bar(&self, frame: &mut Frame, area: Rect) {
        let input_style = if self.running {
            Style::default()
                .fg(Color::Gray)
                .bg(Color::Black)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::White).bg(Color::Black)
        };

        let content = if self.running {
            Line::from(Span::styled(
                " 等待响应...",
                Style::default().fg(Color::Gray),
            ))
        } else if self.input.is_empty() {
            Line::from(Span::styled(
                " > 输入消息...",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(vec![
                Span::styled(" > ", Style::default().fg(Color::Green)),
                Span::raw(&self.input),
            ])
        };

        let bar = Paragraph::new(content).style(input_style);
        frame.render_widget(bar, area);

        if !self.running {
            let visual_x = 3 + self.visual_cursor_x(); // " > " = 3 列
            let cursor_x = visual_x as u16;
            let cursor_y = area.y;
            frame.set_cursor_position(ratatui::layout::Position::new(
                area.x + cursor_x.min(area.width.saturating_sub(1)),
                cursor_y,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolDispatcher;
    use crate::tools::ToolRegistry;
    use crossterm::event::KeyEvent;

    fn test_dispatcher() -> ToolDispatcher {
        ToolDispatcher::new(ToolRegistry::new())
    }

    #[test]
    fn test_app_new_creates_welcome_messages() {
        let app = App::new("portable", test_dispatcher(), None, None, false, PathBuf::from(""), 5000, PathBuf::from(""), Arc::new(Mutex::new(crate::debug::SessionDebug::new())));
        assert!(!app.messages.is_empty());
        assert!(app.messages[0].content.contains("RHermes v"));
        assert!(app.messages[0].content.contains("portable"));
    }

    #[test]
    fn test_app_initial_state() {
        let app = App::new("traditional", test_dispatcher(), None, None, false, PathBuf::from(""), 5000, PathBuf::from(""), Arc::new(Mutex::new(crate::debug::SessionDebug::new())));
        assert!(!app.should_quit);
        assert!(!app.running);
        assert!(app.input.is_empty());
        assert_eq!(app.cursor_pos, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_stats_update_from_usage() {
        let mut stats = Stats::default();
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 100,
            total_tokens: 1100,
            prompt_cache_hit_tokens: 800,
            prompt_cache_miss_tokens: 200,
        };

        stats.update_from_usage(&usage);
        assert!(stats.round_cost_cny > 0.0);
        assert!(stats.round_cost_cny < 10.0); // 合理的成本
        assert_eq!(stats.total_cost_cny, stats.round_cost_cny);
        assert!((stats.cache_hit_rate - 80.0).abs() < 0.01); // 800/1000 = 80%
    }

    #[test]
    fn test_stats_cache_rate_zero_when_no_cache_data() {
        let mut stats = Stats::default();
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        };

        stats.update_from_usage(&usage);
        assert_eq!(stats.cache_hit_rate, 0.0);
    }

    #[test]
    fn test_handle_key_enter_without_api() {
        let mut app = App::new("test", test_dispatcher(), None, None, false, PathBuf::from(""), 5000, PathBuf::from(""), Arc::new(Mutex::new(crate::debug::SessionDebug::new())));
        app.input = "hello".into();
        app.cursor_pos = 5;

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // 没有 API 时回到模拟模式
        assert!(app.input.is_empty());
        assert_eq!(app.cursor_pos, 0);
        // welcome + tools + user + assistant(simulated) ≥ 3
        assert!(app.messages.len() >= 3);
        assert!(app.messages.iter().any(|m| m.content.contains("hello")));
    }

    #[test]
    fn test_handle_key_quit() {
        let mut app = App::new("test", test_dispatcher(), None, None, false, PathBuf::from(""), 5000, PathBuf::from(""), Arc::new(Mutex::new(crate::debug::SessionDebug::new())));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn test_handle_key_text_input() {
        let mut app = App::new("test", test_dispatcher(), None, None, false, PathBuf::from(""), 5000, PathBuf::from(""), Arc::new(Mutex::new(crate::debug::SessionDebug::new())));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.input, "a");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn test_message_constructors() {
        assert_eq!(Message::user("hello").role, Role::User);
        assert_eq!(Message::assistant("hi").role, Role::Assistant);
        assert_eq!(Message::system("info").role, Role::System);
    }

    #[test]
    fn test_stats_default() {
        let stats = Stats::default();
        assert_eq!(stats.round_cost_cny, 0.0);
        assert_eq!(stats.model, "deepseek-v4-flash");
    }
}
