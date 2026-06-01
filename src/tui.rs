//! RHermes TUI 界面
//!
//! 基于 ratatui + crossterm 的终端交互界面。
//! 通过 channel 与 API 客户端异步通信。

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::mpsc;

use crate::api::{ApiEvent, ApiMessage, DeepSeekClient, Usage};
use crate::config::Config;
use crate::context::Context;
use crate::dispatcher::ToolDispatcher;
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

#[derive(Debug, Clone)]
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

/// USD → CNY 汇率（约）
const USD_TO_CNY: f64 = 7.2;

#[derive(Debug, Clone)]
pub struct Stats {
    pub round_cost_usd: f64,
    pub total_cost_usd: f64,
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
            round_cost_usd: 0.0,
            total_cost_usd: 0.0,
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

impl Stats {
    pub fn round_cost_cny(&self) -> f64 {
        self.round_cost_usd * USD_TO_CNY
    }
    pub fn total_cost_cny(&self) -> f64 {
        self.total_cost_usd * USD_TO_CNY
    }

    /// 根据 Token 用量估算成本
    /// DeepSeek v4-flash: $0.15/M input tokens, $0.60/M output tokens
    fn update_from_usage(&mut self, usage: &Usage) {
        self.input_tokens = usage.prompt_tokens;
        self.output_tokens = usage.completion_tokens;
        self.cache_hit_tokens = usage.prompt_cache_hit_tokens;
        self.cache_miss_tokens = usage.prompt_cache_miss_tokens;

        let input_cost = usage.prompt_tokens as f64 * 0.15 / 1_000_000.0;
        let output_cost = usage.completion_tokens as f64 * 0.60 / 1_000_000.0;
        self.round_cost_usd = input_cost + output_cost;
        self.total_cost_usd += self.round_cost_usd;

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
}

/// 可用命令列表（用于 / 补全）
const ALL_COMMANDS: &[&str] = &["/help", "/clear", "/quit", "/exit", "/tool"];

impl App {
    /// 创建新的 App 实例
    pub fn new(mode: &str, dispatcher: ToolDispatcher) -> Self {
        let (_event_tx, event_rx) = mpsc::unbounded_channel();

        let mut app = Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            stats: Stats::default(),
            running: false,
            should_quit: false,
            cmd_suggestions: Vec::new(),
            suggestion_idx: 0,
            cmd_tx: None,
            event_rx,
            streaming_buffer: String::new(),
            context: None,
            dispatcher: Some(dispatcher),
        };
        app.stats.mode = mode.to_string();

        app.messages.push(Message::system(format!(
            "RHermes v{} 已启动 · 部署模式: {} · 输入 /help 查看命令",
            env!("CARGO_PKG_VERSION"),
            mode,
        )));

        app
    }

    /// 初始化 API 客户端（设置 API Key 后调用）
    pub fn init_api(&mut self, config: Config, path_mgr: &crate::path::PathManager) {
        self.stats.model = config.model.clone();

        // 初始化 Context
        let system_prompt = format!(
            "你是 RHermes，一个终端 AI 编程 Agent。\
             \n当前工作目录: {}\
             \n部署模式: {}",
            path_mgr.data_root().display(),
            path_mgr.mode().name(),
        );
        let mut ctx = Context::new(system_prompt);
        ctx.extend_prefix("\n---\n工具: read_file, write_file, search, run_command\n".as_bytes());
        self.context = Some(ctx);

        // 创建事件通道（API → TUI）
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ApiEvent>();
        self.event_rx = event_rx;

        // 创建命令通道（TUI → API）
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();
        self.cmd_tx = Some(cmd_tx);

        let client = DeepSeekClient::new(config);

        // 后台 API 任务
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    AppCommand::SendMessage(msg) => {
                        // 构建请求
                        let request = crate::api::ChatRequest {
                            model: client.model().to_string(),
                            messages: vec![ApiMessage {
                                role: "user".into(),
                                content: msg,
                            }],
                            stream: true,
                            max_tokens: Some(4096),
                            temperature: None,
                        };

                        // 调用流式 API
                        let result = client.chat_stream(request, event_tx.clone()).await;
                        if let Err(e) = result {
                            let _ = event_tx.send(ApiEvent::Error(format!("API 错误: {e}")));
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
                    self.streaming_buffer.push_str(&chunk);
                    self.running = true;
                }
                ApiEvent::Done => {
                    // 流结束，将缓冲内容作为完整消息
                    let content = self.streaming_buffer.clone();
                    if !content.is_empty() {
                        self.messages.push(Message::assistant(&content));
                    }
                    self.streaming_buffer.clear();
                    self.running = false;
                    self.scroll_offset = 0;
                }
                ApiEvent::Usage(usage) => {
                    self.stats.update_from_usage(&usage);
                }
                ApiEvent::Error(err) => {
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
                    "/quit" | "/exit" => self.should_quit = true,
                    "/clear" => {
                        self.messages.clear();
                        self.messages.push(Message::system("对话已清空"));
                    }
                    "/help" | "/?" => {
                        let help_text = "\
可用命令:
  /help  /?    — 显示此帮助
  /clear       — 清空对话
  /quit  /exit — 退出程序
  /tool <name> — 查看工具信息

快捷键:
  Ctrl+Q       — 退出
  ↑↓ PageUp/Dn — 滚动对话
  Home/End     — 光标到行首/行尾";
                        self.messages.push(Message::system(help_text));
                    }
                    _ => {
                        // 正常消息：发送给 API
                        self.messages.push(Message::user(&input));

                        if let Some(tx) = &self.cmd_tx {
                            let _ = tx.send(AppCommand::SendMessage(input));
                            self.running = true;
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
                    self.update_suggestions();
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

            // ---- 滚动 ----
            KeyCode::Up => self.scroll_offset += 1,
            KeyCode::Down => self.scroll_offset = self.scroll_offset.saturating_sub(1),
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

    /// 更新命令建议列表
    fn update_suggestions(&mut self) {
        let input = self.input.trim();
        if input.starts_with('/') && input.len() > 1 {
            let lower = input.to_lowercase();
            let matches: Vec<&'static str> = ALL_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(&lower))
                .copied()
                .collect();
            if matches.is_empty() {
                self.cmd_suggestions = ALL_COMMANDS.to_vec();
            } else {
                self.cmd_suggestions = matches;
            }
            self.suggestion_idx = 0;
        } else if input == "/" {
            self.cmd_suggestions = ALL_COMMANDS.to_vec();
            self.suggestion_idx = 0;
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
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_title_bar(frame, chunks[0]);
        self.render_main_panel(frame, chunks[1]);
        self.render_stats_bar(frame, chunks[2]);
        // 命令补全弹窗（位于输入栏上方）
        if !self.cmd_suggestions.is_empty() {
            let popup_area = Rect {
                x: chunks[3].x,
                y: chunks[3].y.saturating_sub(1),
                width: chunks[3].width,
                height: 1,
            };
            self.render_suggestion_popup(frame, popup_area);
        }
        self.render_input_bar(frame, chunks[3]);
    }

    fn render_title_bar(&self, frame: &mut Frame, area: Rect) {
        let title = format!(
            " RHermes v{} · 部署:{} · 模型:{} ",
            env!("CARGO_PKG_VERSION"),
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
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::White).bg(Color::Black));
        frame.render_widget(paragraph, inner);
    }

    fn render_stats_bar(&self, frame: &mut Frame, area: Rect) {
        let s = &self.stats;

        // 成本颜色
        let cost_color = if s.round_cost_usd >= 0.20 {
            Color::Red
        } else if s.round_cost_usd >= 0.05 {
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

        let parts = vec![
            // 本轮成本 (USD + CNY)
            Span::styled(
                format!(" ⚡ ${:.3} ", s.round_cost_usd),
                Style::default().fg(cost_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("¥{:.3}", s.round_cost_cny()),
                Style::default().fg(cost_color),
            ),
            Span::raw(" │ "),

            // 累计
            Span::styled(
                format!(" 📊 ${:.3} ", s.total_cost_usd),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("¥{:.3}", s.total_cost_cny()),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" │ "),

            // Token 用量
            Span::styled(
                format!(" 📝 {}→{} ", s.input_tokens, s.output_tokens),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" │ "),

            // 缓存命中
            Span::styled(
                format!(" 🔄 {:.1}% ", s.cache_hit_rate),
                Style::default().fg(cache_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("hit={} miss={}", s.cache_hit_tokens, s.cache_miss_tokens),
                Style::default().fg(Color::DarkGray),
            ),
        ];

        let bar = Paragraph::new(Line::from(parts))
            .style(Style::default().bg(Color::Black).fg(Color::White));
        frame.render_widget(bar, area);
    }

    /// 命令补全弹窗（显示在输入栏上方）
    fn render_suggestion_popup(&self, frame: &mut Frame, area: Rect) {
        if self.cmd_suggestions.is_empty() {
            return;
        }
        let idx = self.suggestion_idx.min(self.cmd_suggestions.len() - 1);
        let _selected = self.cmd_suggestions[idx];

        let mut spans = Vec::new();
        for (i, cmd) in self.cmd_suggestions.iter().enumerate() {
            if i == idx {
                spans.push(Span::styled(
                    format!("▶{cmd} "),
                    Style::default()
                        .fg(Color::Cyan)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!(" {cmd} "),
                    Style::default().fg(Color::White),
                ));
            }
        }

        let bar = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::Black));
        frame.render_widget(bar, area);
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
    use crate::dispatcher::ToolDispatcher;
    use crate::tool::ToolRegistry;
    use crossterm::event::KeyEvent;

    fn test_dispatcher() -> ToolDispatcher {
        ToolDispatcher::new(ToolRegistry::new())
    }

    #[test]
    fn test_app_new_creates_welcome_messages() {
        let app = App::new("portable", test_dispatcher());
        assert!(!app.messages.is_empty());
        assert!(app.messages[0].content.contains("RHermes v"));
        assert!(app.messages[0].content.contains("portable"));
    }

    #[test]
    fn test_app_initial_state() {
        let app = App::new("traditional", test_dispatcher());
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
        assert!(stats.round_cost_usd > 0.0);
        assert!(stats.round_cost_usd < 1.0); // 合理的成本
        assert_eq!(stats.total_cost_usd, stats.round_cost_usd);
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
        let mut app = App::new("test", test_dispatcher());
        app.input = "hello".into();
        app.cursor_pos = 5;

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // 没有 API 时回到模拟模式
        assert!(app.input.is_empty());
        assert_eq!(app.cursor_pos, 0);
        // welcome + user + assistant(simulated) = 3
        assert_eq!(app.messages.len(), 3);
        assert!(app.messages[1].content.contains("hello"));
    }

    #[test]
    fn test_handle_key_quit() {
        let mut app = App::new("test", test_dispatcher());
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn test_handle_key_text_input() {
        let mut app = App::new("test", test_dispatcher());
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
        assert_eq!(stats.round_cost_usd, 0.0);
        assert_eq!(stats.model, "deepseek-v4-flash");
    }
}
