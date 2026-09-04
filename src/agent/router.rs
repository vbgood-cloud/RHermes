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

/// 学习资料分块读取 + 落盘中转
///
/// 解决 /learn 大文件内容丢失问题：
/// - 二进制文档（pdf/docx/pptx/xlsx/png 等）用 parse_document 解析为文本
/// - 文本文件 read_to_string
/// - 内容 ≤ 24000 字符：直接返回全文，内联注入 kickoff
/// - 内容 > 24000 字符：按行分块（每块 ≤ CHUNK_CHARS），逐块落盘到
///   data_root/knowledge/sources/<stem>/part_NNNN.md，返回文件清单，
///   Agent 按清单逐段 read_file，100% 覆盖全文
const CHUNK_CHARS: usize = 20000;

/// 二进制文档扩展名（需 parse_document 解析）
fn is_binary_doc(ext: &str) -> bool {
    matches!(
        ext,
        "pdf" | "docx" | "pptx" | "xlsx" | "xls"
            | "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "tif" | "tiff"
    )
}

/// 清洗文件名：保留字母/数字/汉字和 - _，其余替换为下划线
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 建库校验：返回指定知识库当前的节点总数（库不存在返回 0）
fn kb_build_check(name: &str) -> usize {
    use crate::knowledge as kb;
    kb::open_db().ok()
        .and_then(|c| kb::store::topic_id(&c, name).ok().flatten())
        .and_then(|tid| kb::open_db().ok().and_then(|c| kb::store::stats(&c, tid).ok()))
        .map(|s| s.total_nodes)
        .unwrap_or(0)
}

/// 建库重试上限
const KB_BUILD_MAX_RETRIES: usize = 3;
/// 建库成功最低节点数（kickoff 要求 8-40 个，取宽松阈值避免误判）
const KB_BUILD_MIN_NODES: usize = 5;
/// 建库心跳间隔（秒）：建库执行期间每分钟推送一次进度，让用户知道库还在建
const KB_BUILD_HEARTBEAT_SECS: u64 = 60;

/// 启动建库进度心跳：建库执行期间每分钟向通道推送一次实时进度
/// （查库取当前节点数；TUI 通道本身有实时工具进度显示，由调用方跳过）
fn spawn_build_heartbeat(
    channel_mgr: Arc<ChannelManager>,
    channel: String,
    chat_id: String,
    name: String,
    done: Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    tokio::spawn(async move {
        let mut minutes = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(KB_BUILD_HEARTBEAT_SECS)).await;
            if done.load(Ordering::Relaxed) {
                break;
            }
            minutes += 1;
            let nodes = kb_build_check(&name);
            let text = if nodes >= KB_BUILD_MIN_NODES {
                format!("🔨 「{name}」建库进行中：已建 {nodes} 个知识点，仍在补充完善…（已运行 {minutes} 分钟）")
            } else {
                format!("⏳ 「{name}」正在阅读资料并抽取知识点…（已运行 {minutes} 分钟）")
            };
            if let Some(ch) = channel_mgr.get(&channel) {
                let _ = ch.send_message(&chat_id, &text).await;
            }
        }
    });
}

/// 读取学习资料，返回 (全文或空, 分块文件清单, 总字符数)
/// - 全文可直接注入时返回空清单
/// - 分块时返回按序排列的 part 文件绝对路径
async fn load_study_material(
    file_path: &str,
    stem: &str,
) -> Result<(String, Vec<std::path::PathBuf>, usize), String> {
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 1) 读入：二进制走 parse_document，文本走 read_to_string
    let content = if is_binary_doc(&ext) {
        match crate::tools::liteparse::parse_document_text(file_path).await {
            Ok(text) => text,
            Err(e) => {
                // 解析失败（如 pdfium 缺失）：退回文本读取，若仍失败则报错
                tracing::warn!("[learn] parse_document 解析 {file_path} 失败: {e}");
                std::fs::read_to_string(file_path).unwrap_or_default()
            }
        }
    } else {
        std::fs::read_to_string(file_path).map_err(|e| format!("读取文件失败: {file_path}（{e}）"))?
    };

    let total_chars = content.chars().count();
    if total_chars <= 24000 {
        return Ok((content, Vec::new(), total_chars));
    }

    // 2) 大文件：按行分块落盘
    let dir = crate::knowledge::sources_dir().join(stem);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建分段目录失败: {e}"))?;

    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    let mut buf = String::new();
    let mut buf_chars = 0usize;
    let mut part_no = 0usize;

    for line in content.lines() {
        let line_chars = line.chars().count() + 1; // +1 换行
        if buf_chars + line_chars > CHUNK_CHARS && !buf.is_empty() {
            part_no += 1;
            let p = dir.join(format!("part_{part_no:04}.md"));
            std::fs::write(&p, &buf).map_err(|e| format!("写入分段文件失败: {e}"))?;
            parts.push(p);
            buf.clear();
            buf_chars = 0;
        }
        buf.push_str(line);
        buf.push('\n');
        buf_chars += line_chars;
    }
    if !buf.is_empty() {
        part_no += 1;
        let p = dir.join(format!("part_{part_no:04}.md"));
        std::fs::write(&p, &buf).map_err(|e| format!("写入分段文件失败: {e}"))?;
        parts.push(p);
    }

    tracing::info!(
        "[learn] 大文件 {file_path} 已分 {part_no} 段落盘到 {}（{total_chars} 字符）",
        dir.display()
    );
    Ok((String::new(), parts, total_chars))
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

        // /learn export / 导出：导出知识库（纯本地数据操作，不经过 LLM）
        if arg == "export" || arg == "导出" || arg.starts_with("export ") || arg.starts_with("导出 ") {
            let reply = self.handle_kb_export_command(&arg);
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
            return;
        }

        // /learn import / 导入：导入 .kb.json 导出文件
        if arg == "import" || arg == "导入" || arg.starts_with("import ") || arg.starts_with("导入 ") {
            let reply = self.handle_kb_import_command(&arg);
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
            return;
        }

        // /learn reset <名称> / 复位：清空学习进度（纯本地操作，零 token）
        if arg == "reset" || arg == "复位" || arg.starts_with("reset ") || arg.starts_with("复位 ") {
            let reply = self.handle_kb_reset_command(&arg);
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
            return;
        }

        if arg.is_empty() {
            let reply = "用法：\n  /learn <文件路径> — 从文件内容建库学习\n  /learn <名称> [主题提示] — 继续或创建知识库\n  /learn list — 列出所有知识库\n  /learn export <名称> [--kb] — 导出知识库（默认含学习记录；--kb 只导库结构）\n  /learn import <路径> [--as 新名] — 导入知识库导出文件\n  /learn reset <名称> — 复位学习进度（清空掌握度/测验史，保留知识点）\n  /stop — 退出学习模式\n  /summary — 阶段总结（不退出，随时可用）";
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, reply).await;
            return;
        }

        // 参数是文件路径？
        let first_token = arg.split_whitespace().next().unwrap_or("").to_string();
        let looks_like_file = first_token.contains('.') || first_token.contains('\\') || first_token.starts_with('/')
            || first_token.starts_with("~/") || first_token.starts_with("./")
            || std::path::Path::new(&first_token).exists();

        use crate::knowledge as kb;
        // (库名, kickoff 指令, 用户提示, 是否需要建库校验重试)
        let (name, kickoff, user_notice, need_build_check): (String, String, String, bool) = if looks_like_file {
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
                    // 按扩展名标注每个文件的读取方式：二进制→parse_document，文本→read_file 分批
                    let fl: Vec<String> = files.iter().map(|f| {
                        let ext = std::path::Path::new(f)
                            .extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                        let method = if is_binary_doc(&ext) {
                            "parse_document(file_path=...)"
                        } else {
                            "read_file(path=...) 分段读取"
                        };
                        format!("  {f}\n    → {method}")
                    }).collect();
                    let fl = fl.join("\n");
                    let d_kickoff = format!("[学习模式·目录建库] 请遍历目录 {file_path} 下的 {fc} 个文件，构建知识库「{d_stem}」并开始教学。\n要求：逐个读完全部文件（按标注方式），大文本文件用 read_file 分段读完，再用 kb_create 建库，kb_graph 出图，kb_learn 开始教学。\n建库纪律：kb_create 必须成功，若调用失败（参数不合法/输出被截断等）必须分析原因修正后重新调用，直到建库成功，禁止放弃；内容过长时先用 kb_create 建核心骨架（8-15 个节点），再用 kb_append(topic=\"{d_stem}\", nodes=..., edges=...) 分批追加剩余节点。\n注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。\n\n===== 文件列表（含读取方式）=====\n{fl}");
                    let d_notice = format!("📚 学习模式：{d_stem}（目录 {fc} 个文件）\n正在遍历并构建知识库…");
                    (d_stem, d_kickoff, d_notice, true)
                } else if m.is_file() {
                    let size_kb = m.len() / 1024;
                    let stem = std::path::Path::new(&file_path)
                        .file_stem().and_then(|s| s.to_str()).unwrap_or("study").to_string();
                    let rest_hint = arg.strip_prefix(&first_token).unwrap_or("").trim().to_string();

                    match kb::open_db().and_then(|c| kb::store::topic_id(&c, &stem)).ok().flatten() {
                        Some(tid) => {
                            let st = kb::open_db().ok().and_then(|c| kb::store::stats(&c, tid).ok());
                            let progress = st.map(|s| format!("（已点亮 {}/{} 节点 · 平均掌握度 {}%）", s.lit_nodes, s.total_nodes, s.avg_mastery)).unwrap_or_default();
                            let kickoff = format!("[学习模式·继续] 知识库「{stem}」{progress}。请用 kb_learn(topic=\"{stem}\") 取下一个知识点开始持续教学：讲完一个知识点立即用 kb_quiz 出题判分，判分后不要询问用户是否继续，直接 kb_learn 取下一个，循环到全部节点掌握度 ≥80%（kb_learn 会返回学习完成）或用户 /stop 停止。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                            (stem.clone(), kickoff, format!("📚 学习模式：{stem}（来源文件 {size_kb}KB，已有进度，继续学习）"), false)
                        }
                        None => {
                            // 大文件分段管线：二进制→parse_document，文本→read_to_string，
                            // 超限内容分段落盘到 data_root/knowledge/sources/<stem>/
                            let (content, parts, total_chars) = match load_study_material(&file_path, &stem).await {
                                Ok(v) => v,
                                Err(e) => {
                                    let reply = format!("⚠ 读取学习资料失败: {e}");
                                    self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                                    return;
                                }
                            };

                            let content_block = if parts.is_empty() {
                                // 小文件：全文注入
                                format!("\n\n===== 文件内容（{file_path}）=====\n{content}")
                            } else {
                                // 大文件：给出分块清单 + 强制读取指令
                                let part_lines: Vec<String> = parts.iter().enumerate()
                                    .map(|(i, p)| format!("  第{}段: read_file(path=\"{}\")", i + 1, p.display()))
                                    .collect();
                                format!(
                                    "\n\n【文件较大】已分段落盘（共 {} 字符，{} 段）。必须按顺序用 read_file 逐段读完，一段都不能遗漏：\n{}\n\n全部读完后，综合所有段的内容抽取 8-40 个知识点，用 kb_create 一次性建库。",
                                    total_chars,
                                    parts.len(),
                                    part_lines.join("\n")
                                )
                            };
                            let hint = if rest_hint.is_empty() { String::new() } else { format!("用户补充要求：{rest_hint}。") };
                            let kickoff = format!("[学习模式·文件建库] 请基于文件为用户构建知识库「{stem}」并开始教学。{hint}\n要求：通读内容，抽取 8-40 个知识点，用 kb_create 建库，kb_graph 出图，kb_learn 开始教学。\n建库纪律：kb_create 必须成功，若调用失败（参数不合法/输出被截断等）必须分析原因修正后重新调用，直到建库成功，禁止放弃；内容过长时先用 kb_create 建核心骨架（8-15 个节点），再用 kb_append(topic=\"{stem}\", nodes=..., edges=...) 分批追加剩余节点。\n注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。{content_block}");
                            let notice = if parts.is_empty() {
                                format!("📚 学习模式：{stem}（来源文件 {size_kb}KB）\n正在读取并构建知识库…")
                            } else {
                                format!("📚 学习模式：{stem}（来源文件 {size_kb}KB，大文件已分 {n} 段）\n正在读取并构建知识库…", n = parts.len())
                            };
                            (stem.clone(), kickoff, notice, true)
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
                    let kickoff = format!("[学习模式·继续] 知识库「{name}」{progress}。请用 kb_learn(topic=\"{name}\") 取下一个知识点开始持续教学：讲完一个知识点立即用 kb_quiz 出题判分，判分后不要询问用户是否继续，直接 kb_learn 取下一个，循环到全部节点掌握度 ≥80%（kb_learn 会返回学习完成）或用户 /stop 停止。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                    (name.clone(), kickoff, format!("📚 学习模式：{name}{progress}\n正在载入下一个知识点…"), false)
                }
                None => {
                    let hint = if topic_hint.is_empty() { String::new() } else { format!("用户主题提示：{topic_hint}。") };
                    let kickoff = format!("[学习模式·新建] 知识库「{name}」尚不存在。{hint}请询问用户想学的具体主题或资料路径，然后按 knowledge-base-tutor 技能用 kb_create 建库（节点规模由内容决定），kb_graph 出图后开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。");
                    (name.clone(), kickoff, format!("📚 学习模式：{name}（新库）\n请告诉我想学的主题，或提供资料路径。"), false)
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

            // 建库心跳：kickoff + 重试循环全程，每分钟向通道推送进度
            // （TUI 通道本身有实时工具进度显示，跳过）
            let hb_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if need_build_check && inbound.channel != "tui" {
                spawn_build_heartbeat(
                    self.channel_mgr.clone(),
                    inbound.channel.clone(),
                    inbound.chat_id.clone(),
                    name.clone(),
                    hb_done.clone(),
                );
            }

            session.handle_message(&kickoff).await;

            // 建库闭环校验：文件/目录建库完成后查库确认是否真的建成，
            // 未建成则注入系统重试消息，直到建库成功（最多 KB_BUILD_MAX_RETRIES 次）
            if need_build_check {
                for attempt in 1..=KB_BUILD_MAX_RETRIES {
                    let node_count = kb_build_check(&name);
                    if node_count >= KB_BUILD_MIN_NODES {
                        break;
                    }
                    if attempt == KB_BUILD_MAX_RETRIES {
                        let reply = format!(
                            "⚠ 建库仍未成功（自动重试 {} 次后「{name}」只有 {node_count} 个知识点）。\n可再发一次 /learn <文件路径> 重试，或用 /learn {name} 手动指导建库。",
                            KB_BUILD_MAX_RETRIES - 1
                        );
                        self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
                        break;
                    }
                    tracing::warn!(
                        "[learn] 建库校验未通过（第 {attempt} 次）：「{name}」当前 {node_count} 个节点，注入重试指令"
                    );
                    let retry_kickoff = format!(
                        "[系统] 建库校验未通过：知识库「{name}」尚未成功创建（当前仅 {node_count} 个知识点，要求至少 {KB_BUILD_MIN_NODES} 个）。这是第 {attempt}/{KB_BUILD_MAX_RETRIES} 次重试，必须完成建库，禁止放弃。\n请重新执行建库：重新通读资料，用 kb_create 建库。若上次因内容过长导致输出截断，先用 kb_create 只建核心骨架（8-15 个节点），再用 kb_append(topic=\"{name}\", nodes=..., edges=...) 分批追加剩余节点。\n建库成功后 kb_graph 出图并开始教学。注意：不要向用户展示你的思考过程、计划步骤或任何内部推理，直接给出最终结果。回复必须全部使用中文。"
                    );
                    session.handle_message(&retry_kickoff).await;
                }
            }

            // 建库流程结束（成功/重试完毕），停止心跳
            hb_done.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            let reply = "⚠ 会话未就绪，请稍后再试".to_string();
            self.reply_to_channel(&inbound.channel, &inbound.chat_id, &reply).await;
        }
    }

    /// /learn export <名称> [--kb]：导出知识库为 .kb.json（纯本地操作，零 token）
    fn handle_kb_export_command(&self, arg: &str) -> String {
        use crate::knowledge as kb;

        // 解析参数：名称（可含空格）+ 可选 --kb 标记（位置不限）
        let rest = arg.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim();
        let mut name_parts: Vec<&str> = Vec::new();
        let mut kb_only = false;
        for t in rest.split_whitespace() {
            if t == "--kb" || t == "--kb-only" {
                kb_only = true;
            } else {
                name_parts.push(t);
            }
        }
        let name = name_parts.join(" ");
        if name.is_empty() {
            return "用法：/learn export <名称> [--kb]\n  默认导出库＋学习记录（掌握度/测验史/会话）；--kb 只导库结构，适合分享他人从零学习".to_string();
        }

        let conn = match kb::open_db() {
            Ok(c) => c,
            Err(e) => return format!("⚠ 知识库打开失败: {e}"),
        };
        let tid = match kb::store::topic_id(&conn, &name) {
            Ok(Some(tid)) => tid,
            Ok(None) => return format!("⚠ 知识库「{name}」不存在。/learn list 查看已有库。"),
            Err(e) => return format!("⚠ 查询失败: {e}"),
        };
        let data = match kb::store::export_topic(&conn, tid, !kb_only) {
            Ok(d) => d,
            Err(e) => return format!("⚠ 导出失败: {e}"),
        };
        let json = match serde_json::to_string_pretty(&data) {
            Ok(j) => j,
            Err(e) => return format!("⚠ 序列化失败: {e}"),
        };
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let path = kb::exports_dir().join(format!("{}-{ts}.kb.json", sanitize_filename(&name)));
        if let Err(e) = std::fs::write(&path, json) {
            return format!("⚠ 写入导出文件失败: {e}");
        }

        let n = data.graph.nodes.len();
        let e = data.graph.edges.len();
        if kb_only {
            format!(
                "📦 导出完成：「{name}」纯库模式\n   知识点 {n} 个 · 关系 {e} 条\n   文件：{}\n   分享给他人从零学习；用 /learn import <路径> 导入",
                path.display()
            )
        } else {
            let l = data.learning.as_ref();
            let quiz = l.map(|x| x.quiz_log.len()).unwrap_or(0);
            let steps = l.map(|x| x.sessions.len()).unwrap_or(0);
            format!(
                "📦 导出完成：「{name}」完整版（含学习记录）\n   知识点 {n} 个 · 关系 {e} 条 · 测验记录 {quiz} 条 · 学习会话 {steps} 次\n   文件：{}\n   备份/换机：拷贝此文件到新机器，/learn import <路径> 导入后进度无损续接",
                path.display()
            )
        }
    }

    /// /learn import <路径> [--as 新名]：导入 .kb.json 导出文件（纯本地操作，零 token）
    fn handle_kb_import_command(&self, arg: &str) -> String {
        use crate::knowledge as kb;

        // 解析参数：文件路径 + 可选 --as 新名
        let rest = arg.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim();
        let mut tokens = rest.split_whitespace();
        let mut path_parts: Vec<&str> = Vec::new();
        let mut new_name: Option<String> = None;
        while let Some(t) = tokens.next() {
            if t == "--as" {
                new_name = tokens.next().map(|s| s.to_string());
            } else {
                path_parts.push(t);
            }
        }
        let path_raw = path_parts.join(" ");
        if path_raw.is_empty() {
            return "用法：/learn import <文件路径> [--as 新名]\n  导入 .kb.json 导出文件；目标库已存在时用 --as 换名导入".to_string();
        }
        let file_path = if path_raw.starts_with("~/") {
            let home = std::env::var("HOME").unwrap_or_default();
            path_raw.replacen("~", &home, 1)
        } else {
            path_raw
        };

        let text = match std::fs::read_to_string(&file_path) {
            Ok(t) => t,
            Err(e) => return format!("⚠ 读取导出文件失败: {file_path}（{e}）"),
        };
        let data: kb::store::KbExport = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(e) => return format!("⚠ 不是有效的知识库导出文件（{file_path}）: {e}"),
        };
        let conn = match kb::open_db() {
            Ok(c) => c,
            Err(e) => return format!("⚠ 知识库打开失败: {e}"),
        };
        match kb::store::import_topic(&conn, &data, new_name.as_deref()) {
            Ok(rep) => {
                let mut lines = format!(
                    "✅ 导入成功：「{}」\n   知识点 {} 个 · 关系 {} 条（跳过 {}）",
                    rep.topic, rep.nodes_imported, rep.edges_ok, rep.edges_skipped
                );
                if rep.with_learning {
                    lines.push_str(&format!(
                        "\n   学习记录已还原：测验 {} 条 · 会话 {} 次",
                        rep.quiz_log_imported, rep.sessions_imported
                    ));
                }
                lines.push_str(&format!("\n   输入 /learn {} 开始学习", rep.topic));
                lines
            }
            Err(e) => format!("⚠ 导入失败: {e}"),
        }
    }

    /// /learn reset <名称>：复位学习进度（清空掌握度/测验史/会话，保留知识结构）
    fn handle_kb_reset_command(&self, arg: &str) -> String {
        use crate::knowledge as kb;

        let rest = arg.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim();
        let name = rest.trim();
        if name.is_empty() {
            return "用法：/learn reset <名称>\n  复位该知识库的学习进度（掌握度/测验史/学习会话归零），知识点与关系保留。".to_string();
        }

        let conn = match kb::open_db() {
            Ok(c) => c,
            Err(e) => return format!("⚠ 知识库打开失败: {e}"),
        };
        let tid = match kb::store::topic_id(&conn, name) {
            Ok(Some(tid)) => tid,
            Ok(None) => return format!("⚠ 知识库「{name}」不存在。/learn list 查看已有库。"),
            Err(e) => return format!("⚠ 查询失败: {e}"),
        };
        match kb::store::reset_topic_progress(&conn, tid) {
            Ok((nodes, quiz, sessions)) => format!(
                "✅ 学习进度已复位：「{name}」\n   保留知识点 {nodes} 个（结构不变）\n   已清除测验记录 {quiz} 条 · 学习会话 {sessions} 次\n   掌握度全部归零。输入 /learn {name} 从头开始学习。"
            ),
            Err(e) => format!("⚠ 复位失败: {e}"),
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
