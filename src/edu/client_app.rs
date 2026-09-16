//! 学生侧「进入课堂」客户端：按配置**同时**接入多位老师的多个教学班。
//!
//! 对应设计文档 §10.4 的 `StudentRuntime` 装配，视角是「学生的一条命令」：
//!
//! ```bash
//! rhermes-stu live              # 读 [edu.student] + [[edu.student.teachers]]
//! rhermes-stu live --offline    # 离网/局域网（无中继，仅显式地址直连）
//! ```
//!
//! ## 关键点：一个学生节点，多位老师
//!
//! 学生的 `EndpointId` **全程只有一个**（一台设备一把钥匙，由
//! [`super::identity`] 持久化）。它在每位老师处分别被白名单收录，老师之间互不感知；
//! 于是「一位学生选多位老师的课」不需要开多个进程，也不会出现身份错位。
//!
//! ## 分层
//!
//! [`join_from_config`] 是**纯逻辑**（不碰标准输入），因此可以在测试里直接调用；
//! [`run_live`] 只是「读配置 → 调它 → 进交互台」的薄壳。

use std::path::Path;

use iroh::{EndpointAddr, EndpointId};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;

use crate::core::{Config, EduTeacherPeer};

use super::authz::AuthRegistry;
use super::host::{self, HostEvent, SectionHost};
use super::identity;
use super::net::{client, P2pNode};
// `StudentRuntime` 仍在此处使用：`join_from_config` 负责**建连装配**
// （那是宿主的输入），装配完成后再交给 `SectionHost` 独占驱动。
use super::runtime::{Enrollment, SectionKey, StudentRuntime};
use super::serve::home_dir;

// `Joined` / `LiveSession` 已下沉到 `host` —— 宿主是下层，接入结果是它的输入。
// 这里 re-export 保持既有引用路径（`edu::client_app::Joined` 等）不断。
pub use super::host::{Joined, LiveSession};

/// 把配置里的一位老师解析成 `EndpointAddr`。
///
/// 只看 id 时可用于生产（N0 中继 + DNS 发现会补全地址）；
/// 离网 / 局域网场景必须同时给 `addr`（`host:port`），否则没有可达路径。
pub fn peer_to_addr(peer: &EduTeacherPeer) -> anyhow::Result<EndpointAddr> {
    let raw = peer.teacher.trim();
    if raw.is_empty() {
        anyhow::bail!("[[edu.student.teachers]] 缺少 teacher（老师 EndpointId）");
    }
    let id: EndpointId = raw
        .parse()
        .map_err(|e| anyhow::anyhow!("老师地址 '{raw}' 不是合法 EndpointId：{e}"))?;
    let mut addr = EndpointAddr::new(id);

    let ip = peer.addr.trim();
    if !ip.is_empty() {
        let sock: std::net::SocketAddr = ip
            .parse()
            .map_err(|e| anyhow::anyhow!("老师直连地址 '{ip}' 非法（应为 host:port）：{e}"))?;
        addr = addr.with_ip_addr(sock);
    }
    Ok(addr)
}

/// 学生本机学号（新字段优先，回退旧字段）
fn resolve_student_no(cfg: &Config) -> &str {
    let s = cfg.edu.student.student_no.trim();
    if s.is_empty() {
        cfg.edu.student_no.trim()
    } else {
        s
    }
}

/// 从配置接入全部老师（**纯逻辑，可测**）。
///
/// `offline_override = true` 强制离网；否则取「任一老师标了 offline」。
/// `verbose` 控制进度输出（测试里关掉更清爽）。
pub async fn join_from_config(
    cfg: &Config,
    home: &Path,
    offline_override: bool,
    verbose: bool,
) -> anyhow::Result<LiveSession> {
    let student_no = resolve_student_no(cfg);
    if student_no.is_empty() {
        anyhow::bail!(
            "缺少学号 —— 请在 [edu.student].student_no（或旧字段 [edu].student_no）中填写"
        );
    }
    if cfg.edu.student.teachers.is_empty() {
        anyhow::bail!(
            "还没有选修任何老师 —— 请在配置里加上：\n\n[[edu.student.teachers]]\nteacher = \"<老师 EndpointId>\"\naddr    = \"<host:port>\"   # 离网/局域网必填\npassword = \"<密码>\"\n"
        );
    }

    let student_no = student_no.to_string();
    let display_name = {
        let d = cfg.edu.student.display_name.trim();
        if d.is_empty() { student_no.clone() } else { d.to_string() }
    };

    let offline = offline_override || cfg.edu.student.teachers.iter().any(|p| p.offline);

    let key = identity::load_or_import(
        &identity::identity_dir(home),
        &student_no,
        &cfg.edu.student.secret_key,
    )?;

    if verbose {
        println!();
        println!("🎒 学生端进入课堂");
        println!("   学号     : {student_no}（{display_name}）");
        println!(
            "   身份     : {} · {}",
            identity::key_path(&identity::identity_dir(home), &student_no).display(),
            identity::fingerprint(&key)
        );
        println!(
            "   模式     : {}",
            if offline { "离网直连（无中继）" } else { "N0 中继 + 发现" }
        );
        println!();
    }

    // ── 建节点（同一把钥匙贯穿「认证」与「入班」）──
    let node = P2pNode::student_with_key(AuthRegistry::new(), offline, Some(key)).await?;

    // ── 逐位老师认证，各自换回票据 ──
    let mut enrollments: Vec<Enrollment> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for peer in &cfg.edu.student.teachers {
        let addr = match peer_to_addr(peer) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{e}"));
                continue;
            }
        };
        let sno = peer.effective_student_no(&student_no);
        let pw = peer.effective_password(&cfg.edu.auth_token);

        let resp = match client::authenticate(&node.endpoint, addr.clone(), sno, pw).await {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}（{e}）", short_id(&addr.id)));
                continue;
            }
        };
        if !resp.ok {
            failures.push(format!("{}（{}）", short_id(&addr.id), resp.message));
            continue;
        }
        if resp.tickets.is_empty() {
            failures.push(format!("{}（未下发任何教学班票据）", short_id(&addr.id)));
            continue;
        }
        if verbose {
            println!(
                "   ✅ 已认证 {} · 拿到 {} 个教学班票据",
                short_id(&addr.id),
                resp.tickets.len()
            );
        }
        enrollments.push(Enrollment { teacher_addr: addr, tickets: resp.tickets });
    }

    if enrollments.is_empty() {
        anyhow::bail!("没有任何一位老师接入成功：\n   {}", failures.join("\n   "));
    }

    // ── 登记标签（票据里就带课程码 / 班级名，无需再问老师）──
    let mut joined: Vec<Joined> = Vec::new();
    for e in &enrollments {
        for t in &e.tickets {
            joined.push(Joined {
                key: SectionKey::new(e.teacher_addr.id, t.section_id),
                course_code: t.course_code.clone(),
                course_name: t.course_name.clone(),
                class_name: t.section_name.clone(),
            });
        }
    }

    // ── 入班（复用同一节点：认证过的身份 == 入班的身份）──
    let rt = StudentRuntime::from_node_multi(node, enrollments).await?;

    Ok(LiveSession { rt, joined, student_no, display_name, failures })
}

/// 命令入口：`rhermes-stu live [--offline]`
///
/// 这是**第一个** [`SectionHost`] 驱动：与会话打交道的事全交给宿主，
/// 本函数只剩「读配置 → 建连 → 起宿主 → 打印欢迎 → 进交互台 → 收尾」。
pub async fn run_live(config_path: &Path, args: &[String]) -> anyhow::Result<()> {
    let cfg = Config::load(config_path).map_err(|e| anyhow::anyhow!("读取配置失败：{e}"))?;
    let home = home_dir(config_path);
    let offline = args.iter().any(|a| a == "--offline");

    let session = join_from_config(&cfg, &home, offline, true).await?;

    // 起宿主：从这一刻起会话归宿主独占的 task，本函数只经命令 / 订阅与它打交道
    let host = SectionHost::start(session).await?;
    // 订阅必须尽量早 —— 宿主广播之前无订阅者的事件会被丢弃（有意），
    // 因此不能等到欢迎语打完才订阅。
    let events = host.subscribe();
    let snap = host.snapshot();

    for f in &snap.failures {
        println!("   ⚠️  接入失败：{f}");
    }
    println!();
    println!("   📚 已接入 {} 个教学班：", snap.joined.len());
    for j in &snap.joined {
        println!("      [{}] {} · {}", j.key, j.course_code, j.class_name);
    }
    println!();
    println!("   输入 help 查看命令，quit 退出。");
    println!();

    repl(&host, events).await;

    let _ = host.shutdown().await;
    println!("👋 已离开课堂");
    Ok(())
}

/// 事件 + 命令双路循环（第一个 [`SectionHost`] 驱动）。
///
/// ⚠️ 与 v0.7.13 的机制差别：事件改从宿主 `broadcast` 来，回执改走 `oneshot`。
///    事件**先到先看** —— 订阅之前发生的事件会被丢弃。这是有意的：界面还没
///    就绪时本就无从展示，回放旧通知只会让学生以为「刚发生」。
async fn repl(host: &SectionHost, mut events: broadcast::Receiver<HostEvent>) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    enum Tick {
        Event(Option<HostEvent>),
        Line(Option<String>),
        Interrupt,
    }

    loop {
        // `broadcast::Receiver::recv` 与 `lines.next_line` 都是取消安全的。
        let tick = tokio::select! {
            ev = events.recv() => Tick::Event(ev.ok()),
            l = lines.next_line() => Tick::Line(l.ok().flatten()),
            _ = tokio::signal::ctrl_c() => Tick::Interrupt,
        };

        match tick {
            Tick::Interrupt => {
                println!();
                break;
            }
            Tick::Line(None) => break, // EOF
            // 广播通道关闭（宿主 task 已退出）——等价于 v0.7.13 的 next_event()==None
            Tick::Event(None) => {
                println!("⚠️  与教学班的连接已结束");
                break;
            }
            Tick::Event(Some(ev)) => {
                if let Some(line) = host::render_event(&ev, &host.snapshot().labels) {
                    println!("{line}");
                }
                // `Stopped` 不上屏，但它意味着运行时已拆除 —— 收尾
                if ev == HostEvent::Stopped {
                    println!("⚠️  与教学班的连接已结束");
                    break;
                }
            }
            Tick::Line(Some(line)) => {
                if !handle_command(host, &line).await {
                    break;
                }
            }
        }
    }
}

/// 处理一条命令；返回 `false` 表示退出。
///
/// 只读 `host.snapshot()` 拿接入列表，发消息走 `host.ask` / `host.chat`
/// —— 驱动不再接触 `StudentRuntime`。
async fn handle_command(host: &SectionHost, line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let snap = host.snapshot();

    match cmd {
        "quit" | "exit" | "q" => return false,
        "help" | "?" => print_help(),
        "list" | "ls" => {
            println!("   课程        班级                      老师");
            for j in &snap.joined {
                println!(
                    "   {:<10} {:<24} {}",
                    j.course_code,
                    j.class_name,
                    short_id(&j.key.teacher)
                );
            }
        }
        "ask" | "chat" => {
            let rest: Vec<&str> = parts.collect();
            if rest.len() < 3 {
                println!("   用法：{cmd} <课程码> <班级> <内容>");
                return true;
            }
            let (code, class) = (rest[0], rest[1]);
            let text = rest[2..].join(" ");
            match snap
                .joined
                .iter()
                .find(|j| j.course_code == code && j.class_name == class)
            {
                Some(j) => {
                    let sent = if cmd == "ask" {
                        host.ask(&j.key, &text).await
                    } else {
                        host.chat(&j.key, &text).await
                    };
                    match sent {
                        Ok(()) => println!("   ✅ 已发送到 {} / {}", code, class),
                        Err(e) => println!("   ❌ 发送失败：{e}"),
                    }
                }
                None => println!("   ❌ 没有接入 '{code} / {class}' —— 先 list 看看"),
            }
        }
        other => println!("   未知命令 `{other}` —— 输入 help 查看帮助"),
    }
    true
}

fn print_help() {
    println!("   命令：");
    println!("     list                              列出已接入的教学班");
    println!("     ask  <课程码> <班级> <问题>        向该班提问");
    println!("     chat <课程码> <班级> <内容>        向该班发讨论");
    println!("     quit                              退出");
}

/// EndpointId 前 8 位（避免刷屏）
fn short_id(id: &EndpointId) -> String {
    let s = id.to_string();
    s[..8.min(s.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(teacher: &str, addr: &str) -> EduTeacherPeer {
        EduTeacherPeer {
            teacher: teacher.to_string(),
            addr: addr.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_peer_to_addr_id_only() {
        let id = iroh::SecretKey::generate().public();
        let a = peer_to_addr(&peer(&id.to_string(), "")).unwrap();
        assert_eq!(a.id, id);
        assert!(a.addrs.is_empty(), "只给 id 时不应有显式地址（靠发现服务）");
    }

    #[test]
    fn test_peer_to_addr_with_ip() {
        let id = iroh::SecretKey::generate().public();
        let a = peer_to_addr(&peer(&id.to_string(), "127.0.0.1:5000")).unwrap();
        assert_eq!(a.ip_addrs().count(), 1);
        let ip = a.ip_addrs().next().unwrap();
        assert_eq!(ip.to_string(), "127.0.0.1:5000");
    }

    #[test]
    fn test_peer_to_addr_rejects_bad_input() {
        assert!(peer_to_addr(&peer("", "")).is_err(), "空 teacher 必须报错");
        assert!(peer_to_addr(&peer("not-hex", "")).is_err(), "非法 id 必须报错");
        let id = iroh::SecretKey::generate().public();
        assert!(
            peer_to_addr(&peer(&id.to_string(), "not-a-socket")).is_err(),
            "非法直连地址必须报错"
        );
    }

    /// 取「必然失败」的错误文本。
    ///
    /// 不用 `unwrap_err()`：那要求 `Ok` 侧实现 `Debug`，而 `LiveSession` 内含
    /// 运行时对象（会话 / 接收端），刻意不实现 `Debug`。
    async fn join_err(cfg: &Config, home: &std::path::Path) -> String {
        match join_from_config(cfg, home, true, false).await {
            Ok(_) => panic!("本应报错，却意外成功了"),
            Err(e) => e.to_string(),
        }
    }

    #[tokio::test]
    async fn test_join_from_config_requires_student_no_and_teachers() {
        let tmp = tempfile::tempdir().unwrap();

        // 没有学号 → 报错
        let cfg = Config::default();
        let err = join_err(&cfg, tmp.path()).await;
        assert!(err.contains("学号"), "{err}");

        // 有学号但没老师 → 报错
        let mut cfg = Config::default();
        cfg.edu.student.student_no = "2024001".into();
        let err = join_err(&cfg, tmp.path()).await;
        assert!(err.contains("选修"), "{err}");
    }

    #[test]
    fn test_resolve_student_no_falls_back_to_legacy() {
        let mut cfg = Config::default();
        cfg.edu.student_no = "8001".into();
        assert_eq!(resolve_student_no(&cfg), "8001");

        // 新字段优先
        cfg.edu.student.student_no = "9001".into();
        assert_eq!(resolve_student_no(&cfg), "9001");
    }
}
