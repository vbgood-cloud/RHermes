//! 师端「托管服务」：一次把**多门课程 / 多个班级**挂上 P2P 并保持在线。
//!
//! 对应设计文档 §10.4 的运行时装配，但视角是「长期在线的服务」而不是「一条命令」。
//!
//! ## 为什么要这个命令
//!
//! 老师真实的工作形态是：一学期同时带好几门课、每门课好几个班，而且**一直在线**。
//! 若没有常驻服务，就得为每个班各开一个进程，凭据、Topic、白名单全都散着。
//!
//! `rhermes edu teacher serve` 把这件事收敛成一条命令：
//!
//! ```bash
//! rhermes edu teacher serve                 # 用配置里的 [[edu.teacher.serve]]
//! rhermes edu teacher serve --all           # 该老师名下全部教学班
//! rhermes edu teacher serve CS201 信工2201 CS202 信工2202   # 命令行显式指定
//! rhermes edu teacher serve --offline       # 离网/局域网（无中继，仅直连）
//! ```
//!
//! 启动后进入交互台：`list` / `addr` / `announce …` / `quit`。
//!
//! ## 身份
//!
//! 私钥来自 `home/edu_identities/<工号>.key`（见 [`super::identity`]）——
//! 这是「每位老师一套凭据」的落点。**同一台机器可以并排托管多位老师**
//! （换 `account` 即可），互不干扰。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::core::{Config, EduServeSection};

use super::identity;
use super::runtime::{self, TeacherRuntime};
use super::store::{EduStore, SectionRow, Teacher};

/// 一个待托管的教学班（含展示用的课程码）
#[derive(Debug, Clone)]
pub struct ServedSection {
    pub section_id: i64,
    pub course_code: String,
    pub course_name: String,
    pub class_name: String,
    pub term: String,
    /// 已入班（会话已建立、白名单已广播）
    pub online: bool,
}

/// 从配置里挑出本机司职的老师。
///
/// - `account` 非空 → 按姓名精确匹配（`edu_teachers` 历史表只有 `name` 列）；
/// - `account` 为空 → 库里只有一位老师时默认认领他，多位则列出候选请用户指定。
pub fn pick_teacher(store: &EduStore, account: &str) -> anyhow::Result<Teacher> {
    let account = account.trim();
    if !account.is_empty() {
        return store
            .find_teacher_by_name(account)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "教务库里没有名为 '{account}' 的老师；请检查 [edu.teacher].account 或先运行 `rhermes-teacher init`"
                )
            });
    }

    let all = store.list_teachers()?;
    match all.len() {
        0 => anyhow::bail!("教务库里还没有任何老师 —— 请先初始化教师身份"),
        1 => Ok(all.into_iter().next().expect("长度已判")),
        _ => {
            let names: Vec<String> = all.iter().map(|t| t.name.clone()).collect();
            anyhow::bail!(
                "库里有 {} 位老师（{}），请在 [edu.teacher].account 中指定本机是哪一位",
                all.len(),
                names.join(" / ")
            )
        }
    }
}

/// 把「课程码 + 班级名」解析成库里真实的教学班。
///
/// - `wanted` 为空 → 返回该老师名下**全部**教学班（即 `--all` 的语义）；
/// - `class` 留空 → 该课程下的全部班级；
/// - 找不到即报错（宁可启动失败，也不要静默少挂一个班）。
pub fn resolve_sections(
    store: &EduStore,
    teacher_id: i64,
    wanted: &[EduServeSection],
) -> anyhow::Result<Vec<ServedSection>> {
    // 课程码表：section.course_id → course_code/name
    let courses: std::collections::HashMap<i64, (String, String)> = store
        .list_courses_by_teacher(teacher_id)?
        .into_iter()
        .map(|c| (c.id, (c.course_code, c.name)))
        .collect();

    let to_served = |s: &SectionRow| -> ServedSection {
        let (code, name) = courses
            .get(&s.course_id)
            .cloned()
            .unwrap_or_else(|| (String::from("?"), String::new()));
        ServedSection {
            section_id: s.id,
            course_code: code,
            course_name: name,
            class_name: s.name.clone(),
            term: s.term.clone(),
            online: false,
        }
    };

    if wanted.is_empty() {
        return Ok(store.sections_by_teacher(teacher_id)?.iter().map(to_served).collect());
    }

    let mut out: Vec<ServedSection> = Vec::new();
    let mut push = |sec: &SectionRow, out: &mut Vec<ServedSection>| {
        if !out.iter().any(|s| s.section_id == sec.id) {
            out.push(to_served(sec));
        }
    };

    for w in wanted {
        if w.course.trim().is_empty() {
            anyhow::bail!("托管项缺少课程码（形如 `CS201 信工2201`）");
        }
        let course = store
            .get_course(w.course.trim())?
            .ok_or_else(|| anyhow::anyhow!("课程 '{}' 不存在", w.course.trim()))?;
        let classes = store.get_classes_by_course(course.id)?;

        if w.class.trim().is_empty() {
            // 只给课程码 → 该课程全部班级
            for c in &classes {
                if let Some(sec) = store.get_section(c.id)? {
                    push(&sec, &mut out);
                }
            }
            continue;
        }

        let target = classes
            .iter()
            .find(|c| c.name == w.class.trim())
            .ok_or_else(|| {
                anyhow::anyhow!("课程 '{}' 下没有名为 '{}' 的班级", w.course.trim(), w.class.trim())
            })?;
        let sec = store
            .get_section(target.id)?
            .ok_or_else(|| anyhow::anyhow!("教学班 {} 读取失败", target.id))?;

        // 学期若显式给出则不匹配就报错，避免挂错班
        if !w.term.trim().is_empty() && sec.term != w.term.trim() {
            anyhow::bail!(
                "教学班 '{}' 的学期是 '{}'，与配置的 '{}' 不符",
                sec.name,
                sec.term,
                w.term.trim()
            );
        }
        push(&sec, &mut out);
    }
    Ok(out)
}

/// 命令行托管项解析：`[CS201 信工2201 CS202]` → `[{CS201,信工2201},{CS202,""}]`
///
/// 约定**成对**出现；奇数个参数时最后一个视为「整门课」。
pub fn parse_serve_args(args: &[String]) -> Vec<EduServeSection> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let course = args[i].trim().to_string();
        if course.is_empty() {
            i += 1;
            continue;
        }
        // 下一个参数若不像「课程码」就不能当班级名（课程码统一是大写字母+数字）
        let class = match args.get(i + 1) {
            Some(c) if !looks_like_course_code(c) => {
                i += 2;
                c.trim().to_string()
            }
            _ => {
                i += 1;
                String::new()
            }
        };
        out.push(EduServeSection { course, class, term: String::new() });
    }
    out
}

/// 课程码判定：全大写字母/数字，且含字母（用来和中文班级名区分）
fn looks_like_course_code(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------------------------------------------------------------------------
// 命令入口
// ---------------------------------------------------------------------------

/// `rhermes edu teacher serve [...]
pub async fn run_serve(config_path: &Path, args: &[String]) -> anyhow::Result<()> {
    let cfg = Config::load(config_path).map_err(|e| anyhow::anyhow!("读取配置失败：{e}"))?;
    let home = home_dir(config_path);
    let db_path = home.join("edu.db");
    let store = EduStore::open(&db_path)?;

    let teacher = pick_teacher(&store, &cfg.edu.teacher.account)?;

    // 托管清单优先级：命令行 > 配置文件 > 全部
    let flags: Vec<String> = args.iter().filter(|a| a.starts_with("--")).cloned().collect();
    let positional: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    let from_cli = parse_serve_args(&positional);
    let all_flag = flags.iter().any(|f| f == "--all");
    let offline = flags.iter().any(|f| f == "--offline");

    let wanted: Vec<EduServeSection> = if !from_cli.is_empty() {
        from_cli
    } else if all_flag || cfg.edu.teacher.serve.is_empty() {
        Vec::new() // 空 = 全部
    } else {
        cfg.edu.teacher.serve.clone()
    };

    let mut sections = resolve_sections(&store, teacher.id, &wanted)?;
    if sections.is_empty() {
        anyhow::bail!(
            "老师 '{}'（id={}）名下没有任何教学班可托管 —— 先用 `rhermes-teacher class create <课程码> <班级名>` 建班",
            teacher.name,
            teacher.id
        );
    }

    // ── 身份：每位老师一套凭据（工号 → 密钥文件）──
    let owner = if cfg.edu.teacher.account.trim().is_empty() {
        teacher.name.clone()
    } else {
        cfg.edu.teacher.account.trim().to_string()
    };
    let key = identity::load_or_import(
        &identity::identity_dir(&home),
        &owner,
        &cfg.edu.teacher.secret_key,
    )?;

    let mut rt = TeacherRuntime::start_with_key(db_path.clone(), offline, Some(key)).await?;
    let addr = rt.node.endpoint_addr();

    println!();
    println!("👩‍🏫 教学班托管服务已启动");
    println!("   老师     : {}（id={}）", teacher.name, teacher.id);
    println!("   身份     : {} · {}", owner, identity::fingerprint(rt.node.secret_key()));
    println!("   模式     : {}", if offline { "离网直连（无中继）" } else { "N0 中继 + 发现" });
    println!("   老师地址 : {}", addr.id);
    for a in addr.ip_addrs() {
        println!("              ip:{}", a);
    }
    for r in addr.relay_urls() {
        println!("              relay:{}", r);
    }
    println!();

    // ── 教师 AI 答疑 Provider（修复 v0.7.16：旧版收到学生提问无人消费）──
    let ai = runtime::ai_from_config(&cfg).map(std::sync::Arc::new);
    match &ai {
        Some(a) => println!(
            "   🤖 AI 答疑：{} @ {}（模型 {}）",
            "已启用",
            a.base_url,
            a.model
        ),
        None => println!("   🤖 AI 答疑：未配置（[agent].default_provider）——仅记录提问"),
    }
    println!();

    // AI 答疑配置 + 教师真名注入；答疑循环随每条会话创建路径自动启动（含轮换重建）
    rt.set_answer_config(ai.clone(), teacher.name.clone());

    // ── 逐班入班：建会话 + 广播白名单 ──
    for s in sections.iter_mut() {
        match rt.ensure_session(s.section_id).await {
            Ok(_) => {
                match rt.publish_allowlist(s.section_id).await {
                    Ok(signed) => {
                        s.online = true;
                        println!(
                            "   ✅ [{}] {} / {} · 学生 {} 人 · epoch {}",
                            s.section_id,
                            s.course_code,
                            s.class_name,
                            signed.student_count(),
                            signed.epoch
                        );
                    }
                    Err(e) => println!(
                        "   ⚠️  [{}] {} 白名单广播失败：{e}",
                        s.section_id, s.class_name
                    ),
                }
            }
            Err(e) => println!("   ❌ [{}] {} 入班失败：{e}", s.section_id, s.class_name),
        }
    }
    println!();
    println!("   把「老师地址」填进学生的 [[edu.student.teachers]] 即可接入。");
    println!("   输入 help 查看可用命令，quit 退出。");
    println!();

    repl(&mut rt, &store, &mut sections, &teacher).await;

    println!("👋 托管服务已停止");
    Ok(())
}

/// 交互台
async fn repl(
    rt: &mut TeacherRuntime,
    store: &EduStore,
    sections: &mut [ServedSection],
    teacher: &Teacher,
) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("edu> ");
        let _ = std::io::stdout().flush();

        let line = tokio::select! {
            l = lines.next_line() => match l {
                Ok(Some(l)) => l,
                _ => break, // EOF / 读错 → 退出
            },
            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");

        match cmd {
            "quit" | "exit" | "q" => break,
            "help" | "?" => print_help(),
            "list" | "ls" => {
                println!("   id   课程        班级                      学期          状态");
                for s in sections.iter() {
                    println!(
                        "   {:<4} {:<10} {:<24} {:<12} {}",
                        s.section_id,
                        s.course_code,
                        s.class_name,
                        if s.term.is_empty() { "-" } else { &s.term },
                        if s.online { "在线" } else { "离线" }
                    );
                }
            }
            "addr" => {
                let addr = rt.node.endpoint_addr();
                println!("   {}", addr.id);
                for a in addr.ip_addrs() {
                    println!("   ip:{}", a);
                }
                for r in addr.relay_urls() {
                    println!("   relay:{}", r);
                }
            }
            "whoami" => {
                println!("   {}（id={}）· {}", teacher.name, teacher.id, identity::fingerprint(rt.node.secret_key()));
            }
            "announce" => {
                let rest: Vec<&str> = parts.collect();
                match (rest.first(), rest.get(1)) {
                    (Some(code), Some(class)) => {
                        let body = rest[2..].join(" ");
                        match find_served(sections, store, code, class) {
                            Ok(id) => {
                                let (title, text) = split_title_body(&body);
                                match rt.announce(id, &title, &text).await {
                                    Ok(_) => println!("   📣 已广播到 [{}] {} / {}", id, code, class),
                                    Err(e) => println!("   ❌ 广播失败：{e}"),
                                }
                            }
                            Err(e) => println!("   ❌ {e}"),
                        }
                    }
                    _ => println!("   用法：announce <课程码> <班级> <正文>"),
                }
            }
            "allowlist" | "refresh" => {
                let ids: Vec<i64> = sections.iter().map(|s| s.section_id).collect();
                let (mut ok, mut fail) = (0usize, Vec::new());
                for id in ids {
                    match rt.publish_allowlist(id).await {
                        Ok(_) => ok += 1,
                        Err(e) => fail.push(format!("[{id}] {e}")),
                    }
                }
                println!("   🔄 已刷新 {ok} 个班的白名单");
                for f in fail {
                    println!("   ❌ {f}");
                }
            }
            other => println!("   未知命令 `{other}` —— 输入 help 查看帮助"),
        }
    }
}

/// 在已托管的班里按「课程码 + 班级名」定位
fn find_served(
    sections: &[ServedSection],
    store: &EduStore,
    course_code: &str,
    class_name: &str,
) -> anyhow::Result<i64> {
    let course = store
        .get_course(course_code)?
        .ok_or_else(|| anyhow::anyhow!("课程 '{course_code}' 不存在"))?;
    let classes = store.get_classes_by_course(course.id)?;
    let target = classes
        .iter()
        .find(|c| c.name == class_name)
        .ok_or_else(|| anyhow::anyhow!("课程 '{course_code}' 下没有班级 '{class_name}'"))?;
    sections
        .iter()
        .find(|s| s.section_id == target.id)
        .map(|s| s.section_id)
        .ok_or_else(|| {
            anyhow::anyhow!("教学班 '{class_name}' 不在本次托管清单里 —— 先 list 看看")
        })
}

/// 「标题‖正文」拆分：以 `|` 分隔；没有则整段当正文，标题取首行截断
fn split_title_body(body: &str) -> (String, String) {
    match body.split_once('|') {
        Some((t, b)) => (t.trim().to_string(), b.trim().to_string()),
        None => {
            let title: String = body.chars().take(20).collect();
            (title, body.to_string())
        }
    }
}

fn print_help() {
    println!("   命令：");
    println!("     list                      列出本次托管的教学班");
    println!("     addr                      打印本机老师地址（给学生填配置）");
    println!("     whoami                    打印本机老师身份与指纹");
    println!("     announce <课程码> <班级> <正文>   广播公告（正文可用 | 分隔标题）");
    println!("     refresh                   重新签名并广播白名单");
    println!("     quit                      退出");
}

/// `home/` 目录（与 `edu.db`、`edu_identities/` 同级）
pub fn home_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("home")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_serve_args_pairs() {
        let a: Vec<String> = ["CS201", "信工2201", "CS202", "信工2202"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = parse_serve_args(&a);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].course, "CS201");
        assert_eq!(got[0].class, "信工2201");
        assert_eq!(got[1].course, "CS202");
        assert_eq!(got[1].class, "信工2202");
    }

    #[test]
    fn test_parse_serve_args_course_only() {
        // 只给课程码、或奇数个参数 → 该课程全部班级
        let a: Vec<String> = vec!["CS201".to_string()];
        let got = parse_serve_args(&a);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].course, "CS201");
        assert!(got[0].class.is_empty());

        let a: Vec<String> = ["CS201", "信工2201", "CS202"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = parse_serve_args(&a);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].course, "CS202");
        assert!(got[1].class.is_empty());
    }

    #[test]
    fn test_looks_like_course_code() {
        assert!(looks_like_course_code("CS201"));
        assert!(looks_like_course_code("cs201"));
        assert!(looks_like_course_code("CS-201"));
        // 中文班级名不能当课程码
        assert!(!looks_like_course_code("信工2201"));
        assert!(!looks_like_course_code(""));
        // 纯数字也不是课程码
        assert!(!looks_like_course_code("201"));
    }

    #[test]
    fn test_split_title_body() {
        let (t, b) = split_title_body("开学通知|本周日调课");
        assert_eq!(t, "开学通知");
        assert_eq!(b, "本周日调课");

        let (t, b) = split_title_body("只有正文");
        assert_eq!(t, "只有正文");
        assert_eq!(b, "只有正文");
    }

    #[test]
    fn test_pick_teacher_single_auto_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        store.create_teacher("张老师", "pw").unwrap();

        // account 为空 + 库里只有一位 → 自动认领
        let t = pick_teacher(&store, "").unwrap();
        assert_eq!(t.name, "张老师");

        // account 精确匹配
        assert_eq!(pick_teacher(&store, "张老师").unwrap().id, t.id);
        // 不存在的名字要报错，而不是随便返回一个
        assert!(pick_teacher(&store, "李老师").is_err());
    }

    #[test]
    fn test_pick_teacher_ambiguous_requires_account() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        store.create_teacher("张老师", "pw").unwrap();
        store.create_teacher("李老师", "pw").unwrap();

        let err = pick_teacher(&store, "").unwrap_err().to_string();
        assert!(err.contains("指定"), "多位老师必须要求显式指定：{err}");
    }

    #[test]
    fn test_resolve_sections_from_config_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        let t = store.create_teacher("张老师", "pw").unwrap();
        let c1 = store.create_course("CS201", "数据结构", t.id).unwrap();
        store.create_class("信工2201", c1.id).unwrap();
        store.create_class("信工2202", c1.id).unwrap();
        let c2 = store.create_course("CS202", "操作系统", t.id).unwrap();
        store.create_class("电气2201", c2.id).unwrap();

        // 空 = 全部（3 个班）
        assert_eq!(resolve_sections(&store, t.id, &[]).unwrap().len(), 3);

        // 指定到班
        let w = vec![EduServeSection {
            course: "CS201".into(),
            class: "信工2201".into(),
            term: String::new(),
        }];
        let got = resolve_sections(&store, t.id, &w).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].class_name, "信工2201");
        assert_eq!(got[0].course_code, "CS201");

        // 只给课程码 = 该课程全部班级
        let w = vec![EduServeSection {
            course: "CS201".into(),
            class: String::new(),
            term: String::new(),
        }];
        assert_eq!(resolve_sections(&store, t.id, &w).unwrap().len(), 2);

        // 重复项去重
        let w = vec![
            EduServeSection { course: "CS202".into(), class: "电气2201".into(), term: String::new() },
            EduServeSection { course: "CS202".into(), class: "电气2201".into(), term: String::new() },
        ];
        assert_eq!(resolve_sections(&store, t.id, &w).unwrap().len(), 1);

        // 不存在的班必须报错（不能静默漏挂）
        let w = vec![EduServeSection {
            course: "CS201".into(),
            class: "信工9999".into(),
            term: String::new(),
        }];
        assert!(resolve_sections(&store, t.id, &w).is_err());
    }

    #[test]
    fn test_resolve_sections_term_mismatch_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        let t = store.create_teacher("张老师", "pw").unwrap();
        let c = store.create_course("CS201", "数据结构", t.id).unwrap();
        let class = store.create_class("信工2201", c.id).unwrap();
        // create_class 的 term 是空串 → 显式给出非空学期即视为不匹配
        let w = vec![EduServeSection {
            course: "CS201".into(),
            class: "信工2201".into(),
            term: "2026-2027-1".into(),
        }];
        let err = resolve_sections(&store, t.id, &w).unwrap_err().to_string();
        assert!(err.contains("学期"), "学期不符必须拦下：{err}");
        assert!(store.get_section(class.id).unwrap().is_some());
    }
}
