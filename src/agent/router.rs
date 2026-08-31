//! SessionRouter — 多会话路由
//!
//! 管理多个 AgentSession，按 `channel:chat_id` 分配会话。
//! 每个外部消息来源自动获得或复用对应的会话实例。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::agent::event_sink::{ChannelSink, EventSink, TuiSink};
use crate::agent::session::{AgentSession, SessionConfig};
use crate::agent::MemorySystem;
use crate::agent::SkillEngine;
use crate::channel::telegram::TelegramSink;
use crate::channel::{ChannelManager, InboundMessage};
use crate::provider::Transport;
use crate::tools::ToolDispatcher;

/// 已认证学生身份（供反思记录落库时确定 student_id）
#[derive(Clone, Debug)]
struct StudentIdentity {
    student_id: i64,
    student_no: String,
    name: String,
    /// 学生所在班级（头）ID，用于读取按头覆盖的课程参数
    class_id: Option<i64>,
}

/// 会话路由器 — 按 `channel:chat_id` 管理 AgentSession
/// 知识库学习模式命令种类（dispatch 拦截层使用）
enum KbCommand {
    Learn,
    Summary,
    Stop,
}

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
    /// 每个用户当前激活的 CourseProfile（chat_id → profile）
    course_profiles: std::collections::HashMap<String, crate::edu::course::CourseProfile>,
    /// 每个用户当前激活的课程码（chat_id → course_code），用于精确定位 course_profiles 中的 profile
    current_course: std::collections::HashMap<String, String>,
    /// 每个用户的向导状态（chat_id → SetupState）
    setup_states: std::collections::HashMap<String, crate::edu::setup::SetupState>,
    /// 每个用户已认证的学生身份（chat_id → 学生信息，用于反思记录落库）
    student_ids: std::collections::HashMap<String, StudentIdentity>,
    /// 教育模式角色："teacher" / "student" / ""
    edu_role: String,
    /// 教育数据库路径
    edu_db_path: std::path::PathBuf,
    /// TUI 事件发送端（tui 会话用 TuiSink 渲染到终端）
    tui_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::api::ApiEvent>>,
    /// volatile 层文本（时间/画像/AGENTS.md，会话创建时注入，与 TUI 本地会话对齐）
    volatile_text: String,
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
            course_profiles: std::collections::HashMap::new(),
            current_course: std::collections::HashMap::new(),
            setup_states: std::collections::HashMap::new(),
            student_ids: std::collections::HashMap::new(),
            edu_role: String::new(),
            tui_event_tx: None,
            volatile_text: String::new(),
            edu_db_path: config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("home/edu.db"),
        }
    }

    /// 注入 TUI 事件发送端（tui 会话经 TuiSink 直接渲染到终端）
    pub fn set_tui_event_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::api::ApiEvent>) {
        self.tui_event_tx = Some(tx);
    }

    /// 注入 volatile 层文本（所有新建会话统一注入）
    pub fn set_volatile_text(&mut self, text: String) {
        self.volatile_text = text;
    }

    /// 设置教育模式角色
    pub fn set_edu_role(&mut self, role: &str) {
        self.edu_role = role.to_string();

        // 学生模式：从 config.toml 恢复已保存的认证状态
        if role == "student" {
            if let Ok(cfg) = crate::core::Config::load(&self.config_path) {
                let student_no = cfg.edu.student_no.as_str();
                if !student_no.is_empty() {
                    // 从 EduStore 恢复学生身份
                    if let Ok(store) = crate::edu::store::EduStore::open(&self.edu_db_path) {
                        if let Ok(Some(student)) = store.get_student(student_no) {
                            tracing::info!("[edu] 从 config 恢复学生身份: {} ({})", student.name, student_no);
                            self.student_ids.insert(
                                "__restored__".to_string(),
                                StudentIdentity {
                                    student_id: student.id,
                                    student_no: student_no.to_string(),
                                    name: student.name.clone(),
                                    class_id: student.primary_class_id,
                                },
                            );
                        }
                    }
                }
            }
        }
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
            let reply = self.handle_sw_command(&inbound.content, &inbound.chat_id);
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

        // 拦截知识库学习模式命令（Gateway 模式，全通道）
        // /learn 进入/续学/文件建库；/stop 退出并总结。会话级 kb_mode 由 AgentSession 持有。
        {
            let content = inbound.content.trim();
            let is_learn = content == "/learn" || content.starts_with("/learn ");
            let is_stop = content == "/stop" || content.starts_with("/stop ");
            let is_summary = content == "/summary" || content == "/总结" || content.starts_with("/summary ") || content.starts_with("/总结 ");
            if is_learn || is_stop || is_summary {
                let cmd = if is_learn { KbCommand::Learn } else if is_summary { KbCommand::Summary } else { KbCommand::Stop };
                self.handle_kb_mode_command(&inbound, cmd).await;
                return;
            }
        }

        // 拦截教育模式斜杠命令
        if let Some(reply) = self.handle_edu_slash_command(&inbound.content, &inbound.chat_id) {
            if !reply.is_empty() {
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        }

        // 如果 session 不存在，创建一个新的
        if !self.sessions.contains_key(&key) {
            let sink: Arc<dyn EventSink> = if inbound.channel == "tui" {
                // TUI 通道：事件直接进终端渲染循环（流式/工具进度/成本统计全保留）
                match self.tui_event_tx.clone() {
                    Some(tx) => Arc::new(TuiSink::new(tx)),
                    None => Arc::new(ChannelSink::new(
                        self.channel_mgr.clone(),
                        inbound.channel.clone(),
                        inbound.chat_id.clone(),
                    )),
                }
            } else if inbound.channel == "telegram" {
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

            let mut session = AgentSession::new(
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

            // edu：如有激活的 CourseProfile，立即应用学习模式
            if let Some(profile) = self.course_profiles.values().next() {
                let prompt = if profile.system_prompt_override.is_some() {
                    Some(profile.system_prompt(&self.system_prompt))
                } else {
                    None
                };
                let allowed = if profile.allowed_tools.is_empty() {
                    None
                } else {
                    Some(profile.allowed_tools.clone())
                };
                session.set_learn_mode(
                    prompt,
                    allowed,
                    Some(profile.mode.as_str().to_string()),
                );
            }

            // volatile 层注入（时间/画像/项目上下文——TUI 本地会话有的，router 会话也要有）
            if !self.volatile_text.is_empty() {
                session.append_volatile(&self.volatile_text);
            }

            self.sessions.insert(key.clone(), session);
        } else {
            // session 已存在但 profile 可能变了（/mode 切换）→ 同步
            if let Some(profile) = self.course_profiles.values().next() {
                if let Some(session) = self.sessions.get_mut(&key) {
                    let prompt = if profile.system_prompt_override.is_some() {
                        Some(profile.system_prompt(&self.system_prompt))
                    } else {
                        None
                    };
                    let allowed = if profile.allowed_tools.is_empty() {
                        None
                    } else {
                        Some(profile.allowed_tools.clone())
                    };
                    session.set_learn_mode(
                        prompt,
                        allowed,
                        Some(profile.mode.as_str().to_string()),
                    );
                }
            }
        }

        // 处理消息
        if let Some(session) = self.sessions.get_mut(&key) {
            session.handle_message(&inbound.content).await;
        }

        // edu 反思闭环：消费 session 产出的反思评分记录，落库到 edu_learning_journal
        //
        // 流程：session.handle_message 内若触发了反思评分（学生回答了上一轮的反思提示），
        // 会把 ReflectionRecord 放入 outbox。这里取出并持久化。
        // 落库前提：该 chat_id 已通过 /auth login 认证（有 student_id）。
        let reflection = self
            .sessions
            .get_mut(&key)
            .and_then(|s| s.take_reflection_record());
        if let Some(rec) = reflection {
            let chat_id = &inbound.chat_id;
            let student = self.student_ids.get(chat_id).cloned();
            // 按该用户当前激活的课程码精确取 profile，避免多课程场景下
            // course_profiles.values().next() 取到错误的课程导致串库
            let course_profile = self
                .current_course
                .get(chat_id)
                .and_then(|code| self.course_profiles.get(code))
                .cloned();

            match (student, course_profile) {
                (Some(stu), Some(profile)) => {
                    // course_code → course_id（落库需要数字 id）
                    let journal_id = (|| {
                        let store = crate::edu::store::EduStore::open(&self.edu_db_path)?;
                        let course = store.get_course(&profile.course_code)?;
                        let course = course.ok_or_else(|| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Null,
                                format!("课程不存在: {}", profile.course_code).into(),
                            )
                        })?;
                        let lesson_num = profile.lesson_num.unwrap_or(0);
                        store.write_journal(
                            stu.student_id,
                            course.id,
                            lesson_num,
                            &rec.topic,
                            &rec.tools_csv,
                            &rec.reflection_text,
                            rec.overall_score,
                            rec.depth,
                            0,  // token_usage：暂未精确统计
                            0,  // duration_secs：暂未精确统计
                        )
                    })();

                    match journal_id {
                        Ok(id) => tracing::info!(
                            "edu 反思已落库 journal_id={id} (student={}, score={:.2})",
                            stu.student_no,
                            rec.overall_score
                        ),
                        Err(e) => tracing::warn!("edu 反思落库失败: {e}"),
                    }
                }
                _ => {
                    // 未认证或未选课：反思评分仍在 session 内即时反馈给学生，只是不入库
                    if self.student_ids.get(chat_id).is_none() {
                        tracing::debug!("edu 反思未落库：chat_id={chat_id} 未认证");
                    } else {
                        tracing::debug!("edu 反思未落库：未选择课程");
                    }
                }
            }
        }
    }

    /// 处理 /sw 课程切换命令
    /// 知识库学习模式命令（Gateway 全通道）：/learn [名称|文件路径|list] · /summary · /stop
    ///
    /// 与 TUI 版行为一致：
    /// - /learn list         列出所有知识库
    /// - /learn <文件路径>   文件内容建库（库名=文件名去扩展名；小文件注入全文，大文件给预览+分批读取指引）
    /// - /learn <名称> [提示] 存在则续学（带进度），不存在则问主题
    /// - /stop               退出并 kb_stats html=true 总结
    /// - /summary            阶段总结（不退出学习模式，学习中随时可用）
    async fn handle_kb_mode_command(&mut self, inbound: &InboundMessage, cmd: KbCommand) {
        let content = inbound.content.trim().to_string();
        let key = format!(
            "{}:{}{}",
            inbound.channel,
            inbound.chat_id,
            inbound.metadata.get("course_suffix").cloned().unwrap_or_default()
        );

        if matches!(cmd, KbCommand::Stop) {
            // /stop：退出学习模式 + 总结
            let topic = self
                .sessions
                .get_mut(&key)
                .and_then(|s| s.kb_mode().map(|t| t.to_string()));
            match topic {
                Some(t) => {
                    if let Some(session) = self.sessions.get_mut(&key) {
                        session.exit_kb_mode();
                        let kickoff = format!(
                            "[系统] 用户结束了「{t}」的学习。请调用 kb_stats(topic=\"{t}\", html=true) 生成 Bento 战绩，把 HTML 文件路径（浏览器打开）连同简短学习小结一起给用户，然后停止教学行为。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。"
                        );
                        session.handle_message(&kickoff).await;
                    }
                    return;
                }
                None => {
                    let reply = "当前不在学习模式中。用 /learn <名称> 或 /learn <文件路径> 开始。";
                    self.reply_to_channel(&inbound.channel, &inbound.chat_id, reply).await;
                    return;
                }
            }
        }

        if matches!(cmd, KbCommand::Summary) {
            // /summary（/总结）：阶段总结，不退出学习模式，学习中随时可用
            let topic = self
                .sessions
                .get_mut(&key)
                .and_then(|s| s.kb_mode().map(|t| t.to_string()));
            match topic {
                Some(t) => {
                    if let Some(session) = self.sessions.get_mut(&key) {
                        let kickoff = format!(
                            "[系统] 用户想看「{t}」的阶段总结（不退出学习模式）。请调用 kb_stats(topic=\"{t}\", html=true) 生成 Bento 战绩，把 HTML 文件路径连同阶段性小结（已点亮节点、平均掌握度、薄弱点、下一步建议）一起给用户，然后询问是继续下一个知识点还是休息。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。"
                        );
                        session.handle_message(&kickoff).await;
                    }
                    return;
                }
                None => {
                    let reply = "当前不在学习模式中。/summary 仅学习模式可用；用 /learn <名称> 或 /learn <文件路径> 开始。";
                    self.reply_to_channel(&inbound.channel, &inbound.chat_id, reply).await;
                    return;
                }
            }
        }

        // /learn ...
        let arg = content
            .strip_prefix("/learn")
            .unwrap_or("")
            .trim()
            .to_string();

        // /learn list
        if arg == "list" || arg == "列表" {
            use crate::knowledge as kb;
            let reply = match kb::open_db().and_then(|c| kb::store::list_topics(&c)) {
                Ok(rows) if rows.is_empty() => "📚 还没有任何知识库。/learn <文件路径> 从文件建库，或 /learn <名称> 后告诉我想学什么。".to_string(),
                Ok(rows) => {
                    let lines: Vec<String> = rows.iter().map(|(name, source, total, lit, avg)| {
                        format!("📚 {name}（{source}）— 点亮 {lit}/{total} · 平均掌握 {avg}%")
                    }).collect();
                    format!("📚 我的知识库（用 /learn <名称> 继续）：\n{}", lines.join("\n"))
                }
                Err(e) => format!("⚠ 知识库读取失败: {e}"),
            };
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
            return;
        }

        if arg.is_empty() {
            let reply = "用法：\n  /learn <文件路径> — 从文件内容建库学习\n  /learn <名称> [主题提示] — 继续或创建知识库\n  /learn list — 列出所有知识库\n  /stop — 退出学习模式\n  /summary — 阶段总结（不退出，随时可用）";
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, reply).await;
            return;
        }

        // 参数是文件路径？
        let first_token = arg.split_whitespace().next().unwrap_or("").to_string();
        let looks_like_file = first_token.contains('.') || first_token.contains('\\') || first_token.starts_with('/')
            || first_token.starts_with("~/") || first_token.starts_with("./")
            || std::path::Path::new(&first_token).exists();

        use crate::knowledge as kb;
        let (name, kickoff, user_notice) = if looks_like_file {
            let file_path = if first_token.starts_with("~/") {
                let home = std::env::var("HOME").unwrap_or_default();
                first_token.replacen("~", &home, 1)
            } else {
                first_token.clone()
            };
            // 支持目录和文件
            let meta = std::fs::metadata(&file_path).ok();
            if let Some(ref m) = meta {
            if m.is_dir() {
                    // 目录模式：收集文本文件
                    let mut files = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(&file_path) {
                        for entry in entries.filter_map(|e| e.ok()) {
                            let p = entry.path();
                            if p.is_file() {
                                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                                if ["txt","md","pdf","docx","pptx","xlsx","html","csv","json"].contains(&ext.as_str()) {
                                    files.push(p.display().to_string());
                                }
                            }
                        }
                    }
                    files.sort();
                    let d_stem = std::path::Path::new(&file_path)
                        .file_name().and_then(|s| s.to_str()).unwrap_or("study").to_string();
                    let fc = files.len();
                    let fl = files.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n");
                    let d_kickoff = format!("[学习模式·目录建库] 请遍历目录 {file_path} 下的 {fc} 个文件，构建知识库「{d_stem}」并开始教学。\n要求：逐个读完全部文件后，用 kb_create 建库，kb_graph 出图，kb_learn 开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。\n\n===== 文件列表 =====\n{fl}");
                    let d_notice = format!("📚 学习模式：{d_stem}（目录 {fc} 个文件）\n正在遍历并构建知识库…");
                    (d_stem, d_kickoff, d_notice)
                } else if m.is_file() {
                    let size_kb = m.len() / 1024;
                    let stem = std::path::Path::new(&file_path)
                        .file_stem().and_then(|s| s.to_str()).unwrap_or("study").to_string();
                    let rest_hint = arg.strip_prefix(&first_token).unwrap_or("").trim().to_string();

                    match kb::open_db().and_then(|c| kb::store::topic_id(&c, &stem)).ok().flatten() {
                        Some(tid) => {
                            let st = kb::open_db().ok().and_then(|c| kb::store::stats(&c, tid).ok());
                            let progress = st.map(|s| format!("（已点亮 {}/{} 节点 · 平均掌握度 {}%）", s.lit_nodes, s.total_nodes, s.avg_mastery)).unwrap_or_default();
                            let kickoff = format!("[学习模式·继续] 知识库「{stem}」{progress}。请用 kb_learn(topic=\"{stem}\") 取下一个知识点开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                            (stem.clone(), kickoff, format!("📚 学习模式：{stem}（来源文件 {size_kb}KB，已有进度，继续学习）"))
                        }
                        None => {
                            let content = std::fs::read_to_string(&file_path).unwrap_or_default();
                            let total_lines = content.lines().count();
                            let content_block = if content.chars().count() <= 24000 {
                                format!("\n\n===== 文件内容（{file_path}）=====\n{content}")
                            } else {
                                let preview: String = content.lines().take(80).collect::<Vec<_>>().join("\n");
                                format!("\n\n===== 文件预览（前 80 行 / 共 {total_lines} 行）=====\n{preview}\n…\n【文件较大】请先用 read_file(path=\"{file_path}\", range=\"81-400\") 等分批读完全文，再抽取知识点建库。")
                            };
                            let hint = if rest_hint.is_empty() { String::new() } else { format!("用户补充要求：{rest_hint}。") };
                            let kickoff = format!("[学习模式·文件建库] 请基于文件为用户构建知识库「{stem}」并开始教学。{hint}\n要求：通读内容，抽取 8-40 个知识点，用 kb_create 建库，kb_graph 出图，kb_learn 开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。{content_block}");
                            (stem.clone(), kickoff, format!("📚 学习模式：{stem}（来源文件 {size_kb}KB）\n正在读取并构建知识库…"))
                        }
                    }
                } else {
                    let reply = format!("⚠ {file_path} 不是普通文件或目录");
                    self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                    return;
                }
            } else {
                let reply = format!("⚠ 路径不存在: {file_path}");
                self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                return;
            }
        } else {
            // 名称模式
            let mut parts = arg.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").trim().to_string();
            let topic_hint = parts.next().unwrap_or("").trim().to_string();
            match kb::open_db().and_then(|c| kb::store::topic_id(&c, &name)).ok().flatten() {
                Some(tid) => {
                    let st = kb::open_db().ok().and_then(|c| kb::store::stats(&c, tid).ok());
                    let progress = st.map(|s| format!("（已点亮 {}/{} 节点 · 平均掌握度 {}%）", s.lit_nodes, s.total_nodes, s.avg_mastery)).unwrap_or_default();
                    let kickoff = format!("[学习模式·继续] 知识库「{name}」{progress}。请用 kb_learn(topic=\"{name}\") 取下一个知识点开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                    (name.clone(), kickoff, format!("📚 学习模式：{name}{progress}\n正在载入下一个知识点…"))
                }
                None => {
                    let hint = if topic_hint.is_empty() { String::new() } else { format!("用户主题提示：{topic_hint}。") };
                    let kickoff = format!("[学习模式·新建] 知识库「{name}」尚不存在。{hint}请询问用户想学的具体主题或资料路径，然后按 knowledge-base-tutor 技能用 kb_create 建库（节点规模由内容决定），kb_graph 出图后开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                    (name.clone(), kickoff, format!("📚 学习模式：{name}（新库）\n请告诉我想学的主题，或提供资料路径。"))
                }
            }
        };

        // 确保 session 存在（若首次消息就是 /learn，session 尚未创建，需要先建）
        if !self.sessions.contains_key(&key) {
            let sink: Arc<dyn EventSink> = if inbound.channel == "tui" {
                match self.tui_event_tx.clone() {
                    Some(tx) => Arc::new(TuiSink::new(tx)),
                    None => Arc::new(ChannelSink::new(
                        self.channel_mgr.clone(),
                        inbound.channel.clone(),
                        inbound.chat_id.clone(),
                    )),
                }
            } else if inbound.channel == "telegram" {
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

        self.reply_to_channel(&inbound.channel, &inbound.chat_id, &user_notice).await;
        if let Some(session) = self.sessions.get_mut(&key) {
            session.enter_kb_mode(&name);
            session.handle_message(&kickoff).await;
        } else {
            let reply = "⚠ 会话未就绪，请稍后再试".to_string();
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
        }
    }

    fn handle_sw_command(&mut self, input: &str, chat_id: &str) -> String {
        use crate::edu::course::{parse_sw_command, CourseProfile, LearnMode, SwCommand};

        match parse_sw_command(input) {
            SwCommand::List => {
                // 优先展示该用户当前选中的课程
                let cur_code = self.current_course.get(chat_id).cloned();
                let profile = cur_code
                    .as_deref()
                    .and_then(|c| self.course_profiles.get(c));
                if let Some(profile) = profile {
                    format!("📚 当前课程:\n{}", profile.status_line())
                } else if let Some(profile) = self.course_profiles.values().next() {
                    format!("📚 当前课程:\n{}", profile.status_line())
                } else {
                    "📚 当前未选择课程。\n切换: /sw <课程码> 或 /sw <课程码>#<课次>".to_string()
                }
            }
            SwCommand::Switch { course_code, lesson_num } => {
                // 从 DB 读取课程配置，构建 CourseProfile
                let store = match crate::edu::store::EduStore::open(&self.edu_db_path) {
                    Ok(s) => s,
                    Err(e) => return format!("❌ 无法打开教育数据库: {e}"),
                };
                let course = match store.get_course(&course_code) {
                    Ok(Some(c)) => c,
                    Ok(None) => return format!("❌ 课程不存在: {course_code}"),
                    Err(e) => return format!("❌ {e}"),
                };
                // 若该用户是已认证学生且有所在班级（头），读取「模板 + 头覆盖」合并后的课程参数；
                // 这样学生看到的是自己所在头的独立配置（如果教师为该头做过 /set 修改）。
                let course = if let Some(class_id) = self
                    .student_ids
                    .get(chat_id)
                    .and_then(|s| s.class_id)
                {
                    match store.resolve_course_for_class(class_id, &course) {
                        Ok(merged) => merged,
                        Err(_) => course, // 合并失败时降级为模板值
                    }
                } else {
                    course // 未认证或无班级：用模板值
                };
                // 默认 explore 模式（可由 /mode 切换）
                let profile = CourseProfile::from_course(&course, LearnMode::Explore, lesson_num);
                let suffix = profile.session_suffix();
                let status = profile.status_line();
                self.course_contexts.insert(course_code.clone(), suffix);
                self.course_profiles.insert(course_code.clone(), profile);
                // 记录该用户当前激活的课程码，供反思落库精确取 profile
                self.current_course.insert(chat_id.to_string(), course_code.clone());
                format!("✅ 已切换到课程 {course_code}{}\n📊 模式: {}", lesson_num.map(|n| format!(" 第{}次课", n)).unwrap_or_default(), status)
            }
            SwCommand::Invalid(msg) => {
                format!("⚠️ 无效命令: {msg}")
            }
        }
    }

    /// 处理教育模式斜杠命令（教师端 + 学生端）
    fn handle_edu_slash_command(&mut self, input: &str, chat_id: &str) -> Option<String> {
        // 教师端命令
        if self.edu_role == "teacher" {
            // /setup 启动交互式向导（步骤状态机）
            if input.trim() == "/setup" {
                let state = crate::edu::setup::SetupState::new();
                let reply = crate::edu::setup::step_prompt(&state);
                self.setup_states.insert(chat_id.to_string(), state);
                return Some(reply);
            }
            // 如果用户正在向导流程中，处理回复
            if let Some(mut state) = self.setup_states.get(chat_id).cloned() {
                if state.step != crate::edu::setup::SetupStep::Done {
                    let db_path = self.edu_db_path.clone();
                    let reply = crate::edu::setup::handle_step_reply(&mut state, input.trim(), &db_path);
                    if state.step == crate::edu::setup::SetupStep::Done {
                        self.setup_states.remove(chat_id);
                    } else {
                        self.setup_states.insert(chat_id.to_string(), state);
                    }
                    return Some(reply);
                }
            }
            // /setup cancel 取消向导
            if input.trim() == "/setup cancel" {
                self.setup_states.remove(chat_id);
                return Some("向导已取消。".to_string());
            }
            if let Some(reply) = self.handle_teacher_slash(input) {
                return Some(reply);
            }
        }

        // 学生端命令
        if self.edu_role == "student" {
            if let Some(reply) = self.handle_student_slash(input, chat_id) {
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
            // 导入课程到头（班级）：复制课程模板参数到该头，之后可独立修改
            "/import" => {
                let course_code = args.get(0).copied().unwrap_or("");
                let class_name = args.get(1).copied().unwrap_or("");
                if course_code.is_empty() || class_name.is_empty() {
                    return Some(
                        "用法: /import <课程码> <班级名>\n  将课程导入到该头，之后该头可独立修改（不影响其他头）".into(),
                    );
                }
                match mgr.import_course_to_class(course_code, class_name) {
                    Ok(_info) => Some(format!(
                        "✅ 课程 {course_code} 已导入到「{class_name}」\n\
                         该头现在可以独立修改课程参数，不影响其他头。\n\
                         修改: /set {course_code} {class_name} <tools|desc|modes> <值>"
                    )),
                    Err(e) => Some(format!("❌ {e}")),
                }
            }
            // 修改该头的课程参数（只影响该头，不影响模板和其他头）
            "/set" => {
                let course_code = args.get(0).copied().unwrap_or("");
                let class_name = args.get(1).copied().unwrap_or("");
                let field = args.get(2).copied().unwrap_or("");
                // 第4个参数起为值（允许含空格）
                let value = args.get(3..).map(|v| v.join(" ")).unwrap_or_default();
                if course_code.is_empty() || class_name.is_empty() || field.is_empty() {
                    return Some(
                        "用法: /set <课程码> <班级名> <字段> <值>\n\
                         字段:\n  \
                           tools  — 工具白名单(JSON数组, 如 [\"read_file\",\"glob\"])\n  \
                           desc   — 课程描述\n  \
                           modes  — 允许模式(JSON数组, 如 [\"explore\",\"scaffold\"])\n\
                         示例: /set CS101 信工一班 tools [\"read_file\",\"glob\"]".into(),
                    );
                }
                match mgr.update_class_course_override(course_code, class_name, field, &value) {
                    Ok(_) => Some(format!(
                        "✅ 「{class_name}」的课程 {course_code} 已更新: {field}\n\
                         （此修改只影响该头，不影响其他头和课程模板）"
                    )),
                    Err(e) => Some(format!("❌ {e}")),
                }
            }
            _ => None,
        }
    }

    /// 学生端斜杠命令
    fn handle_student_slash(&mut self, input: &str, chat_id: &str) -> Option<String> {
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
                use crate::edu::course::{CourseProfile, LearnMode};
                let mode_str = args.get(0).copied().unwrap_or("");
                if mode_str.is_empty() {
                    // 显示当前模式
                    let cur = self.course_profiles.values().next()
                        .map(|p| format!("{} ({})", p.mode.icon(), p.mode.as_str()))
                        .unwrap_or_else(|| "未设置".to_string());
                    return Some(format!("当前模式: {cur}\n可切换: /mode <explore|scaffold|locked>"));
                }
                let new_mode = LearnMode::from_str(mode_str);
                // 需要从 DB 重新构建 profile（保留课程码，换模式）
                let store = crate::edu::store::EduStore::open(&self.edu_db_path).ok()?;
                // 找到当前激活的课程码
                let cur_code = self.course_profiles.keys().next()?.clone();
                let course = store.get_course(&cur_code).ok()??;
                // 若学生有所在班级（头），合并头覆盖参数
                let course = if let Some(class_id) = self
                    .student_ids
                    .get(chat_id)
                    .and_then(|s| s.class_id)
                {
                    store.resolve_course_for_class(class_id, &course).unwrap_or(course)
                } else {
                    course
                };
                let old_profile = self.course_profiles.get(&cur_code)?;
                let new_profile = CourseProfile::from_course(
                    &course,
                    new_mode,
                    old_profile.lesson_num,
                );
                let status = new_profile.status_line();
                self.course_profiles.insert(cur_code, new_profile);
                // 注意：实际应用到 AgentSession 在 dispatch() 的同步逻辑中完成
                Some(format!("✅ 学习模式已切换为: {status}\n（下一条消息生效）"))
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
                            Ok(result) => {
                                // 记录学生身份（内存，供反思记录落库）
                                self.student_ids.insert(
                                    chat_id.to_string(),
                                    StudentIdentity {
                                        student_id: result.student_id,
                                        student_no: no.to_string(),
                                        name: result.student_name.clone(),
                                        class_id: result.primary_class_id,
                                    },
                                );
                                // 持久化到 config.toml（供 TUI/submit/feedback 使用）
                                let mut cfg = crate::core::Config::load(&self.config_path).unwrap_or_default();
                                cfg.edu.student_no = no.to_string();
                                cfg.edu.auth_token = result.token;
                                let _ = cfg.save(&self.config_path);
                                Some(format!("✅ 认证成功！欢迎, {}", result.student_name))
                            }
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
            help.push_str("  /import <课程码> <班级名> — 导入课程到该头（之后可独立修改）\n");
            help.push_str("  /set <课程码> <班级名> <tools|desc|modes> <值> — 修改该头的课程参数\n");
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
