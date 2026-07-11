//! SessionRouter — 多会话路由
//!
//! 管理多个 AgentSession，按 `channel:chat_id` 分配会话。
//! 每个外部消息来源自动获得或复用对应的会话实例。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::agent::event_sink::{ChannelSink, EventSink};
use crate::agent::session::{AgentSession, SessionConfig};
use crate::agent::MemorySystem;
use crate::agent::SkillEngine;
use crate::channel::telegram::TelegramSink;
use crate::channel::{ChannelManager, InboundMessage};
use crate::provider::Transport;
use crate::tools::ToolDispatcher;

/// 会话路由器 — 按 `channel:chat_id` 管理 AgentSession
pub struct SessionRouter {
    sessions: HashMap<String, AgentSession>,
    dispatcher: Option<ToolDispatcher>,
    memory: Option<Arc<Mutex<MemorySystem>>>,
    skill_engine: Option<Arc<Mutex<SkillEngine>>>,
    transport: Arc<dyn Transport>,
    channel_mgr: Arc<ChannelManager>,
    config: SessionConfig,
    system_prompt: String,
    debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
    /// 配置文件路径（用于 /model 等斜杠命令）
    config_path: std::path::PathBuf,
    /// 每个用户的课程上下文（chat_id → course_suffix）
    course_contexts: std::collections::HashMap<String, String>,
    /// 教育模式角色："teacher" / "student" / ""
    edu_role: String,
    /// 教育数据库路径
    edu_db_path: std::path::PathBuf,
}

impl SessionRouter {
    pub fn new(
        dispatcher: Option<ToolDispatcher>,
        memory: Option<Arc<Mutex<MemorySystem>>>,
        skill_engine: Option<Arc<Mutex<SkillEngine>>>,
        transport: Arc<dyn Transport>,
        channel_mgr: Arc<ChannelManager>,
        config: &SessionConfig,
        system_prompt: String,
        debug: Option<Arc<Mutex<crate::debug::SessionDebug>>>,
        config_path: std::path::PathBuf,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            dispatcher,
            memory,
            skill_engine,
            transport,
            channel_mgr,
            config: config.clone(),
            system_prompt,
            debug,
            config_path: config_path.clone(),
            course_contexts: std::collections::HashMap::new(),
            edu_role: String::new(),
            edu_db_path: config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("home/edu.db"),
        }
    }

    /// 设置教育模式角色
    pub fn set_edu_role(&mut self, role: &str) {
        self.edu_role = role.to_string();
    }

    /// 路由一条入站消息到对应的 AgentSession
    pub async fn dispatch(&mut self, inbound: InboundMessage) {
        // 从 metadata 获取课程上下文
        let course_suffix = inbound
            .metadata
            .get("course_suffix")
            .cloned()
            .unwrap_or_default();
        let key = format!("{}:{}{}", inbound.channel, inbound.chat_id, course_suffix);

        // 拦截 /sw 命令（课程切换）
        if inbound.content.starts_with("/sw") {
            let reply = self.handle_sw_command(&inbound.content);
            if !reply.is_empty() {
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        }

        // 拦截斜杠命令（Gateway 模式）
        if inbound.content.starts_with("/model") {
            let reply = self.handle_model_command(&inbound.content);
            if !reply.is_empty() {
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        }

        // 拦截教育模式斜杠命令
        if let Some(reply) = self.handle_edu_slash_command(&inbound.content) {
            if !reply.is_empty() {
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        }

        // 如果 session 不存在，创建一个新的
        if !self.sessions.contains_key(&key) {
            let sink: Arc<dyn EventSink> = if inbound.channel == "telegram" {
                Arc::new(TelegramSink::new(
                    self.channel_mgr.clone(),
                    inbound.chat_id.clone(),
                ))
            } else {
                Arc::new(ChannelSink::new(
                    self.channel_mgr.clone(),
                    inbound.channel.clone(),
                    inbound.chat_id.clone(),
                ))
            };

            let session = AgentSession::new(
                key.clone(),
                self.system_prompt.clone(),
                self.dispatcher.clone(),
                self.memory.clone(),
                self.skill_engine.clone(),
                self.transport.clone(),
                sink,
                self.config.clone(),
                self.debug.clone(),
            );
            self.sessions.insert(key.clone(), session);
        }

        // 处理消息
        if let Some(session) = self.sessions.get_mut(&key) {
            session.handle_message(&inbound.content).await;
        }
    }

    /// 处理 /sw 课程切换命令
    fn handle_sw_command(&mut self, input: &str) -> String {
        use crate::edu::course::{parse_sw_command, SwCommand};

        match parse_sw_command(input) {
            SwCommand::List => {
                let current = self.course_contexts.values().next();
                if self.course_contexts.is_empty() {
                    "📚 当前未选择课程。\n切换: /sw <课程码> 或 /sw <课程码>#<课次>".to_string()
                } else {
                    format!("📚 当前课程上下文:\n{}", current.unwrap_or(&String::new()))
                }
            }
            SwCommand::Switch { course_code, lesson_num } => {
                let suffix = if let Some(ln) = lesson_num {
                    format!(":{}#{}", course_code, ln)
                } else {
                    format!(":{}", course_code)
                };
                self.course_contexts.insert(course_code.clone(), suffix.clone());
                format!("✅ 已切换到课程 {}{}", course_code, lesson_num.map(|n| format!(" 第{}次课", n)).unwrap_or_default())
            }
            SwCommand::Invalid(msg) => {
                format!("⚠️ 无效命令: {msg}")
            }
        }
    }

    /// 处理教育模式斜杠命令（教师端 + 学生端）
    fn handle_edu_slash_command(&self, input: &str) -> Option<String> {
        // 教师端命令
        if self.edu_role == "teacher" {
            // /setup 在非 TUI 环境返回引导文本
            if input.trim() == "/setup" {
                return Some(crate::edu::setup::setup_guide_text());
            }
            if let Some(reply) = self.handle_teacher_slash(input) {
                return Some(reply);
            }
        }

        // 学生端命令
        if self.edu_role == "student" {
            if let Some(reply) = self.handle_student_slash(input) {
                return Some(reply);
            }
        }

        // 两端共有
        if input.starts_with("/help") {
            return Some(self.edu_help_text());
        }

        None
    }

    /// 教师端斜杠命令
    fn handle_teacher_slash(&self, input: &str) -> Option<String> {
        let parts: Vec<&str> = input.trim().splitn(2, char::is_whitespace).collect();
        let cmd = parts[0];
        let args_str = parts.get(1).copied().unwrap_or("");
        let args: Vec<&str> = args_str.split_whitespace().collect();

        let mgr = match crate::edu::teacher::TeacherManager::new(&self.edu_db_path) {
            Ok(m) => m,
            Err(e) => return Some(format!("❌ {e}")),
        };

        match cmd {
            "/course" => {
                let action = args.get(0).copied().unwrap_or("");
                match action {
                    "create" => {
                        let code = args.get(1).copied().unwrap_or("");
                        let name = args.get(2).copied().unwrap_or("");
                        if code.is_empty() || name.is_empty() {
                            return Some("用法: /course create <课程码> <课程名>".to_string());
                        }
                        match mgr.create_course(1, code, name) {
                            Ok(_) => Some("".to_string()),
                            Err(e) => Some(format!("❌ {e}")),
                        }
                    }
                    "list" => {
                        let mut buf = String::new();
                        match mgr.list_courses(1) {
                            Ok(courses) => {
                                if courses.is_empty() { buf.push_str("（无课程）"); }
                                for c in &courses {
                                    buf.push_str(&format!("  {} {}\n", c.course_code, c.name));
                                }
                            }
                            Err(e) => buf.push_str(&format!("❌ {e}")),
                        }
                        Some(buf)
                    }
                    _ => Some("用法: /course <create <码> <名> | list>".to_string()),
                }
            }
            "/class" => {
                let code = args.get(1).copied().unwrap_or("");
                let name = args.get(2).copied().unwrap_or("");
                if code.is_empty() || name.is_empty() {
                    return Some("用法: /class create <课程码> <班级名>".to_string());
                }
                match mgr.create_class(code, name) {
                    Ok(_) => Some("".to_string()),
                    Err(e) => Some(format!("❌ {e}")),
                }
            }
            "/lesson" => {
                let code = args.get(1).copied().unwrap_or("");
                let class_name = args.get(2).copied().unwrap_or("");
                let num_str = args.get(3).copied().unwrap_or("");
                let topic = args.get(4).copied().unwrap_or("");
                if code.is_empty() || class_name.is_empty() || num_str.is_empty() {
                    return Some("用法: /lesson create <课程码> <班级> <序号> <主题>".to_string());
                }
                let num: i64 = num_str.parse().unwrap_or(1);
                match mgr.create_lesson(code, class_name, num, topic) {
                    Ok(_) => Some("".to_string()),
                    Err(e) => Some(format!("❌ {e}")),
                }
            }
            "/student" => {
                let action = args.get(0).copied().unwrap_or("");
                match action {
                    "add" => {
                        let no = args.get(1).copied().unwrap_or("");
                        let name = args.get(2).copied().unwrap_or("");
                        let course = args.get(3).copied().unwrap_or("");
                        let class_name = args.get(4).copied().unwrap_or("");
                        if no.is_empty() || name.is_empty() || course.is_empty() || class_name.is_empty() {
                            return Some("用法: /student add <学号> <姓名> <课程码> <班级名>".to_string());
                        }
                        match mgr.add_student(no, name, "123456", Some(class_name), Some(course)) {
                            Ok(_) => Some("".to_string()),
                            Err(e) => Some(format!("❌ {e}")),
                        }
                    }
                    _ => Some("用法: /student add <学号> <姓名> <课程码> <班级名>".to_string()),
                }
            }
            "/roster" => {
                let code = args.get(0).copied().unwrap_or("");
                if code.is_empty() {
                    return Some("用法: /roster <课程码>".to_string());
                }
                let mut buf = String::new();
                match mgr.list_roster(code) {
                    Ok(_) => {}
                    Err(e) => buf.push_str(&format!("❌ {e}")),
                }
                Some(buf)
            }
            _ => None,
        }
    }

    /// 学生端斜杠命令
    fn handle_student_slash(&self, input: &str) -> Option<String> {
        let parts: Vec<&str> = input.trim().splitn(2, char::is_whitespace).collect();
        let cmd = parts[0];
        let args: Vec<&str> = parts.get(1).copied().unwrap_or("").split_whitespace().collect();

        match cmd {
            "/courses" => {
                let store = crate::edu::store::EduStore::open(&self.edu_db_path).ok()?;
                // 简化版：列出所有课程
                let courses = store.list_courses_by_teacher(1).ok()?;
                let mut buf = "📚 可用课程:\n".to_string();
                for c in &courses {
                    buf.push_str(&format!("  {} {}\n", c.course_code, c.name));
                }
                buf.push_str("\n用 /sw <课程码> 切换课程");
                Some(buf)
            }
            "/profile" => {
                Some("📊 学习档案\n   （需要先认证 — 输入 /auth login <学号> <密码>）".to_string())
            }
            "/report" => {
                Some("📝 成长报告\n   （需要先认证 — 输入 /auth login <学号> <密码>）".to_string())
            }
            "/mode" => {
                let mode = args.get(0).copied().unwrap_or("");
                if mode.is_empty() {
                    return Some("当前模式: explore\n可切换: /mode <explore|scaffold|locked>".to_string());
                }
                Some(format!("✅ 学习模式已切换为: {mode}"))
            }
            "/auth" => {
                let action = args.get(0).copied().unwrap_or("");
                match action {
                    "login" => {
                        let no = args.get(1).copied().unwrap_or("");
                        let pwd = args.get(2).copied().unwrap_or("");
                        if no.is_empty() || pwd.is_empty() {
                            return Some("用法: /auth login <学号> <密码>".to_string());
                        }
                        let store = crate::edu::store::EduStore::open(&self.edu_db_path).ok()?;
                        match crate::edu::auth::authenticate(&store, no, pwd) {
                            Ok(result) => Some(format!("✅ 认证成功！欢迎, {}", result.student_name)),
                            Err(e) => Some(format!("❌ {e}")),
                        }
                    }
                    _ => Some("用法: /auth login <学号> <密码>".to_string()),
                }
            }
            _ => None,
        }
    }

    /// 教育模式帮助文本
    fn edu_help_text(&self) -> String {
        let mut help = "📖 可用命令:\n".to_string();
        help.push_str("  /sw [课程码] — 列出/切换课程\n");
        help.push_str("  /model [set <模型>] — 查看/切换模型\n");
        if self.edu_role == "teacher" {
            help.push_str("\n👩‍🏫 教师命令:\n");
            help.push_str("  /course create <码> <名> — 创建课程\n");
            help.push_str("  /course list — 列出课程\n");
            help.push_str("  /class create <码> <班> — 创建班级\n");
            help.push_str("  /lesson create <码> <班> <序号> <主题> — 创建课次\n");
            help.push_str("  /student add <学号> <名> <课程> <班> — 添加学生\n");
            help.push_str("  /roster <课程码> — 查看花名册\n");
        }
        if self.edu_role == "student" {
            help.push_str("\n🧑‍🎓 学生命令:\n");
            help.push_str("  /courses — 列出可用课程\n");
            help.push_str("  /auth login <学号> <密码> — 认证\n");
            help.push_str("  /profile — 查看学习档案\n");
            help.push_str("  /report — 生成成长报告\n");
            help.push_str("  /mode [explore|scaffold] — 查看/切换学习模式\n");
        }
        help
    }
    fn handle_model_command(&self, input: &str) -> String {
        let rest = input.trim_start_matches("/model").trim();
        if rest.is_empty() {
            format!(
                "当前模型: {}\n切换: /model set <模型名称>\n列表: /model list",
                self.transport.model_name()
            )
        } else if let Some(new_model) = rest.strip_prefix("set ") {
            let new_model = new_model.trim();
            if new_model.is_empty() {
                "用法: /model set <模型名称>".into()
            } else {
                // 热切换 + 持久化到 config.toml
                self.transport.set_model(new_model);
                if let Ok(mut cfg) = crate::core::Config::load(&self.config_path) {
                    cfg.api.model = new_model.to_string();
                    let provider_name = if cfg.agent.default_provider.is_empty() {
                        "deepseek"
                    } else {
                        &cfg.agent.default_provider
                    };
                    if let Some(p) = cfg.providers.get_mut(provider_name) {
                        p.model = Some(new_model.to_string());
                    }
                    let _ = cfg.save(&self.config_path);
                }
                format!("✅ 模型已切换为: {new_model}（已立即生效）")
            }
        } else if rest == "list" {
            // 从 config.toml 列出已配置的 providers 和模型
            let cfg = crate::core::Config::load(&self.config_path).unwrap_or_default();
            let mut lines = Vec::new();
            for (name, p) in &cfg.providers {
                if p.api_key.is_empty() && !matches!(name.as_str(), "ollama" | "lmstudio") { continue; }
                let model = p.model.clone().unwrap_or_default();
                if model.is_empty() {
                    lines.push(format!("  · {name}: (使用默认模型)"));
                } else {
                    lines.push(format!("  · {name}: {model}"));
                }
            }
            let body = if lines.is_empty() { "(无已配置的 provider)".into() }
                       else { lines.join("\n") };
            format!("可用模型列表:\n{body}\n切换: /model set <模型名称>")
        } else {
            "用法:\n  /model            — 查看当前模型\n  /model set <名称> — 切换模型（立即生效）\n  /model list       — 列出可用模型".into()
        }
    }

    /// 直接回复到外部通道（不经过 AgentSession）
    async fn reply_to_channel(&self, channel: &str, chat_id: &str, text: &str) {
        if let Some(ch) = self.channel_mgr.get(channel) {
            let _ = ch.send_message(chat_id, text).await;
        }
    }

    /// 获取会话数量
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 热切换模型（所有 session 共享同一个 transport，切换后立即生效）
    pub fn set_model(&self, model: &str) {
        self.transport.set_model(model);
    }
}
