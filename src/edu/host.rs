//! 驱动无关的**教学班会话宿主**（`SectionHost`）—— 学生端所有前端共用的那一层。
//!
//! ## 为什么要有它
//!
//! 学生端原本只有一个前端：`rhermes-stu live` 的 REPL（[`super::client_app`]）。
//! 它把三件事揉在一处：
//!
//! 1. **持有会话** —— [`StudentRuntime`]（它独占 `events` 接收端，且
//!    `next_event(&mut self)` 要 `&mut`）；
//! 2. **终端 I/O** —— `tokio::select!` 三路（事件 / stdin / ctrl_c）；
//! 3. **事件渲染** —— 把 [`SectionEvent`] 变成给人看的中文文本。
//!
//! TUI 与微信/企微/Telegram 要的是**同一批能力**，差别只在「消息从哪来」「结果往哪去」。
//! 若照抄三遍，网络、纪元轮换、白名单这三块最不该分叉的逻辑就会分叉成四份。
//!
//! 所以本模块只做一件事：**管会话，不管界面**。
//!
//! ```text
//!   驱动侧                                  宿主（本模块）
//!   ┌────────┐   HostCommand                ┌──────────────────────────┐
//!   │ REPL   │─────────────────────────────►│ 独占 task: StudentRuntime │
//!   ├────────┤                              │  loop { next_event() }   │
//!   │ TUI    │─────────────────────────────►└────────────┬─────────────┘
//!   ├────────┤                                           │ broadcast
//!   │ 微信   │◄──────────────────────────────────────────┘
//!   │ 企微/TG│   subscribe() → HostEvent → 各自渲染/推送
//!   └────────┘
//! ```
//!
//! ## 两条硬约束（照抄自 `runtime.rs`，改不得）
//!
//! - `StudentRuntime::next_event(&mut self)` 需要 `&mut`，且 `events` 是
//!   `mpsc::UnboundedReceiver`（**非 `Sync`**）→ 运行时**必须独占一个 tokio task**，
//!   不能 `Arc<Mutex<..>>` 让多个驱动直接调。因此对外只暴露「命令进、事件出」。
//! - `broadcast::Receiver::send` 需要多订阅者 → 事件走 `broadcast` 通道，
//!   REPL / TUI / 渠道各自 `subscribe()`，互不阻塞。
//!
//! ## 纯函数边界
//!
//! [`translate`]（`SectionEvent → HostEvent`）与 [`render_event`]（`HostEvent → 一行文本`）
//! **都不碰网络、不碰 IO**，因此能被单测钉死。这是从 v0.7.13 的缺陷 K 学到的：
//! 凡是「只做映射」的逻辑都要独立成纯函数，否则它只能靠真机冒烟才会暴露。

use std::collections::HashMap;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::gossip::SectionMsg;
use super::runtime::{SectionEvent, SectionKey, StudentRuntime};

/// 事件广播缓冲深度。订阅者跟不上时允许丢旧事件（只影响展示，不影响协议状态）。
pub const NOTICE_BUFFER: usize = 256;

/// 等待宿主 task 收尾的上限。
///
/// 退出路径（Ctrl-C / 渠道 `/class stop`）最忌卡死：网络拆除若因对端失联而拖长，
/// 也不能把调用方一起拖住。
pub const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ---------------------------------------------------------------------------
// 接入结果（原在 client_app.rs，移到此处让宿主成为下层）
// ---------------------------------------------------------------------------

/// 已接入的一个教学班（含展示用的课程/班级名）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub key: SectionKey,
    pub course_code: String,
    pub course_name: String,
    pub class_name: String,
}

/// 一次「进入课堂」的结果
pub struct LiveSession {
    pub rt: StudentRuntime,
    /// 已接入的教学班（票据里直接带出课程码 / 班级名，无需再问老师）
    pub joined: Vec<Joined>,
    pub student_no: String,
    pub display_name: String,
    /// 认证失败 / 被拒的老师（不致命，只影响其名下班级）
    pub failures: Vec<String>,
}

// ---------------------------------------------------------------------------
// 通知（宿主 → 驱动）
// ---------------------------------------------------------------------------

/// 通知的类别。**只用于过滤，不参与排版** —— 排版统一走 [`render_event`]。
///
/// 渠道驱动默认只推 [`NoticeKind::Announce`] / [`NoticeKind::Assignment`] /
/// [`NoticeKind::Answer`]；[`NoticeKind::Chat`] 与 [`NoticeKind::Question`]
/// 量大，按需开启，避免刷屏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// 教师公告 / 模式切换
    Announce,
    /// 作业发布（只带哈希与票据，文件走 blobs）
    Assignment,
    /// 班内提问
    Question,
    /// 老师或同学的回答
    Answer,
    /// 班内自由讨论
    Chat,
}

/// 宿主 → 驱动。与终端 / 渠道无关的**抽象通知**。
///
/// 实现 `PartialEq` 是为了让测试能直接比对映射结果（`translate` 是纯函数，
/// 结果可判定相等）。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// 班内有人说话（公告 / 作业 / 提问 / 回答 / 讨论）
    Notice {
        key: SectionKey,
        kind: NoticeKind,
        /// 已经过 `SectionMsg::summary()` 渲染，驱动无需再认识协议细节
        text: String,
    },
    /// 白名单生效（入班 / 撤销落地）。
    ///
    /// `members` 为 `None` 只出现在「防御性路径」：正常流程里
    /// `AllowlistUpdate` 消息由 runtime 收循环就地消化成
    /// [`SectionEvent::AllowlistApplied`]（那里带着解析后的真实人数），
    /// 不会以 `Message` 形态到达驱动。真到了也不该把 `0 人` 报给用户。
    Roster {
        key: SectionKey,
        epoch: u32,
        members: Option<usize>,
    },
    /// Topic 已轮换（被撤销的同伴已被踢出，本端自动换到新 Topic）
    Rotated { key: SectionKey, new_epoch: u32 },
    /// 某个班的接收循环结束
    Closed { key: SectionKey },
    /// 宿主已退出（运行时已拆除），驱动收到即收尾
    Stopped,
}

// ---------------------------------------------------------------------------
// 命令（驱动 → 宿主）
// ---------------------------------------------------------------------------

/// 驱动 → 宿主。
///
/// `reply` 让「发消息」这条命令有返回值 —— 保留 REPL 原有的
/// `✅ 已发送` / `❌ 发送失败：…` 反馈，同时不把运行时泄漏给驱动。
pub enum HostCommand {
    /// 向该班提问
    Ask {
        key: SectionKey,
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 向该班发一条讨论
    Chat {
        key: SectionKey,
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 优雅停止：拆掉运行时并结束 task
    Shutdown,
}

// ---------------------------------------------------------------------------
// 快照（同步读，驱动渲染「我接入了什么」用）
// ---------------------------------------------------------------------------

/// 接入结果的可读快照。
///
/// **接入后不再变化**，所以不需要锁，读到的一律是 [`SectionHost::start`]
/// 那一刻的事实（本批不支持运行中动态入班，见设计 §12.6）。
#[derive(Debug, Clone)]
pub struct HostSnapshot {
    pub student_no: String,
    pub display_name: String,
    pub joined: Vec<Joined>,
    /// 班键 → 「CS101 / 计算机2301」这类可读标签
    pub labels: HashMap<SectionKey, String>,
    /// 建连失败的老师（部分接入时给学生看）
    pub failures: Vec<String>,
}

// ---------------------------------------------------------------------------
// 宿主
// ---------------------------------------------------------------------------

/// 驱动无关的教学班会话宿主。
///
/// 生命周期：[`SectionHost::start`] 接过一个已经建好连的 [`LiveSession`]，
/// 把 [`StudentRuntime`] 交给独占 task；驱动侧只通过
/// [`SectionHost::ask`] / [`SectionHost::chat`] 发命令、
/// 通过 [`SectionHost::subscribe`] 收通知，最后 [`SectionHost::shutdown`] 收尾。
pub struct SectionHost {
    cmd_tx: mpsc::UnboundedSender<HostCommand>,
    event_tx: broadcast::Sender<HostEvent>,
    snapshot: HostSnapshot,
    task: JoinHandle<()>,
}

impl SectionHost {
    /// 接过一个已建连的会话，启动宿主 task。
    pub async fn start(session: LiveSession) -> anyhow::Result<Self> {
        let LiveSession {
            mut rt,
            joined,
            student_no,
            display_name,
            failures,
        } = session;

        let labels = build_labels(&joined);
        let snapshot = HostSnapshot {
            student_no: student_no.clone(),
            display_name: display_name.clone(),
            joined,
            labels,
            failures,
        };

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HostCommand>();
        let (event_tx, _) = broadcast::channel::<HostEvent>(NOTICE_BUFFER);

        let ev_tx = event_tx.clone();
        let task = tokio::spawn(async move {
            loop {
                // ⚠️ 两个分支都只借用 `rt` 到 select 结束，因此在 match 里可以安全地
                //    可变借用它。`Receiver::recv` 是取消安全的，分支被丢弃不会丢事件。
                tokio::select! {
                    ev = rt.next_event() => match ev {
                        Some(ev) => {
                            if let Some(he) = translate(ev) {
                                let _ = ev_tx.send(he);
                            }
                        }
                        // 所有班的接收循环都结束了 —— 无源可听，收摊
                        None => break,
                    },
                    cmd = cmd_rx.recv() => match cmd {
                        Some(HostCommand::Ask { key, text, reply }) => {
                            let msg =
                                SectionMsg::question(&student_no, &display_name, "", &text);
                            let sent = rt.broadcast(&key, &msg).await.map_err(|e| e.to_string());
                            let _ = reply.send(sent);
                        }
                        Some(HostCommand::Chat { key, text, reply }) => {
                            let msg = SectionMsg::chat(&student_no, &display_name, &text);
                            let sent = rt.broadcast(&key, &msg).await.map_err(|e| e.to_string());
                            let _ = reply.send(sent);
                        }
                        // 显式停止，或驱动全部消失（cmd_rx 关闭）
                        Some(HostCommand::Shutdown) | None => break,
                    },
                }
            }

            // 先拆运行时，再报 Stopped —— 驱动收到 Stopped 时资源已释放干净
            rt.shutdown().await.ok();
            let _ = ev_tx.send(HostEvent::Stopped);
        });

        Ok(Self {
            cmd_tx,
            event_tx,
            snapshot,
            task,
        })
    }

    /// 接入结果快照（同步读，无需 async）
    pub fn snapshot(&self) -> &HostSnapshot {
        &self.snapshot
    }

    /// 订阅宿主通知。可多个驱动同时订阅（broadcast）。
    pub fn subscribe(&self) -> broadcast::Receiver<HostEvent> {
        self.event_tx.subscribe()
    }

    /// 向某个教学班提问。
    pub async fn ask(&self, key: &SectionKey, text: &str) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(HostCommand::Ask {
                key: key.clone(),
                text: text.to_string(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("教学班会话已结束"))?;
        finish_reply(rx.await)
    }

    /// 向某个教学班发一条讨论。
    pub async fn chat(&self, key: &SectionKey, text: &str) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(HostCommand::Chat {
                key: key.clone(),
                text: text.to_string(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("教学班会话已结束"))?;
        finish_reply(rx.await)
    }

    /// 优雅停止：拆运行时 + 结束 task，超时兜底为强制中止。
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = self.cmd_tx.send(HostCommand::Shutdown);
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut self.task)
            .await
            .is_err()
        {
            // 网络拆除拖太久 —— 不能让退出路径卡死
            self.task.abort();
        }
        Ok(())
    }
}

impl Drop for SectionHost {
    fn drop(&mut self) {
        // 忘记 shutdown 时不留孤儿 task（那会把 P2P 节点一直挂着）
        self.task.abort();
    }
}

/// 把「发消息」的返回值统一成 `anyhow::Result`。
fn finish_reply(reply: Result<Result<(), String>, oneshot::error::RecvError>) -> anyhow::Result<()> {
    match reply {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
        Err(_) => Err(anyhow::anyhow!("教学班会话已结束")),
    }
}

// ---------------------------------------------------------------------------
// 纯函数：协议事件 → 宿主通知 → 一行文本
// ---------------------------------------------------------------------------

/// 班键 → 可读标签（`CS101 / 计算机2301`）。
pub fn build_labels(joined: &[Joined]) -> HashMap<SectionKey, String> {
    joined
        .iter()
        .map(|j| {
            (
                j.key.clone(),
                format!("{} / {}", j.course_code, j.class_name),
            )
        })
        .collect()
}

/// `SectionMsg` 的内容类别。返回 `None` 表示它不该以「班内发言」的形式展示。
fn notice_kind(msg: &SectionMsg) -> Option<NoticeKind> {
    match msg {
        SectionMsg::Announce { .. } => Some(NoticeKind::Announce),
        SectionMsg::AssignmentPosted { .. } => Some(NoticeKind::Assignment),
        SectionMsg::Question { .. } => Some(NoticeKind::Question),
        SectionMsg::Answer { .. } => Some(NoticeKind::Answer),
        SectionMsg::Chat { .. } => Some(NoticeKind::Chat),
        // 这两条由 runtime 收循环**就地消化**成 `AllowlistApplied` / `TopicRotated`
        // （见 `runtime.rs` 的收循环），正常不会以 `Message` 形态到达这里。
        // 若真的到达，[`translate`] 里的前置分支会先接住，不会落到这里。
        SectionMsg::TopicRotate { .. } | SectionMsg::AllowlistUpdate { .. } => None,
    }
}

/// 协议事件 → 宿主通知（**纯函数**）。
///
/// 返回 `None` 表示这条事件对驱动没有展示价值（目前只有「不该出现的形态」）。
pub fn translate(ev: SectionEvent) -> Option<HostEvent> {
    match ev {
        SectionEvent::Message { key, msg } => match msg {
            // ⚠️ 这两条在 runtime 收循环里已被消化，这里纯属兜底：
            //    宁可语义正确，也不要退化成一个语义不对的 `Notice`。
            SectionMsg::TopicRotate { new_epoch, .. } => {
                Some(HostEvent::Rotated { key, new_epoch })
            }
            SectionMsg::AllowlistUpdate { epoch, .. } => Some(HostEvent::Roster {
                key,
                epoch,
                members: None,
            }),
            other => notice_kind(&other).map(|kind| HostEvent::Notice {
                key,
                kind,
                text: other.summary(),
            }),
        },
        SectionEvent::AllowlistApplied {
            key,
            epoch,
            members,
        } => Some(HostEvent::Roster {
            key,
            epoch,
            members: Some(members),
        }),
        SectionEvent::TopicRotated { key, new_epoch } => {
            Some(HostEvent::Rotated { key, new_epoch })
        }
        SectionEvent::Closed { key } => Some(HostEvent::Closed { key }),
    }
}

/// 宿主通知 → 一行文本（**纯函数**）。各驱动共用，保证终端 / TUI / 渠道显示一致。
///
/// 返回 `None` 表示这条通知不上屏（目前只有 [`HostEvent::Stopped`]，驱动应借此收尾）。
pub fn render_event(ev: &HostEvent, labels: &HashMap<SectionKey, String>) -> Option<String> {
    let tag = |k: &SectionKey| labels.get(k).cloned().unwrap_or_else(|| k.to_string());
    match ev {
        HostEvent::Notice { key, text, .. } => Some(format!("📨 [{}] {text}", tag(key))),
        HostEvent::Roster {
            key,
            epoch,
            members: Some(n),
        } => Some(format!(
            "🔐 [{}] 成员表已更新：{n} 人 · epoch {epoch}",
            tag(key)
        )),
        HostEvent::Roster {
            key,
            epoch,
            members: None,
        } => Some(format!("🔐 [{}] 成员白名单已更新（纪元 {epoch}）", tag(key))),
        HostEvent::Rotated { key, new_epoch } => Some(format!(
            "🔄 [{}] Topic 已轮换 → epoch {new_epoch}",
            tag(key)
        )),
        HostEvent::Closed { key } => Some(format!("🔌 [{}] 接收循环结束", tag(key))),
        HostEvent::Stopped => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(section_id: i64) -> SectionKey {
        SectionKey::new(iroh::SecretKey::generate().public(), section_id)
    }

    fn msg_event(k: &SectionKey, msg: SectionMsg) -> SectionEvent {
        SectionEvent::Message {
            key: k.clone(),
            msg,
        }
    }

    fn announce(title: &str, body: &str) -> SectionMsg {
        SectionMsg::Announce {
            title: title.into(),
            body: body.into(),
            ts: "2026-09-16T12:00:00Z".into(),
        }
    }

    fn label_map(k: &SectionKey) -> HashMap<SectionKey, String> {
        build_labels(&[Joined {
            key: k.clone(),
            course_code: "CS101".into(),
            course_name: "Python 编程基础".into(),
            class_name: "计算机2301".into(),
        }])
    }

    // ── translate：内容类消息 → Notice（逐个 SectionMsg 变体）──

    #[test]
    fn test_translate_announce_is_notice_announce() {
        let k = key(1);
        let got = translate(msg_event(&k, announce("调课", "周日补课"))).unwrap();
        match got {
            HostEvent::Notice { key, kind, text } => {
                assert_eq!(key, k);
                assert_eq!(kind, NoticeKind::Announce);
                assert!(text.contains("调课") && text.contains("周日补课"), "{text}");
            }
            other => panic!("应为 Notice，实得 {other:?}"),
        }
    }

    #[test]
    fn test_translate_assignment_is_notice_assignment() {
        let k = key(1);
        let ev = msg_event(
            &k,
            SectionMsg::AssignmentPosted {
                assignment_id: 7,
                title: "实验三".into(),
                blob_hash: "aa".into(),
                blob_ticket: "bb".into(),
                due_date: "2026-09-30".into(),
                posted_by: "张老师".into(),
                ts: "t".into(),
            },
        );
        match translate(ev).unwrap() {
            HostEvent::Notice { kind, text, .. } => {
                assert_eq!(kind, NoticeKind::Assignment);
                assert!(text.contains("实验三"), "{text}");
                assert!(text.contains("2026-09-30"), "应带截止日期：{text}");
            }
            other => panic!("应为 Notice，实得 {other:?}"),
        }
    }

    #[test]
    fn test_translate_question_is_notice_question() {
        let k = key(1);
        let ev = msg_event(&k, SectionMsg::question("2024001", "张三", "", "红黑树怎么删"));
        match translate(ev).unwrap() {
            HostEvent::Notice { kind, text, .. } => {
                assert_eq!(kind, NoticeKind::Question);
                assert!(text.contains("张三") && text.contains("红黑树怎么删"), "{text}");
            }
            other => panic!("应为 Notice，实得 {other:?}"),
        }
    }

    #[test]
    fn test_translate_answer_is_notice_answer() {
        let k = key(1);
        let ev = msg_event(
            &k,
            SectionMsg::Answer {
                from: "t001".into(),
                display_name: "张老师".into(),
                reply_to: "q1".into(),
                text: "分四种情况是因为…".into(),
                ts: "t".into(),
            },
        );
        match translate(ev).unwrap() {
            HostEvent::Notice { kind, text, .. } => {
                assert_eq!(kind, NoticeKind::Answer);
                assert!(text.contains("张老师"), "{text}");
            }
            other => panic!("应为 Notice，实得 {other:?}"),
        }
    }

    #[test]
    fn test_translate_chat_is_notice_chat() {
        let k = key(1);
        let ev = msg_event(&k, SectionMsg::chat("2024002", "李四", "同学们好"));
        match translate(ev).unwrap() {
            HostEvent::Notice { kind, text, .. } => {
                assert_eq!(kind, NoticeKind::Chat);
                assert!(text.contains("同学们好"), "{text}");
            }
            other => panic!("应为 Notice，实得 {other:?}"),
        }
    }

    // ── translate：状态类事件 ──

    #[test]
    fn test_translate_allowlist_applied_carries_member_count() {
        let k = key(1);
        let ev = SectionEvent::AllowlistApplied {
            key: k.clone(),
            epoch: 3,
            members: 12,
        };
        assert_eq!(
            translate(ev).unwrap(),
            HostEvent::Roster {
                key: k,
                epoch: 3,
                members: Some(12)
            }
        );
    }

    #[test]
    fn test_translate_topic_rotated_is_rotated() {
        let k = key(1);
        let ev = SectionEvent::TopicRotated {
            key: k.clone(),
            new_epoch: 4,
        };
        assert_eq!(
            translate(ev).unwrap(),
            HostEvent::Rotated {
                key: k,
                new_epoch: 4
            }
        );
    }

    #[test]
    fn test_translate_closed_is_closed() {
        let k = key(1);
        let ev = SectionEvent::Closed { key: k.clone() };
        assert_eq!(translate(ev).unwrap(), HostEvent::Closed { key: k });
    }

    // ── translate：兜底形态（正常不该到达）──

    #[test]
    fn test_translate_message_allowlist_update_is_not_a_notice() {
        // ⚠️ 关键：`AllowlistUpdate` 消息若被当成普通班内发言，
        //    学生会先看到「📨 成员白名单已更新」再看到 runtime 的
        //    「🔐 成员表已更新」—— 同一件事报两遍。必须只出 Roster 一次。
        let k = key(1);
        let ev = msg_event(
            &k,
            SectionMsg::AllowlistUpdate {
                section_id: 1,
                epoch: 5,
                signed: vec![],
            },
        );
        match translate(ev).unwrap() {
            HostEvent::Roster { epoch, members, .. } => {
                assert_eq!(epoch, 5);
                // 拿不到解析后的人数 → 必须如实报 None，不能编个 0
                assert_eq!(members, None, "不得把「未知人数」写成 0 人");
            }
            other => panic!("应为 Roster，实得 {other:?}"),
        }
    }

    #[test]
    fn test_translate_message_topic_rotate_is_rotated() {
        let k = key(1);
        let ev = msg_event(
            &k,
            SectionMsg::TopicRotate {
                new_epoch: 9,
                reason: "撤销".into(),
                ts: "t".into(),
            },
        );
        assert_eq!(
            translate(ev).unwrap(),
            HostEvent::Rotated {
                key: k,
                new_epoch: 9
            }
        );
    }

    // ── render_event：排版（与 v0.7.13 REPL 输出逐字一致）──

    #[test]
    fn test_render_event_keeps_legacy_terminal_format() {
        // 这四条字符串是 v0.7.13 `client_app::render_event` 的原文，
        // 换驱动时**不许**改：真机冒烟脚本就是按它们比对的。
        let k = key(1);
        let labels = label_map(&k);

        let notice = HostEvent::Notice {
            key: k.clone(),
            kind: NoticeKind::Announce,
            text: "📢 调课：周日补课".into(),
        };
        assert_eq!(
            render_event(&notice, &labels).unwrap(),
            "📨 [CS101 / 计算机2301] 📢 调课：周日补课"
        );

        let roster = HostEvent::Roster {
            key: k.clone(),
            epoch: 2,
            members: Some(5),
        };
        assert_eq!(
            render_event(&roster, &labels).unwrap(),
            "🔐 [CS101 / 计算机2301] 成员表已更新：5 人 · epoch 2"
        );

        let rotated = HostEvent::Rotated {
            key: k.clone(),
            new_epoch: 3,
        };
        assert_eq!(
            render_event(&rotated, &labels).unwrap(),
            "🔄 [CS101 / 计算机2301] Topic 已轮换 → epoch 3"
        );

        let closed = HostEvent::Closed { key: k.clone() };
        assert_eq!(
            render_event(&closed, &labels).unwrap(),
            "🔌 [CS101 / 计算机2301] 接收循环结束"
        );
    }

    #[test]
    fn test_render_roster_without_count_does_not_say_zero() {
        let k = key(1);
        let labels = label_map(&k);
        let ev = HostEvent::Roster {
            key: k,
            epoch: 5,
            members: None,
        };
        let out = render_event(&ev, &labels).unwrap();
        assert!(out.contains("成员白名单已更新"), "{out}");
        assert!(!out.contains("0 人"), "不得把未知人数显示成 0 人：{out}");
    }

    #[test]
    fn test_render_stopped_is_none() {
        let labels = HashMap::new();
        assert!(render_event(&HostEvent::Stopped, &labels).is_none());
    }

    #[test]
    fn test_render_falls_back_to_key_when_label_missing() {
        // 标签表里没有的班（例如事件先于快照到达）不能渲染成空白
        let k = key(7);
        let labels = HashMap::new();
        let ev = HostEvent::Closed { key: k.clone() };
        let out = render_event(&ev, &labels).unwrap();
        assert!(out.contains(&k.to_string()), "{out}");
    }

    // ── build_labels ──

    #[test]
    fn test_build_labels_uses_course_and_class() {
        let a = key(1);
        let b = key(2);
        let labels = build_labels(&[
            Joined {
                key: a.clone(),
                course_code: "CS101".into(),
                course_name: "Python 编程基础".into(),
                class_name: "计算机2301".into(),
            },
            Joined {
                key: b.clone(),
                course_code: "MA201".into(),
                course_name: "高等数学".into(),
                class_name: "电信2302".into(),
            },
        ]);
        assert_eq!(labels.get(&a).unwrap(), "CS101 / 计算机2301");
        assert_eq!(labels.get(&b).unwrap(), "MA201 / 电信2302");
        assert_eq!(labels.len(), 2);
    }
}
